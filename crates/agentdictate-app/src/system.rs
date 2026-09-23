use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use agentdictate_core::{ClientCommand, JobId, PasteShortcut, ServerMessageKind, Settings};
use agentdictate_linux::{
    audio_ducking::{PlaybackDucker, SystemPactl},
    clipboard::{ClipboardError, ClipboardSelection, SelectionOwner},
    command::SystemCommandRunner,
    focus::{FocusError, observe_x11_focus},
    injection::PasteInjector,
    paste::{
        DeliveryAction, DeliveryFailure, DeliveryObservation, PasteDelivery, ShortcutMode,
        X11FocusObservation, resolve_focus_target,
    },
    recorder::{PwRecordRecorder, Recording, RecordingExitObserver},
};
use agentdictate_runtime::IpcClient;
use agentdictate_runtime::{
    Deliverer, DeliveryDisposition, DeliveryMethod, ExternalError, Recorder, RecordingJob,
};

use crate::{CapturedRecording, RecordingController};

const RECORDER_START_TIMEOUT: Duration = Duration::from_secs(10);
const RECORDER_STOP_TIMEOUT: Duration = Duration::from_secs(10);
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(5);
/// How long after the paste key press a target may take to request the
/// text and still count as having taken the paste. The chord itself takes
/// 50–75 ms of this.
const PASTE_REQUEST_WINDOW: Duration = Duration::from_millis(150);
static RECORDER_EVENT_REQUEST_ID: AtomicU64 = AtomicU64::new(1_000_000);

struct ActiveRecording {
    job_id: JobId,
    started_at: Instant,
    recording: Recording,
}

pub struct SystemRecordingController {
    recorder: RecorderOwner,
    ducker: PlaybackDucker,
    settings: Settings,
    runtime_directory: PathBuf,
}

struct RecorderStartResult {
    observer: Option<RecordingExitObserver>,
}

enum RecorderOwnerCommand {
    Start {
        job_id: JobId,
        audio_path: PathBuf,
        deadline: Instant,
        reply: SyncSender<Result<RecorderStartResult, String>>,
    },
    Finish {
        job_id: JobId,
        deadline: Instant,
        reply: SyncSender<Result<CapturedRecording, String>>,
    },
    Shutdown,
}

/// Owns every `pw-record` child from one daemon-lifetime thread. Linux ties
/// `PR_SET_PDEATHSIG` to the thread that forks, so spawning from per-client IPC
/// threads would make a successful request kill its own recorder on return.
struct RecorderOwner {
    commands: SyncSender<RecorderOwnerCommand>,
    worker: Option<JoinHandle<()>>,
}

impl RecorderOwner {
    fn start(recorder: PwRecordRecorder) -> Self {
        let (commands, receiver) = sync_channel(0);
        let worker = std::thread::Builder::new()
            .name("agentdictate-recorder-owner".into())
            .spawn(move || recorder_owner_loop(recorder, &receiver))
            .expect("recorder owner thread should start");
        Self {
            commands,
            worker: Some(worker),
        }
    }

    fn begin(
        &self,
        job_id: JobId,
        audio_path: PathBuf,
        deadline: Instant,
    ) -> Result<RecorderStartResult, ExternalError> {
        let (reply, response) = sync_channel(0);
        self.commands
            .send(RecorderOwnerCommand::Start {
                job_id,
                audio_path,
                deadline,
                reply,
            })
            .map_err(|_| ExternalError::new("the recorder owner is unavailable"))?;
        response
            .recv()
            .map_err(|_| ExternalError::new("the recorder owner stopped before replying"))?
            .map_err(ExternalError::new)
    }

    fn finish(&self, job_id: JobId, deadline: Instant) -> Result<CapturedRecording, ExternalError> {
        let (reply, response) = sync_channel(0);
        self.commands
            .send(RecorderOwnerCommand::Finish {
                job_id,
                deadline,
                reply,
            })
            .map_err(|_| ExternalError::new("the recorder owner is unavailable"))?;
        response
            .recv()
            .map_err(|_| ExternalError::new("the recorder owner stopped before replying"))?
            .map_err(ExternalError::new)
    }
}

impl Drop for RecorderOwner {
    fn drop(&mut self) {
        let _ = self.commands.send(RecorderOwnerCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn recorder_owner_loop(recorder: PwRecordRecorder, commands: &Receiver<RecorderOwnerCommand>) {
    let mut active: Option<ActiveRecording> = None;
    while let Ok(command) = commands.recv() {
        match command {
            RecorderOwnerCommand::Start {
                job_id,
                audio_path,
                deadline,
                reply,
            } => {
                let result = if active.is_some() {
                    Err("a recorder process is already active".to_owned())
                } else {
                    let started_at = Instant::now();
                    recorder
                        .start(&audio_path, deadline)
                        .map_err(|error| error.to_string())
                        .map(|recording| {
                            let observer = match recording.exit_observer() {
                                Ok(observer) => Some(observer),
                                Err(error) => {
                                    tracing::warn!(job_id = %job_id, %error, "recorder exit monitoring unavailable");
                                    None
                                }
                            };
                            active = Some(ActiveRecording {
                                job_id,
                                started_at,
                                recording,
                            });
                            RecorderStartResult { observer }
                        })
                };
                let _ = reply.send(result);
            }
            RecorderOwnerCommand::Finish {
                job_id,
                deadline,
                reply,
            } => {
                let result = match active.take() {
                    None => Err("the recorder process is not active".to_owned()),
                    Some(recording) if recording.job_id != job_id => {
                        let active_job = recording.job_id;
                        active = Some(recording);
                        Err(format!(
                            "the active recorder belongs to {active_job}, not {job_id}"
                        ))
                    }
                    Some(recording) => {
                        let duration_seconds = recording.started_at.elapsed().as_secs_f64();
                        recording
                            .recording
                            .stop(deadline)
                            .map(|_| CapturedRecording { duration_seconds })
                            .map_err(|error| error.to_string())
                    }
                };
                let _ = reply.send(result);
            }
            RecorderOwnerCommand::Shutdown => break,
        }
    }
}

impl SystemRecordingController {
    /// Creates the daemon's recorder. Opening the ducker restores an output
    /// volume that a previous daemon left ducked when it died.
    #[must_use]
    pub fn for_system(
        settings: &Settings,
        runtime_directory: &Path,
        ducking_state_file: &Path,
    ) -> Self {
        Self::new(
            settings,
            runtime_directory,
            "pw-record",
            PlaybackDucker::open(SystemPactl::discover(), ducking_state_file),
        )
    }

    fn new(
        settings: &Settings,
        runtime_directory: &Path,
        recorder_program: impl Into<PathBuf>,
        ducker: PlaybackDucker,
    ) -> Self {
        Self {
            recorder: RecorderOwner::start(PwRecordRecorder::new(
                SystemCommandRunner,
                recorder_program,
            )),
            ducker,
            settings: settings.clone(),
            runtime_directory: runtime_directory.to_owned(),
        }
    }

    #[cfg(test)]
    fn for_program(settings: &Settings, runtime_directory: &Path, recorder_program: &Path) -> Self {
        Self::new(
            settings,
            runtime_directory,
            recorder_program,
            PlaybackDucker::open(
                SystemPactl::discover(),
                runtime_directory.join("ducking.json"),
            ),
        )
    }

    pub fn update_settings(&mut self, settings: &Settings) {
        self.settings = settings.clone();
        if !settings.audio_ducking_enabled {
            self.ducker.restore();
        }
    }
}

impl Recorder for SystemRecordingController {
    fn start(&mut self, job: &RecordingJob) -> Result<(), ExternalError> {
        // Ducking only hands its work to a worker, so the recorder starts at
        // once while the output fades down beside it.
        self.ducker.duck(&self.settings);
        let started = match self.recorder.begin(
            job.id,
            job.audio_path.clone(),
            Instant::now() + RECORDER_START_TIMEOUT,
        ) {
            Ok(started) => started,
            Err(error) => {
                self.ducker.restore();
                return Err(error);
            }
        };
        if let Some(observer) = started.observer {
            let runtime_directory = self.runtime_directory.clone();
            let job_id = job.id;
            if let Err(error) = std::thread::Builder::new()
                .name("agentdictate-recorder-exit".into())
                .spawn(move || {
                    if let Err(error) = observer.wait() {
                        tracing::error!(job_id = %job_id, %error, "could not observe recorder exit");
                        return;
                    }
                    notify_recorder_exit(&runtime_directory, job_id);
                })
            {
                tracing::warn!(job_id = %job.id, %error, "recorder exit monitoring unavailable");
            }
        }
        Ok(())
    }

    fn abort_start(&mut self, job: &RecordingJob) -> Result<(), ExternalError> {
        let result = self
            .recorder
            .finish(job.id, Instant::now() + RECORDER_STOP_TIMEOUT)
            .map(|_| ());
        // Ducking is a best-effort side effect and must never survive a failed
        // durable Recording checkpoint, even when recorder finalization fails.
        self.ducker.restore();
        result
    }
}

fn notify_recorder_exit(runtime_directory: &Path, job_id: JobId) {
    let result = (|| -> anyhow::Result<()> {
        let (mut client, _) = IpcClient::connect(runtime_directory)?;
        let request_id = RECORDER_EVENT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        let response = client.send(ClientCommand::recorder_exited(request_id, job_id))?;
        if let ServerMessageKind::CommandRejected { error, .. } = response.kind {
            anyhow::bail!(error);
        }
        Ok(())
    })();
    if let Err(error) = result {
        tracing::error!(job_id = %job_id, %error, "could not publish recorder exit");
    }
}

impl RecordingController for SystemRecordingController {
    fn finish(&mut self, job: &RecordingJob) -> Result<CapturedRecording, ExternalError> {
        let result = self
            .recorder
            .finish(job.id, Instant::now() + RECORDER_STOP_TIMEOUT);
        self.ducker.restore();
        result
    }
}

/// Reads the active X11 window before a paste; tests substitute a fake.
type FocusReader = fn(Instant) -> Result<X11FocusObservation, FocusError>;

/// The X selections a delivery publishes to; tests substitute a fake.
trait Selections: Send {
    /// Serves `text` on exactly `selections` once the X server confirms
    /// AgentDictate owns them.
    fn publish(
        &mut self,
        text: &str,
        selections: &[ClipboardSelection],
        deadline: Instant,
    ) -> Result<(), ClipboardError>;

    /// When an application requested the text at or after `since`, waiting
    /// until `until` for it.
    fn text_requested_since(&self, since: Instant, until: Instant) -> Option<Instant>;
}

impl Selections for SelectionOwner {
    fn publish(
        &mut self,
        text: &str,
        selections: &[ClipboardSelection],
        deadline: Instant,
    ) -> Result<(), ClipboardError> {
        SelectionOwner::publish(self, text, selections, deadline)
    }

    fn text_requested_since(&self, since: Instant, until: Instant) -> Option<Instant> {
        SelectionOwner::text_requested_since(self, since, until)
    }
}

pub struct SystemDeliverer {
    /// Serves the published text until the next delivery or until another
    /// application takes the selection, so a target that reads
    /// asynchronously never finds it empty.
    selections: Box<dyn Selections>,
    focus: FocusReader,
    injector: PasteInjector,
    shortcut_mode: ShortcutMode,
    wayland_session: bool,
}

impl SystemDeliverer {
    #[must_use]
    pub fn for_environment(paste_shortcut: PasteShortcut) -> Self {
        Self {
            selections: Box::new(SelectionOwner::new()),
            focus: observe_x11_focus,
            injector: PasteInjector::new(),
            shortcut_mode: paste_shortcut.into(),
            wayland_session: std::env::var("XDG_SESSION_TYPE")
                .is_ok_and(|session| session.eq_ignore_ascii_case("wayland")),
        }
    }

    pub fn update_shortcut(&mut self, paste_shortcut: PasteShortcut) {
        self.shortcut_mode = paste_shortcut.into();
    }

    fn observe_focus(
        &self,
        deadline: Instant,
    ) -> Result<agentdictate_linux::paste::FocusTarget, ExternalError> {
        match (self.focus)(deadline) {
            Ok(observation) => Ok(resolve_focus_target(
                self.wayland_session,
                Some(observation),
            )),
            Err(_) if self.wayland_session => Ok(resolve_focus_target(true, None)),
            Err(error) => Err(ExternalError::new(error.to_string())),
        }
    }

    pub fn copy_text(&mut self, text: &str) -> Result<(), ExternalError> {
        self.selections
            .publish(
                text,
                &[ClipboardSelection::Clipboard],
                Instant::now() + DELIVERY_TIMEOUT,
            )
            .map_err(|error| ExternalError::new(error.to_string()))
    }

    fn publish_delivery_text(
        &mut self,
        text: &str,
        deadline: Instant,
    ) -> Result<(), ClipboardError> {
        let selections: &[ClipboardSelection] = match self.shortcut_mode {
            // Automatic mode's Shift+Insert pastes the primary selection in
            // terminals and the clipboard everywhere else.
            ShortcutMode::Auto => &[ClipboardSelection::Primary, ClipboardSelection::Clipboard],
            ShortcutMode::Standard | ShortcutMode::Terminal => &[ClipboardSelection::Clipboard],
        };
        self.selections.publish(text, selections, deadline)
    }

    /// Pastes with exactly one injected shortcut. Every failure before that
    /// shortcut is `NotSent`, so the text can be delivered again safely.
    fn paste(&mut self, job: &RecordingJob) -> DeliveryDisposition {
        let deadline = Instant::now() + DELIVERY_TIMEOUT;
        let mut delivery = PasteDelivery::new(self.shortcut_mode);
        let mut copied_this_attempt = false;
        // Time per stage, logged with the submitted paste.
        let (mut focus_time, mut clipboard_time) = (Duration::ZERO, Duration::ZERO);
        loop {
            let next = match delivery.action() {
                DeliveryAction::ObserveFocus => {
                    if Instant::now() >= deadline {
                        delivery.advance(DeliveryObservation::DeadlineReached)
                    } else {
                        let started = Instant::now();
                        let observed = self.observe_focus(deadline);
                        focus_time += started.elapsed();
                        match observed {
                            Ok(target) => delivery.advance(DeliveryObservation::Focus(target)),
                            Err(error) => {
                                return DeliveryDisposition::NotSent {
                                    copied_to_clipboard: copied_this_attempt,
                                    reason: format!(
                                        "could not find the focused window, so nothing was pasted: {error}"
                                    ),
                                };
                            }
                        }
                    }
                }
                DeliveryAction::PublishClipboard(protocol) => {
                    let started = Instant::now();
                    let published = self.publish_delivery_text(&job.final_text, deadline);
                    clipboard_time += started.elapsed();
                    if let Err(error) = published {
                        return DeliveryDisposition::NotSent {
                            copied_to_clipboard: false,
                            reason: format!(
                                "could not copy the text, so nothing was pasted: {error}"
                            ),
                        };
                    }
                    copied_this_attempt = true;
                    delivery.advance(DeliveryObservation::ClipboardReady(protocol))
                }
                DeliveryAction::InjectPaste { target, shortcut } => {
                    // This is deliberately exactly one injection. Once the
                    // command starts, an error is ambiguous and must not retry.
                    let protocol = target.protocol();
                    let started = Instant::now();
                    let injected = self.injector.inject(shortcut, deadline);
                    let inject_ms = started.elapsed().as_millis() as u64;
                    match injected {
                        Ok(key_pressed) => {
                            // Clipboard managers fetch the text as soon as it
                            // is published; only a request after the key
                            // press comes from the paste.
                            let requested = self.selections.text_requested_since(
                                key_pressed,
                                key_pressed + PASTE_REQUEST_WINDOW,
                            );
                            tracing::info!(
                                job_id = %job.id,
                                ?protocol,
                                window_class = target.window_class(),
                                ?shortcut,
                                method = "uinput",
                                focus_ms = focus_time.as_millis() as u64,
                                clipboard_ms = clipboard_time.as_millis() as u64,
                                inject_ms,
                                consumed = requested.is_some(),
                                "paste command submitted"
                            );
                            if let Some(requested) = requested {
                                tracing::info!(
                                    job_id = %job.id,
                                    request_ms =
                                        requested.duration_since(key_pressed).as_millis() as u64,
                                    "target requested the text"
                                );
                            }
                            delivery.advance(DeliveryObservation::PasteSent {
                                consumed: requested.is_some(),
                            })
                        }
                        Err(error) => {
                            tracing::warn!(
                                ?protocol,
                                ?shortcut,
                                %error,
                                "paste command outcome is ambiguous"
                            );
                            delivery.advance(DeliveryObservation::InjectionFailed)
                        }
                    }
                }
                DeliveryAction::Finished(result) => {
                    return match result.failure {
                        None => DeliveryDisposition::Submitted {
                            copied_to_clipboard: result.copied,
                            paste_triggered: result.paste_triggered,
                        },
                        Some(DeliveryFailure::InjectionAmbiguous) => {
                            DeliveryDisposition::Ambiguous {
                                copied_to_clipboard: result.copied,
                            }
                        }
                        Some(DeliveryFailure::FocusUnstable) => DeliveryDisposition::NotSent {
                            copied_to_clipboard: result.copied,
                            reason: "the focused window kept changing, so nothing was pasted"
                                .to_owned(),
                        },
                        Some(DeliveryFailure::ClipboardUnavailable) => {
                            DeliveryDisposition::NotSent {
                                copied_to_clipboard: result.copied,
                                reason: "the clipboard was not ready, so nothing was pasted"
                                    .to_owned(),
                            }
                        }
                    };
                }
            };
            if matches!(next, DeliveryAction::Finished(_)) {
                continue;
            }
        }
    }
}

impl Deliverer for SystemDeliverer {
    fn deliver(
        &mut self,
        job: &RecordingJob,
        method: DeliveryMethod,
    ) -> Result<DeliveryDisposition, ExternalError> {
        Ok(match method {
            DeliveryMethod::Paste => self.paste(job),
            DeliveryMethod::CopyOnly => match self.copy_text(&job.final_text) {
                Ok(()) => DeliveryDisposition::Submitted {
                    copied_to_clipboard: true,
                    paste_triggered: false,
                },
                Err(error) => DeliveryDisposition::NotSent {
                    copied_to_clipboard: false,
                    reason: format!("could not copy the text: {error}"),
                },
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::Write,
        os::unix::fs::PermissionsExt,
        sync::{Arc, Mutex},
        thread,
    };

    use agentdictate_core::{JobId, JobStage};
    use agentdictate_runtime::{DeliveryStatus, Recorder};
    use chrono::Utc;
    use tempfile::tempdir;

    use evdev::{Device as EvdevReader, EventType as EvdevEventType, KeyCode as EvdevKeyCode};

    /// Grabs the given injector's own uinput device (EVIOCGRAB) so injected
    /// chords are consumed by the test instead of reaching the live desktop.
    /// Targeting the injector's node keeps this unambiguous even while a real
    /// agentdictated daemon (with an identically named device) is running.
    fn grab_injection_device(injector: &mut PasteInjector) -> EvdevReader {
        let node = injector
            .device_node()
            .expect("injector exposes a device node");
        // udev applies the session ACL to a fresh uinput node asynchronously;
        // retry the open briefly instead of failing on the race.
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut reader = loop {
            match EvdevReader::open(&node) {
                Ok(reader) => break reader,
                Err(error)
                    if error.kind() == std::io::ErrorKind::PermissionDenied
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(error) => panic!("open {} failed: {error}", node.display()),
            }
        };
        reader.grab().expect("injection device is grabbable");
        reader
            .set_nonblocking(true)
            .expect("reader supports nonblocking");
        reader
    }

    /// Written straight to stderr because libtest hides `eprintln!` output of
    /// passing tests.
    fn skip(test: &str) {
        let _ = writeln!(std::io::stderr(), "SKIPPED {test}: /dev/uinput is missing");
    }

    fn injected_key_events(reader: &mut EvdevReader, expected: usize) -> Vec<(EvdevKeyCode, i32)> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut events = Vec::new();
        while events.len() < expected && Instant::now() < deadline {
            match reader.fetch_events() {
                Ok(batch) => events.extend(
                    batch
                        .filter(|event| event.event_type() == EvdevEventType::KEY)
                        .map(|event| (EvdevKeyCode::new(event.code()), event.value())),
                ),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("reading injected events failed: {error}"),
            }
        }
        events
    }

    /// Records publications instead of owning the desktop's selections, and
    /// reports every paste as requested by its target.
    #[derive(Clone, Default)]
    struct FakeSelections {
        published: Arc<Mutex<Vec<Publication>>>,
    }

    type Publication = (String, Vec<ClipboardSelection>);

    impl Selections for FakeSelections {
        fn publish(
            &mut self,
            text: &str,
            selections: &[ClipboardSelection],
            _deadline: Instant,
        ) -> Result<(), ClipboardError> {
            self.published
                .lock()
                .unwrap()
                .push((text.to_owned(), selections.to_vec()));
            Ok(())
        }

        fn text_requested_since(&self, since: Instant, _until: Instant) -> Option<Instant> {
            Some(since)
        }
    }

    fn ready_job(directory: &std::path::Path, final_text: &str) -> RecordingJob {
        let now = Utc::now();
        RecordingJob {
            options: None,
            id: JobId::new(),
            started_at: now,
            updated_at: now,
            stage: JobStage::ReadyToDeliver,
            audio_path: directory.join("recording.wav"),
            duration_seconds: 1.0,
            transcription_model: "test".to_owned(),
            raw_transcript: final_text.to_lowercase(),
            final_text: final_text.to_owned(),
            copied_to_clipboard: false,
            paste_triggered: false,
            delivery_status: DeliveryStatus::NotAttempted,
            error_message: None,
        }
    }

    #[test]
    fn successful_paste_command_is_reported_as_submitted() {
        if !std::path::Path::new("/dev/uinput").exists() {
            skip("successful_paste_command_is_reported_as_submitted");
            return;
        }
        let mut injector = PasteInjector::new();
        let mut reader = grab_injection_device(&mut injector);
        let directory = tempdir().unwrap();
        let selections = FakeSelections::default();
        let mut deliverer = SystemDeliverer {
            selections: Box::new(selections.clone()),
            focus: |_| {
                Ok(X11FocusObservation {
                    window_id: 42,
                    window_class: "chatgpt Chatgpt".to_owned(),
                    focused: true,
                })
            },
            injector,
            shortcut_mode: ShortcutMode::Standard,
            wayland_session: false,
        };
        let job = ready_job(directory.path(), "Submitted words.");

        let disposition = deliverer.deliver(&job, DeliveryMethod::Paste).unwrap();

        assert_eq!(
            disposition,
            DeliveryDisposition::Submitted {
                copied_to_clipboard: true,
                paste_triggered: true,
            }
        );
        assert_eq!(
            *selections.published.lock().unwrap(),
            [(job.final_text.clone(), vec![ClipboardSelection::Clipboard])]
        );
        assert_eq!(
            injected_key_events(&mut reader, 4),
            vec![
                (EvdevKeyCode::KEY_LEFTCTRL, 1),
                (EvdevKeyCode::KEY_V, 1),
                (EvdevKeyCode::KEY_V, 0),
                (EvdevKeyCode::KEY_LEFTCTRL, 0),
            ],
        );
    }

    #[test]
    fn automatic_delivery_to_a_terminal_publishes_both_selections_before_one_universal_paste() {
        if !std::path::Path::new("/dev/uinput").exists() {
            skip(
                "automatic_delivery_to_a_terminal_publishes_both_selections_before_one_universal_paste",
            );
            return;
        }
        let mut injector = PasteInjector::new();
        let mut reader = grab_injection_device(&mut injector);
        let directory = tempdir().unwrap();
        let selections = FakeSelections::default();
        let mut deliverer = SystemDeliverer {
            selections: Box::new(selections.clone()),
            focus: |_| {
                Ok(X11FocusObservation {
                    window_id: 84,
                    window_class: "xterm XTerm".to_owned(),
                    focused: true,
                })
            },
            injector,
            shortcut_mode: ShortcutMode::Auto,
            wayland_session: true,
        };
        let job = ready_job(directory.path(), "Terminal transcript.");

        let disposition = deliverer.deliver(&job, DeliveryMethod::Paste).unwrap();

        assert_eq!(
            disposition,
            DeliveryDisposition::Submitted {
                copied_to_clipboard: true,
                paste_triggered: true,
            }
        );
        assert_eq!(
            *selections.published.lock().unwrap(),
            [(
                job.final_text.clone(),
                vec![ClipboardSelection::Primary, ClipboardSelection::Clipboard]
            )]
        );
        assert_eq!(
            injected_key_events(&mut reader, 4),
            vec![
                (EvdevKeyCode::KEY_LEFTSHIFT, 1),
                (EvdevKeyCode::KEY_INSERT, 1),
                (EvdevKeyCode::KEY_INSERT, 0),
                (EvdevKeyCode::KEY_LEFTSHIFT, 0),
            ],
        );
    }

    use super::*;

    #[test]
    fn recorder_survives_the_ipc_worker_thread_that_started_it() {
        let directory = tempdir().unwrap();
        let recorder = directory.path().join("fake-pw-record");
        let stopped = directory.path().join("stopped");
        fs::write(
            &recorder,
            format!(
                "#!/bin/sh\n\
                 for output do :; done\n\
                 trap 'printf stopped > \"{}\"; exit 0' INT TERM\n\
                 printf 'RIFF\\000\\000\\000\\000WAVEfmt \\020\\000\\000\\000\\001\\000\\001\\000\\200\\076\\000\\000\\000\\175\\000\\000\\002\\000\\020\\000data\\000\\000\\000\\000' > \"$output\"\n\
                 while :; do printf '0000000000000000' >> \"$output\"; sleep 0.01; done\n",
                stopped.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&recorder, fs::Permissions::from_mode(0o755)).unwrap();
        let settings = Settings {
            audio_ducking_enabled: false,
            ..Settings::default()
        };
        let controller = Arc::new(Mutex::new(SystemRecordingController::for_program(
            &settings,
            directory.path(),
            &recorder,
        )));
        let now = Utc::now();
        let job = RecordingJob {
            options: None,
            id: JobId::new(),
            started_at: now,
            updated_at: now,
            stage: JobStage::Starting,
            audio_path: directory.path().join("recording.wav"),
            duration_seconds: 0.0,
            transcription_model: "test".to_owned(),
            raw_transcript: String::new(),
            final_text: String::new(),
            copied_to_clipboard: false,
            paste_triggered: false,
            delivery_status: DeliveryStatus::NotAttempted,
            error_message: None,
        };
        let starter = {
            let controller = Arc::clone(&controller);
            let job = job.clone();
            thread::spawn(move || controller.lock().unwrap().start(&job))
        };

        starter.join().unwrap().unwrap();
        let observation_deadline = Instant::now() + Duration::from_millis(200);
        while !stopped.exists() && Instant::now() < observation_deadline {
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            !stopped.exists(),
            "the recorder inherited the lifetime of a completed IPC thread"
        );

        controller.lock().unwrap().abort_start(&job).unwrap();
        assert!(
            stopped.exists(),
            "checkpoint-failure compensation still reaches the recorder"
        );
    }
}
