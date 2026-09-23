use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use agentdictate_app::{
    AgentProcess, AppPaths, DaemonHandle, SERVICE_ARGUMENT, START_SERVICE_ARGUMENT,
    connect_or_start_daemon, init_file_logging, settings_executable_for_current_process,
    start_hotkey_listener, start_overlay_presenter, start_system_tray,
};
use agentdictate_core::{ClientCommand, ServerMessageKind};
use agentdictate_runtime::{IpcClient, IpcServer, load_settings};

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn main() -> anyhow::Result<()> {
    let argument = std::env::args().nth(1);
    let paths = AppPaths::from_environment()?;
    let _log_guard = init_file_logging(&paths.logs, "agentdictated.log")?;
    match argument.as_deref() {
        Some(SERVICE_ARGUMENT) => run_daemon(paths),
        None | Some(START_SERVICE_ARGUMENT) => run_legacy_login_entry(&paths),
        Some(argument) => anyhow::bail!("unknown agentdictated argument: {argument}"),
    }
}

/// Older versions started the daemon at login from an XDG autostart entry
/// that runs `agentdictated` or `agentdictated --start-service`. The daemon
/// deletes that entry once it has enabled its unit; until then, it keeps
/// working and honours the saved "Start on login" choice.
fn run_legacy_login_entry(paths: &AppPaths) -> anyhow::Result<()> {
    if load_settings(&paths.config_file)?.start_on_login {
        tracing::info!("legacy login entry is starting the daemon service");
        connect_or_start_daemon(paths)?;
    }
    Ok(())
}

fn run_daemon(paths: AppPaths) -> anyhow::Result<()> {
    tracing::info!("native daemon starting");
    let runtime = paths.runtime.clone();
    let server = IpcServer::bind(&paths.runtime)?;
    let mut process = AgentProcess::open(paths)?;
    // The GPUI overlay runs in the sibling desktop binary, so the daemon never
    // links GPUI. Never use `$APPIMAGE` here: it would remount per dictation.
    let overlay_presenter = match std::env::current_exe()
        .map_err(anyhow::Error::from)
        .and_then(|executable| {
            start_overlay_presenter(executable.with_file_name("agentdictate"))
                .map_err(anyhow::Error::from)
        }) {
        Ok((controller, thread)) => {
            controller
                .notify_health_changes_at(runtime.join(agentdictate_app::OVERLAY_HEALTH_FILE));
            process.set_overlay_controller(controller);
            Some(thread)
        }
        Err(error) => {
            tracing::warn!(%error, "recording overlay is unavailable; dictation will continue");
            None
        }
    };
    start_hotkey_listener(&mut process, &runtime)?;
    let _maintenance_thread = match process.start_post_listener_maintenance() {
        Ok(thread) => Some(thread),
        Err(error) => {
            tracing::warn!(%error, "could not start nonessential maintenance");
            None
        }
    };
    let show_tray_icon = process.show_tray_icon();
    let handle = DaemonHandle::new(process, runtime.clone());
    let shutdown_failed = Arc::new(AtomicBool::new(false));
    // Serves IPC until a graceful Quit. It ends with an error when the daemon
    // can no longer work, so the process exits non-zero and systemd's
    // Restart=on-failure brings it back.
    let ipc_thread = std::thread::Builder::new()
        .name("agentdictate-ipc".into())
        .spawn({
            let shutdown_failed = Arc::clone(&shutdown_failed);
            move || -> anyhow::Result<()> {
                loop {
                    if shutdown_failed.load(Ordering::Acquire) {
                        anyhow::bail!("graceful shutdown failed after a termination signal");
                    }
                    if handle.should_quit() {
                        return Ok(());
                    }
                    if let Err(error) = server.serve_next_concurrent(handle.clone()) {
                        tracing::warn!(%error, "could not accept IPC session");
                    }
                }
            }
        })?;
    if let Err(error) = start_signal_listener(runtime.clone(), shutdown_failed) {
        tracing::warn!(%error, "signal listener is unavailable; daemon will continue");
    }
    let _tray_handle = if show_tray_icon {
        match settings_executable_for_current_process() {
            Ok(executable) => match start_system_tray(runtime, executable) {
                Ok(handle) => Some(handle),
                Err(error) => {
                    tracing::warn!(%error, "native tray is unavailable; daemon will continue");
                    None
                }
            },
            Err(error) => {
                tracing::warn!(%error, "settings launcher is unavailable; tray will stay hidden");
                None
            }
        }
    } else {
        None
    };
    ipc_thread
        .join()
        .map_err(|_| anyhow::anyhow!("daemon IPC thread panicked"))??;
    if let Some(thread) = overlay_presenter {
        thread
            .join()
            .map_err(|_| anyhow::anyhow!("overlay presenter thread panicked"))?;
    }
    Ok(())
}

/// Turns the first SIGINT or SIGTERM into a graceful Quit. If Quit fails, it
/// sets `shutdown_failed` and wakes the IPC loop, which then ends the daemon
/// with an error: later signals would be ignored, and an exit is safer than
/// waiting for systemd's SIGKILL. The recorder's parent-death signal
/// finalizes any WAV, and startup reconciliation keeps its job.
fn start_signal_listener(
    runtime: std::path::PathBuf,
    shutdown_failed: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let mut signals = signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
    ])?;
    std::thread::Builder::new()
        .name("agentdictate-signals".into())
        .spawn(move || {
            if signals.forever().next().is_some()
                && let Err(error) = request_shutdown(&runtime)
            {
                tracing::error!(%error, "graceful shutdown request failed; exiting");
                shutdown_failed.store(true, Ordering::Release);
                // Fails harmlessly when the daemon is already shutting down.
                let _ = IpcClient::wake(&runtime);
            }
        })?;
    Ok(())
}

fn request_shutdown(runtime: &std::path::Path) -> anyhow::Result<()> {
    let (mut client, _) = IpcClient::connect(runtime)?;
    let request_id = REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let response = client.send(ClientCommand::quit(request_id))?;
    if let ServerMessageKind::CommandRejected { error, .. } = response.kind {
        anyhow::bail!(error)
    }
    Ok(())
}
