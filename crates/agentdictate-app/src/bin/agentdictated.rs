use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use agentdictate_app::{
    AgentProcess, AppPaths, SERVICE_ARGUMENT, START_SERVICE_ARGUMENT, bootstrap_daemon_service,
    init_file_logging, is_overlay_helper_argument, run_overlay_helper,
    settings_executable_for_current_process, start_hotkey_listener, start_overlay_presenter,
    start_system_tray,
};
use agentdictate_core::{ClientCommand, ServerMessageKind};
use agentdictate_runtime::{IpcClient, IpcServer};

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn main() -> anyhow::Result<()> {
    let argument = std::env::args().nth(1);
    let paths = AppPaths::from_environment()?;
    let _log_guard = init_file_logging(&paths.logs, "agentdictated.log")?;
    if is_overlay_helper_argument(argument.as_deref()) {
        tracing::info!("transient recording overlay starting");
        return run_overlay_helper();
    }
    if argument.as_deref() != Some(SERVICE_ARGUMENT) {
        if argument.is_some() && argument.as_deref() != Some(START_SERVICE_ARGUMENT) {
            anyhow::bail!(
                "unknown agentdictated argument: {}",
                argument.as_deref().unwrap_or_default()
            )
        }
        tracing::info!("daemon service bootstrap starting");
        let executable = std::env::current_exe()?;
        return bootstrap_daemon_service(&paths.runtime, &paths.daemon_service_file, &executable);
    }
    run_daemon(paths)
}

fn run_daemon(paths: AppPaths) -> anyhow::Result<()> {
    tracing::info!("native daemon starting");
    let runtime = paths.runtime.clone();
    let server = IpcServer::bind(&paths.runtime)?;
    let mut process = AgentProcess::open(paths)?;
    let overlay_presenter = match std::env::current_exe()
        .map_err(anyhow::Error::from)
        .and_then(|executable| start_overlay_presenter(executable).map_err(anyhow::Error::from))
    {
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
    let _chatgpt_dictation_importer = match process.start_chatgpt_dictation_importer() {
        Ok(thread) => Some(thread),
        Err(error) => {
            tracing::warn!(%error, "could not start ChatGPT dictation usage importer");
            None
        }
    };
    let show_tray_icon = process.show_tray_icon();
    let process = Arc::new(Mutex::new(process));
    let ipc_thread = std::thread::Builder::new()
        .name("agentdictate-ipc".into())
        .spawn(move || {
            loop {
                match process.lock() {
                    Ok(process) if process.should_quit() => break,
                    Ok(_) => {}
                    Err(_) => {
                        tracing::error!("daemon process lock is poisoned");
                        break;
                    }
                }
                if let Err(error) = server.serve_next_concurrent(Arc::clone(&process)) {
                    tracing::warn!(%error, "could not accept IPC session");
                }
            }
        })?;
    if let Err(error) = start_signal_listener(runtime.clone()) {
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
        .map_err(|_| anyhow::anyhow!("daemon IPC thread panicked"))?;
    if let Some(thread) = overlay_presenter {
        thread
            .join()
            .map_err(|_| anyhow::anyhow!("overlay presenter thread panicked"))?;
    }
    Ok(())
}

fn start_signal_listener(runtime: std::path::PathBuf) -> anyhow::Result<()> {
    let mut signals = signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
    ])?;
    let request_shutdown = move || -> anyhow::Result<()> {
        let (mut client, _) = IpcClient::connect(&runtime)?;
        let request_id = REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        let response = client.send(ClientCommand::quit(request_id))?;
        if let ServerMessageKind::CommandRejected { error, .. } = response.kind {
            anyhow::bail!(error)
        }
        Ok(())
    };
    std::thread::Builder::new()
        .name("agentdictate-signals".into())
        .spawn(move || {
            if signals.forever().next().is_some()
                && let Err(error) = request_shutdown()
            {
                tracing::error!(%error, "graceful shutdown request failed");
            }
        })?;
    Ok(())
}
