use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Sender},
    thread::JoinHandle,
};

use ksni::blocking::TrayMethods;

use crate::{DaemonHandle, Trigger, TriggerOutcome, startup::running_app_image};

/// User intent emitted by the desktop tray. Menu callbacks only enqueue these
/// values; daemon and process work happens away from the status-notifier
/// thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayAction {
    OpenSettings,
    ToggleDictation,
    StartLiteral,
    /// Discards a recording, or stops waiting for a transcription, whose
    /// result then waits in Recovery.
    Cancel,
    Quit,
}

#[derive(Debug)]
struct AgentDictateTray {
    actions: Sender<TrayAction>,
}

impl ksni::Tray for AgentDictateTray {
    fn id(&self) -> String {
        "agentdictate".to_owned()
    }

    fn title(&self) -> String {
        "AgentDictate".to_owned()
    }

    fn icon_name(&self) -> String {
        "agentdictate".to_owned()
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{MenuItem, StandardItem};

        let open_actions = self.actions.clone();
        let toggle_actions = self.actions.clone();
        let quit_actions = self.actions.clone();
        let literal_actions = self.actions.clone();
        let cancel_actions = self.actions.clone();
        vec![
            StandardItem {
                label: "Open AgentDictate".to_owned(),
                icon_name: "agentdictate".to_owned(),
                activate: Box::new(move |_| {
                    let _ = open_actions.send(TrayAction::OpenSettings);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Toggle dictation".to_owned(),
                activate: Box::new(move |_| {
                    let _ = toggle_actions.send(TrayAction::ToggleDictation);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Start literal dictation".into(),
                activate: Box::new(move |_| {
                    let _ = literal_actions.send(TrayAction::StartLiteral);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Cancel dictation".into(),
                activate: Box::new(move |_| {
                    let _ = cancel_actions.send(TrayAction::Cancel);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit AgentDictate".to_owned(),
                icon_name: "application-exit".to_owned(),
                activate: Box::new(move |_| {
                    let _ = quit_actions.send(TrayAction::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Keeps the status-notifier service and its nonblocking action worker alive.
pub struct SystemTrayHandle {
    _tray: ksni::blocking::Handle<AgentDictateTray>,
    _worker: JoinHandle<()>,
}

/// Starts the native status-notifier item. Missing desktop tray support is
/// treated as an offline watcher by ksni, so the daemon and global shortcut
/// remain available even when the shell has no tray extension.
pub fn start_system_tray(
    handle: DaemonHandle,
    settings_executable: PathBuf,
) -> anyhow::Result<SystemTrayHandle> {
    let (actions, incoming) = mpsc::channel();
    let worker = std::thread::Builder::new()
        .name("agentdictate-tray-actions".to_owned())
        .spawn(move || {
            while let Ok(action) = incoming.recv() {
                if let Err(error) = execute_tray_action(action, &handle, &settings_executable) {
                    tracing::error!(?action, %error, "tray action failed");
                }
            }
        })?;
    let tray = AgentDictateTray { actions }
        .assume_sni_available(true)
        .spawn()?;
    Ok(SystemTrayHandle {
        _tray: tray,
        _worker: worker,
    })
}

/// Resolves a stable settings launcher for installed binaries and AppImages.
pub fn settings_executable_for_current_process() -> std::io::Result<PathBuf> {
    let executable = std::env::current_exe()?;
    Ok(running_app_image(&executable).unwrap_or_else(|| executable.with_file_name("agentdictate")))
}

fn execute_tray_action(
    action: TrayAction,
    handle: &DaemonHandle,
    settings_executable: &Path,
) -> anyhow::Result<()> {
    let trigger = match action {
        TrayAction::OpenSettings => {
            drop(
                Command::new(settings_executable)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()?,
            );
            return Ok(());
        }
        TrayAction::Quit => return Ok(handle.quit()?),
        TrayAction::ToggleDictation => Trigger::TrayToggle,
        TrayAction::StartLiteral => Trigger::TrayStartLiteral,
        TrayAction::Cancel => Trigger::TrayCancel,
    };
    match handle.trigger(trigger) {
        TriggerOutcome::Failed(error) => anyhow::bail!(error),
        outcome => {
            tracing::info!(?action, ?outcome, "tray action completed");
            Ok(())
        }
    }
}
