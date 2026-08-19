use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agentdictate_app::{AppPaths, WorkspaceClient, init_file_logging};
use agentdictate_core::{ClientCommand, ServerMessageKind};
use agentdictate_runtime::{IpcClient, IpcError};
use agentdictate_ui::{
    Route, ShellViewModel, run_settings_shell_with_workspace_actions,
    run_settings_shell_with_workspace_actions_and_updates,
};

fn main() -> anyhow::Result<()> {
    let paths = AppPaths::from_environment()?;
    let _log_guard = init_file_logging(&paths.logs, "agentdictate.log")?;
    tracing::info!("native settings window starting");
    let (mut bootstrap_client, initial) = connect_or_start_daemon(&paths)?;
    let workspace = bootstrap_client.send(ClientCommand::get_workspace(1))?;
    // UI actions use short-lived sessions so a closed settings window cannot
    // retain an unnecessary daemon connection.
    drop(bootstrap_client);
    let ServerMessageKind::Snapshot {
        snapshot, settings, ..
    } = initial.kind
    else {
        anyhow::bail!("AgentDictate daemon rejected its initial snapshot request")
    };
    let settings = *settings;
    let ServerMessageKind::Workspace { workspace, .. } = workspace.kind else {
        anyhow::bail!("AgentDictate daemon did not provide workspace data")
    };
    let runtime = paths.runtime.clone();
    let workspace_client = Arc::new(WorkspaceClient::new(runtime.clone(), *workspace));
    let mut workspace_model = workspace_client.view_model().map_err(anyhow::Error::msg)?;
    let workspace_updates = match workspace_client
        .watch_with_catalog(&paths.database_file, paths.model_catalog_cache_file())
    {
        Ok(updates) => {
            // The watcher is registered before this refresh. A database write
            // racing with window startup is therefore either observed by the
            // refresh or queued by inotify, rather than being silently lost.
            match workspace_client.refresh() {
                Ok(refreshed) => workspace_model = refreshed,
                Err(error) => tracing::warn!(%error, "initial live workspace refresh failed"),
            }
            Some(updates)
        }
        Err(error) => {
            tracing::warn!(%error, "live workspace updates are unavailable");
            None
        }
    };
    let workspace_action_sink = {
        let workspace_client = Arc::clone(&workspace_client);
        Arc::new(move |action| workspace_client.perform(action))
    };
    let command_sink = Arc::new(move |command| {
        let (mut client, _) = IpcClient::connect(&runtime).map_err(|error| error.to_string())?;
        let response = client.send(command).map_err(|error| error.to_string())?;
        match response.kind {
            ServerMessageKind::CommandRejected { error, .. } => Err(error),
            ServerMessageKind::Snapshot { .. }
            | ServerMessageKind::Workspace { .. }
            | ServerMessageKind::HistoryPage { .. } => Ok(()),
        }
    });
    let model = ShellViewModel::from_app_snapshot(Route::Overview, snapshot)
        .with_workspace(workspace_model);
    match workspace_updates {
        Some(updates) => run_settings_shell_with_workspace_actions_and_updates(
            model,
            settings.values,
            settings.has_api_key,
            command_sink,
            workspace_action_sink,
            updates,
        ),
        None => run_settings_shell_with_workspace_actions(
            model,
            settings.values,
            settings.has_api_key,
            command_sink,
            workspace_action_sink,
        ),
    }
    Ok(())
}

fn connect_or_start_daemon(
    paths: &AppPaths,
) -> anyhow::Result<(IpcClient, agentdictate_core::ServerMessage)> {
    match IpcClient::connect(&paths.runtime) {
        Ok(connection) => return Ok(connection),
        Err(IpcError::Io(error))
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) => {}
        Err(error) => return Err(error.into()),
    }
    let daemon = std::env::current_exe()?.with_file_name("agentdictated");
    let child = Command::new(&daemon)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| anyhow::anyhow!("could not launch {}: {error}", daemon.display()))?;
    wait_for_daemon(child, paths, Instant::now() + Duration::from_secs(10))
}

fn wait_for_daemon(
    mut child: Child,
    paths: &AppPaths,
    deadline: Instant,
) -> anyhow::Result<(IpcClient, agentdictate_core::ServerMessage)> {
    loop {
        match IpcClient::connect(&paths.runtime) {
            Ok(connection) => {
                std::thread::Builder::new()
                    .name("agentdictate-daemon-reaper".into())
                    .spawn(move || {
                        let _ = child.wait();
                    })?;
                return Ok(connection);
            }
            Err(_) if child.try_wait()?.is_some() => {
                anyhow::bail!("AgentDictate daemon exited during startup")
            }
            Err(_) if Instant::now() >= deadline => {
                anyhow::bail!("AgentDictate daemon did not become ready")
            }
            Err(_) => std::thread::yield_now(),
        }
    }
}
