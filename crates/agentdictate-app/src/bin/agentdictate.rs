use std::sync::Arc;

use agentdictate_app::{
    AppPaths, SetupClient, WindowInstance, WorkspaceClient, WorkspaceError,
    connect_or_start_daemon, grant_native_access, init_file_logging, is_overlay_helper_argument,
    raise_open_window, run_overlay_helper,
};
use agentdictate_core::{
    ClientCommand, ClientCommandKind, HotkeyCaptureOutcome, ServerMessageKind,
};
use agentdictate_runtime::IpcClient;
use agentdictate_ui::{
    SettingsRequest, SettingsWindow, ShellViewModel, UiActionError, WindowSinks,
    run_settings_window,
};

fn main() -> anyhow::Result<()> {
    let paths = AppPaths::from_environment()?;
    let args: Vec<_> = std::env::args().skip(1).collect();
    // The daemon spawns this binary as its per-recording overlay helper so the
    // daemon itself stays free of GPUI. The helper logs into the daemon's file.
    if is_overlay_helper_argument(args.first().map(String::as_str)) {
        let _log_guard = init_file_logging(&paths.logs, "agentdictated.log")?;
        tracing::info!("transient recording overlay starting");
        return run_overlay_helper();
    }
    if let [command] = args.as_slice()
        && command == "setup-access"
    {
        // Asks for an administrator password, so it runs only on request.
        grant_native_access(&paths.native_access)?;
        println!(
            "Keyboard and paste access is set up. If AgentDictate still cannot read the \
             keyboard, log out and back in."
        );
        return Ok(());
    }
    if !args.is_empty() {
        let command = match args.as_slice() {
            [command] if command == "stop" => ClientCommandKind::StopRecording.into(),
            [command] if command == "cancel" => ClientCommandKind::Cancel.into(),
            // Bindable to a shortcut in the desktop's keyboard settings.
            [command] if command == "paste-last" => ClientCommandKind::PasteLast.into(),
            [command] if command == "start" => ClientCommand::start_recording(),
            [command, flag, mode] if command == "start" && flag == "--mode" => {
                ClientCommand::start_recording_in_mode(mode.parse().map_err(anyhow::Error::msg)?)
            }
            _ => anyhow::bail!(
                "Usage: agentdictate [start [--mode dictate|literal] | stop | cancel | paste-last | setup-access]"
            ),
        };
        let (mut client, _) = IpcClient::connect(&paths.runtime)?;
        if let ServerMessageKind::CommandRejected { error, .. } = client.send(command)?.kind {
            anyhow::bail!(error);
        }
        return Ok(());
    }
    let _log_guard = init_file_logging(&paths.logs, "agentdictate.log")?;
    let window_lock = match WindowInstance::acquire(&paths.runtime)? {
        WindowInstance::Primary(lock) => lock,
        WindowInstance::Secondary => {
            tracing::info!("settings window already open; asking it to come to the front");
            raise_open_window(&paths.runtime)?;
            return Ok(());
        }
    };
    let raise_requests = window_lock
        .raise_requests()
        .inspect_err(|error| tracing::warn!(%error, "window raise requests are unavailable"))
        .ok();
    tracing::info!("native settings window starting");
    // UI actions use short-lived sessions so a closed settings window cannot
    // retain an unnecessary daemon connection.
    let (_, initial) = connect_or_start_daemon(&paths)?;
    let ServerMessageKind::Snapshot {
        snapshot, settings, ..
    } = initial.kind
    else {
        anyhow::bail!("AgentDictate daemon rejected its initial snapshot request")
    };
    let settings = *settings;
    let runtime = paths.runtime.clone();
    // History, usage and Recovery are read from the database directly; the
    // daemon only carries out changes.
    let workspace_client = Arc::new(WorkspaceClient::new(
        runtime.clone(),
        paths.database_file.clone(),
        snapshot.clone(),
    ));
    // The watcher is registered before the one read below. A database write
    // racing with window startup is therefore either in that read or queued
    // by inotify, rather than being silently lost.
    let workspace_updates = workspace_client
        .watch()
        .inspect_err(|error| tracing::warn!(%error, "live workspace updates are unavailable"))
        .ok();
    let workspace_model = workspace_client.refresh()?;
    let workspace_action_sink = {
        let workspace_client = Arc::clone(&workspace_client);
        Arc::new(move |action| {
            workspace_client
                .perform(action)
                .map_err(|error| -> UiActionError { Box::new(error) })
        })
    };
    // Settings requests and shortcut captures each use their own short-lived
    // session.
    let send = move |command| -> Result<ServerMessageKind, UiActionError> {
        let (mut client, _) = IpcClient::connect(&runtime)
            .map_err(|error| -> UiActionError { Box::new(WorkspaceError::from(error)) })?;
        let response = client
            .send(command)
            .map_err(|error| -> UiActionError { Box::new(WorkspaceError::from(error)) })?;
        match response.kind {
            ServerMessageKind::CommandRejected { error, .. } => {
                Err(Box::new(WorkspaceError::CommandRejected { message: error }))
            }
            reply => Ok(reply),
        }
    };
    // Answers with the settings the daemon holds once it made the change.
    let settings_sink = {
        let send = send.clone();
        Arc::new(move |request| -> Result<_, UiActionError> {
            let command = match request {
                SettingsRequest::Change(change) => ClientCommand::change_setting(change),
                SettingsRequest::SetApiKey(api_key) => ClientCommand::set_api_key(api_key),
                SettingsRequest::CancelHotkeyCapture => {
                    ClientCommandKind::CancelHotkeyCapture.into()
                }
            };
            match send(command)? {
                ServerMessageKind::Snapshot { settings, .. } => Ok(*settings),
                _ => Err(Box::new(WorkspaceError::UnexpectedSettingsResponse)),
            }
        })
    };
    let hotkey_capture = Arc::new(move || -> Result<HotkeyCaptureOutcome, UiActionError> {
        match send(ClientCommandKind::CaptureHotkey.into())? {
            ServerMessageKind::HotkeyCaptured { outcome, .. } => Ok(outcome),
            _ => Err("the daemon did not answer the shortcut capture".into()),
        }
    });
    // Opens on Setup while dictation can't work yet, otherwise on Home.
    run_settings_window(SettingsWindow {
        model: ShellViewModel::from_app_snapshot(snapshot).with_workspace(workspace_model),
        settings,
        sinks: WindowSinks {
            settings: settings_sink,
            hotkey_capture,
            actions: workspace_action_sink,
            setup: Arc::new(SetupClient::new(
                Arc::clone(&workspace_client),
                paths.native_access.clone(),
            )),
        },
        workspace_updates,
        raise_requests,
    });
    drop(window_lock);
    Ok(())
}
