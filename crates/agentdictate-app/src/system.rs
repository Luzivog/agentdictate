use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[cfg(test)]
use agentdictate_core::TranscriptionProvider;
use agentdictate_core::{ClientCommand, JobId, ServerMessageKind, Settings};
use agentdictate_linux::{
    audio_ducking::PlaybackDucker,
    clipboard::{ClipboardPublication, ClipboardSelection, CommandClipboard},
    command::{
        PlatformCapability, PlatformCommandError, PlatformExecutable, PlatformTool,
        SystemCommandRunner,
    },
    focus::X11FocusObserver,
    injection::PasteInjector,
    paste::{
        ClipboardProtocol, DeliveryAction, DeliveryFailure, DeliveryObservation, PasteDelivery,
        ShortcutMode, resolve_focus_target,
    },
    recorder::{PwRecordRecorder, Recording, RecordingExitObserver},
};
use agentdictate_runtime::IpcClient;
use agentdictate_runtime::{Deliverer, DeliveryDisposition, ExternalError, Recorder, RecordingJob};

use crate::{CapturedRecording, RecordingController};

const WORK_AREA_DETECTION_TIMEOUT: Duration = Duration::from_millis(250);

pub fn detect_primary_work_area() -> Option<agentdictate_ui::LogicalRect> {
    let xprop = PlatformExecutable::discover(PlatformTool::Xprop);
    detect_primary_work_area_with(
        &SystemCommandRunner,
        &xprop,
        Instant::now() + WORK_AREA_DETECTION_TIMEOUT,
    )
}

fn detect_primary_work_area_with(
    runner: &SystemCommandRunner,
    xprop: &PlatformExecutable,
    deadline: Instant,
) -> Option<agentdictate_ui::LogicalRect> {
    let output = runner
        .run_output(
            PlatformCapability::FocusObservation,
            xprop,
            &[
                OsString::from("-root"),
                OsString::from("_NET_CURRENT_DESKTOP"),
                OsString::from("_NET_WORKAREA"),
            ],
            deadline,
        )
        .ok()?;
    parse_x11_work_area(&String::from_utf8_lossy(&output))
}

#[must_use]
pub fn parse_x11_work_area(output: &str) -> Option<agentdictate_ui::LogicalRect> {
    let desktop = output
        .lines()
        .find(|line| line.contains("_NET_CURRENT_DESKTOP"))
        .and_then(|line| line.split_once('='))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let values = output
        .lines()
        .find(|line| line.contains("_NET_WORKAREA"))
        .and_then(|line| line.split_once('='))?
        .1
        .split(',')
        .map(|value| value.trim().parse::<i64>())
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let offset = desktop.checked_mul(4)?;
    let geometry = values.get(offset..offset + 4)?;
    Some(agentdictate_ui::LogicalRect::new(
        i32::try_from(geometry[0]).ok()?,
        i32::try_from(geometry[1]).ok()?,
        u32::try_from(geometry[2]).ok()?,
        u32::try_from(geometry[3]).ok()?,
    ))
}

const RECORDER_START_TIMEOUT: Duration = Duration::from_secs(10);
const RECORDER_STOP_TIMEOUT: Duration = Duration::from_secs(10);
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(5);
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
    #[must_use]
    pub fn for_system(settings: &Settings, runtime_directory: &Path) -> Self {
        Self::new(settings, runtime_directory, "pw-record")
    }

    fn new(
        settings: &Settings,
        runtime_directory: &Path,
        recorder_program: impl Into<PathBuf>,
    ) -> Self {
        Self {
            recorder: RecorderOwner::start(PwRecordRecorder::new(
                SystemCommandRunner,
                recorder_program,
            )),
            ducker: PlaybackDucker::default(),
            settings: settings.clone(),
            runtime_directory: runtime_directory.to_owned(),
        }
    }

    #[cfg(test)]
    fn for_program(settings: &Settings, runtime_directory: &Path, recorder_program: &Path) -> Self {
        Self::new(settings, runtime_directory, recorder_program)
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

pub struct SystemDeliverer {
    clipboard: CommandClipboard,
    focus: X11FocusObserver,
    injector: PasteInjector,
    shortcut_mode: ShortcutMode,
    wayland_session: bool,
    /// Clipboard protocols are ownership based. Keeping the publisher alive
    /// after injection prevents a target that reads asynchronously from seeing
    /// an empty clipboard.
    active_publications: Vec<ClipboardPublication>,
}

impl SystemDeliverer {
    #[must_use]
    pub fn for_environment(paste_shortcut: &str) -> Self {
        let runner = SystemCommandRunner;
        Self {
            clipboard: CommandClipboard::for_system(runner),
            focus: X11FocusObserver::for_system(runner),
            injector: PasteInjector::new(),
            shortcut_mode: shortcut_mode(paste_shortcut),
            wayland_session: std::env::var("XDG_SESSION_TYPE")
                .is_ok_and(|session| session.eq_ignore_ascii_case("wayland")),
            active_publications: Vec::new(),
        }
    }

    pub fn update_shortcut(&mut self, paste_shortcut: &str) {
        self.shortcut_mode = shortcut_mode(paste_shortcut);
    }

    fn observe_focus(
        &self,
        deadline: Instant,
    ) -> Result<agentdictate_linux::paste::FocusTarget, ExternalError> {
        match self.focus.observe(deadline) {
            Ok(observation) => Ok(resolve_focus_target(
                self.wayland_session,
                Some(observation),
            )),
            Err(_) if self.wayland_session => Ok(resolve_focus_target(true, None)),
            Err(error) => Err(ExternalError::new(error.to_string())),
        }
    }

    pub fn copy_text(&mut self, text: &str) -> Result<(), ExternalError> {
        let publication = self
            .clipboard
            .publish(text.as_bytes(), Instant::now() + DELIVERY_TIMEOUT)
            .map_err(|error| ExternalError::new(error.to_string()))?;
        self.active_publications = vec![publication];
        Ok(())
    }

    fn publish_delivery_text(
        &self,
        protocol: ClipboardProtocol,
        contents: &[u8],
        deadline: Instant,
    ) -> Result<Vec<ClipboardPublication>, PlatformCommandError> {
        let selections: &[ClipboardSelection] = match (self.shortcut_mode, protocol) {
            (ShortcutMode::Auto, ClipboardProtocol::Wayland) => &[
                // Some terminals bind Shift+Insert to the primary selection,
                // while regular applications bind it to the clipboard. Publish
                // primary first so a later failure never claims a new clipboard.
                ClipboardSelection::Primary,
                ClipboardSelection::Clipboard,
            ],
            _ => &[ClipboardSelection::Clipboard],
        };
        selections
            .iter()
            .map(|selection| {
                self.clipboard
                    .publish_selection(*selection, contents, deadline)
            })
            .collect()
    }
}

impl Deliverer for SystemDeliverer {
    fn deliver(&mut self, job: &RecordingJob) -> Result<DeliveryDisposition, ExternalError> {
        let deadline = Instant::now() + DELIVERY_TIMEOUT;
        let mut delivery = PasteDelivery::new(self.shortcut_mode);
        let mut copied_this_attempt = false;
        loop {
            let next = match delivery.action() {
                DeliveryAction::ObserveFocus => {
                    if Instant::now() >= deadline {
                        delivery.advance(DeliveryObservation::DeadlineReached)
                    } else {
                        match self.observe_focus(deadline) {
                            Ok(target) => delivery.advance(DeliveryObservation::Focus(target)),
                            Err(_) if copied_this_attempt => {
                                return Ok(DeliveryDisposition::Ambiguous {
                                    copied_to_clipboard: true,
                                });
                            }
                            Err(error) => return Err(error),
                        }
                    }
                }
                DeliveryAction::PublishClipboard(protocol) => {
                    let publications = self
                        .publish_delivery_text(protocol, job.final_text.as_bytes(), deadline)
                        .map_err(|error| ExternalError::new(error.to_string()))?;
                    debug_assert!(
                        publications
                            .iter()
                            .all(|publication| publication.evidence.confirms_ready())
                    );
                    self.active_publications = publications;
                    copied_this_attempt = true;
                    delivery.advance(DeliveryObservation::ClipboardReady(protocol))
                }
                DeliveryAction::InjectPaste { target, shortcut } => {
                    // This is deliberately exactly one injection. Once the
                    // command starts, an error is ambiguous and must not retry.
                    let protocol = target.protocol();
                    let sent = match self.injector.inject(shortcut, deadline) {
                        Ok(()) => {
                            tracing::info!(
                                ?protocol,
                                ?shortcut,
                                method = "uinput",
                                "paste command submitted"
                            );
                            true
                        }
                        Err(error) => {
                            tracing::warn!(
                                ?protocol,
                                ?shortcut,
                                %error,
                                "paste command outcome is ambiguous"
                            );
                            false
                        }
                    };
                    delivery.advance(DeliveryObservation::InjectionFinished(sent))
                }
                DeliveryAction::Finished(result) => {
                    return match result.failure {
                        None => Ok(DeliveryDisposition::Submitted {
                            copied_to_clipboard: result.copied,
                            paste_triggered: result.paste_triggered,
                        }),
                        Some(
                            DeliveryFailure::FocusUnstable | DeliveryFailure::InjectionAmbiguous,
                        ) if result.copied => Ok(DeliveryDisposition::Ambiguous {
                            copied_to_clipboard: true,
                        }),
                        Some(failure) => Err(ExternalError::new(format!(
                            "text delivery failed: {failure:?}"
                        ))),
                    };
                }
            };
            if matches!(next, DeliveryAction::Finished(_)) {
                continue;
            }
        }
    }
}

fn shortcut_mode(paste_shortcut: &str) -> ShortcutMode {
    let shortcut = paste_shortcut.to_ascii_lowercase();
    if shortcut.starts_with("terminal") || shortcut == "ctrl+shift+v" {
        ShortcutMode::Terminal
    } else if shortcut.starts_with("standard") || shortcut == "ctrl+v" {
        ShortcutMode::Standard
    } else {
        ShortcutMode::Auto
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
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
        let node = injector.device_node().expect("injector exposes a device node");
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
        reader.set_nonblocking(true).expect("reader supports nonblocking");
        reader
    }

    fn injected_key_events(
        reader: &mut EvdevReader,
        expected: usize,
    ) -> Vec<(EvdevKeyCode, i32)> {
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

    #[test]
    fn work_area_detection_returns_after_its_command_deadline() {
        let directory = tempdir().unwrap();
        let xprop = directory.path().join("xprop");
        fs::write(&xprop, "#!/bin/sh\nwhile :; do :; done\n").unwrap();
        fs::set_permissions(&xprop, fs::Permissions::from_mode(0o755)).unwrap();
        let executable = PlatformExecutable::at(PlatformTool::Xprop, xprop);
        let started = Instant::now();

        let result = detect_primary_work_area_with(
            &SystemCommandRunner,
            &executable,
            started + Duration::from_millis(20),
        );

        assert_eq!(result, None);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn successful_paste_command_is_reported_as_submitted() {
        if !std::path::Path::new("/dev/uinput").exists() {
            return;
        }
        let mut injector = PasteInjector::new();
        let mut reader = grab_injection_device(&mut injector);
        let directory = tempdir().unwrap();
        let clipboard_state = directory.path().join("clipboard.txt");
        let xsel = directory.path().join("xsel");
        fs::write(
            &xsel,
            format!(
                concat!(
                    "#!/bin/sh\n",
                    "case \"$*\" in\n",
                    "  *--output*) cat '{}' ;;\n",
                    "  *) cat > '{}'; exec tail -f /dev/null ;;\n",
                    "esac\n",
                ),
                clipboard_state.display(),
                clipboard_state.display(),
            ),
        )
        .unwrap();
        fs::set_permissions(&xsel, fs::Permissions::from_mode(0o755)).unwrap();
        let xdotool = directory.path().join("xdotool");
        fs::write(
            &xdotool,
            "#!/bin/sh\ncase \"$1\" in\n  getactivewindow) printf '42\\n' ;;\nesac\n",
        )
        .unwrap();
        fs::set_permissions(&xdotool, fs::Permissions::from_mode(0o755)).unwrap();
        let xprop = directory.path().join("xprop");
        fs::write(
            &xprop,
            concat!(
                "#!/bin/sh\n",
                "printf '%s\\n' 'WM_CLASS(STRING) = \"chatgpt\", \"Chatgpt\"'\n",
                "printf '%s\\n' '_NET_WM_STATE(ATOM) = _NET_WM_STATE_FOCUSED'\n",
            ),
        )
        .unwrap();
        fs::set_permissions(&xprop, fs::Permissions::from_mode(0o755)).unwrap();
        let runner = SystemCommandRunner;
        let xdotool = PlatformExecutable::at(PlatformTool::Xdotool, xdotool);
        let mut deliverer = SystemDeliverer {
            clipboard: CommandClipboard::new(
                runner,
                PlatformExecutable::at(PlatformTool::Xsel, xsel),
            ),
            focus: X11FocusObserver::new(
                runner,
                xdotool,
                PlatformExecutable::at(PlatformTool::Xprop, xprop),
            ),
            injector,
            shortcut_mode: ShortcutMode::Standard,
            wayland_session: false,
            active_publications: Vec::new(),
        };
        let now = Utc::now();
        let job = RecordingJob {
            id: JobId::new(),
            legacy_id: 1,
            started_at: now,
            updated_at: now,
            stage: JobStage::ReadyToDeliver,
            audio_path: directory.path().join("recording.wav"),
            duration_seconds: 1.0,
            transcription_provider: TranscriptionProvider::OpenAiApi,
            transcription_model: "test".to_owned(),
            raw_transcript: "submitted words".to_owned(),
            final_text: "Submitted words.".to_owned(),
            copied_to_clipboard: false,
            paste_triggered: false,
            delivery_status: DeliveryStatus::NotAttempted,
            error_message: None,
            cleanup_error: None,
        };

        let disposition = deliverer.deliver(&job).unwrap();

        assert_eq!(
            disposition,
            DeliveryDisposition::Submitted {
                copied_to_clipboard: true,
                paste_triggered: true,
            }
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
    fn automatic_wayland_delivery_prepares_both_selections_before_one_universal_paste() {
        if !std::path::Path::new("/dev/uinput").exists() {
            return;
        }
        let mut injector = PasteInjector::new();
        let mut reader = grab_injection_device(&mut injector);
        let directory = tempdir().unwrap();
        let clipboard_state = directory.path().join("clipboard.txt");
        let primary_state = directory.path().join("primary.txt");
        let xsel_log = directory.path().join("xsel.log");
        let xsel = directory.path().join("xsel");
        fs::write(
            &xsel,
            format!(
                concat!(
                    "#!/bin/sh\n",
                    "printf '%s\\n' \"$*\" >> '{}'\n",
                    "case \"$*\" in\n",
                    "  *--primary*) state='{}' ;;\n",
                    "  *) state='{}' ;;\n",
                    "esac\n",
                    "case \"$*\" in\n",
                    "  *--output*) cat \"$state\" ;;\n",
                    "  *) cat > \"$state\"; exec tail -f /dev/null ;;\n",
                    "esac\n",
                ),
                xsel_log.display(),
                primary_state.display(),
                clipboard_state.display(),
            ),
        )
        .unwrap();
        fs::set_permissions(&xsel, fs::Permissions::from_mode(0o755)).unwrap();
        let runner = SystemCommandRunner;
        let mut deliverer = SystemDeliverer {
            clipboard: CommandClipboard::new(
                runner,
                PlatformExecutable::at(PlatformTool::Xsel, xsel),
            ),
            focus: X11FocusObserver::new(
                runner,
                PlatformExecutable::missing(PlatformTool::Xdotool),
                PlatformExecutable::missing(PlatformTool::Xprop),
            ),
            injector,
            shortcut_mode: ShortcutMode::Auto,
            wayland_session: true,
            active_publications: Vec::new(),
        };
        let now = Utc::now();
        let job = RecordingJob {
            id: JobId::new(),
            legacy_id: 1,
            started_at: now,
            updated_at: now,
            stage: JobStage::ReadyToDeliver,
            audio_path: directory.path().join("recording.wav"),
            duration_seconds: 1.0,
            transcription_provider: TranscriptionProvider::OpenAiApi,
            transcription_model: "test".to_owned(),
            raw_transcript: "wayland transcript".to_owned(),
            final_text: "Wayland transcript.".to_owned(),
            copied_to_clipboard: false,
            paste_triggered: false,
            delivery_status: DeliveryStatus::NotAttempted,
            error_message: None,
            cleanup_error: None,
        };

        let disposition = deliverer.deliver(&job).unwrap();

        assert_eq!(
            disposition,
            DeliveryDisposition::Submitted {
                copied_to_clipboard: true,
                paste_triggered: true,
            }
        );
        assert_eq!(
            fs::read(&clipboard_state).unwrap(),
            job.final_text.as_bytes()
        );
        assert_eq!(fs::read(&primary_state).unwrap(), job.final_text.as_bytes());
        let xsel_arguments = fs::read_to_string(xsel_log).unwrap();
        let owner_lines = xsel_arguments
            .lines()
            .filter(|arguments| arguments.ends_with("--input --nodetach"))
            .collect::<Vec<_>>();
        // Primary is claimed before the clipboard so a later failure never
        // leaves a fresh clipboard without its primary counterpart.
        assert_eq!(
            owner_lines,
            [
                "--primary --input --nodetach",
                "--clipboard --input --nodetach",
            ],
        );
        assert!(
            xsel_arguments
                .lines()
                .any(|arguments| arguments == "--primary --output")
        );
        assert!(
            xsel_arguments
                .lines()
                .any(|arguments| arguments == "--clipboard --output")
        );
        assert!(xsel_arguments.lines().all(|arguments| {
            matches!(
                arguments,
                "--primary --input --nodetach"
                    | "--clipboard --input --nodetach"
                    | "--primary --output"
                    | "--clipboard --output"
            )
        }));
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
                 printf 'RIFF0000WAVEfmt 00000000000000000000data0000000000000000' > \"$output\"\n\
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
            id: JobId::new(),
            legacy_id: 1,
            started_at: now,
            updated_at: now,
            stage: JobStage::Starting,
            audio_path: directory.path().join("recording.wav"),
            duration_seconds: 0.0,
            transcription_provider: TranscriptionProvider::OpenAiApi,
            transcription_model: "test".to_owned(),
            raw_transcript: String::new(),
            final_text: String::new(),
            copied_to_clipboard: false,
            paste_triggered: false,
            delivery_status: DeliveryStatus::NotAttempted,
            error_message: None,
            cleanup_error: None,
        };
        let starter = {
            let controller = Arc::clone(&controller);
            let job = job.clone();
            thread::spawn(move || controller.lock().unwrap().start(&job))
        };

        starter.join().unwrap().unwrap();
        let observation_deadline = Instant::now() + Duration::from_millis(200);
        while !stopped.exists() && Instant::now() < observation_deadline {
            thread::yield_now();
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
