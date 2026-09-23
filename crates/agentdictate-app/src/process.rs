use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use agentdictate_core::{
    ClientCommand, ClientCommandKind, ClientCommandTag, Hotkey, HotkeyCaptureOutcome,
    HotkeyReadiness, RecordingMode, ServerMessage, Settings, WorkflowPhase,
};
use agentdictate_linux::hotkey::{HotkeySignal, HotkeySpec};
use agentdictate_runtime::{
    DeferredReply, FinishedJobCleanup, IpcClient, IpcHandler, Runtime, RuntimeError, load_settings,
    save_settings,
};

use crate::{
    AppPaths, Daemon, OverlayController, ReqwestOpenAiTransport, SystemDeliverer,
    SystemRecordingController, TranscriptionPipeline, startup::LoginStartup,
};

pub type ProductionTranscriber = TranscriptionPipeline<ReqwestOpenAiTransport>;
pub type ProductionDaemon =
    Daemon<SystemRecordingController, ProductionTranscriber, SystemDeliverer>;

/// How long a shortcut capture waits for a key press.
const HOTKEY_CAPTURE_TIMEOUT: Duration = Duration::from_secs(10);

/// Control seam over the live shortcut listener, used by settings updates
/// and the settings window's shortcut capture.
pub trait HotkeyControl: Send + Sync {
    /// Returns success only after the live listener accepted the shortcut.
    fn reconfigure(&self, spec: HotkeySpec) -> anyhow::Result<()>;
    /// Blocks until the next chord, Esc, a cancel, or `timeout`. The shortcut
    /// does not start dictation meanwhile.
    fn capture(&self, timeout: Duration) -> anyhow::Result<HotkeyCaptureOutcome>;
    fn cancel_capture(&self) -> anyhow::Result<()>;
}

pub struct AgentProcess {
    daemon: ProductionDaemon,
    config_file: PathBuf,
    login_startup: LoginStartup,
    database_file: PathBuf,
    recordings_directory: PathBuf,
    runtime_directory: PathBuf,
    hotkey_control: Option<Arc<dyn HotkeyControl>>,
    recording_mode_control: Option<Arc<RwLock<RecordingMode>>>,
    should_quit: bool,
}

impl AgentProcess {
    pub fn open(paths: AppPaths) -> anyhow::Result<Self> {
        paths.ensure_directories()?;
        let mut settings = load_settings(&paths.config_file)?;
        let runtime = Runtime::open(&paths.database_file)?;
        runtime.reconcile_recovery_deletions(&paths.recordings)?;
        retire_replacement_rules(&runtime, &mut settings, &paths.config_file);
        let transcriber = TranscriptionPipeline::new(
            settings.clone(),
            ReqwestOpenAiTransport::new(&settings.openai_api_key),
        );
        let recorder = SystemRecordingController::for_system(
            &settings,
            &paths.runtime,
            &paths.ducking_state_file,
        );
        let deliverer = SystemDeliverer::for_environment(settings.paste_shortcut);
        Ok(Self {
            daemon: Daemon::new(
                runtime,
                settings,
                paths.clone(),
                recorder,
                transcriber,
                deliverer,
            ),
            login_startup: LoginStartup::new(&paths),
            config_file: paths.config_file,
            database_file: paths.database_file,
            recordings_directory: paths.recordings,
            runtime_directory: paths.runtime,
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
    pub const fn hotkey(&self) -> &Hotkey {
        &self.daemon.settings().hotkey
    }

    #[must_use]
    pub const fn recording_mode(&self) -> RecordingMode {
        self.daemon.settings().recording_mode
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

    /// See `Daemon::recording_flag`.
    #[must_use]
    pub fn recording_flag(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.daemon.recording_flag()
    }

    pub fn set_hotkey_control(&mut self, control: Arc<dyn HotkeyControl>) {
        self.hotkey_control = Some(control);
    }

    pub fn set_recording_mode_control(&mut self, recording_mode: Arc<RwLock<RecordingMode>>) {
        self.recording_mode_control = Some(recording_mode);
    }

    /// Starts reconciliation that is useful but must never delay or prevent
    /// the native shortcut listener from starting. Work failures are logged
    /// and the daemon remains available.
    pub fn start_post_listener_maintenance(&self) -> std::io::Result<std::thread::JoinHandle<()>> {
        let settings = self.daemon.settings().clone();
        let login_startup = self.login_startup.clone();
        let database_file = self.database_file.clone();
        let recordings_directory = self.recordings_directory.clone();
        std::thread::Builder::new()
            .name("agentdictate-maintenance".into())
            .spawn(move || {
                run_post_listener_maintenance(
                    &settings,
                    &login_startup,
                    &database_file,
                    &recordings_directory,
                );
            })
    }

    fn snapshot_message(&self, request_id: u64) -> ServerMessage {
        ServerMessage::snapshot(request_id, self.daemon.snapshot(), self.daemon.settings())
    }

    fn workspace_message(&self, request_id: u64) -> Result<ServerMessage, RuntimeError> {
        Ok(ServerMessage::workspace(
            request_id,
            self.daemon.workspace_snapshot()?,
        ))
    }

    fn history_page_message(
        &self,
        request_id: u64,
        request: &agentdictate_core::HistoryPageRequest,
    ) -> Result<ServerMessage, RuntimeError> {
        Ok(ServerMessage::history_page(
            request_id,
            self.daemon.history_page(request)?,
        ))
    }

    fn update_settings(&mut self, mut settings: Settings) -> anyhow::Result<()> {
        settings.validate()?;
        let start_on_login_changed =
            settings.start_on_login != self.daemon.settings().start_on_login;
        let hotkey_changed = settings.hotkey != self.daemon.settings().hotkey;
        let recording_mode_changed =
            settings.recording_mode != self.daemon.settings().recording_mode;
        let new_hotkey = hotkey_changed.then(|| HotkeySpec::from(&settings.hotkey));
        settings.openai_api_key = self.daemon.settings().openai_api_key.clone();
        let old_hotkey = hotkey_changed.then(|| HotkeySpec::from(&self.daemon.settings().hotkey));
        if let Some(spec) = new_hotkey.as_ref() {
            self.hotkey_control()?.reconfigure(spec.clone())?;
        }
        if let Err(error) = save_settings(&self.config_file, &settings) {
            if new_hotkey.is_some()
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
        if start_on_login_changed
            && let Err(error) = self.login_startup.sync(settings.start_on_login)
        {
            tracing::warn!(%error, "settings saved but login startup reconciliation failed");
        }
        self.daemon
            .transcriber_mut()
            .update_settings(settings.clone());
        self.daemon.recorder_mut().update_settings(&settings);
        self.daemon
            .deliverer_mut()
            .update_shortcut(settings.paste_shortcut);
        self.daemon.update_settings(settings);
        if recording_mode_changed && let Some(mode) = &self.recording_mode_control {
            *mode
                .write()
                .map_err(|_| anyhow::anyhow!("recording-mode control is unavailable"))? =
                self.daemon.settings().recording_mode;
        }
        Ok(())
    }

    fn hotkey_control(&self) -> anyhow::Result<&Arc<dyn HotkeyControl>> {
        self.hotkey_control
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("hotkey listener is unavailable"))
    }

    fn set_api_key(&mut self, api_key: &str) -> anyhow::Result<()> {
        let mut settings = self.daemon.settings().clone();
        settings.openai_api_key = api_key.trim().to_owned();
        save_settings(&self.config_file, &settings)?;
        let transcriber = self.daemon.transcriber_mut();
        transcriber
            .speech_mut()
            .set_api_key(&settings.openai_api_key);
        transcriber.update_settings(settings.clone());
        self.daemon.update_settings(settings);
        Ok(())
    }
}

/// Moves the retired Replacements feature's enabled rules into vocabulary
/// and logs each one. A failure is logged and never stops the daemon.
fn retire_replacement_rules(runtime: &Runtime, settings: &mut Settings, config_file: &Path) {
    match runtime.retire_replacement_rules(settings, config_file) {
        Ok(retired) => {
            for rule in retired {
                tracing::info!(
                    source_phrase = %rule.source_phrase,
                    replacement_phrase = %rule.replacement_phrase,
                    outcome = ?rule.outcome,
                    "retired a Replacements rule"
                );
            }
        }
        Err(error) => tracing::warn!(%error, "could not move Replacements rules into vocabulary"),
    }
}

fn run_post_listener_maintenance(
    settings: &Settings,
    login_startup: &LoginStartup,
    database_file: &std::path::Path,
    recordings_directory: &std::path::Path,
) {
    if let Err(error) = login_startup.sync(settings.start_on_login) {
        tracing::warn!(%error, "could not reconcile login startup");
    }
    // The listener is already live, so a recording may have started. The
    // reconciling `Runtime::open` would mark it interrupted.
    let mut runtime = match Runtime::open_background_writer(database_file) {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::warn!(%error, "could not open maintenance database connection");
            return;
        }
    };
    match runtime.clean_up_finished_jobs(settings, recordings_directory) {
        Ok(cleanup) if cleanup == FinishedJobCleanup::default() => {}
        Ok(cleanup) => tracing::info!(
            recorded_deliveries = cleanup.recorded_deliveries,
            removed_jobs = cleanup.removed_jobs,
            removed_recordings = cleanup.removed_recordings,
            failed_removals = cleanup.failed_removals,
            "cleaned up finished dictations"
        ),
        Err(error) => tracing::warn!(%error, "could not clean up finished dictations"),
    }
}

impl IpcHandler for AgentProcess {
    fn snapshot(&self, request_id: u64) -> ServerMessage {
        self.snapshot_message(request_id)
    }

    /// A shortcut capture waits up to `HOTKEY_CAPTURE_TIMEOUT` for the user,
    /// so it runs without the daemon lock.
    fn deferred_reply(&self, command: &ClientCommand) -> Option<DeferredReply> {
        let ClientCommandKind::CaptureHotkey { request_id } = command.kind else {
            return None;
        };
        let control = self.hotkey_control().cloned();
        Some(Box::new(move || {
            match control.and_then(|control| control.capture(HOTKEY_CAPTURE_TIMEOUT)) {
                Ok(outcome) => ServerMessage::hotkey_captured(request_id, outcome),
                Err(error) => ServerMessage::command_rejected(request_id, error.to_string()),
            }
        }))
    }

    fn handle(&mut self, command: ClientCommand) -> ServerMessage {
        let command_tag = command.kind();
        let request_id = request_id(&command.kind);
        let history_request = match &command.kind {
            ClientCommandKind::GetHistoryPage { request, .. } => Some(request.clone()),
            _ => None,
        };
        let returns_workspace = matches!(
            command_tag,
            ClientCommandTag::GetWorkspace
                | ClientCommandTag::RetryTranscription
                | ClientCommandTag::RetryDelivery
                | ClientCommandTag::DeleteRecovery
                | ClientCommandTag::DeleteHistory
                | ClientCommandTag::ClearHistory
                | ClientCommandTag::CopyTranscript
        );
        let result: anyhow::Result<()> = match command.kind {
            ClientCommandKind::GetSnapshot { .. } => Ok(()),
            ClientCommandKind::GetWorkspace { .. } => Ok(()),
            ClientCommandKind::GetHistoryPage { .. } => Ok(()),
            ClientCommandKind::StartRecording { mode, .. } => self
                .daemon
                .start_recording_in_mode(mode)
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
                .transcript_text(id)
                .map_err(anyhow::Error::from)
                .and_then(|text| {
                    text.ok_or_else(|| anyhow::anyhow!("transcript {id} was not found"))
                })
                .and_then(|text| {
                    self.daemon
                        .deliverer_mut()
                        .copy_text(&text)
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
            ClientCommandKind::CaptureHotkey { .. } => Err(anyhow::anyhow!(
                "shortcut capture must be answered by its deferred reply"
            )),
            ClientCommandKind::CancelHotkeyCapture { .. } => self
                .hotkey_control()
                .and_then(|control| control.cancel_capture()),
            ClientCommandKind::Quit { .. } => self
                .daemon
                .shutdown()
                .map(|()| {
                    self.should_quit = true;
                    let _ = IpcClient::wake(&self.runtime_directory);
                })
                .map_err(Into::into),
        };
        match result {
            Ok(()) if history_request.is_some() => self
                .history_page_message(request_id, &history_request.expect("checked above"))
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
        | ClientCommandKind::GetHistoryPage { request_id, .. }
        | ClientCommandKind::StartRecording { request_id, .. }
        | ClientCommandKind::StopRecording { request_id }
        | ClientCommandKind::Cancel { request_id }
        | ClientCommandKind::RecorderExited { request_id, .. }
        | ClientCommandKind::RetryTranscription { request_id, .. }
        | ClientCommandKind::RetryDelivery { request_id, .. }
        | ClientCommandKind::DeleteRecovery { request_id, .. }
        | ClientCommandKind::DeleteHistory { request_id, .. }
        | ClientCommandKind::ClearHistory { request_id }
        | ClientCommandKind::CopyTranscript { request_id, .. }
        | ClientCommandKind::UpdateSettings { request_id, .. }
        | ClientCommandKind::SetApiKey { request_id, .. }
        | ClientCommandKind::HotkeyStatusChanged { request_id, .. }
        | ClientCommandKind::CaptureHotkey { request_id }
        | ClientCommandKind::CancelHotkeyCapture { request_id }
        | ClientCommandKind::Quit { request_id } => *request_id,
    }
}

/// Converts one dispatcher-approved hotkey edge into at most one lifecycle command.
/// The native listener owns repeat suppression; the daemon dispatcher owns toggle rearming.
#[must_use]
pub fn command_for_hotkey(
    mode: RecordingMode,
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
        (RecordingMode::Hold, HotkeySignal::Pressed) if !is_recording => {
            Some(ClientCommand::start_recording(request_id))
        }
        (RecordingMode::Hold, HotkeySignal::Released) if is_recording => {
            Some(ClientCommand::stop_recording(request_id))
        }
        (RecordingMode::Hold, _) => None,
        (RecordingMode::Toggle, HotkeySignal::Pressed) if is_recording => {
            Some(ClientCommand::stop_recording(request_id))
        }
        (RecordingMode::Toggle, HotkeySignal::Pressed) => {
            Some(ClientCommand::start_recording(request_id))
        }
        (RecordingMode::Toggle, HotkeySignal::Released) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use agentdictate_core::{ClientCommand, JobStage, ServerMessageKind};
    use agentdictate_runtime::{
        ExternalError, IpcHandler, Recorder, RecordingJob, RecordingRequest,
    };
    use tempfile::tempdir;

    use super::*;
    use crate::DaemonSupervision;

    #[derive(Default)]
    struct RejectingHotkeyControl {
        attempts: Mutex<Vec<String>>,
    }

    impl HotkeyControl for RejectingHotkeyControl {
        fn reconfigure(&self, spec: HotkeySpec) -> anyhow::Result<()> {
            let hotkey = spec.display().to_owned();
            self.attempts.lock().unwrap().push(hotkey.clone());
            anyhow::bail!("{hotkey} is not supported by an active keyboard")
        }

        fn capture(&self, _timeout: Duration) -> anyhow::Result<HotkeyCaptureOutcome> {
            unreachable!("settings tests never capture a shortcut")
        }

        fn cancel_capture(&self) -> anyhow::Result<()> {
            unreachable!("settings tests never capture a shortcut")
        }
    }

    struct RecordingHotkeyControl {
        attempts: Mutex<Vec<String>>,
    }

    struct PersistenceOrderingControl {
        config_file: PathBuf,
        observed_hotkey: Mutex<Option<String>>,
    }

    impl HotkeyControl for PersistenceOrderingControl {
        fn reconfigure(&self, _spec: HotkeySpec) -> anyhow::Result<()> {
            let persisted = load_settings(&self.config_file)?.hotkey;
            *self.observed_hotkey.lock().unwrap() = Some(persisted.label().to_owned());
            Ok(())
        }

        fn capture(&self, _timeout: Duration) -> anyhow::Result<HotkeyCaptureOutcome> {
            unreachable!("settings tests never capture a shortcut")
        }

        fn cancel_capture(&self) -> anyhow::Result<()> {
            unreachable!("settings tests never capture a shortcut")
        }
    }

    impl RecordingHotkeyControl {
        fn new() -> Self {
            Self {
                attempts: Mutex::new(Vec::new()),
            }
        }
    }

    impl HotkeyControl for RecordingHotkeyControl {
        fn reconfigure(&self, spec: HotkeySpec) -> anyhow::Result<()> {
            self.attempts
                .lock()
                .unwrap()
                .push(spec.display().to_owned());
            Ok(())
        }

        fn capture(&self, _timeout: Duration) -> anyhow::Result<HotkeyCaptureOutcome> {
            unreachable!("settings tests never capture a shortcut")
        }

        fn cancel_capture(&self) -> anyhow::Result<()> {
            unreachable!("settings tests never capture a shortcut")
        }
    }

    /// Captures F9 at once and counts cancellations.
    #[derive(Default)]
    struct CapturingHotkeyControl {
        cancels: AtomicUsize,
    }

    impl HotkeyControl for CapturingHotkeyControl {
        fn reconfigure(&self, _spec: HotkeySpec) -> anyhow::Result<()> {
            unreachable!("capture tests never change the shortcut")
        }

        fn capture(&self, _timeout: Duration) -> anyhow::Result<HotkeyCaptureOutcome> {
            Ok(HotkeyCaptureOutcome::Captured {
                hotkey: "F9".parse()?,
            })
        }

        fn cancel_capture(&self) -> anyhow::Result<()> {
            self.cancels.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[test]
    fn shortcut_capture_replies_outside_the_daemon_lock_and_cancel_reaches_the_listener() {
        let directory = tempdir().unwrap();
        let mut process = AgentProcess::open(app_paths(directory.path())).unwrap();
        let unavailable = process
            .deferred_reply(&ClientCommand::capture_hotkey(2))
            .expect("a capture always waits outside the lock");
        assert!(matches!(
            unavailable().kind,
            ServerMessageKind::CommandRejected { request_id: 2, .. }
        ));

        let control = Arc::new(CapturingHotkeyControl::default());
        process.set_hotkey_control(control.clone());
        assert!(
            process
                .deferred_reply(&ClientCommand::get_snapshot(3))
                .is_none()
        );
        let reply = process
            .deferred_reply(&ClientCommand::capture_hotkey(4))
            .expect("a capture always waits outside the lock");
        assert!(matches!(
            reply().kind,
            ServerMessageKind::HotkeyCaptured {
                request_id: 4,
                outcome: HotkeyCaptureOutcome::Captured { hotkey },
            } if hotkey.label() == "F9"
        ));

        let cancelled = process.handle(ClientCommand::cancel_hotkey_capture(5));
        assert!(matches!(cancelled.kind, ServerMessageKind::Snapshot { .. }));
        assert_eq!(control.cancels.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn rejected_hotkey_change_keeps_config_process_and_listener_on_the_old_shortcut() {
        let directory = tempdir().unwrap();
        let paths = app_paths(directory.path());
        let mut process = AgentProcess::open(paths.clone()).unwrap();
        let control = Arc::new(RejectingHotkeyControl::default());
        process.set_hotkey_control(control.clone());
        let changed = Settings {
            hotkey: "F9".parse().unwrap(),
            ..Settings::default()
        };

        let response = process.handle(ClientCommand::update_settings(7, &changed));

        assert!(matches!(
            response.kind,
            ServerMessageKind::CommandRejected { .. }
        ));
        assert_eq!(process.hotkey().label(), "Ctrl+Space");
        assert_eq!(
            load_settings(&paths.config_file).unwrap().hotkey.label(),
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
        process.set_hotkey_control(control.clone());
        process.config_file = directory.path().join("not-a-file");
        std::fs::create_dir(&process.config_file).unwrap();
        let changed = Settings {
            hotkey: "F9".parse().unwrap(),
            ..Settings::default()
        };

        let response = process.handle(ClientCommand::update_settings(8, &changed));

        assert!(matches!(
            response.kind,
            ServerMessageKind::CommandRejected { .. }
        ));
        assert_eq!(process.hotkey().label(), "Ctrl+Space");
        assert_eq!(
            control.attempts.lock().unwrap().as_slice(),
            ["F9", "Ctrl+Space"]
        );
    }

    #[test]
    fn invalid_settings_are_rejected_without_being_saved() {
        let directory = tempdir().unwrap();
        let paths = app_paths(directory.path());
        let mut process = AgentProcess::open(paths.clone()).unwrap();
        let changed = Settings {
            audio_ducking_volume_percent: 150,
            ..Settings::default()
        };

        let response = process.handle(ClientCommand::update_settings(6, &changed));

        assert!(matches!(
            response.kind,
            ServerMessageKind::CommandRejected { ref error, .. }
                if error.contains("Ducked volume")
        ));
        assert_eq!(
            load_settings(&paths.config_file)
                .unwrap()
                .audio_ducking_volume_percent,
            15
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
        process.set_hotkey_control(control.clone());
        let changed = Settings {
            hotkey: "F9".parse().unwrap(),
            ..Settings::default()
        };

        let response = process.handle(ClientCommand::update_settings(9, &changed));

        assert!(matches!(response.kind, ServerMessageKind::Snapshot { .. }));
        assert_eq!(
            control.observed_hotkey.lock().unwrap().as_deref(),
            Some("Ctrl+Space")
        );
        assert_eq!(
            load_settings(&paths.config_file).unwrap().hotkey.label(),
            "F9"
        );
        assert_eq!(process.hotkey().label(), "F9");
    }

    #[test]
    fn startup_maintenance_replaces_the_legacy_login_entry_with_the_unit() {
        let directory = tempdir().unwrap();
        let root = directory.path();
        let paths = AppPaths::from_roots(
            root.join("config"),
            root.join("data"),
            root.join("state"),
            root.join("cache"),
            root.join("runtime"),
        );
        let DaemonSupervision::SystemdUser { unit_file } = paths.daemon_supervision.clone() else {
            unreachable!()
        };
        std::fs::create_dir_all(paths.legacy_autostart_file.parent().unwrap()).unwrap();
        std::fs::write(&paths.legacy_autostart_file, "[Desktop Entry]\n").unwrap();

        let mut process = AgentProcess::open(paths.clone()).unwrap();
        process.login_startup.systemctl = PathBuf::from("/bin/true");
        assert!(!unit_file.exists());

        process
            .start_post_listener_maintenance()
            .unwrap()
            .join()
            .unwrap();
        assert!(unit_file.exists());
        assert!(!paths.legacy_autostart_file.exists());
    }

    struct StartedRecorder;

    impl Recorder for StartedRecorder {
        fn start(&mut self, _job: &RecordingJob) -> Result<(), ExternalError> {
            Ok(())
        }
    }

    #[test]
    fn startup_maintenance_leaves_a_recording_that_started_before_it_alone() {
        let directory = tempdir().unwrap();
        let paths = app_paths(directory.path());
        let process = AgentProcess::open(paths.clone()).unwrap();
        // A hotkey press lands between the listener going live and maintenance.
        let recording = Runtime::open_background_writer(&paths.database_file)
            .unwrap()
            .start_recording(
                RecordingRequest {
                    id: agentdictate_core::JobId::new(),
                    options: None,
                    audio_path: directory.path().join("recording.wav"),
                    started_at: chrono::Utc::now(),
                    transcription_model: "test".into(),
                },
                &mut StartedRecorder,
            )
            .unwrap();

        process
            .start_post_listener_maintenance()
            .unwrap()
            .join()
            .unwrap();

        let observer = Runtime::open_observer(&paths.database_file).unwrap();
        assert_eq!(
            observer.job(recording.id).unwrap().unwrap().stage,
            JobStage::Recording
        );
    }

    /// An isolated instance, so no test can reach the host's systemd.
    fn app_paths(root: &Path) -> AppPaths {
        AppPaths::isolated(root)
    }
}
