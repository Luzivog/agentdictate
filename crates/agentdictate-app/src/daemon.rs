use std::fs;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use agentdictate_core::{
    AppSnapshot, HistoryPageRequest, HistoryPageSnapshot, HotkeyReadiness, JobId, JobStage,
    Settings, Workflow, WorkflowError, WorkflowPhase, WorkflowSignal, WorkflowSnapshot,
    WorkspaceSnapshot,
};
use agentdictate_runtime::{
    Deliverer, DeliveryDisposition, DeliveryGate, DeliveryGateError, DeliveryMethod, ExternalError,
    HeadlessDeliveryGate, Recorder, RecordingJob, RecordingRequest, Runtime, RuntimeError,
    StoredTranscript,
};
use chrono::Utc;
use thiserror::Error;

use crate::{
    ActiveRecordingUpdate, AppPaths, LiveTranscription, OverlayController, OverlayUpdate,
    ProcessingTicket, Transcriber, TranscriptionCompletion,
};

/// Recovery's message on a transcript that finished after its dictation was
/// cancelled.
const CANCELLED_NOTE: &str = "Cancelled before paste";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CapturedRecording {
    pub duration_seconds: f64,
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
    /// The recording reached the "Maximum recording length" setting.
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
/// timing log. The runtime calls the overlay gate, then the deliverer, whose
/// return marks the moment the paste was submitted.
struct Timed<'a, T> {
    inner: &'a mut T,
    ran: Option<(Instant, Instant)>,
}

impl<'a, T> Timed<'a, T> {
    const fn new(inner: &'a mut T) -> Self {
        Self { inner, ran: None }
    }

    fn record<R>(&mut self, step: impl FnOnce(&mut T) -> R) -> R {
        let started = Instant::now();
        let result = step(self.inner);
        self.ran = Some((started, Instant::now()));
        result
    }
}

impl<G: DeliveryGate> DeliveryGate for Timed<'_, G> {
    fn confirm_ready(&mut self) -> Result<(), DeliveryGateError> {
        self.record(G::confirm_ready)
    }
}

impl<D: Deliverer> Deliverer for Timed<'_, D> {
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
}

/// A result that arrives this long after the user stopped recording is only
/// copied: by then they may have moved on to another window.
const STALE_PASTE_AFTER: Duration = Duration::from_secs(8);

/// The one dictation the daemon may deliver.
enum Activity {
    Idle,
    Recording(ActiveRecording),
    Processing(ActiveProcessing),
}

struct ActiveRecording {
    job_id: JobId,
    overlay: ActiveRecordingUpdate,
    /// Live transcription listening while the job records, if it asked for
    /// one. Dropping it cancels the session.
    session: Option<LiveTranscription>,
}

/// A job whose transcription runs away from the daemon lock.
#[derive(Clone, Copy)]
struct ActiveProcessing {
    job_id: JobId,
    /// When the user stopped the recording or asked to transcribe it again.
    stopped_at: Instant,
    /// `Paste` for a dictation, `CopyOnly` for a Recovery retry.
    delivery: DeliveryMethod,
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
    last_transcript: Option<String>,
    hotkey: HotkeyReadiness,
    overlay: OverlayDeliveryGate,
    recording: Arc<AtomicBool>,
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
            last_transcript: None,
            hotkey: HotkeyReadiness::Starting,
            overlay: OverlayDeliveryGate::Headless(HeadlessDeliveryGate),
            recording: Arc::new(AtomicBool::new(false)),
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
            session: self.transcriber.open_session(&job),
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
        let kept_at = match self.runtime.recoverable_jobs() {
            Ok(jobs) => jobs
                .into_iter()
                .find(|job| job.id == job_id)
                .map(|job| job.stage),
            Err(error) => {
                tracing::error!(%job_id, %error, "could not read the failed start's recovery state");
                None
            }
        };
        match kept_at {
            Some(at) => self.settle(job_id, WorkflowSignal::Interrupted { job_id, at }),
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
                    format!("recording could not be finalized: {error}"),
                );
                self.settle(
                    id,
                    WorkflowSignal::Interrupted {
                        job_id: id,
                        at: JobStage::Interrupted,
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
                    },
                );
                return Err(error.into());
            }
        };
        let processing = Activity::Processing(ActiveProcessing {
            job_id: id,
            stopped_at,
            delivery: DeliveryMethod::Paste,
        });
        let session = match std::mem::replace(&mut self.activity, processing) {
            Activity::Recording(recording) => recording.session,
            Activity::Idle | Activity::Processing(_) => None,
        };
        self.advance(id, WorkflowSignal::CaptureFinalized { job_id: id });
        self.publish_overlay_update();
        Ok(ProcessingTicket::new(
            job,
            self.transcriber.clone(),
            session,
        ))
    }

    /// Records a ticket's result. Only the job the daemon is processing is
    /// delivered: pasted, or copied when the result arrived more than
    /// `STALE_PASTE_AFTER` after the stop. Any other job's result is only
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
                    let at = self.persisted_stage(id);
                    self.settle(id, WorkflowSignal::Interrupted { job_id: id, at });
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
                tracing::warn!(job_id = %id, error = ?job.error_message, "transcription failed");
                self.settle(
                    id,
                    WorkflowSignal::Interrupted {
                        job_id: id,
                        at: JobStage::Failed,
                    },
                );
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
        let method = match processing.delivery {
            DeliveryMethod::Paste if waited > STALE_PASTE_AFTER => {
                tracing::info!(
                    job_id = %id,
                    waited_ms = millis(waited),
                    "the transcript arrived late, so it is copied instead of pasted"
                );
                DeliveryMethod::CopyOnly
            }
            method => method,
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
        let result = match delivered {
            Ok(result) => result,
            Err(error) => {
                tracing::error!(job_id = %id, %error, "dictation delivery failed");
                // The runtime already marked the job failed or ambiguous.
                let at = self.persisted_stage(id);
                self.settle(id, WorkflowSignal::Interrupted { job_id: id, at });
                return Err(error.into());
            }
        };
        let end = if result.stage == JobStage::Delivered {
            if let Err(error) = self.runtime.complete_delivered(id, &self.settings) {
                // The paste command was already submitted. A bookkeeping failure
                // must never make it retryable and risk a duplicate paste; the
                // next daemon start completes the job instead.
                tracing::error!(job_id = %id, %error, "could not complete delivered dictation");
            }
            self.cleanup_completed_audio(&result);
            WorkflowSignal::DeliverySubmitted { job_id: id }
        } else {
            WorkflowSignal::Interrupted {
                job_id: id,
                at: result.stage,
            }
        };
        self.last_transcript = Some(result.final_text.clone());
        self.settle(id, end);
        tracing::info!(
            job_id = %id,
            stage = ?result.stage,
            ?method,
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
    /// Escape. Its audio is deleted too, unless "Preserve temporary audio"
    /// is on. Shutdown and platform failures must use the separate recovery
    /// preservation path below.
    pub fn discard_recording(&mut self) -> Result<RecordingJob, DaemonError> {
        let id = self.recording_job()?;
        tracing::info!(job_id = %id, "dictation discard requested");
        let job = self.runtime.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
        self.end_session(id);
        let capture = match self.recorder.finish(&job) {
            Ok(capture) => capture,
            Err(error) => {
                let interrupted = self.runtime.interrupt_job(
                    id,
                    JobStage::Recording,
                    format!("recording could not be finalized while discarding: {error}"),
                );
                self.settle(
                    id,
                    WorkflowSignal::Interrupted {
                        job_id: id,
                        at: JobStage::Interrupted,
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
                tracing::info!(job_id = %id, "dictation discarded");
                Ok(discarded)
            }
            Err(error) => {
                self.settle(
                    id,
                    WorkflowSignal::Interrupted {
                        job_id: id,
                        at: JobStage::Captured,
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
                .preserve_active_recording(
                    "recorder exited unexpectedly before the dictation completed; audio was preserved",
                )
                .map(|_| None),
            RecorderEvent::Stalled { .. } => self
                .preserve_active_recording(
                    "the microphone stopped sending audio, so the recording was stopped; audio was preserved",
                )
                .map(|_| None),
        }
    }

    /// Finalizes active audio without transcribing or deleting it, so process
    /// shutdown can never discard an in-progress dictation.
    pub fn shutdown(&mut self) -> Result<(), DaemonError> {
        if matches!(self.activity, Activity::Recording(_)) {
            self.preserve_active_recording(
                "AgentDictate shut down before this dictation completed; audio was preserved",
            )?;
        }
        Ok(())
    }

    /// "Transcribe again" from Recovery. The returned ticket transcribes the
    /// item; `complete_transcription` then copies the text to the clipboard,
    /// because the request came from AgentDictate's own window, which has the
    /// focus. No other dictation can start meanwhile.
    pub fn retry_transcription(&mut self, id: JobId) -> Result<ProcessingTicket<T>, DaemonError> {
        self.require_idle()?;
        tracing::info!(job_id = %id, "recovery transcription retry requested");
        let job = self
            .runtime
            .prepare_transcription_retry(id)
            .inspect_err(|error| tracing::warn!(job_id = %id, %error, "recovery retry failed"))?;
        self.activity = Activity::Processing(ActiveProcessing {
            job_id: id,
            stopped_at: Instant::now(),
            delivery: DeliveryMethod::CopyOnly,
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
        self.last_transcript = Some(result.final_text.clone());
        self.clear_attention_for(id);
        self.recount_recoveries();
        self.publish_overlay_update();
        copied(result)
    }

    /// Deletes one Recovery item. Only a prompt about that item is cleared:
    /// a recording or transcription in progress, and every way to stop it,
    /// stays untouched.
    pub fn delete_recovery(&mut self, id: JobId) -> Result<RecordingJob, DaemonError> {
        let result = self.runtime.delete_recovery(id)?;
        tracing::info!(job_id = %id, "recovery item deleted");
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

    pub fn workspace_snapshot(&self) -> Result<WorkspaceSnapshot, RuntimeError> {
        Ok(WorkspaceSnapshot {
            overlay_unavailable: matches!(&self.overlay, OverlayDeliveryGate::Live(controller) if controller.is_unavailable()),
            recoveries: self.runtime.recoveries()?,
            history: self.runtime.history_page(&HistoryPageRequest::default())?,
            usage: self.runtime.usage()?,
        })
    }

    pub fn history_page(
        &self,
        request: &HistoryPageRequest,
    ) -> Result<HistoryPageSnapshot, RuntimeError> {
        self.runtime.history_page(request)
    }

    pub fn delete_history(&mut self, id: i64) -> Result<bool, RuntimeError> {
        self.runtime.delete_history(id)
    }

    pub fn clear_history(&mut self) -> Result<(), RuntimeError> {
        self.runtime.clear_history()
    }

    pub fn transcript_text(&self, id: i64) -> Result<Option<String>, RuntimeError> {
        self.runtime.transcript_text(id)
    }

    #[must_use]
    pub fn snapshot(&self) -> AppSnapshot {
        AppSnapshot {
            workflow: self.workflow.snapshot(),
            hotkey: self.hotkey.clone(),
            recoverable_count: self.recoverable_count,
            last_transcript: self.last_transcript.clone(),
        }
    }

    pub fn set_hotkey_readiness(&mut self, readiness: HotkeyReadiness) {
        self.hotkey = readiness;
        self.publish_overlay_update();
    }

    pub fn set_overlay_controller(&mut self, controller: OverlayController) {
        self.overlay = OverlayDeliveryGate::Live(controller);
        self.publish_overlay_update();
    }

    /// True while a recording is starting or running. Other threads read it
    /// without the daemon lock; the hotkey listener uses it to drop Esc
    /// presses that could not cancel anything.
    #[must_use]
    pub fn recording_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.recording)
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
        self.settings = settings;
        self.publish_overlay_update();
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

    /// Publishes the workflow to the overlay helper and the recording flag.
    /// Every workflow change ends with this call.
    fn publish_overlay_update(&self) {
        let recording = matches!(
            self.workflow.snapshot().phase,
            WorkflowPhase::Starting { .. } | WorkflowPhase::Recording { .. }
        );
        self.recording.store(recording, Ordering::Release);
        if let OverlayDeliveryGate::Live(overlay) = &self.overlay {
            overlay.update(self.overlay_update());
        }
    }

    fn recover_after_capture_checkpoint_failure(&mut self, id: JobId, primary: &RuntimeError) {
        self.end_session(id);
        if let Err(recovery_error) = self.runtime.interrupt_job(
            id,
            JobStage::Recording,
            format!("recording was finalized but its capture checkpoint failed: {primary}"),
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
            },
        );
    }

    fn preserve_active_recording(
        &mut self,
        reason: &'static str,
    ) -> Result<RecordingJob, DaemonError> {
        let id = self.recording_job()?;
        self.end_session(id);
        let job = self.runtime.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
        let capture = match self.recorder.finish(&job) {
            Ok(capture) => capture,
            Err(error) => {
                let interrupted = self.runtime.interrupt_job(
                    id,
                    JobStage::Recording,
                    format!("{reason}; recording could not be finalized: {error}"),
                );
                self.settle(
                    id,
                    WorkflowSignal::Interrupted {
                        job_id: id,
                        at: JobStage::Interrupted,
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
        let interrupted = self
            .runtime
            .interrupt_job(id, JobStage::Captured, reason.to_owned());
        self.settle(
            id,
            WorkflowSignal::Interrupted {
                job_id: id,
                at: JobStage::Interrupted,
            },
        );
        interrupted.map_err(Into::into)
    }

    /// Cancels the recording's live session, if any, before its audio is
    /// discarded or preserved: nothing more is streamed or committed.
    fn end_session(&mut self, id: JobId) {
        if let Activity::Recording(recording) = &mut self.activity
            && recording.job_id == id
        {
            recording.session = None;
        }
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
    /// Ready. This never fails. It clears the activity first, rebuilds the
    /// workflow when the transition is out of order, and recounts Recovery
    /// best-effort, so a failed bookkeeping step can never leave a stale
    /// active job that rejects every later command.
    fn settle(&mut self, id: JobId, end: WorkflowSignal) {
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
        self.publish_overlay_update();
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

    /// The job's stored stage, for the workflow after a failed step.
    fn persisted_stage(&self, id: JobId) -> JobStage {
        self.runtime
            .job(id)
            .ok()
            .flatten()
            .map_or(JobStage::Failed, |job| job.stage)
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
    /// Recovery retries run from the settings window, so they show no overlay.
    fn overlay_update(&self) -> OverlayUpdate {
        let workflow = match self.activity {
            Activity::Processing(ActiveProcessing {
                delivery: DeliveryMethod::CopyOnly,
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
