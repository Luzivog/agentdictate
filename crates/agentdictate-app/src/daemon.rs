use std::fs;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use agentdictate_core::{
    AppSnapshot, HistoryPageRequest, HistoryPageSnapshot, HotkeyReadiness, JobId, JobStage,
    ReplacementRule, Settings, Workflow, WorkflowError, WorkflowPhase, WorkflowSignal,
    WorkspaceSnapshot,
};
use agentdictate_runtime::{
    Deliverer, DeliveryDisposition, DeliveryGate, DeliveryGateError, DeliveryMethod, ExternalError,
    HeadlessDeliveryGate, Recorder, RecordingJob, RecordingRequest, Runtime, RuntimeError,
    Transcriber,
};
use chrono::Utc;
use thiserror::Error;

use crate::{ActiveRecordingUpdate, AppPaths, OverlayController, OverlayUpdate};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CapturedRecording {
    pub duration_seconds: f64,
}

/// Recorder lifecycle owned by the daemon. `Recorder::start` is called only
/// after the durable Starting checkpoint; `finish` must finalize the WAV before
/// the Captured checkpoint is written.
pub trait RecordingController: Recorder {
    fn finish(&mut self, job: &RecordingJob) -> Result<CapturedRecording, ExternalError>;
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
    #[error("a recording is already active")]
    AlreadyRecording,
    #[error("no recording is active")]
    NotRecording,
    #[error("no speech was found in this recording")]
    NoSpeech,
    #[error("{reason}")]
    NotCopied { reason: String },
}

/// A dictation from its first audio frame until it settles.
struct ActiveDictation {
    job_id: JobId,
    recording: ActiveRecordingUpdate,
}

pub struct Daemon<R, T, D> {
    runtime: Runtime,
    settings: Settings,
    paths: AppPaths,
    recorder: R,
    transcriber: T,
    deliverer: D,
    workflow: Workflow,
    /// The dictation being recorded or processed; `settle` clears it.
    active: Option<ActiveDictation>,
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
            active: None,
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
        if self.active.is_some() {
            return Err(DaemonError::AlreadyRecording);
        }
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
        let options = agentdictate_core::DictationOptions::from_settings(
            &recording_settings,
            self.runtime.replacement_rules()?,
        );
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
        self.workflow
            .apply(WorkflowSignal::FirstAudioFrameWritten { job_id: job.id })?;
        self.transcriber.begin_recording(&job);
        self.active = Some(ActiveDictation {
            job_id: job.id,
            recording: ActiveRecordingUpdate {
                audio_path: job.audio_path.clone(),
                // Match the previous overlay: elapsed time starts only after
                // the recorder has produced its first durable audio frame.
                started_at_unix_millis: Utc::now().timestamp_millis(),
            },
        });
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

    pub fn stop_recording(&mut self) -> Result<RecordingJob, DaemonError> {
        let stop_started = Instant::now();
        let id = self.active_job().ok_or(DaemonError::NotRecording)?;
        let job = self.runtime.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
        self.workflow.apply(WorkflowSignal::StopRequested)?;
        tracing::info!(job_id = %id, "recording stop requested");
        self.publish_overlay_update();
        let capture = match self.recorder.finish(&job) {
            Ok(capture) => capture,
            Err(error) => {
                self.transcriber.cancel_recording(id);
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
        self.workflow
            .apply(WorkflowSignal::CaptureFinalized { job_id: id })?;
        self.publish_overlay_update();
        let mut gate = Timed::new(&mut self.overlay);
        let mut deliverer = Timed::new(&mut self.deliverer);
        let processed =
            self.runtime
                .process_captured(id, &mut self.transcriber, &mut gate, &mut deliverer);
        let (gate_ran, delivery_ran) = (gate.ran, deliverer.ran);
        let result = match processed {
            Ok(result) => result,
            Err(error) => {
                tracing::error!(job_id = %id, %error, "dictation processing failed");
                // The runtime marks the job failed. Even if this read fails
                // too, the session must end so the next dictation can start.
                let persisted_stage = self
                    .runtime
                    .job(id)
                    .ok()
                    .flatten()
                    .map_or(JobStage::Failed, |job| job.stage);
                self.settle(
                    id,
                    WorkflowSignal::Interrupted {
                        job_id: id,
                        at: persisted_stage,
                    },
                );
                return Err(error.into());
            }
        };
        if result.stage == JobStage::NoSpeech {
            self.cleanup_completed_audio(&result);
            self.settle(id, WorkflowSignal::NoSpeechDetected { job_id: id });
            tracing::info!(job_id = %id, "empty dictation finished quietly");
            return Ok(result);
        }
        // Processing ran synchronously, so these steps only record what it did.
        self.workflow
            .apply(WorkflowSignal::TranscriptStored { job_id: id })?;
        self.workflow
            .apply(WorkflowSignal::DeliveryStarted { job_id: id })?;
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
            gate_ms = gate_ran.map(|(started, finished)| millis(finished - started)),
            stop_to_paste_ms = delivery_ran.map(|(_, finished)| millis(finished - stop_started)),
            stop_to_flow_complete_ms = millis(stop_started.elapsed()),
            "dictation flow completed"
        );
        Ok(result)
    }

    /// Discards the active recording because the user explicitly pressed
    /// Escape. Its audio is deleted too, unless "Preserve temporary audio"
    /// is on. Shutdown and platform failures must use the separate recovery
    /// preservation path below.
    pub fn discard_recording(&mut self) -> Result<RecordingJob, DaemonError> {
        let id = self.active_job().ok_or(DaemonError::NotRecording)?;
        self.transcriber.cancel_recording(id);
        tracing::info!(job_id = %id, "dictation discard requested");
        let job = self.runtime.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
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

    /// Handles a kernel-observed recorder exit. A stale notification from a
    /// normal stop is ignored; an active unexpected exit preserves whatever
    /// audio was finalized for explicit recovery instead of guessing that the
    /// dictation was complete.
    pub fn recorder_exited(&mut self, id: JobId) -> Result<Option<RecordingJob>, DaemonError> {
        if self.active_job() != Some(id) {
            return Ok(None);
        }
        tracing::warn!(job_id = %id, "recorder exited without an explicit stop");
        self.preserve_active_recording(
            "recorder exited unexpectedly before the dictation completed; audio was preserved",
        )
        .map(Some)
    }

    /// Finalizes active audio without transcribing or deleting it, so process
    /// shutdown can never discard an in-progress dictation.
    pub fn shutdown(&mut self) -> Result<(), DaemonError> {
        if let Some(id) = self.active_job() {
            self.transcriber.cancel_recording(id);
            self.preserve_active_recording(
                "AgentDictate shut down before this dictation completed; audio was preserved",
            )?;
        }
        Ok(())
    }

    /// "Transcribe again" from Recovery: transcribes the item again and
    /// copies the result to the clipboard.
    pub fn retry_transcription(&mut self, id: JobId) -> Result<RecordingJob, DaemonError> {
        if self.active.is_some() {
            return Err(DaemonError::AlreadyRecording);
        }
        tracing::info!(job_id = %id, "recovery transcription retry requested");
        let result = self
            .runtime
            .retry_transcription(id, &mut self.transcriber, &mut self.deliverer)
            .inspect_err(|error| tracing::warn!(job_id = %id, %error, "recovery retry failed"))?;
        self.finish_retry(result)
    }

    /// "Paste again" from Recovery: copies the stored transcript to the
    /// clipboard.
    pub fn retry_delivery(&mut self, id: JobId) -> Result<RecordingJob, DaemonError> {
        if self.active.is_some() {
            return Err(DaemonError::AlreadyRecording);
        }
        tracing::info!(job_id = %id, "recovery copy retry requested");
        let result = self
            .runtime
            .retry_delivery(id, &mut self.deliverer)
            .inspect_err(|error| tracing::warn!(job_id = %id, %error, "recovery retry failed"))?;
        self.finish_retry(result)
    }

    /// Deletes one Recovery item. The workflow returns to Ready only when no
    /// dictation is active: deleting an older item mid-recording must leave
    /// the live recording, and every way to stop it, untouched.
    pub fn delete_recovery(&mut self, id: JobId) -> Result<RecordingJob, DaemonError> {
        let result = self.runtime.delete_recovery(id)?;
        tracing::info!(job_id = %id, "recovery item deleted");
        if self.active.is_none() {
            self.workflow = Workflow::new();
        }
        self.recoverable_count = self.attention_recovery_count()?;
        self.publish_overlay_update();
        Ok(result)
    }

    pub fn workspace_snapshot(&self) -> Result<WorkspaceSnapshot, RuntimeError> {
        Ok(WorkspaceSnapshot {
            overlay_unavailable: matches!(&self.overlay, OverlayDeliveryGate::Live(controller) if controller.is_unavailable()),
            recoveries: self.runtime.recoveries()?,
            history: self.runtime.history_page(&HistoryPageRequest::default())?,
            replacements: self.runtime.replacement_rules()?,
            usage: self.runtime.usage()?,
        })
    }

    pub fn history_page(
        &self,
        request: &HistoryPageRequest,
    ) -> Result<HistoryPageSnapshot, RuntimeError> {
        self.runtime.history_page(request)
    }

    pub fn create_replacement(
        &mut self,
        rule: ReplacementRule,
    ) -> Result<ReplacementRule, RuntimeError> {
        self.runtime.create_replacement(rule)
    }

    pub fn update_replacement(
        &mut self,
        rule: ReplacementRule,
    ) -> Result<ReplacementRule, RuntimeError> {
        self.runtime.update_replacement(rule)
    }

    pub fn delete_replacement(&mut self, id: i64) -> Result<bool, RuntimeError> {
        self.runtime.delete_replacement(id)
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
        self.transcriber.cancel_recording(id);
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
        let id = self.active_job().ok_or(DaemonError::NotRecording)?;
        self.transcriber.cancel_recording(id);
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

    /// Records a finished Recovery retry. Retries only copy, because their
    /// button sits in AgentDictate's own window, which has the focus. Any
    /// outcome other than copied text is an error, so the window never tells
    /// the user to paste text that is not on the clipboard.
    fn finish_retry(&mut self, result: RecordingJob) -> Result<RecordingJob, DaemonError> {
        let id = result.id;
        tracing::info!(job_id = %id, stage = ?result.stage, "recovery retry finished");
        self.workflow = Workflow::new();
        if result.stage == JobStage::Delivered
            && let Err(error) = self.runtime.complete_delivered(id, &self.settings)
        {
            tracing::error!(job_id = %id, %error, "could not complete retried dictation");
        }
        if matches!(result.stage, JobStage::Delivered | JobStage::NoSpeech) {
            self.cleanup_completed_audio(&result);
        }
        if result.stage != JobStage::NoSpeech {
            self.last_transcript = Some(result.final_text.clone());
        }
        self.recoverable_count = self.attention_recovery_count()?;
        self.publish_overlay_update();
        match result.stage {
            JobStage::Delivered => Ok(result),
            JobStage::NoSpeech => Err(DaemonError::NoSpeech),
            _ => Err(DaemonError::NotCopied {
                reason: result
                    .error_message
                    .unwrap_or_else(|| "the text was not copied".to_owned()),
            }),
        }
    }

    /// Ends the active dictation session with its final workflow transition:
    /// `Interrupted` keeps the job in Recovery, any other signal returns to
    /// Ready. This never fails. It clears the active dictation first,
    /// rebuilds the workflow when the transition is out of order, and
    /// recounts Recovery best-effort, so a failed bookkeeping step can never
    /// leave a stale active job that rejects every later command.
    fn settle(&mut self, id: JobId, end: WorkflowSignal) {
        self.active = None;
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
        match self.attention_recovery_count() {
            Ok(count) => self.recoverable_count = count,
            Err(recovery_error) => {
                tracing::error!(
                    job_id = %id,
                    %recovery_error,
                    "could not recount recoverable recordings"
                );
                if needs_attention {
                    self.recoverable_count = self.recoverable_count.max(1);
                }
            }
        }
        self.publish_overlay_update();
    }

    fn attention_recovery_count(&self) -> Result<usize, RuntimeError> {
        Ok(self.runtime.recoveries()?.len())
    }

    fn active_job(&self) -> Option<JobId> {
        self.active.as_ref().map(|active| active.job_id)
    }

    /// The overlay samples the recording's audio only while it is captured.
    fn overlay_update(&self) -> OverlayUpdate {
        let workflow = self.workflow.snapshot();
        let capturing = matches!(
            workflow.phase,
            WorkflowPhase::Recording { .. } | WorkflowPhase::Stopping { .. }
        );
        OverlayUpdate {
            workflow,
            active_recording: self
                .active
                .as_ref()
                .filter(|_| capturing)
                .map(|active| active.recording.clone()),
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
