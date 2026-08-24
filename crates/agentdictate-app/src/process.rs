use std::sync::{Arc, RwLock};
use std::{io, path::PathBuf};

use agentdictate_core::{
    ClientCommand, ClientCommandKind, ClientCommandTag, HotkeyReadiness, ServerMessage, Settings,
    WorkflowPhase,
};
use agentdictate_linux::hotkey::{HotkeySignal, HotkeySpec};
use agentdictate_runtime::{
    HistoryIndexMaintenance, IpcClient, IpcHandler, RecordingPriorityGuard, Runtime, RuntimeError,
    load_settings, save_settings,
};

use crate::model_catalog::ModelCatalog;
use crate::{
    AppPaths, CodexSubscriptionTransport, Daemon, OverlayController, ReqwestOpenAiTransport,
    SpeechRouter, SystemDeliverer, SystemRecordingController, TranscriptionPipeline,
    chatgpt_dictation_import::start_chatgpt_dictation_importer, sync_startup_with_systemctl,
};

pub type ProductionTranscriber = TranscriptionPipeline<
    SpeechRouter<ReqwestOpenAiTransport, CodexSubscriptionTransport>,
    ReqwestOpenAiTransport,
>;
pub type ProductionDaemon =
    Daemon<SystemRecordingController, ProductionTranscriber, SystemDeliverer>;

/// Stable control seam used by settings updates. Implementations must only
/// return success after the live listener has accepted the new shortcut.
pub trait HotkeyReconfigurer: Send + Sync {
    fn reconfigure(&self, spec: HotkeySpec) -> anyhow::Result<()>;
}

pub struct AgentProcess {
    daemon: ProductionDaemon,
    config_file: PathBuf,
    autostart_file: PathBuf,
    daemon_service_file: PathBuf,
    systemctl_command: PathBuf,
    database_file: PathBuf,
    runtime_directory: PathBuf,
    model_catalog: ModelCatalog,
    history_index_maintenance: HistoryIndexMaintenance,
    recording_priority: Option<RecordingPriorityGuard>,
    hotkey_control: Option<Arc<dyn HotkeyReconfigurer>>,
    recording_mode_control: Option<Arc<RwLock<String>>>,
    should_quit: bool,
}

impl AgentProcess {
    pub fn open(paths: AppPaths) -> anyhow::Result<Self> {
        paths.ensure_directories()?;
        let settings = load_settings(&paths.config_file)?;
        let runtime = Runtime::open(&paths.database_file)?;
        let model_catalog = ModelCatalog::open(&paths.cache, &settings.openai_api_key);
        let speech = SpeechRouter::new(
            ReqwestOpenAiTransport::new(&settings.openai_api_key),
            CodexSubscriptionTransport::new(),
        );
        let cleanup = ReqwestOpenAiTransport::new(&settings.openai_api_key);
        let transcriber = TranscriptionPipeline::new(settings.clone(), speech, cleanup);
        let recorder = SystemRecordingController::for_system(&settings, &paths.runtime);
        let deliverer = SystemDeliverer::for_environment(&settings.paste_shortcut);
        let history_index_maintenance = HistoryIndexMaintenance::new();
        Ok(Self {
            daemon: Daemon::new(
                runtime,
                settings,
                paths.clone(),
                recorder,
                transcriber,
                deliverer,
            ),
            config_file: paths.config_file,
            autostart_file: paths.autostart_file,
            daemon_service_file: paths.daemon_service_file,
            systemctl_command: PathBuf::from("systemctl"),
            database_file: paths.database_file,
            runtime_directory: paths.runtime,
            model_catalog,
            history_index_maintenance,
            recording_priority: None,
            hotkey_control: None,
            recording_mode_control: None,
            should_quit: false,
        })
    }

    #[must_use]
    pub const fn should_quit(&self) -> bool {
        self.should_quit
    }

    #[must_use]
    pub fn hotkey(&self) -> &str {
        &self.daemon.settings().hotkey
    }

    #[must_use]
    pub fn recording_mode(&self) -> &str {
        &self.daemon.settings().recording_mode
    }

    #[must_use]
    pub const fn show_tray_icon(&self) -> bool {
        self.daemon.settings().show_tray_icon
    }

    pub fn set_hotkey_readiness(&mut self, readiness: HotkeyReadiness) {
        self.daemon.set_hotkey_readiness(readiness);
    }

    pub fn set_overlay_controller(&mut self, controller: OverlayController) {
        self.daemon.set_overlay_controller(controller);
    }

    pub fn set_hotkey_reconfigurer(&mut self, control: Arc<dyn HotkeyReconfigurer>) {
        self.hotkey_control = Some(control);
    }

    pub fn set_recording_mode_control(&mut self, recording_mode: Arc<RwLock<String>>) {
        self.recording_mode_control = Some(recording_mode);
    }

    /// Starts reconciliation that is useful but must never delay or prevent
    /// the native shortcut listener from starting. Work failures are logged
    /// and the daemon remains available.
    pub fn start_post_listener_maintenance(&self) -> std::io::Result<std::thread::JoinHandle<()>> {
        let settings = self.daemon.settings().clone();
        if let Err(error) = self
            .model_catalog
            .refresh_in_background(&settings.openai_api_key)
        {
            tracing::warn!(%error, "could not start OpenAI model discovery");
        }
        let autostart_file = self.autostart_file.clone();
        let daemon_service_file = self.daemon_service_file.clone();
        let systemctl_command = self.systemctl_command.clone();
        let database_file = self.database_file.clone();
        let history_index_maintenance = self.history_index_maintenance.clone();
        std::thread::Builder::new()
            .name("agentdictate-maintenance".into())
            .spawn(move || {
                run_post_listener_maintenance(
                    &settings,
                    &autostart_file,
                    &daemon_service_file,
                    &systemctl_command,
                    &database_file,
                    &history_index_maintenance,
                );
            })
    }

    /// Watches the ChatGPT desktop receipt directory and adds completed
    /// dictations to AgentDictate's usage totals.
    pub fn start_chatgpt_dictation_importer(&self) -> std::io::Result<std::thread::JoinHandle<()>> {
        start_chatgpt_dictation_importer(self.database_file.clone())
    }

    fn snapshot_message(&self, request_id: u64) -> ServerMessage {
        ServerMessage::snapshot(request_id, self.daemon.snapshot(), self.daemon.settings())
    }

    fn workspace_message(&self, request_id: u64) -> Result<ServerMessage, RuntimeError> {
        let mut workspace = self.daemon.workspace_snapshot()?;
        workspace.model_catalog = self.model_catalog.snapshot(
            self.daemon.settings().active_transcription_model(),
            self.daemon.settings().active_cleanup_model(),
        );
        Ok(ServerMessage::workspace(request_id, workspace))
    }

    fn history_page_message(
        &self,
        request_id: u64,
        request: agentdictate_core::HistoryPageRequest,
    ) -> Result<ServerMessage, RuntimeError> {
        Ok(ServerMessage::history_page(
            request_id,
            self.daemon.history_page_snapshot(request)?,
        ))
    }

    fn update_settings(&mut self, mut settings: Settings) -> anyhow::Result<()> {
        let start_on_login_changed =
            settings.start_on_login != self.daemon.settings().start_on_login;
        let hotkey_changed = settings.hotkey != self.daemon.settings().hotkey;
        let recording_mode_changed =
            settings.recording_mode != self.daemon.settings().recording_mode;
        let parsed_hotkey = hotkey_changed
            .then(|| settings.hotkey.parse::<HotkeySpec>())
            .transpose()?;
        settings.openai_api_key = self.daemon.settings().openai_api_key.clone();
        let old_hotkey = hotkey_changed
            .then(|| self.daemon.settings().hotkey.parse::<HotkeySpec>().ok())
            .flatten();
        if let Some(spec) = parsed_hotkey.as_ref() {
            self.hotkey_control
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("hotkey listener is unavailable"))?
                .reconfigure(spec.clone())?;
        }
        if let Err(error) = save_settings(&self.config_file, &settings) {
            if parsed_hotkey.is_some()
                && let Some(control) = &self.hotkey_control
                && let Some(old_hotkey) = old_hotkey
                && let Err(rollback_error) = control.reconfigure(old_hotkey)
            {
                self.daemon
                    .set_hotkey_readiness(HotkeyReadiness::Unavailable {
                        message: format!(
                            "Settings were not saved and the shortcut rollback failed: {rollback_error}"
                        ),
                    });
                anyhow::bail!(
                    "could not save settings: {error}; shortcut rollback also failed: {rollback_error}"
                );
            }
            return Err(error.into());
        }
        if let Err(error) = self.daemon.sync_pricing(&settings) {
            // The settings file is authoritative and startup will reconcile
            // pricing again. Do not report a failed save after it committed.
            tracing::error!(%error, "could not synchronize pricing cache");
        }
        if start_on_login_changed {
            match std::env::current_exe() {
                Ok(executable) => {
                    let daemon_executable = executable.with_file_name("agentdictated");
                    if let Err(error) = sync_startup_with_systemctl(
                        &self.autostart_file,
                        &self.daemon_service_file,
                        settings.start_on_login,
                        &daemon_executable,
                        &self.systemctl_command,
                    ) {
                        tracing::warn!(%error, "settings saved but login startup reconciliation failed");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "settings saved but daemon executable was not found");
                }
            }
        }
        self.daemon
            .transcriber_mut()
            .update_settings(settings.clone());
        self.daemon.recorder_mut().update_settings(&settings);
        self.daemon
            .deliverer_mut()
            .update_shortcut(&settings.paste_shortcut);
        self.daemon.update_settings(settings);
        if recording_mode_changed && let Some(mode) = &self.recording_mode_control {
            *mode
                .write()
                .map_err(|_| anyhow::anyhow!("recording-mode control is unavailable"))? =
                self.daemon.settings().recording_mode.clone();
        }
        Ok(())
    }

    fn set_api_key(&mut self, api_key: &str) -> anyhow::Result<()> {
        let mut settings = self.daemon.settings().clone();
        settings.openai_api_key = api_key.trim().to_owned();
        save_settings(&self.config_file, &settings)?;
        let transcriber = self.daemon.transcriber_mut();
        transcriber
            .speech_mut()
            .openai_mut()
            .set_api_key(&settings.openai_api_key);
        transcriber
            .cleanup_mut()
            .set_api_key(&settings.openai_api_key);
        transcriber.update_settings(settings.clone());
        self.daemon.update_settings(settings);
        self.refresh_model_catalog()?;
        Ok(())
    }

    fn refresh_model_catalog(&self) -> io::Result<()> {
        self.model_catalog
            .refresh_in_background(&self.daemon.settings().openai_api_key)?;
        Ok(())
    }

    fn begin_recording_priority(&mut self) {
        if self.recording_priority.is_none() {
            self.recording_priority = Some(self.history_index_maintenance.prioritize_recording());
        }
    }

    fn synchronize_recording_priority(&mut self) {
        let is_recording = matches!(
            self.daemon.snapshot().workflow.phase,
            WorkflowPhase::Starting { .. } | WorkflowPhase::Recording { .. }
        );
        if !is_recording {
            self.recording_priority = None;
        }
    }
}

fn run_post_listener_maintenance(
    settings: &Settings,
    autostart_file: &std::path::Path,
    daemon_service_file: &std::path::Path,
    systemctl_command: &std::path::Path,
    database_file: &std::path::Path,
    history_index_maintenance: &HistoryIndexMaintenance,
) {
    match std::env::current_exe() {
        Ok(executable) => {
            let daemon_executable = executable.with_file_name("agentdictated");
            if let Err(error) = sync_startup_with_systemctl(
                autostart_file,
                daemon_service_file,
                settings.start_on_login,
                &daemon_executable,
                systemctl_command,
            ) {
                tracing::warn!(%error, "could not reconcile login startup");
            }
        }
        Err(error) => tracing::warn!(%error, "could not locate daemon for autostart"),
    }
    let mut runtime = match Runtime::open(database_file) {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::warn!(%error, "could not open maintenance database connection");
            return;
        }
    };
    if let Err(error) = runtime.sync_pricing(settings) {
        tracing::warn!(%error, "could not synchronize pricing cache");
    }
    match runtime.backfill_delivered_sessions(settings) {
        Ok(0) => {}
        Ok(repaired_history) => {
            tracing::info!(repaired_history, "repaired delivered session history");
        }
        Err(error) => tracing::warn!(%error, "could not repair delivered session history"),
    }
    if let Err(error) = history_index_maintenance.prepare_history_search(&mut runtime) {
        tracing::warn!(%error, "could not prepare indexed transcript search");
    }
}

impl IpcHandler for AgentProcess {
    fn snapshot(&self, request_id: u64) -> ServerMessage {
        self.snapshot_message(request_id)
    }

    fn handle(&mut self, command: ClientCommand) -> ServerMessage {
        let command_tag = command.kind();
        if command_tag == ClientCommandTag::StartRecording {
            self.begin_recording_priority();
        }
        let request_id = request_id(&command.kind);
        let history_request = match &command.kind {
            ClientCommandKind::GetHistoryPage { request, .. } => Some(request.clone()),
            _ => None,
        };
        let returns_workspace = matches!(
            command_tag,
            ClientCommandTag::GetWorkspace
                | ClientCommandTag::RefreshModelCatalog
                | ClientCommandTag::RetryTranscription
                | ClientCommandTag::RetryDelivery
                | ClientCommandTag::DeleteRecovery
                | ClientCommandTag::CreateReplacement
                | ClientCommandTag::UpdateReplacement
                | ClientCommandTag::DeleteReplacement
                | ClientCommandTag::DeleteHistory
                | ClientCommandTag::ClearHistory
                | ClientCommandTag::CopyTranscript
        );
        let result: anyhow::Result<()> = match command.kind {
            ClientCommandKind::GetSnapshot { .. } => Ok(()),
            ClientCommandKind::GetWorkspace { .. } => Ok(()),
            ClientCommandKind::RefreshModelCatalog { .. } => {
                self.refresh_model_catalog().map_err(Into::into)
            }
            ClientCommandKind::GetHistoryPage { .. } => Ok(()),
            ClientCommandKind::StartRecording { .. } => self
                .daemon
                .start_recording()
                .map(|_| ())
                .map_err(Into::into),
            ClientCommandKind::StopRecording { .. } => {
                self.daemon.stop_recording().map(|_| ()).map_err(Into::into)
            }
            ClientCommandKind::Cancel { .. } => self
                .daemon
                .discard_recording()
                .map(|_| ())
                .map_err(Into::into),
            ClientCommandKind::RecorderExited { job_id, .. } => self
                .daemon
                .recorder_exited(job_id)
                .map(|_| ())
                .map_err(Into::into),
            ClientCommandKind::RetryTranscription { job_id, .. } => self
                .daemon
                .retry_transcription(job_id)
                .map(|_| ())
                .map_err(Into::into),
            ClientCommandKind::RetryDelivery { job_id, .. } => self
                .daemon
                .retry_delivery(job_id)
                .map(|_| ())
                .map_err(Into::into),
            ClientCommandKind::DeleteRecovery { job_id, .. } => self
                .daemon
                .delete_recovery(job_id)
                .map(|_| ())
                .map_err(Into::into),
            ClientCommandKind::CreateReplacement { rule, .. } => self
                .daemon
                .create_replacement(rule)
                .map(|_| ())
                .map_err(Into::into),
            ClientCommandKind::UpdateReplacement { rule, .. } => self
                .daemon
                .update_replacement(rule)
                .map(|_| ())
                .map_err(Into::into),
            ClientCommandKind::DeleteReplacement { id, .. } => self
                .daemon
                .delete_replacement(id)
                .map_err(anyhow::Error::from)
                .and_then(|deleted| {
                    deleted
                        .then_some(())
                        .ok_or_else(|| anyhow::anyhow!("replacement {id} was not found"))
                }),
            ClientCommandKind::DeleteHistory { id, .. } => self
                .daemon
                .delete_history(id)
                .map_err(anyhow::Error::from)
                .and_then(|deleted| {
                    deleted
                        .then_some(())
                        .ok_or_else(|| anyhow::anyhow!("transcript {id} was not found"))
                }),
            ClientCommandKind::ClearHistory { .. } => {
                self.daemon.clear_history().map_err(Into::into)
            }
            ClientCommandKind::CopyTranscript { id, .. } => self
                .daemon
                .history(id)
                .map_err(anyhow::Error::from)
                .and_then(|entry| {
                    entry.ok_or_else(|| anyhow::anyhow!("transcript {id} was not found"))
                })
                .and_then(|entry| {
                    self.daemon
                        .deliverer_mut()
                        .copy_text(&entry.final_text)
                        .map_err(Into::into)
                }),
            ClientCommandKind::UpdateSettings { settings, .. } => self.update_settings(*settings),
            ClientCommandKind::SetApiKey { api_key, .. } => {
                self.set_api_key(api_key.expose_secret())
            }
            ClientCommandKind::HotkeyStatusChanged { readiness, .. } => {
                self.daemon.set_hotkey_readiness(readiness);
                Ok(())
            }
            ClientCommandKind::Quit { .. } => self
                .daemon
                .shutdown()
                .map(|()| {
                    self.should_quit = true;
                    let _ = IpcClient::wake(&self.runtime_directory);
                })
                .map_err(Into::into),
        };
        self.synchronize_recording_priority();
        match result {
            Ok(()) if history_request.is_some() => self
                .history_page_message(request_id, history_request.expect("checked above"))
                .unwrap_or_else(|error| {
                    ServerMessage::command_rejected(request_id, error.to_string())
                }),
            Ok(()) if returns_workspace => {
                self.workspace_message(request_id).unwrap_or_else(|error| {
                    ServerMessage::command_rejected(request_id, error.to_string())
                })
            }
            Ok(()) => self.snapshot_message(request_id),
            Err(error) => ServerMessage::command_rejected(request_id, error.to_string()),
        }
    }
}

const fn request_id(command: &ClientCommandKind) -> u64 {
    match command {
        ClientCommandKind::GetSnapshot { request_id }
        | ClientCommandKind::GetWorkspace { request_id }
        | ClientCommandKind::RefreshModelCatalog { request_id }
        | ClientCommandKind::GetHistoryPage { request_id, .. }
        | ClientCommandKind::StartRecording { request_id }
        | ClientCommandKind::StopRecording { request_id }
        | ClientCommandKind::Cancel { request_id }
        | ClientCommandKind::RecorderExited { request_id, .. }
        | ClientCommandKind::RetryTranscription { request_id, .. }
        | ClientCommandKind::RetryDelivery { request_id, .. }
        | ClientCommandKind::DeleteRecovery { request_id, .. }
        | ClientCommandKind::CreateReplacement { request_id, .. }
        | ClientCommandKind::UpdateReplacement { request_id, .. }
        | ClientCommandKind::DeleteReplacement { request_id, .. }
        | ClientCommandKind::DeleteHistory { request_id, .. }
        | ClientCommandKind::ClearHistory { request_id }
        | ClientCommandKind::CopyTranscript { request_id, .. }
        | ClientCommandKind::UpdateSettings { request_id, .. }
        | ClientCommandKind::SetApiKey { request_id, .. }
        | ClientCommandKind::HotkeyStatusChanged { request_id, .. }
        | ClientCommandKind::Quit { request_id } => *request_id,
    }
}

/// Converts one dispatcher-approved hotkey edge into at most one lifecycle command.
/// The native listener owns repeat suppression; the daemon dispatcher owns toggle rearming.
#[must_use]
pub fn command_for_hotkey(
    mode: &str,
    signal: HotkeySignal,
    phase: WorkflowPhase,
    request_id: u64,
) -> Option<ClientCommand> {
    let is_recording = matches!(
        phase,
        WorkflowPhase::Starting { .. } | WorkflowPhase::Recording { .. }
    );
    match (mode, signal) {
        (_, HotkeySignal::Cancelled) if is_recording => Some(ClientCommand::cancel(request_id)),
        (_, HotkeySignal::Cancelled) => None,
        (_, HotkeySignal::Released) if mode != "hold" => None,
        ("hold", HotkeySignal::Pressed) if !is_recording => {
            Some(ClientCommand::start_recording(request_id))
        }
        ("hold", HotkeySignal::Released) if is_recording => {
            Some(ClientCommand::stop_recording(request_id))
        }
        ("hold", _) => None,
        (_, HotkeySignal::Pressed) if is_recording => {
            Some(ClientCommand::stop_recording(request_id))
        }
        (_, HotkeySignal::Pressed) => Some(ClientCommand::start_recording(request_id)),
        (_, HotkeySignal::Released) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        sync::{Arc, Mutex},
    };

    use agentdictate_core::{ClientCommand, ServerMessageKind};
    use agentdictate_runtime::IpcHandler;
    use tempfile::tempdir;

    use super::*;

    #[derive(Default)]
    struct RejectingHotkeyControl {
        attempts: Mutex<Vec<String>>,
    }

    impl HotkeyReconfigurer for RejectingHotkeyControl {
        fn reconfigure(&self, spec: HotkeySpec) -> anyhow::Result<()> {
            let hotkey = spec.display().to_owned();
            self.attempts.lock().unwrap().push(hotkey.clone());
            anyhow::bail!("{hotkey} is not supported by an active keyboard")
        }
    }

    struct RecordingHotkeyControl {
        attempts: Mutex<Vec<String>>,
    }

    struct PersistenceOrderingControl {
        config_file: PathBuf,
        observed_hotkey: Mutex<Option<String>>,
    }

    impl HotkeyReconfigurer for PersistenceOrderingControl {
        fn reconfigure(&self, _spec: HotkeySpec) -> anyhow::Result<()> {
            let persisted = load_settings(&self.config_file)?.hotkey;
            *self.observed_hotkey.lock().unwrap() = Some(persisted);
            Ok(())
        }
    }

    impl RecordingHotkeyControl {
        fn new() -> Self {
            Self {
                attempts: Mutex::new(Vec::new()),
            }
        }
    }

    impl HotkeyReconfigurer for RecordingHotkeyControl {
        fn reconfigure(&self, spec: HotkeySpec) -> anyhow::Result<()> {
            self.attempts
                .lock()
                .unwrap()
                .push(spec.display().to_owned());
            Ok(())
        }
    }

    #[test]
    fn rejected_hotkey_change_keeps_config_process_and_listener_on_the_old_shortcut() {
        let directory = tempdir().unwrap();
        let paths = app_paths(directory.path());
        let mut process = AgentProcess::open(paths.clone()).unwrap();
        let control = Arc::new(RejectingHotkeyControl::default());
        process.set_hotkey_reconfigurer(control.clone());
        let mut changed = Settings::default();
        changed.hotkey = "F9".into();

        let response = process.handle(ClientCommand::update_settings(7, &changed));

        assert!(matches!(
            response.kind,
            ServerMessageKind::CommandRejected { .. }
        ));
        assert_eq!(process.hotkey(), "Ctrl+Space");
        assert_eq!(
            load_settings(&paths.config_file).unwrap().hotkey,
            "Ctrl+Space"
        );
        assert_eq!(control.attempts.lock().unwrap().as_slice(), ["F9"]);
    }

    #[test]
    fn failed_settings_persist_rolls_the_native_listener_back_to_the_old_shortcut() {
        let directory = tempdir().unwrap();
        let paths = app_paths(directory.path());
        let mut process = AgentProcess::open(paths).unwrap();
        let control = Arc::new(RecordingHotkeyControl::new());
        process.set_hotkey_reconfigurer(control.clone());
        process.config_file = directory.path().join("not-a-file");
        std::fs::create_dir(&process.config_file).unwrap();
        let mut changed = Settings::default();
        changed.hotkey = "F9".into();

        let response = process.handle(ClientCommand::update_settings(8, &changed));

        assert!(matches!(
            response.kind,
            ServerMessageKind::CommandRejected { .. }
        ));
        assert_eq!(process.hotkey(), "Ctrl+Space");
        assert_eq!(
            control.attempts.lock().unwrap().as_slice(),
            ["F9", "Ctrl+Space"]
        );
    }

    #[test]
    fn shortcut_is_persisted_only_after_the_live_listener_accepts_it() {
        let directory = tempdir().unwrap();
        let paths = app_paths(directory.path());
        let mut process = AgentProcess::open(paths.clone()).unwrap();
        let control = Arc::new(PersistenceOrderingControl {
            config_file: paths.config_file.clone(),
            observed_hotkey: Mutex::new(None),
        });
        process.set_hotkey_reconfigurer(control.clone());
        let mut changed = Settings::default();
        changed.hotkey = "F9".into();

        let response = process.handle(ClientCommand::update_settings(9, &changed));

        assert!(matches!(response.kind, ServerMessageKind::Snapshot { .. }));
        assert_eq!(
            control.observed_hotkey.lock().unwrap().as_deref(),
            Some("Ctrl+Space")
        );
        assert_eq!(load_settings(&paths.config_file).unwrap().hotkey, "F9");
        assert_eq!(process.hotkey(), "F9");
    }

    #[test]
    fn opening_the_shortcut_process_does_not_run_nonessential_maintenance() {
        let directory = tempdir().unwrap();
        let paths = app_paths(directory.path());

        let mut process = AgentProcess::open(paths.clone()).unwrap();
        process.systemctl_command = PathBuf::from("/bin/true");

        assert!(!paths.autostart_file.exists());
        process
            .start_post_listener_maintenance()
            .unwrap()
            .join()
            .unwrap();
        assert!(paths.autostart_file.exists());
        assert!(paths.daemon_service_file.exists());
    }

    fn app_paths(root: &Path) -> AppPaths {
        AppPaths::from_roots(
            root.join("config"),
            root.join("data"),
            root.join("state"),
            root.join("cache"),
            root.join("runtime"),
        )
    }
}
