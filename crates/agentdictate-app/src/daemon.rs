use std::fs;
use std::sync::{
    Arc, Condvar, Mutex, PoisonError,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use agentdictate_core::{
    AppSnapshot, DesktopReadiness, DictationNotice, FailureKind, HotkeyReadiness, JobId, JobStage,
    MissingTool, Readiness, RecordingMode, Settings, Workflow, WorkflowError, WorkflowPhase,
    WorkflowSignal, WorkflowSnapshot,
};
use agentdictate_runtime::{
    Deliverer, DeliveryDisposition, DeliveryGate, DeliveryGateError, DeliveryMethod, ExternalError,
    HeadlessDeliveryGate, JobFailure, ObservedFocus, Recorder, RecordingJob, RecordingRequest,
    Runtime, RuntimeError, StoredTranscript,
};
use chrono::Utc;
use thiserror::Error;

use crate::{
    ActiveRecordingUpdate, AppPaths, FinishingEncode, Notifier, OverlayController, OverlayUpdate,
    ProcessingTicket, Transcriber, TranscriptionCompletion,
};

/// Recovery's message on a transcript that finished after its dictation was
/// cancelled.
const CANCELLED_NOTE: &str = "Cancelled before paste";

/// A recording whose WAV the recorder finalized.
#[derive(Debug)]
pub struct CapturedRecording {
    pub duration_seconds: f64,
    /// Its upload audio, encoded while it recorded. Dropping it, as a discard
    /// does, kills the encoder; without it, the saved WAV is encoded.
    pub encoding: Option<FinishingEncode>,
}

/// Recorder lifecycle owned by the daemon. `Recorder::start` is called only
/// after the durable Starting checkpoint; `finish` must finalize the WAV before
/// the Captured checkpoint is written.
pub trait RecordingController: Recorder {
    fn finish(&mut self, job: &RecordingJob) -> Result<CapturedRecording, ExternalError>;

    /// Follows saved settings, such as audio ducking.
    fn update_settings(&mut self, _settings: &Settings) {}
}

/// What the recorder reports about the active recording, at most once per
/// recording. The daemon stops the recording, or preserves it for Recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecorderEvent {
    /// The recorder process exited without being asked to.
    Exited { job_id: JobId },
    /// The microphone stopped delivering audio.
    Stalled { job_id: JobId },
    /// The recording reached the "Stop recording after" setting.
    MaxDurationReached { job_id: JobId },
}

impl RecorderEvent {
    #[must_use]
    pub const fn job_id(self) -> JobId {
        match self {
            Self::Exited { job_id }
            | Self::Stalled { job_id }
            | Self::MaxDurationReached { job_id } => job_id,
        }
    }
}

/// The daemon's delivery adapter: the runtime's paste-or-copy step, plus
/// copying a History entry and following the paste-shortcut setting.
pub trait DaemonDeliverer: Deliverer {
    fn copy_text(&mut self, text: &str) -> Result<(), ExternalError>;

    fn update_settings(&mut self, _settings: &Settings) {}
}

enum OverlayDeliveryGate {
    Headless(HeadlessDeliveryGate),
    Live(OverlayController),
}

impl DeliveryGate for OverlayDeliveryGate {
    fn confirm_ready(&mut self) -> Result<(), DeliveryGateError> {
        match self {
            Self::Headless(gate) => gate.confirm_ready(),
            Self::Live(gate) => gate.confirm_ready(),
        }
    }
}

/// Wraps one delivery step to record when it ran, for the per-dictation
/// timing log, and what it reported. The runtime calls the overlay gate,
/// then the deliverer, whose return marks the moment the paste was
/// submitted and tells whether the target took it.
struct Timed<'a, T, R> {
    inner: &'a mut T,
    ran: Option<(Instant, Instant)>,
    reported: Option<R>,
}

impl<'a, T, R: Clone> Timed<'a, T, R> {
    const fn new(inner: &'a mut T) -> Self {
        Self {
            inner,
            ran: None,
            reported: None,
        }
    }

    fn record<E>(&mut self, step: impl FnOnce(&mut T) -> Result<R, E>) -> Result<R, E> {
        let started = Instant::now();
        let result = step(self.inner);
        self.ran = Some((started, Instant::now()));
        self.reported = result.as_ref().ok().cloned();
        result
    }
}

impl<G: DeliveryGate> DeliveryGate for Timed<'_, G, ()> {
    fn confirm_ready(&mut self) -> Result<(), DeliveryGateError> {
        self.record(G::confirm_ready)
    }
}

impl<D: Deliverer> Deliverer for Timed<'_, D, DeliveryDisposition> {
    fn deliver(
        &mut self,
        job: &RecordingJob,
        method: DeliveryMethod,
    ) -> Result<DeliveryDisposition, ExternalError> {
        self.record(|deliverer| deliverer.deliver(job, method))
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Workflow(#[from] WorkflowError),
    #[error("recording operation failed: {0}")]
    Recording(#[from] ExternalError),
    #[error("recording storage could not be prepared: {0}")]
    Io(#[from] std::io::Error),
    #[error("another dictation is still in progress")]
    Busy { phase: WorkflowPhase },
    #[error("no recording is active")]
    NotRecording,
    #[error("no transcription is in progress")]
    NotProcessing,
    #[error("AgentDictate is shutting down")]
    ShuttingDown,
    #[error("no speech was found in this recording")]
    NoSpeech,
    #[error("{reason}")]
    NotCopied { reason: String },
    #[error("dictation {job_id} left transcription before its result arrived")]
    StaleResult { job_id: JobId },
    #[error("there is no dictation to paste yet")]
    NothingToPaste,
    #[error("dictation {job_id} is neither the last one nor waiting in Recovery")]
    PasteUnavailable { job_id: JobId },
    #[error("the dictation was not pasted again: {reason}")]
    NotPasted { reason: String },
}

/// Daemon state that other threads read without the daemon lock. The hotkey
/// loop must never wait for that lock: saving settings holds it while it
/// waits for the hotkey loop to accept a new shortcut.
#[derive(Debug)]
pub struct DaemonStatus {
    /// A recording is starting or running, so Esc can cancel it.
    recording: AtomicBool,
    recording_mode: Mutex<RecordingMode>,
    hotkey: Mutex<HotkeyReadiness>,
    /// Counts the changes the tray and the settings window show: whether a
    /// recording runs, the shortcut's readiness, and the settings.
    changes: Mutex<u64>,
    changed: Condvar,
}

impl DaemonStatus {
    fn new(recording_mode: RecordingMode) -> Self {
        Self {
            recording: AtomicBool::new(false),
            recording_mode: Mutex::new(recording_mode),
            hotkey: Mutex::new(HotkeyReadiness::Starting),
            changes: Mutex::new(0),
            changed: Condvar::new(),
        }
    }

    /// Records that something shown changed, waking every watcher.
    pub fn notify_changed(&self) {
        *self.changes.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        self.changed.notify_all();
    }

    /// Blocks until something shown changed after the change count `seen`,
    /// and returns the new count. Start from 0.
    #[must_use]
    pub fn wait_for_change(&self, seen: u64) -> u64 {
        let changes = self.changes.lock().unwrap_or_else(PoisonError::into_inner);
        *self
            .changed
            .wait_while(changes, |changes| *changes == seen)
            .unwrap_or_else(PoisonError::into_inner)
    }

    #[must_use]
    pub fn is_recording(&self) -> bool {
        self.recording.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn recording_mode(&self) -> RecordingMode {
        *self
            .recording_mode
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    #[must_use]
    pub fn hotkey_readiness(&self) -> HotkeyReadiness {
        self.hotkey
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn set_hotkey_readiness(&self, readiness: HotkeyReadiness) {
        let mut hotkey = self.hotkey.lock().unwrap_or_else(PoisonError::into_inner);
        if *hotkey != readiness {
            *hotkey = readiness;
            drop(hotkey);
            self.notify_changed();
        }
    }

    fn set_recording_mode(&self, mode: RecordingMode) {
        *self
            .recording_mode
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = mode;
    }
}

/// A result that arrives this long after the user stopped recording, beyond
/// the time its length normally takes to transcribe, is only copied: by then
/// they may have moved on to another window.
const STALE_PASTE_MARGIN: Duration = Duration::from_secs(8);

/// Upper bound on normal transcription time per second of audio (measured
/// about 27 ms); long dictations legitimately take longer before the paste.
const TRANSCRIPTION_TIME_PER_AUDIO_SECOND: Duration = Duration::from_millis(30);

/// How long after stop a dictation of this length may still be pasted.
pub(crate) fn stale_paste_after(audio_seconds: f64) -> Duration {
    STALE_PASTE_MARGIN + TRANSCRIPTION_TIME_PER_AUDIO_SECOND.mul_f64(audio_seconds.max(0.0))
}

/// Why a dictation's text is copied instead of pasted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CopyReason {
    /// It arrived long after the stop, for its length.
    ArrivedLate,
    /// The focused window observably changed since the stop.
    FocusMoved,
}

/// Whether a finished dictation is only copied instead of pasted (COR-8,
/// D19): when it arrived late, or when the focus moved since the stop, the
/// user may be somewhere else by now, and a paste could land in the wrong
/// place. `focus_now` is read only for a result that is not late.
pub(crate) fn copy_instead_of_paste(
    waited: Duration,
    audio_seconds: f64,
    focus_at_stop: ObservedFocus,
    focus_now: impl FnOnce() -> ObservedFocus,
) -> Option<CopyReason> {
    if waited > stale_paste_after(audio_seconds) {
        return Some(CopyReason::ArrivedLate);
    }
    focus_at_stop
        .moved_to(focus_now())
        .then_some(CopyReason::FocusMoved)
}

/// What to tell the user after a dictation's text was delivered: nothing
/// after a paste its target acknowledged; otherwise the text is on the
/// clipboard, and pressing Ctrl+V is up to them. An unacknowledged paste is
/// never sent again, since it may still have landed.
pub(crate) const fn delivered_notice(
    method: DeliveryMethod,
    consumed: bool,
) -> Option<DictationNotice> {
    match method {
        DeliveryMethod::Paste if consumed => None,
        DeliveryMethod::Paste | DeliveryMethod::CopyOnly => Some(DictationNotice::Copied),
    }
}

/// The one dictation the daemon may deliver.
enum Activity {
    Idle,
    Recording(ActiveRecording),
    Processing(ActiveProcessing),
}

struct ActiveRecording {
    job_id: JobId,
    overlay: ActiveRecordingUpdate,
}

/// A job whose transcription runs away from the daemon lock.
#[derive(Clone, Copy)]
struct ActiveProcessing {
    job_id: JobId,
    /// When the user stopped the recording or asked to transcribe it again.
    stopped_at: Instant,
    /// The focused window when the user stopped the recording.
    focus_at_stop: ObservedFocus,
    requester: Requester,
}

/// Who asked for a transcription, which decides how its text is delivered
/// and whether its end is announced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Requester {
    /// A dictation: pasted, or copied when it arrives late; announced on
    /// the overlay and as a notification when it is not pasted.
    Dictation,
    /// "Transcribe again" in the settings window, which has the focus and
    /// reports the result itself: copied, and not announced.
    Window,
    /// "Try again" on a notification: copied, and announced.
    Notification,
}

/// Owns the dictation lifecycle and its durable checkpoints. Every method is
/// bounded work: transcription itself runs from a `ProcessingTicket` that
/// `stop_recording` or `retry_transcription` returns, and its result comes
/// back through `complete_transcription`.
pub struct Daemon<R, T, D> {
    runtime: Runtime,
    settings: Settings,
    paths: AppPaths,
    recorder: R,
    transcriber: T,
    deliverer: D,
    workflow: Workflow,
    activity: Activity,
    recoverable_count: usize,
    /// The last dictation delivered, or whose delivery failed, for "Paste
    /// last dictation". It is kept in memory only, and forgotten when its
    /// text is deleted.
    last_dictation: Option<RecordingJob>,
    overlay: OverlayDeliveryGate,
    notifier: Option<Notifier>,
    /// Set while shutting down, when a dictation ending is not announced.
    quiet: bool,
    /// Checks what the desktop provides; tests keep the default, which
    /// reports everything in place.
    check_desktop: fn() -> DesktopReadiness,
    status: Arc<DaemonStatus>,
}

impl<R, T, D> Daemon<R, T, D>
where
    R: RecordingController,
    T: Transcriber,
    D: Deliverer,
{
    #[must_use]
    pub fn new(
        runtime: Runtime,
        settings: Settings,
        paths: AppPaths,
        recorder: R,
        transcriber: T,
        deliverer: D,
    ) -> Self {
        let recoverable_count = runtime.recoveries().map_or(0, |entries| entries.len());
        let status = Arc::new(DaemonStatus::new(settings.recording_mode));
        Self {
            runtime,
            settings,
            paths,
            recorder,
            transcriber,
            deliverer,
            workflow: Workflow::new(),
            activity: Activity::Idle,
            recoverable_count,
            last_dictation: None,
            overlay: OverlayDeliveryGate::Headless(HeadlessDeliveryGate),
            notifier: None,
            quiet: false,
            check_desktop: DesktopReadiness::default,
            status,
        }
    }

    pub fn start_recording(&mut self) -> Result<RecordingJob, DaemonError> {
        self.start_recording_in_mode(None)
    }

    pub fn start_recording_in_mode(
        &mut self,
        mode: Option<agentdictate_core::DictationMode>,
    ) -> Result<RecordingJob, DaemonError> {
        let requested_at = Instant::now();
        self.require_idle()?;
        fs::create_dir_all(&self.paths.recordings)?;
        let now = Utc::now();
        let job_id = JobId::new();
        let path = self.paths.recordings.join(format!(
            "dictation-{}-{job_id}.wav",
            now.format("%Y%m%dT%H%M%S%.fZ"),
        ));
        let mut recording_settings = self.settings.clone();
        if let Some(mode) = mode {
            recording_settings.dictation_mode = mode;
        }
        let options = agentdictate_core::DictationOptions::from_settings(&recording_settings);
        // Publishing Starting before the recorder comes up lets the overlay
        // helper open its window in parallel with the microphone.
        self.workflow
            .apply(WorkflowSignal::StartRequested { job_id })?;
        self.publish_overlay_update();
        let job = match self.runtime.start_recording(
            RecordingRequest {
                id: job_id,
                options: Some(options),
                audio_path: path,
                started_at: now,
                transcription_model: self.settings.transcription_model.clone(),
            },
            &mut self.recorder,
        ) {
            Ok(job) => job,
            Err(error) => {
                self.abandon_start(job_id);
                return Err(error.into());
            }
        };
        self.activity = Activity::Recording(ActiveRecording {
            job_id: job.id,
            overlay: ActiveRecordingUpdate {
                audio_path: job.audio_path.clone(),
                // Match the previous overlay: elapsed time starts only after
                // the recorder has produced its first durable audio frame.
                started_at_unix_millis: Utc::now().timestamp_millis(),
            },
        });
        self.advance(
            job.id,
            WorkflowSignal::FirstAudioFrameWritten { job_id: job.id },
        );
        self.recoverable_count = self.attention_recovery_count()?;
        self.publish_overlay_update();
        tracing::info!(
            job_id = %job.id,
            audio_path = %job.audio_path.display(),
            capture_ready_ms = millis(requested_at.elapsed()),
            "recording ready"
        );
        Ok(job)
    }

    /// Ends a start whose recorder or checkpoint failed. A job the runtime
    /// kept for Recovery needs attention; otherwise nothing was recorded and
    /// the workflow returns to Ready, closing the overlay.
    fn abandon_start(&mut self, job_id: JobId) {
        let kept = match self.runtime.recoverable_jobs() {
            Ok(jobs) => jobs.into_iter().find(|job| job.id == job_id),
            Err(error) => {
                tracing::error!(%job_id, %error, "could not read the failed start's recovery state");
                None
            }
        };
        match kept {
            Some(job) => self.settle(job_id, interruption(&job, FailureKind::Unexpected)),
            None => {
                self.workflow = Workflow::new();
                self.publish_overlay_update();
            }
        }
    }

    /// Stops the recording and checkpoints it as `transcribing`. The
    /// returned ticket transcribes it; hand its completion back to
    /// `complete_transcription`.
    pub fn stop_recording(&mut self) -> Result<ProcessingTicket<T>, DaemonError> {
        let stopped_at = Instant::now();
        let id = self.recording_job()?;
        let focus_at_stop = self.deliverer.observe_focus();
        let job = self.runtime.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
        self.workflow.apply(WorkflowSignal::StopRequested)?;
        tracing::info!(job_id = %id, "recording stop requested");
        self.publish_overlay_update();
        let capture = match self.recorder.finish(&job) {
            Ok(capture) => capture,
            Err(error) => {
                tracing::error!(job_id = %id, %error, "recording finalization failed");
                let persisted = self.runtime.interrupt_job(
                    id,
                    JobStage::Recording,
                    JobFailure::new(
                        FailureKind::Unexpected,
                        format!("recording could not be finalized: {error}"),
                    ),
                );
                self.settle(
                    id,
                    WorkflowSignal::Interrupted {
                        job_id: id,
                        at: JobStage::Interrupted,
                        failure: FailureKind::Unexpected,
                    },
                );
                persisted?;
                return Err(error.into());
            }
        };
        let audio_bytes = std::fs::metadata(&job.audio_path)
            .ok()
            .map(|metadata| metadata.len());
        tracing::info!(
            job_id = %id,
            duration_seconds = capture.duration_seconds,
            ?audio_bytes,
            "recording finalized"
        );
        if let Err(error) = self.runtime.capture_recording(id, capture.duration_seconds) {
            tracing::error!(
                job_id = %id,
                %error,
                "recording finalized but its capture checkpoint failed"
            );
            self.recover_after_capture_checkpoint_failure(id, &error);
            return Err(error.into());
        }
        let job = match self.runtime.begin_transcription(id) {
            Ok(job) => job,
            Err(error) => {
                tracing::error!(job_id = %id, %error, "could not checkpoint the transcription start");
                // The job stays captured, which Recovery can transcribe.
                self.settle(
                    id,
                    WorkflowSignal::Interrupted {
                        job_id: id,
                        at: JobStage::Captured,
                        failure: FailureKind::Unexpected,
                    },
                );
                return Err(error.into());
            }
        };
        self.activity = Activity::Processing(ActiveProcessing {
            job_id: id,
            stopped_at,
            focus_at_stop,
            requester: Requester::Dictation,
        });
        self.advance(id, WorkflowSignal::CaptureFinalized { job_id: id });
        self.publish_overlay_update();
        Ok(ProcessingTicket::new(
            job,
            self.transcriber.clone(),
            capture.encoding,
        ))
    }

    /// Records a ticket's result. Only the job the daemon is processing is
    /// delivered: pasted, or copied when the result arrived later after the
    /// stop than `stale_paste_after` allows for its length. Any other job's result is only
    /// stored, so it waits in Recovery. Returns the job as it was left.
    pub fn complete_transcription(
        &mut self,
        completion: TranscriptionCompletion,
    ) -> Result<RecordingJob, DaemonError> {
        let TranscriptionCompletion {
            job_id: id,
            outcome,
            finished_at,
        } = completion;
        let current = match self.activity {
            Activity::Processing(processing) if processing.job_id == id => Some(processing),
            Activity::Idle | Activity::Recording(_) | Activity::Processing(_) => None,
        };
        let note = current.is_none().then_some(CANCELLED_NOTE);
        let stored = match self.runtime.store_transcript(id, outcome, note) {
            Ok(stored) => stored,
            Err(error) => {
                tracing::error!(job_id = %id, %error, "could not store the transcription result");
                if current.is_some() {
                    let end = self.persisted_interruption(id, FailureKind::Unexpected);
                    self.settle(id, end);
                }
                return Err(error.into());
            }
        };
        let Some(processing) = current else {
            return self.store_detached(id, stored);
        };
        match stored {
            StoredTranscript::Stale => {
                tracing::warn!(job_id = %id, "the job left transcription before its result arrived");
                self.settle(id, WorkflowSignal::ProcessingCancelled { job_id: id });
                Err(DaemonError::StaleResult { job_id: id })
            }
            StoredTranscript::NoSpeech(job) => {
                self.cleanup_completed_audio(&job);
                self.settle(id, WorkflowSignal::NoSpeechDetected { job_id: id });
                tracing::info!(job_id = %id, "empty dictation finished quietly");
                Ok(job)
            }
            StoredTranscript::Failed(job) => {
                tracing::warn!(
                    job_id = %id,
                    error = ?job.error_message,
                    failure = ?job.failure,
                    "transcription failed"
                );
                self.settle(id, interruption(&job, FailureKind::Unexpected));
                Ok(job)
            }
            StoredTranscript::Ready(ready) => self.deliver(processing, ready, finished_at),
        }
    }

    /// Stores the result of a job the daemon no longer processes: the user
    /// cancelled it. It waits in Recovery and is never delivered.
    fn store_detached(
        &mut self,
        id: JobId,
        stored: StoredTranscript,
    ) -> Result<RecordingJob, DaemonError> {
        let job = match stored {
            StoredTranscript::Stale => {
                tracing::warn!(job_id = %id, "a cancelled job left transcription before its result arrived");
                return Err(DaemonError::StaleResult { job_id: id });
            }
            StoredTranscript::NoSpeech(job) => {
                self.cleanup_completed_audio(&job);
                job
            }
            StoredTranscript::Ready(job) | StoredTranscript::Failed(job) => job,
        };
        tracing::info!(job_id = %id, stage = ?job.stage, "a cancelled dictation finished; its result waits in Recovery");
        self.recount_recoveries();
        self.publish_overlay_update();
        Ok(job)
    }

    fn deliver(
        &mut self,
        processing: ActiveProcessing,
        ready: RecordingJob,
        finished_at: Instant,
    ) -> Result<RecordingJob, DaemonError> {
        let id = ready.id;
        let waited = finished_at.saturating_duration_since(processing.stopped_at);
        let method = match processing.requester {
            Requester::Dictation => match copy_instead_of_paste(
                waited,
                ready.duration_seconds,
                processing.focus_at_stop,
                || self.deliverer.observe_focus(),
            ) {
                Some(reason) => {
                    tracing::info!(
                        job_id = %id,
                        ?reason,
                        waited_ms = millis(waited),
                        "the transcript is copied instead of pasted"
                    );
                    DeliveryMethod::CopyOnly
                }
                None => DeliveryMethod::Paste,
            },
            Requester::Window | Requester::Notification => DeliveryMethod::CopyOnly,
        };
        self.advance(id, WorkflowSignal::TranscriptStored { job_id: id });
        self.advance(id, WorkflowSignal::DeliveryStarted { job_id: id });
        let mut deliverer = Timed::new(&mut self.deliverer);
        let (delivered, gate_ran) = match method {
            DeliveryMethod::Paste => {
                let mut gate = Timed::new(&mut self.overlay);
                let delivered =
                    self.runtime
                        .deliver_ready(ready, method, &mut gate, &mut deliverer);
                (delivered, gate.ran)
            }
            // A copy cannot reach transient AgentDictate UI, so no gate.
            DeliveryMethod::CopyOnly => (
                self.runtime.deliver_ready(
                    ready,
                    method,
                    &mut HeadlessDeliveryGate,
                    &mut deliverer,
                ),
                None,
            ),
        };
        let delivery_ran = deliverer.ran;
        let consumed = matches!(
            deliverer.reported,
            Some(DeliveryDisposition::Submitted { consumed: true, .. })
        );
        let result = match delivered {
            Ok(result) => result,
            Err(error) => {
                tracing::error!(job_id = %id, %error, "dictation delivery failed");
                // The runtime already marked the job failed or ambiguous.
                let end = self.persisted_interruption(id, FailureKind::PasteNotConfirmed);
                self.settle(id, end);
                return Err(error.into());
            }
        };
        let (end, notice) = if result.stage == JobStage::Delivered {
            if let Err(error) = self.runtime.complete_delivered(id, &self.settings) {
                // The paste command was already submitted. A bookkeeping failure
                // must never make it retryable and risk a duplicate paste; the
                // next daemon start completes the job instead.
                tracing::error!(job_id = %id, %error, "could not complete delivered dictation");
            }
            self.cleanup_completed_audio(&result);
            (
                WorkflowSignal::DeliverySubmitted { job_id: id },
                delivered_notice(method, consumed),
            )
        } else {
            let end = interruption(&result, FailureKind::PasteNotConfirmed);
            // A failed paste whose text reached the clipboard only needs
            // Ctrl+V; the job also waits in Recovery.
            let notice = match end {
                _ if result.copied_to_clipboard => DictationNotice::Copied,
                WorkflowSignal::Interrupted { failure, .. } => DictationNotice::Failed { failure },
                _ => DictationNotice::Failed {
                    failure: FailureKind::PasteNotConfirmed,
                },
            };
            (end, Some(notice))
        };
        self.last_dictation = Some(result.clone());
        self.settle_with_notice(id, end, notice);
        tracing::info!(
            job_id = %id,
            stage = ?result.stage,
            ?method,
            consumed,
            gate_ms = gate_ran.map(|(started, finished)| millis(finished - started)),
            stop_to_paste_ms = delivery_ran.map(|(_, finished)| millis(finished - processing.stopped_at)),
            stop_to_flow_complete_ms = millis(processing.stopped_at.elapsed()),
            "dictation flow completed"
        );
        Ok(result)
    }

    /// Stops waiting for the transcription in progress, so a new dictation
    /// can start at once. Its result is stored for Recovery when it arrives
    /// and is never pasted.
    pub fn cancel_processing(&mut self) -> Result<JobId, DaemonError> {
        let Activity::Processing(processing) = self.activity else {
            return Err(DaemonError::NotProcessing);
        };
        let id = processing.job_id;
        tracing::info!(job_id = %id, "transcription cancelled; its result will wait in Recovery");
        self.settle(id, WorkflowSignal::ProcessingCancelled { job_id: id });
        Ok(id)
    }

    /// Discards the active recording because the user explicitly pressed
    /// Escape. A recording longer than a few seconds waits in Recovery as
    /// `Cancelled` for a day; a shorter one is deleted with its audio, unless
    /// "Keep audio recordings" is on. Either way the workflow returns to
    /// Ready without asking for attention. Shutdown and platform failures
    /// must use the separate recovery preservation path below.
    pub fn discard_recording(&mut self) -> Result<RecordingJob, DaemonError> {
        let id = self.recording_job()?;
        tracing::info!(job_id = %id, "dictation discard requested");
        let job = self.runtime.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
        let capture = match self.recorder.finish(&job) {
            Ok(capture) => capture,
            Err(error) => {
                let interrupted = self.runtime.interrupt_job(
                    id,
                    JobStage::Recording,
                    JobFailure::new(
                        FailureKind::Unexpected,
                        format!("recording could not be finalized while discarding: {error}"),
                    ),
                );
                self.settle(
                    id,
                    WorkflowSignal::Interrupted {
                        job_id: id,
                        at: JobStage::Interrupted,
                        failure: FailureKind::Unexpected,
                    },
                );
                return interrupted.map_err(Into::into);
            }
        };
        if let Err(error) = self.runtime.capture_recording(id, capture.duration_seconds) {
            tracing::error!(
                job_id = %id,
                %error,
                "discarded recording finalized but its capture checkpoint failed"
            );
            self.recover_after_capture_checkpoint_failure(id, &error);
            return Err(error.into());
        }
        match self
            .runtime
            .discard_recording(id, self.settings.preserve_temp_audio)
        {
            Ok(discarded) => {
                self.settle(id, WorkflowSignal::DiscardCommitted { job_id: id });
                tracing::info!(
                    job_id = %id,
                    duration_seconds = capture.duration_seconds,
                    kept_in_recovery = discarded.stage == JobStage::Cancelled,
                    "dictation discarded"
                );
                Ok(discarded)
            }
            Err(error) => {
                self.settle(
                    id,
                    WorkflowSignal::Interrupted {
                        job_id: id,
                        at: JobStage::Captured,
                        failure: FailureKind::Unexpected,
                    },
                );
                Err(error.into())
            }
        }
    }

    /// Acts on the recorder's report about the active recording; a report
    /// about any other job is stale and ignored. A recording that reached its
    /// maximum length is stopped, and its ticket returned. One whose recorder
    /// exited or stalled is preserved for Recovery and not transcribed: it
    /// may end mid-sentence.
    pub fn recorder_event(
        &mut self,
        event: RecorderEvent,
    ) -> Result<Option<ProcessingTicket<T>>, DaemonError> {
        if self.recording_job().ok() != Some(event.job_id()) {
            tracing::debug!(?event, "ignoring an event about a finished recording");
            return Ok(None);
        }
        match event {
            RecorderEvent::MaxDurationReached { job_id } => {
                tracing::info!(%job_id, "maximum recording length reached; stopping");
                self.stop_recording().map(Some)
            }
            RecorderEvent::Exited { .. } => self
                .preserve_active_recording(JobFailure::new(
                    FailureKind::MicrophoneStalled,
                    "recorder exited unexpectedly before the dictation completed; audio was preserved",
                ))
                .map(|_| None),
            RecorderEvent::Stalled { .. } => self
                .preserve_active_recording(JobFailure::new(
                    FailureKind::MicrophoneStalled,
                    "the microphone stopped sending audio, so the recording was stopped; audio was preserved",
                ))
                .map(|_| None),
        }
    }

    /// Finalizes active audio without transcribing or deleting it, so process
    /// shutdown can never discard an in-progress dictation. From then on,
    /// dictations end without announcements; after a failure the daemon
    /// keeps running, and announces them again.
    pub fn shutdown(&mut self) -> Result<(), DaemonError> {
        self.quiet = true;
        let preserved = if matches!(self.activity, Activity::Recording(_)) {
            self.preserve_active_recording(JobFailure::new(
                FailureKind::Unexpected,
                "AgentDictate shut down before this dictation completed; audio was preserved",
            ))
            .map(drop)
        } else {
            Ok(())
        };
        self.quiet = preserved.is_ok();
        preserved
    }

    /// "Transcribe again" from Recovery. The returned ticket transcribes the
    /// item; `complete_transcription` then copies the text to the clipboard,
    /// because the request came from AgentDictate's own window, which has the
    /// focus. No other dictation can start meanwhile.
    pub fn retry_transcription(&mut self, id: JobId) -> Result<ProcessingTicket<T>, DaemonError> {
        self.begin_retry(id, Requester::Window)
    }

    /// "Try again" on a failure notification. Like "Transcribe again", the
    /// text is copied, since the user may be anywhere by now; its end is
    /// announced like a dictation's.
    pub fn try_again(&mut self, id: JobId) -> Result<ProcessingTicket<T>, DaemonError> {
        self.begin_retry(id, Requester::Notification)
    }

    fn begin_retry(
        &mut self,
        id: JobId,
        requester: Requester,
    ) -> Result<ProcessingTicket<T>, DaemonError> {
        self.require_idle()?;
        tracing::info!(job_id = %id, ?requester, "recovery transcription retry requested");
        let job = self
            .runtime
            .prepare_transcription_retry(id)
            .inspect_err(|error| tracing::warn!(job_id = %id, %error, "recovery retry failed"))?;
        self.activity = Activity::Processing(ActiveProcessing {
            job_id: id,
            stopped_at: Instant::now(),
            focus_at_stop: ObservedFocus::Unknown,
            requester,
        });
        self.advance(id, WorkflowSignal::RetryRequested { job_id: id });
        self.publish_overlay_update();
        Ok(ProcessingTicket::new(job, self.transcriber.clone(), None))
    }

    /// "Paste again" from Recovery: copies the stored transcript to the
    /// clipboard.
    pub fn retry_delivery(&mut self, id: JobId) -> Result<RecordingJob, DaemonError> {
        self.require_idle()?;
        tracing::info!(job_id = %id, "recovery copy retry requested");
        let ready = self
            .runtime
            .prepare_delivery_retry(id)
            .inspect_err(|error| tracing::warn!(job_id = %id, %error, "recovery retry failed"))?;
        let result = self
            .runtime
            .deliver_ready(
                ready,
                DeliveryMethod::CopyOnly,
                &mut HeadlessDeliveryGate,
                &mut self.deliverer,
            )
            .inspect_err(|error| tracing::warn!(job_id = %id, %error, "recovery retry failed"))?;
        tracing::info!(job_id = %id, stage = ?result.stage, "recovery retry finished");
        if result.stage == JobStage::Delivered {
            if let Err(error) = self.runtime.complete_delivered(id, &self.settings) {
                tracing::error!(job_id = %id, %error, "could not complete retried dictation");
            }
            self.cleanup_completed_audio(&result);
        }
        self.last_dictation = Some(result.clone());
        self.clear_attention_for(id);
        self.recount_recoveries();
        self.publish_overlay_update();
        copied(result)
    }

    /// "Paste last dictation": pastes the last dictation's text again into
    /// the focused window; see `paste_again`.
    pub fn paste_last(&mut self) -> Result<(), DaemonError> {
        self.require_idle()?;
        let last = self
            .last_dictation
            .clone()
            .ok_or(DaemonError::NothingToPaste)?;
        self.paste_again(&last)
    }

    /// "Paste again" on a notification about the dictation `job_id`: pastes
    /// that dictation's text, never another's; see `paste_again`. Its text
    /// is the last dictation's, or one waiting in Recovery. When it is
    /// neither, because it was deleted, expired, or pasted before a newer
    /// dictation, nothing is pasted, and the overlay and a notification say
    /// so.
    pub fn paste_dictation(&mut self, job_id: JobId) -> Result<(), DaemonError> {
        self.require_idle()?;
        let dictation = match self.last_dictation.clone().filter(|last| last.id == job_id) {
            Some(last) => Some(last),
            None => self
                .runtime
                .job(job_id)?
                .filter(|job| !job.final_text.trim().is_empty()),
        };
        let Some(dictation) = dictation else {
            self.publish(Some((DictationNotice::PasteUnavailable, job_id)));
            return Err(DaemonError::PasteUnavailable { job_id });
        };
        self.paste_again(&dictation)
    }

    /// Pastes `dictation`'s text again into the focused window, through the
    /// same gate and single paste chord as a dictation. The caller refuses
    /// it while a dictation is in flight, so it can never collide with one.
    /// A paste no application took is announced like a dictation's, and
    /// never sent again.
    fn paste_again(&mut self, dictation: &RecordingJob) -> Result<(), DaemonError> {
        self.overlay
            .confirm_ready()
            .map_err(|error| DaemonError::NotPasted {
                reason: error.to_string(),
            })?;
        self.deliverer.wait_for_released_keys();
        let disposition = self.deliverer.deliver(dictation, DeliveryMethod::Paste)?;
        tracing::info!(job_id = %dictation.id, ?disposition, "dictation pasted again");
        match disposition {
            DeliveryDisposition::Submitted { consumed, .. } => {
                if let Some(notice) = delivered_notice(DeliveryMethod::Paste, consumed) {
                    self.publish(Some((notice, dictation.id)));
                }
                Ok(())
            }
            DeliveryDisposition::Ambiguous { .. } => Err(DaemonError::NotPasted {
                reason: "the paste may not have reached the focused app".to_owned(),
            }),
            DeliveryDisposition::NotSent { reason, .. } => Err(DaemonError::NotPasted { reason }),
        }
    }

    /// Deletes one Recovery item. Only a prompt about that item is cleared:
    /// a recording or transcription in progress, and every way to stop it,
    /// stays untouched.
    pub fn delete_recovery(&mut self, id: JobId) -> Result<RecordingJob, DaemonError> {
        let result = self.runtime.delete_recovery(id)?;
        tracing::info!(job_id = %id, "recovery item deleted");
        self.forget_dictation(id);
        self.clear_attention_for(id);
        self.recoverable_count = self.attention_recovery_count()?;
        self.publish_overlay_update();
        Ok(result)
    }

    /// Whether a transcription ticket is out that the daemon will deliver.
    #[must_use]
    pub const fn is_processing(&self) -> bool {
        matches!(self.activity, Activity::Processing(_))
    }

    #[must_use]
    pub const fn phase(&self) -> WorkflowPhase {
        self.workflow.snapshot().phase
    }

    /// Deletes one History entry; returns whether it existed. Its text can
    /// no longer be pasted again.
    pub fn delete_history(&mut self, id: i64) -> Result<bool, RuntimeError> {
        let deleted = self.runtime.delete_history(id)?;
        if let Some(job_id) = deleted.and_then(|deleted| deleted.job_id) {
            self.forget_dictation(job_id);
        }
        Ok(deleted.is_some())
    }

    /// Deletes all of History, and forgets the last dictation, so "Paste
    /// last dictation" has nothing left to paste.
    pub fn clear_history(&mut self) -> Result<(), RuntimeError> {
        self.last_dictation = None;
        self.runtime.clear_history()
    }

    /// Forgets the last dictation when it is the job `id`, whose text was
    /// deleted.
    fn forget_dictation(&mut self, id: JobId) {
        if self
            .last_dictation
            .as_ref()
            .is_some_and(|last| last.id == id)
        {
            self.last_dictation = None;
        }
    }

    pub fn transcript_text(&self, id: i64) -> Result<Option<String>, RuntimeError> {
        self.runtime.transcript_text(id)
    }

    /// The status snapshot. `history_set_aside` is the process's to fill.
    #[must_use]
    pub fn snapshot(&self) -> AppSnapshot {
        AppSnapshot {
            workflow: self.workflow.snapshot(),
            readiness: self.readiness(),
            recoverable_count: self.recoverable_count,
            overlay_unavailable: matches!(
                &self.overlay,
                OverlayDeliveryGate::Live(controller) if controller.is_unavailable()
            ),
            history_set_aside: None,
        }
    }

    /// Whether dictation can work now. The desktop is checked each time,
    /// so a fix such as installing ffmpeg shows without a restart.
    fn readiness(&self) -> Readiness {
        let mut desktop = (self.check_desktop)();
        // Without audio ducking, lowering other sounds is never needed.
        desktop
            .missing_tools
            .retain(|tool| *tool != MissingTool::Pactl || self.settings.audio_ducking_enabled);
        Readiness {
            shortcut: self.status.hotkey_readiness(),
            transcription_key: !self.settings.openai_api_key.trim().is_empty(),
            desktop,
        }
    }

    /// Checks the real desktop for the readiness the snapshot reports.
    pub fn set_desktop_check(&mut self, check: fn() -> DesktopReadiness) {
        self.check_desktop = check;
    }

    pub fn set_hotkey_readiness(&self, readiness: HotkeyReadiness) {
        self.status.set_hotkey_readiness(readiness);
    }

    pub fn set_overlay_controller(&mut self, controller: OverlayController) {
        self.overlay = OverlayDeliveryGate::Live(controller);
        self.publish_overlay_update();
    }

    /// Shows how dictations that are not pasted end as desktop notifications.
    pub fn set_notifier(&mut self, notifier: Notifier) {
        self.notifier = Some(notifier);
    }

    /// State other threads read without the daemon lock.
    #[must_use]
    pub fn status(&self) -> Arc<DaemonStatus> {
        Arc::clone(&self.status)
    }

    #[must_use]
    pub const fn recorder(&self) -> &R {
        &self.recorder
    }

    #[must_use]
    pub const fn deliverer(&self) -> &D {
        &self.deliverer
    }

    #[must_use]
    pub const fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn update_settings(&mut self, settings: Settings) {
        self.status.set_recording_mode(settings.recording_mode);
        self.settings = settings;
        self.publish_overlay_update();
        self.status.notify_changed();
    }

    pub const fn transcriber_mut(&mut self) -> &mut T {
        &mut self.transcriber
    }

    pub const fn recorder_mut(&mut self) -> &mut R {
        &mut self.recorder
    }

    pub const fn deliverer_mut(&mut self) -> &mut D {
        &mut self.deliverer
    }

    /// Publishes the workflow to the overlay helper and the status mirror.
    /// Every workflow change ends with this call.
    fn publish_overlay_update(&self) {
        self.publish(None);
    }

    /// Publishes the workflow, and announces how the dictation `job_id`
    /// ended when it was not pasted: on the overlay and as a notification.
    fn publish(&self, announcement: Option<(DictationNotice, JobId)>) {
        let recording = matches!(
            self.workflow.snapshot().phase,
            WorkflowPhase::Starting { .. } | WorkflowPhase::Recording { .. }
        );
        if self.status.recording.swap(recording, Ordering::AcqRel) != recording {
            self.status.notify_changed();
        }
        if let OverlayDeliveryGate::Live(overlay) = &self.overlay {
            overlay.update(OverlayUpdate {
                notice: announcement.map(|(notice, _)| notice),
                ..self.overlay_update()
            });
        }
        if let Some((notice, job_id)) = announcement {
            tracing::info!(%job_id, ?notice, "dictation ended without a paste");
            if let Some(notifier) = &self.notifier {
                notifier.notify(notice, job_id);
            }
        }
    }

    fn recover_after_capture_checkpoint_failure(&mut self, id: JobId, primary: &RuntimeError) {
        if let Err(recovery_error) = self.runtime.interrupt_job(
            id,
            JobStage::Recording,
            JobFailure::new(
                FailureKind::Unexpected,
                format!("recording was finalized but its capture checkpoint failed: {primary}"),
            ),
        ) {
            tracing::error!(
                job_id = %id,
                %recovery_error,
                "could not persist capture-checkpoint recovery state"
            );
        }
        self.settle(
            id,
            WorkflowSignal::Interrupted {
                job_id: id,
                at: JobStage::Interrupted,
                failure: FailureKind::Unexpected,
            },
        );
    }

    /// Stops the active recording and keeps its audio for Recovery, with
    /// `failure` saying why.
    fn preserve_active_recording(
        &mut self,
        failure: JobFailure,
    ) -> Result<RecordingJob, DaemonError> {
        let id = self.recording_job()?;
        let job = self.runtime.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
        let capture = match self.recorder.finish(&job) {
            Ok(capture) => capture,
            Err(error) => {
                let kind = failure.kind;
                let interrupted = self.runtime.interrupt_job(
                    id,
                    JobStage::Recording,
                    JobFailure::new(
                        kind,
                        format!(
                            "{}; recording could not be finalized: {error}",
                            failure.message
                        ),
                    ),
                );
                self.settle(
                    id,
                    WorkflowSignal::Interrupted {
                        job_id: id,
                        at: JobStage::Interrupted,
                        failure: kind,
                    },
                );
                return interrupted.map_err(Into::into);
            }
        };
        if let Err(error) = self.runtime.capture_recording(id, capture.duration_seconds) {
            tracing::error!(
                job_id = %id,
                %error,
                "preserved recording finalized but its capture checkpoint failed"
            );
            self.recover_after_capture_checkpoint_failure(id, &error);
            return Err(error.into());
        }
        let kind = failure.kind;
        let interrupted = self.runtime.interrupt_job(id, JobStage::Captured, failure);
        self.settle(
            id,
            WorkflowSignal::Interrupted {
                job_id: id,
                at: JobStage::Interrupted,
                failure: kind,
            },
        );
        interrupted.map_err(Into::into)
    }

    /// Applies a signal that the daemon's own state already guarantees. An
    /// out-of-order signal is logged; the next `settle` rebuilds the workflow.
    fn advance(&mut self, id: JobId, signal: WorkflowSignal) {
        if let Err(workflow_error) = self.workflow.apply(signal) {
            tracing::warn!(job_id = %id, %workflow_error, ?signal, "workflow signal out of order");
        }
    }

    /// Ends the active dictation with its final workflow transition:
    /// `Interrupted` keeps the job in Recovery, any other signal returns to
    /// Ready. A failure, or a recording where nothing was heard, is
    /// announced. This never fails. It clears the activity first, rebuilds
    /// the workflow when the transition is out of order, and recounts
    /// Recovery best-effort, so a failed bookkeeping step can never leave a
    /// stale active job that rejects every later command.
    fn settle(&mut self, id: JobId, end: WorkflowSignal) {
        let notice = match end {
            WorkflowSignal::Interrupted { failure, .. } => {
                Some(DictationNotice::Failed { failure })
            }
            WorkflowSignal::NoSpeechDetected { .. } => Some(DictationNotice::NothingHeard),
            _ => None,
        };
        self.settle_with_notice(id, end, notice);
    }

    /// `settle`, announcing `notice` about the dictation. The settings
    /// window reports its own retries, and a shutdown announces nothing.
    fn settle_with_notice(
        &mut self,
        id: JobId,
        end: WorkflowSignal,
        notice: Option<DictationNotice>,
    ) {
        let announced = !self.quiet
            && !matches!(
                self.activity,
                Activity::Processing(ActiveProcessing {
                    requester: Requester::Window,
                    ..
                })
            );
        self.activity = Activity::Idle;
        let needs_attention = matches!(end, WorkflowSignal::Interrupted { .. });
        if let Err(workflow_error) = self.workflow.apply(end) {
            tracing::warn!(job_id = %id, %workflow_error, ?end, "rebuilding the workflow");
            self.workflow = Workflow::new();
            if needs_attention {
                let _ = self
                    .workflow
                    .apply(WorkflowSignal::StartRequested { job_id: id });
                let _ = self.workflow.apply(end);
            }
        }
        if !self.recount_recoveries() && needs_attention {
            self.recoverable_count = self.recoverable_count.max(1);
        }
        self.publish(notice.filter(|_| announced).map(|notice| (notice, id)));
    }

    /// Clears a Recovery prompt about `id`, and only about `id`.
    fn clear_attention_for(&mut self, id: JobId) {
        if matches!(self.workflow.snapshot().phase, WorkflowPhase::NeedsAttention { job_id, .. } if job_id == id)
        {
            self.workflow = Workflow::new();
        }
    }

    /// Recounts Recovery best-effort; returns whether the count is current.
    fn recount_recoveries(&mut self) -> bool {
        match self.attention_recovery_count() {
            Ok(count) => {
                self.recoverable_count = count;
                true
            }
            Err(recovery_error) => {
                tracing::error!(%recovery_error, "could not recount recoverable recordings");
                false
            }
        }
    }

    fn attention_recovery_count(&self) -> Result<usize, RuntimeError> {
        Ok(self.runtime.recoveries()?.len())
    }

    /// The workflow's end after a failed step, from the job as stored:
    /// `fallback` when it cannot be read or holds no failure.
    fn persisted_interruption(&self, id: JobId, fallback: FailureKind) -> WorkflowSignal {
        match self.runtime.job(id).ok().flatten() {
            Some(job) => interruption(&job, fallback),
            None => WorkflowSignal::Interrupted {
                job_id: id,
                at: JobStage::Failed,
                failure: fallback,
            },
        }
    }

    fn require_idle(&self) -> Result<(), DaemonError> {
        match self.activity {
            Activity::Idle => Ok(()),
            Activity::Recording(_) | Activity::Processing(_) => Err(DaemonError::Busy {
                phase: self.phase(),
            }),
        }
    }

    /// The job being recorded, for the commands that act on a recording.
    fn recording_job(&self) -> Result<JobId, DaemonError> {
        match &self.activity {
            Activity::Recording(recording) => Ok(recording.job_id),
            Activity::Idle => Err(DaemonError::NotRecording),
            Activity::Processing(_) => Err(DaemonError::Busy {
                phase: self.phase(),
            }),
        }
    }

    /// The overlay samples the recording's audio only while it is captured.
    /// Only a dictation shows its progress: a Recovery retry was asked for
    /// away from the text field, so it only announces its end.
    fn overlay_update(&self) -> OverlayUpdate {
        let workflow = match self.activity {
            Activity::Processing(ActiveProcessing {
                requester: Requester::Window | Requester::Notification,
                ..
            }) => WorkflowSnapshot {
                phase: WorkflowPhase::Ready,
            },
            Activity::Idle | Activity::Recording(_) | Activity::Processing(_) => {
                self.workflow.snapshot()
            }
        };
        let active_recording = match &self.activity {
            Activity::Recording(recording)
                if matches!(
                    workflow.phase,
                    WorkflowPhase::Recording { .. } | WorkflowPhase::Stopping { .. }
                ) =>
            {
                Some(recording.overlay.clone())
            }
            Activity::Idle | Activity::Recording(_) | Activity::Processing(_) => None,
        };
        OverlayUpdate {
            workflow,
            active_recording,
            notice: None,
        }
    }

    fn cleanup_completed_audio(&self, job: &RecordingJob) {
        if self.settings.preserve_temp_audio {
            return;
        }
        match fs::remove_file(&job.audio_path) {
            Ok(()) => tracing::debug!(job_id = %job.id, "removed completed recording audio"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(job_id = %job.id, %error, "could not remove completed recording audio");
            }
        }
    }
}

/// Ends the workflow with `job` kept for Recovery, as stored; `fallback`
/// stands in for a failure the job does not record.
fn interruption(job: &RecordingJob, fallback: FailureKind) -> WorkflowSignal {
    WorkflowSignal::Interrupted {
        job_id: job.id,
        at: job.stage,
        failure: job.failure.unwrap_or(fallback),
    }
}

/// What a Recovery retry tells the settings window: success only once the
/// text is on the clipboard, so the window never asks the user to paste text
/// that is not there.
pub(crate) fn copied(job: RecordingJob) -> Result<RecordingJob, DaemonError> {
    match job.stage {
        JobStage::Delivered => Ok(job),
        JobStage::NoSpeech => Err(DaemonError::NoSpeech),
        _ => Err(DaemonError::NotCopied {
            reason: job
                .error_message
                .unwrap_or_else(|| "the text was not copied".to_owned()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_dictations_get_their_transcription_time_before_a_paste_is_stale() {
        assert_eq!(stale_paste_after(0.0), Duration::from_secs(8));
        assert!(stale_paste_after(12.0) < Duration::from_secs(9));
        // A three-minute dictation normally takes about 5 s to transcribe, so
        // a result 9 s after stop is still pasted.
        assert!(stale_paste_after(180.0) > Duration::from_secs(13));
    }

    #[test]
    fn a_result_is_copied_instead_of_pasted_when_late_or_after_the_focus_moved() {
        use ObservedFocus::{Unknown, Wayland, X11};
        let prompt = Duration::from_secs(2);
        let late = Duration::from_secs(9);
        for (waited, at_stop, now, expected) in [
            (prompt, X11(7), X11(7), None),
            (prompt, X11(7), X11(9), Some(CopyReason::FocusMoved)),
            // The X11 window lost the focus to a native Wayland one, or the
            // other way round.
            (prompt, X11(7), Wayland, Some(CopyReason::FocusMoved)),
            (prompt, Wayland, X11(7), Some(CopyReason::FocusMoved)),
            // Two native Wayland windows look the same, and an unreadable
            // focus proves nothing.
            (prompt, Wayland, Wayland, None),
            (prompt, X11(7), Unknown, None),
            (prompt, Unknown, X11(7), None),
            (late, X11(7), X11(7), Some(CopyReason::ArrivedLate)),
        ] {
            assert_eq!(
                copy_instead_of_paste(waited, 3.0, at_stop, || now),
                expected,
                "{waited:?} {at_stop:?} -> {now:?}"
            );
        }
    }

    #[test]
    fn only_an_acknowledged_paste_ends_without_a_notice() {
        assert_eq!(delivered_notice(DeliveryMethod::Paste, true), None);
        assert_eq!(
            delivered_notice(DeliveryMethod::Paste, false),
            Some(DictationNotice::Copied)
        );
        assert_eq!(
            delivered_notice(DeliveryMethod::CopyOnly, false),
            Some(DictationNotice::Copied)
        );
    }
}
