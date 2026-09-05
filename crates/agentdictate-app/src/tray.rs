use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Sender},
    },
    thread::JoinHandle,
};

use agentdictate_core::{ClientCommand, ServerMessageKind, WorkflowPhase};
use agentdictate_runtime::IpcClient;
use ksni::blocking::TrayMethods;

static TRAY_REQUEST_ID: AtomicU64 = AtomicU64::new(10_000);

/// User intent emitted by the desktop tray. Menu callbacks only enqueue these
/// values; IPC and process work happens away from the status-notifier thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayAction {
    OpenSettings,
    ToggleDictation,
    StartLiteral,
    Quit,
}

/// Converts the state-sensitive tray toggle into a single daemon command.
/// Busy processing states intentionally produce no command rather than
/// replaying an action after the current dictation completes.
#[must_use]
pub const fn tray_command_for_phase(
    action: TrayAction,
    phase: WorkflowPhase,
    request_id: u64,
) -> Option<ClientCommand> {
    if matches!(action, TrayAction::StartLiteral) {
        return match phase {
            WorkflowPhase::Ready => Some(ClientCommand::start_recording_in_mode(
                request_id,
                agentdictate_core::DictationMode::Literal,
            )),
            _ => None,
        };
    }
    if !matches!(action, TrayAction::ToggleDictation) {
        return None;
    }
    match phase {
        WorkflowPhase::Ready => Some(ClientCommand::start_recording(request_id)),
        WorkflowPhase::Starting { .. } | WorkflowPhase::Recording { .. } => {
            Some(ClientCommand::stop_recording(request_id))
        }
        WorkflowPhase::Stopping { .. }
        | WorkflowPhase::Processing { .. }
        | WorkflowPhase::NeedsAttention { .. } => None,
    }
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
    runtime_directory: PathBuf,
    settings_executable: PathBuf,
) -> Result<SystemTrayHandle, ksni::Error> {
    let (actions, incoming) = mpsc::channel();
    let worker = std::thread::Builder::new()
        .name("agentdictate-tray-actions".to_owned())
        .spawn(move || {
            while let Ok(action) = incoming.recv() {
                if let Err(error) =
                    execute_tray_action(action, &runtime_directory, &settings_executable)
                {
                    tracing::error!(?action, %error, "tray action failed");
                }
            }
        })
        .expect("tray action worker should start");
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
    if let Some(app_image) = std::env::var_os("APPIMAGE") {
        Ok(app_image.into())
    } else {
        Ok(std::env::current_exe()?.with_file_name("agentdictate"))
    }
}

fn execute_tray_action(
    action: TrayAction,
    runtime_directory: &Path,
    settings_executable: &Path,
) -> anyhow::Result<()> {
    match action {
        TrayAction::OpenSettings => {
            drop(
                Command::new(settings_executable)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()?,
            );
            Ok(())
        }
        TrayAction::ToggleDictation | TrayAction::StartLiteral => {
            let (mut client, initial) = IpcClient::connect(runtime_directory)?;
            let ServerMessageKind::Snapshot { snapshot, .. } = initial.kind else {
                anyhow::bail!("daemon did not provide its current workflow")
            };
            let request_id = TRAY_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
            let Some(command) = tray_command_for_phase(action, snapshot.workflow.phase, request_id)
            else {
                tracing::info!("dictation is busy; tray toggle ignored");
                return Ok(());
            };
            reject_command_error(client.send(command)?.kind)
        }
        TrayAction::Quit => {
            let (mut client, _) = IpcClient::connect(runtime_directory)?;
            let request_id = TRAY_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
            reject_command_error(client.send(ClientCommand::quit(request_id))?.kind)
        }
    }
}

fn reject_command_error(message: ServerMessageKind) -> anyhow::Result<()> {
    if let ServerMessageKind::CommandRejected { error, .. } = message {
        anyhow::bail!(error)
    }
    Ok(())
}
