use std::{
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use agentdictate_app::{
    AgentProcess, AppPaths, DaemonHandle, NEWER_DATABASE_EXIT_STATUS, SERVICE_ARGUMENT,
    START_SERVICE_ARGUMENT, connect_or_start_daemon, daemon_exit_status,
    follow_notification_actions, init_file_logging, settings_executable_for_current_process,
    signal_status_changes, start_hotkey_listener, start_overlay_presenter, start_session_notifier,
    start_system_tray,
};
use agentdictate_runtime::{IpcClient, IpcServer, load_settings};

/// Exits with `daemon_exit_status` on failure, so systemd does not restart a
/// daemon that is older than its database.
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:?}");
            ExitCode::from(daemon_exit_status(&error))
        }
    }
}

fn run() -> anyhow::Result<()> {
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
    let (mut process, recorder_events) = AgentProcess::open(paths).inspect_err(|error| {
        if daemon_exit_status(error) == NEWER_DATABASE_EXIT_STATUS {
            tracing::error!(
                %error,
                "this AgentDictate is older than its history database, so it stops and is \
                 not restarted; update AgentDictate to the latest version"
            );
        }
    })?;
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
    let (notification_actions, clicked_notifications) = std::sync::mpsc::channel();
    if let Some(notifier) = start_session_notifier(notification_actions) {
        process.set_notifier(notifier);
    }
    let show_tray_icon = process.show_tray_icon();
    let settings_executable = settings_executable_for_current_process()
        .inspect_err(|error| {
            tracing::warn!(%error, "settings launcher is unavailable; tray will stay hidden");
        })
        .ok();
    let handle = DaemonHandle::new(process, runtime.clone());
    if let Err(error) = signal_status_changes(handle.status(), &runtime) {
        tracing::warn!(%error, "open windows will not follow the daemon's status");
    }
    handle.forward_recorder_events(recorder_events)?;
    start_hotkey_listener(&handle)?;
    let _maintenance_thread =
        match handle.with_process(|process| process.start_post_listener_maintenance()) {
            Ok(thread) => Some(thread),
            Err(error) => {
                tracing::warn!(%error, "could not start nonessential maintenance");
                None
            }
        };
    let shutdown_failed = Arc::new(AtomicBool::new(false));
    // Serves IPC until a graceful Quit. It ends with an error when the daemon
    // can no longer work, so the process exits non-zero and systemd's
    // Restart=on-failure brings it back.
    let ipc_thread = std::thread::Builder::new()
        .name("agentdictate-ipc".into())
        .spawn({
            let shutdown_failed = Arc::clone(&shutdown_failed);
            let handle = handle.clone();
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
    if let Err(error) = start_signal_listener(handle.clone(), runtime.clone(), shutdown_failed) {
        tracing::warn!(%error, "signal listener is unavailable; daemon will continue");
    }
    if let Some(executable) = settings_executable.clone()
        && let Err(error) =
            follow_notification_actions(handle.clone(), clicked_notifications, executable)
    {
        tracing::warn!(%error, "notification buttons are unavailable");
    }
    let _tray_handle = match settings_executable {
        Some(executable) if show_tray_icon => match start_system_tray(handle, executable) {
            Ok(handle) => Some(handle),
            Err(error) => {
                tracing::warn!(%error, "native tray is unavailable; daemon will continue");
                None
            }
        },
        Some(_) | None => None,
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
    handle: DaemonHandle,
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
                && let Err(error) = handle.quit()
            {
                tracing::error!(%error, "graceful shutdown failed; exiting");
                shutdown_failed.store(true, Ordering::Release);
                // Fails harmlessly when the daemon is already shutting down.
                let _ = IpcClient::wake(&runtime);
            }
        })?;
    Ok(())
}
