use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use agentdictate_core::{
    ClientCommandKind, HistoryPageRequest, Hotkey, HotkeyCaptureOutcome, HotkeyReadiness,
    RecordingMode, ServerMessage, SettingChange, Settings, WorkspaceSnapshot,
};
use agentdictate_linux::hotkey::HotkeySpec;
use agentdictate_runtime::{
    FinishedJobCleanup, Runtime, RuntimeError, load_settings, save_settings,
};

use crate::{
    AppPaths, Daemon, DaemonDeliverer, OverlayController, ProcessingTicket, RecorderEvent,
    RecordingController, ReqwestOpenAiTransport, SystemDeliverer, SystemRecordingController,
    Transcriber, TranscriptionPipeline, startup::LoginStartup,
};

pub type ProductionTranscriber = TranscriptionPipeline<ReqwestOpenAiTransport>;

/// How long a shortcut capture waits for a key press.
pub(crate) const HOTKEY_CAPTURE_TIMEOUT: Duration = Duration::from_secs(10);

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

/// The daemon plus what it needs from the running process: the config file,
/// login startup, and the live hotkey listener. `DaemonHandle` shares it
/// between threads.
pub struct AgentProcess<
    R = SystemRecordingController,
    T = ProductionTranscriber,
    D = SystemDeliverer,
> {
    daemon: Daemon<R, T, D>,
    config_file: PathBuf,
    login_startup: LoginStartup,
    database_file: PathBuf,
    recordings_directory: PathBuf,
    hotkey_control: Option<Arc<dyn HotkeyControl>>,
    /// Where startup moved a history database it could not read.
    history_set_aside: Option<PathBuf>,
}

/// What an IPC command answers with, rendered once the command's work is done.
pub(crate) enum Reply {
    Snapshot,
    Workspace,
    HistoryPage(HistoryPageRequest),
    HotkeyCaptured(HotkeyCaptureOutcome),
    Rejected(String),
}

/// Work an IPC command leaves for after the process lock is released.
pub(crate) enum Followup<T> {
    None,
    /// Transcribe on a processing thread; the reply does not wait.
    Process(ProcessingTicket<T>),
    /// Transcribe, then reply with the copied result (Recovery retries).
    ProcessThenReply(ProcessingTicket<T>),
    /// Wait for the user to press a shortcut, then reply with it.
    CaptureHotkey(Arc<dyn HotkeyControl>),
    Quit,
}

impl AgentProcess {
    /// Opens the production daemon, and the channel its recorder reports
    /// recording events on.
    pub fn open(paths: AppPaths) -> anyhow::Result<(Self, Receiver<RecorderEvent>)> {
        paths.ensure_directories()?;
        let mut settings = load_settings(&paths.config_file)?;
        let (runtime, history_set_aside) = Runtime::open_or_set_aside(&paths.database_file)?;
        if let Some(set_aside) = &history_set_aside {
            tracing::error!(
                set_aside = %set_aside.display(),
                "the history database could not be read; it was set aside and a new one started"
            );
        }
        runtime.reconcile_recovery_deletions(&paths.recordings)?;
        retire_replacement_rules(&runtime, &mut settings, &paths.config_file);
        let transcriber = TranscriptionPipeline::new(
            settings.clone(),
            ReqwestOpenAiTransport::new(&settings.openai_api_key),
        );
        let (recorder, recorder_events) =
            SystemRecordingController::for_system(&settings, &paths.ducking_state_file);
        let deliverer = SystemDeliverer::for_environment(settings.paste_shortcut);
        let daemon = Daemon::new(
            runtime,
            settings,
            paths.clone(),
            recorder,
            transcriber,
            deliverer,
        );
        let process = Self {
            history_set_aside,
            ..Self::from_parts(daemon, &paths)
        };
        Ok((process, recorder_events))
    }
}

impl<R, T, D> AgentProcess<R, T, D>
where
    R: RecordingController,
    T: Transcriber,
    D: DaemonDeliverer,
{
    /// Wraps an assembled daemon; `open` builds the production one.
    #[must_use]
    pub fn from_parts(daemon: Daemon<R, T, D>, paths: &AppPaths) -> Self {
        Self {
            daemon,
            login_startup: LoginStartup::new(paths),
            config_file: paths.config_file.clone(),
            database_file: paths.database_file.clone(),
            recordings_directory: paths.recordings.clone(),
            hotkey_control: None,
            history_set_aside: None,
        }
    }

    #[must_use]
    pub const fn daemon(&self) -> &Daemon<R, T, D> {
        &self.daemon
    }

    pub const fn daemon_mut(&mut self) -> &mut Daemon<R, T, D> {
        &mut self.daemon
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

    pub fn set_overlay_controller(&mut self, controller: OverlayController) {
        self.daemon.set_overlay_controller(controller);
    }

    pub fn set_hotkey_control(&mut self, control: Arc<dyn HotkeyControl>) {
        self.hotkey_control = Some(control);
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

    /// Runs the part of an IPC command that needs the process lock. Anything
    /// slow, such as transcription, is returned as a follow-up for the
    /// caller to run after releasing the lock.
    pub(crate) fn handle_locked(&mut self, command: ClientCommandKind) -> (Reply, Followup<T>) {
        let daemon = &mut self.daemon;
        let handled: anyhow::Result<(Reply, Followup<T>)> = match command {
            ClientCommandKind::GetSnapshot => Ok((Reply::Snapshot, Followup::None)),
            ClientCommandKind::GetWorkspace => Ok((Reply::Workspace, Followup::None)),
            ClientCommandKind::GetHistoryPage { request } => {
                Ok((Reply::HistoryPage(request), Followup::None))
            }
            ClientCommandKind::StartRecording { mode } => daemon
                .start_recording_in_mode(mode)
                .map(|_| (Reply::Snapshot, Followup::None))
                .map_err(Into::into),
            ClientCommandKind::StopRecording => daemon
                .stop_recording()
                .map(|ticket| (Reply::Snapshot, Followup::Process(ticket)))
                .map_err(Into::into),
            // Cancels a recording, or detaches the transcription in progress.
            ClientCommandKind::Cancel if daemon.is_processing() => daemon
                .cancel_processing()
                .map(|_| (Reply::Snapshot, Followup::None))
                .map_err(Into::into),
            ClientCommandKind::Cancel => daemon
                .discard_recording()
                .map(|_| (Reply::Snapshot, Followup::None))
                .map_err(Into::into),
            ClientCommandKind::RetryTranscription { job_id } => daemon
                .retry_transcription(job_id)
                .map(|ticket| (Reply::Workspace, Followup::ProcessThenReply(ticket)))
                .map_err(Into::into),
            ClientCommandKind::RetryDelivery { job_id } => daemon
                .retry_delivery(job_id)
                .map(|_| (Reply::Workspace, Followup::None))
                .map_err(Into::into),
            ClientCommandKind::DeleteRecovery { job_id } => daemon
                .delete_recovery(job_id)
                .map(|_| (Reply::Workspace, Followup::None))
                .map_err(Into::into),
            ClientCommandKind::DeleteHistory { id } => daemon
                .delete_history(id)
                .map_err(anyhow::Error::from)
                .and_then(|deleted| {
                    deleted
                        .then_some((Reply::Workspace, Followup::None))
                        .ok_or_else(|| anyhow::anyhow!("transcript {id} was not found"))
                }),
            ClientCommandKind::ClearHistory => daemon
                .clear_history()
                .map(|()| (Reply::Workspace, Followup::None))
                .map_err(Into::into),
            ClientCommandKind::CopyTranscript { id } => daemon
                .transcript_text(id)
                .map_err(anyhow::Error::from)
                .and_then(|text| {
                    text.ok_or_else(|| anyhow::anyhow!("transcript {id} was not found"))
                })
                .and_then(|text| daemon.deliverer_mut().copy_text(&text).map_err(Into::into))
                .map(|()| (Reply::Workspace, Followup::None)),
            ClientCommandKind::ChangeSetting { change } => self
                .change_setting(change)
                .map(|()| (Reply::Snapshot, Followup::None)),
            ClientCommandKind::SetApiKey { api_key } => self
                .set_api_key(api_key.expose_secret())
                .map(|()| (Reply::Snapshot, Followup::None)),
            ClientCommandKind::CaptureHotkey => self.hotkey_control().map(|control| {
                (
                    Reply::Snapshot,
                    Followup::CaptureHotkey(Arc::clone(control)),
                )
            }),
            ClientCommandKind::CancelHotkeyCapture => self
                .hotkey_control()
                .and_then(|control| control.cancel_capture())
                .map(|()| (Reply::Snapshot, Followup::None)),
            ClientCommandKind::Quit => Ok((Reply::Snapshot, Followup::Quit)),
        };
        handled.unwrap_or_else(|error| (Reply::Rejected(error.to_string()), Followup::None))
    }

    /// Renders an IPC reply from the current state.
    pub(crate) fn render(&self, reply: Reply) -> ServerMessage {
        let rendered = match reply {
            Reply::Snapshot => Ok(ServerMessage::snapshot(
                self.daemon.snapshot(),
                self.daemon.settings(),
            )),
            Reply::Workspace => self.daemon.workspace_snapshot().map(|workspace| {
                ServerMessage::workspace(WorkspaceSnapshot {
                    history_set_aside: self.history_set_aside.clone(),
                    ..workspace
                })
            }),
            Reply::HistoryPage(request) => self
                .daemon
                .history_page(&request)
                .map(ServerMessage::history_page),
            Reply::HotkeyCaptured(outcome) => Ok(ServerMessage::hotkey_captured(outcome)),
            Reply::Rejected(error) => Ok(ServerMessage::command_rejected(error)),
        };
        rendered.unwrap_or_else(|error: RuntimeError| {
            ServerMessage::command_rejected(error.to_string())
        })
    }

    /// Applies one setting to the settings the daemon holds.
    fn change_setting(&mut self, change: SettingChange) -> anyhow::Result<()> {
        let mut settings = self.daemon.settings().clone();
        change.apply(&mut settings)?;
        self.update_settings(settings)
    }

    /// Saves and applies `settings`. A new hotkey must be accepted by the
    /// live listener first, and goes back to the old one if saving fails.
    fn update_settings(&mut self, settings: Settings) -> anyhow::Result<()> {
        let start_on_login_changed =
            settings.start_on_login != self.daemon.settings().start_on_login;
        let hotkey_changed = settings.hotkey != self.daemon.settings().hotkey;
        let new_hotkey = hotkey_changed.then(|| HotkeySpec::from(&settings.hotkey));
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
        self.daemon.transcriber_mut().update_settings(&settings);
        self.daemon.recorder_mut().update_settings(&settings);
        self.daemon.deliverer_mut().update_settings(&settings);
        self.daemon.update_settings(settings);
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
        self.daemon.transcriber_mut().update_settings(&settings);
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
            purged_transcripts = cleanup.purged_transcripts,
            expired_recoveries = cleanup.expired_recoveries,
            failed_removals = cleanup.failed_removals,
            "cleaned up finished dictations"
        ),
        Err(error) => tracing::warn!(%error, "could not clean up finished dictations"),
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

    use agentdictate_core::{JobStage, KeepTranscripts, ServerMessageKind};
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

    /// Captures F9 once the test releases it, and counts cancellations.
    #[derive(Default)]
    struct CapturingHotkeyControl {
        entered: Mutex<Option<std::sync::mpsc::Sender<()>>>,
        release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
        cancels: AtomicUsize,
    }

    impl HotkeyControl for CapturingHotkeyControl {
        fn reconfigure(&self, _spec: HotkeySpec) -> anyhow::Result<()> {
            unreachable!("capture tests never change the shortcut")
        }

        fn capture(&self, _timeout: Duration) -> anyhow::Result<HotkeyCaptureOutcome> {
            if let Some(entered) = self.entered.lock().unwrap().take() {
                entered.send(())?;
            }
            if let Some(release) = self.release.lock().unwrap().take() {
                release.recv()?;
            }
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
        let paths = app_paths(directory.path());
        let (process, _recorder_events) = AgentProcess::open(paths.clone()).unwrap();
        let handle = crate::DaemonHandle::new(process, paths.runtime.clone());
        assert!(matches!(
            handle.handle(ClientCommandKind::CaptureHotkey.into()).kind,
            ServerMessageKind::CommandRejected { .. }
        ));
        let (entered, capture_entered) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let control = Arc::new(CapturingHotkeyControl {
            entered: Mutex::new(Some(entered)),
            release: Mutex::new(Some(released)),
            ..CapturingHotkeyControl::default()
        });
        handle.with_process(|process| process.set_hotkey_control(control.clone()));
        let capturing = {
            let handle = handle.clone();
            std::thread::spawn(move || handle.handle(ClientCommandKind::CaptureHotkey.into()))
        };
        capture_entered.recv().unwrap();

        let snapshot = {
            let handle = handle.clone();
            std::thread::spawn(move || handle.snapshot())
        };
        assert!(matches!(
            snapshot.join().unwrap().kind,
            ServerMessageKind::Snapshot { .. }
        ));
        release.send(()).unwrap();
        assert!(matches!(
            capturing.join().unwrap().kind,
            ServerMessageKind::HotkeyCaptured {
                outcome: HotkeyCaptureOutcome::Captured { hotkey },
            } if hotkey.label() == "F9"
        ));

        let cancelled = handle.handle(ClientCommandKind::CancelHotkeyCapture.into());
        assert!(matches!(cancelled.kind, ServerMessageKind::Snapshot { .. }));
        assert_eq!(control.cancels.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn rejected_hotkey_change_keeps_config_process_and_listener_on_the_old_shortcut() {
        let directory = tempdir().unwrap();
        let paths = app_paths(directory.path());
        let mut process = AgentProcess::open(paths.clone()).unwrap().0;
        let control = Arc::new(RejectingHotkeyControl::default());
        process.set_hotkey_control(control.clone());
        let changed = SettingChange::Hotkey("F9".parse().unwrap());

        assert!(process.change_setting(changed).is_err());

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
        let mut process = AgentProcess::open(paths).unwrap().0;
        let control = Arc::new(RecordingHotkeyControl::new());
        process.set_hotkey_control(control.clone());
        process.config_file = directory.path().join("not-a-file");
        std::fs::create_dir(&process.config_file).unwrap();
        let changed = SettingChange::Hotkey("F9".parse().unwrap());

        assert!(process.change_setting(changed).is_err());

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
        let mut process = AgentProcess::open(paths.clone()).unwrap().0;
        let error = process
            .change_setting(SettingChange::AudioDuckingVolumePercent(150))
            .unwrap_err();

        assert!(error.to_string().contains("Ducked volume"));
        assert_eq!(
            load_settings(&paths.config_file)
                .unwrap()
                .audio_ducking_volume_percent,
            15
        );
    }

    /// Each window sends only the setting it changed, so one window cannot
    /// revert another's edit or the saved API key.
    #[test]
    fn a_setting_change_keeps_every_other_saved_setting() {
        let directory = tempdir().unwrap();
        let paths = app_paths(directory.path());
        paths.ensure_directories().unwrap();
        let saved = Settings {
            openai_api_key: "sk-kept".into(),
            vocabulary: vec![agentdictate_core::VocabularyEntry {
                spelling: "Siobhan".into(),
                aliases: vec!["shiv on".into()],
            }],
            ..Settings::default()
        };
        save_settings(&paths.config_file, &saved).unwrap();
        let mut process = AgentProcess::open(paths.clone()).unwrap().0;

        process
            .change_setting(SettingChange::KeepTranscripts(KeepTranscripts::Never))
            .unwrap();

        assert_eq!(
            load_settings(&paths.config_file).unwrap(),
            Settings {
                keep_transcripts: KeepTranscripts::Never,
                ..saved
            }
        );
    }

    #[test]
    fn shortcut_is_persisted_only_after_the_live_listener_accepts_it() {
        let directory = tempdir().unwrap();
        let paths = app_paths(directory.path());
        let mut process = AgentProcess::open(paths.clone()).unwrap().0;
        let control = Arc::new(PersistenceOrderingControl {
            config_file: paths.config_file.clone(),
            observed_hotkey: Mutex::new(None),
        });
        process.set_hotkey_control(control.clone());
        process
            .change_setting(SettingChange::Hotkey("F9".parse().unwrap()))
            .unwrap();

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

        let mut process = AgentProcess::open(paths.clone()).unwrap().0;
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
        let process = AgentProcess::open(paths.clone()).unwrap().0;
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

    #[test]
    fn an_unreadable_history_database_is_set_aside_and_reported_to_the_window() {
        let directory = tempdir().unwrap();
        let paths = app_paths(directory.path());
        paths.ensure_directories().unwrap();
        std::fs::write(&paths.database_file, b"garbage").unwrap();

        let (process, _recorder_events) = AgentProcess::open(paths).unwrap();

        let agentdictate_core::ServerMessageKind::Workspace { workspace } =
            process.render(Reply::Workspace).kind
        else {
            panic!("a fresh database still serves the workspace");
        };
        let set_aside = workspace.history_set_aside.expect("the notice is reported");
        assert_eq!(std::fs::read(set_aside).unwrap(), b"garbage");
    }

    /// An isolated instance, so no test can reach the host's systemd.
    fn app_paths(root: &Path) -> AppPaths {
        AppPaths::isolated(root)
    }
}
