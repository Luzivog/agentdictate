use std::{fs, sync::mpsc::Sender};

use agentdictate_core::{
    AppSnapshot, HistoryPageCursor, HistoryPageRequest, HistoryPageSnapshot, HistorySnapshot,
    HotkeyReadiness, JobId, JobStage, RecoverySnapshot, ReplacementRule, Settings,
    UsageDaySnapshot, UsageSnapshot, UsageTotalsSnapshot, Workflow, WorkflowError, WorkflowSignal,
    WorkspaceSnapshot,
};
use agentdictate_runtime::{
    Deliverer, DeliveryStatus, ExternalError, HistoryCursor, HistoryEntry, HistoryQuery, Recorder,
    RecordingJob, RecordingRequest, Runtime, RuntimeError, Transcriber, UsageAggregate,
    UsageMetric,
};
use chrono::Utc;
use thiserror::Error;

use crate::{ActiveRecordingUpdate, AppPaths, OverlayUpdate};

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
}

pub struct Daemon<R, T, D> {
    runtime: Runtime,
    settings: Settings,
    paths: AppPaths,
    recorder: R,
    transcriber: T,
    deliverer: D,
    workflow: Workflow,
    active_job: Option<JobId>,
    active_recording: Option<ActiveRecordingUpdate>,
    sequence: u64,
    recoverable_count: usize,
    last_transcript: Option<String>,
    hotkey: HotkeyReadiness,
    overlay_sender: Option<Sender<OverlayUpdate>>,
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
        let recoverable_count = runtime
            .recovery_entries()
            .map_or(0, |entries| entries.len());
        Self {
            runtime,
            settings,
            paths,
            recorder,
            transcriber,
            deliverer,
            workflow: Workflow::new(),
            active_job: None,
            active_recording: None,
            sequence: 0,
            recoverable_count,
            last_transcript: None,
            hotkey: HotkeyReadiness::Starting,
            overlay_sender: None,
        }
    }

    pub fn start_recording(&mut self) -> Result<RecordingJob, DaemonError> {
        if self.active_job.is_some() {
            return Err(DaemonError::AlreadyRecording);
        }
        fs::create_dir_all(&self.paths.recordings)?;
        let now = Utc::now();
        let path = self.paths.recordings.join(format!(
            "dictation-{}-{}.wav",
            now.format("%Y%m%dT%H%M%S%.fZ"),
            JobId::new()
        ));
        let job = match self.runtime.start_recording(
            RecordingRequest {
                audio_path: path.clone(),
                started_at: now,
                transcription_model: self.settings.active_transcription_model().to_owned(),
            },
            &mut self.recorder,
        ) {
            Ok(job) => job,
            Err(error) => {
                if let Some(failed) = self
                    .runtime
                    .recoverable_jobs()?
                    .into_iter()
                    .find(|job| job.audio_path == path)
                {
                    self.workflow
                        .apply(WorkflowSignal::StartRequested { job_id: failed.id })?;
                    self.workflow.apply(WorkflowSignal::Interrupted {
                        job_id: failed.id,
                        at: failed.stage,
                    })?;
                    self.recoverable_count = self.attention_recovery_count()?;
                    self.sequence += 1;
                    self.publish_overlay_update();
                }
                return Err(error.into());
            }
        };
        self.workflow
            .apply(WorkflowSignal::StartRequested { job_id: job.id })?;
        self.workflow
            .apply(WorkflowSignal::FirstAudioFrameWritten { job_id: job.id })?;
        self.active_job = Some(job.id);
        self.active_recording = Some(ActiveRecordingUpdate {
            audio_path: job.audio_path.clone(),
            // Match the previous overlay: elapsed time starts only after the
            // recorder has produced its first durable audio frame.
            started_at_unix_millis: Utc::now().timestamp_millis(),
        });
        self.recoverable_count = self.attention_recovery_count()?;
        self.sequence += 1;
        self.publish_overlay_update();
        tracing::info!(job_id = %job.id, audio_path = %job.audio_path.display(), "recording ready");
        Ok(job)
    }

    pub fn stop_recording(&mut self) -> Result<RecordingJob, DaemonError> {
        let id = self.active_job.ok_or(DaemonError::NotRecording)?;
        let job = self.runtime.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
        self.workflow.apply(WorkflowSignal::StopRequested)?;
        tracing::info!(job_id = %id, "recording stop requested");
        self.sequence += 1;
        self.publish_overlay_update();
        let capture = match self.recorder.finish(&job) {
            Ok(capture) => capture,
            Err(error) => {
                tracing::error!(job_id = %id, %error, "recording finalization failed");
                self.runtime.interrupt_job(
                    id,
                    JobStage::Recording,
                    format!("recording could not be finalized: {error}"),
                )?;
                self.workflow.apply(WorkflowSignal::Interrupted {
                    job_id: id,
                    at: JobStage::Interrupted,
                })?;
                self.active_job = None;
                self.active_recording = None;
                self.recoverable_count = self.attention_recovery_count()?;
                self.sequence += 1;
                self.publish_overlay_update();
                return Err(error.into());
            }
        };
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
        self.active_recording = None;
        self.sequence += 1;
        self.publish_overlay_update();
        let result =
            match self
                .runtime
                .process_captured(id, &mut self.transcriber, &mut self.deliverer)
            {
                Ok(result) => result,
                Err(error) => {
                    tracing::error!(job_id = %id, %error, "dictation processing failed");
                    self.workflow.apply(WorkflowSignal::Interrupted {
                        job_id: id,
                        at: JobStage::Failed,
                    })?;
                    self.active_job = None;
                    self.active_recording = None;
                    self.recoverable_count = self.attention_recovery_count()?;
                    self.sequence += 1;
                    self.publish_overlay_update();
                    return Err(error.into());
                }
            };
        self.workflow
            .apply(WorkflowSignal::TranscriptStored { job_id: id })?;
        self.workflow
            .apply(WorkflowSignal::DeliveryStarted { job_id: id })?;
        if result.stage == JobStage::Delivered {
            self.workflow
                .apply(WorkflowSignal::DeliveryCommitted { job_id: id })?;
            if let Err(error) = self.runtime.record_delivered_session(id, &self.settings) {
                // Delivery already reached the target application. A secondary
                // analytics failure must never turn that committed outcome into
                // a retryable delivery and risk a duplicate paste.
                tracing::error!(job_id = %id, %error, "could not record delivered session history");
            }
            self.cleanup_delivered_audio(&result);
            self.workflow = Workflow::new();
        } else {
            self.workflow.apply(WorkflowSignal::Interrupted {
                job_id: id,
                at: result.stage,
            })?;
        }
        self.active_job = None;
        self.active_recording = None;
        self.last_transcript = Some(result.final_text.clone());
        self.recoverable_count = self.attention_recovery_count()?;
        self.sequence += 1;
        self.publish_overlay_update();
        tracing::info!(job_id = %id, stage = ?result.stage, "dictation flow completed");
        Ok(result)
    }

    /// Discards the active recording because the user explicitly pressed
    /// Escape. Shutdown and platform failures must use the separate recovery
    /// preservation path below.
    pub fn discard_recording(&mut self) -> Result<RecordingJob, DaemonError> {
        let id = self.active_job.ok_or(DaemonError::NotRecording)?;
        tracing::info!(job_id = %id, "dictation discard requested");
        let job = self.runtime.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
        let capture = match self.recorder.finish(&job) {
            Ok(capture) => capture,
            Err(error) => {
                let interrupted = self.runtime.interrupt_job(
                    id,
                    JobStage::Recording,
                    format!("recording could not be finalized while discarding: {error}"),
                )?;
                self.workflow.apply(WorkflowSignal::Interrupted {
                    job_id: id,
                    at: JobStage::Interrupted,
                })?;
                self.active_job = None;
                self.active_recording = None;
                self.recoverable_count = self.attention_recovery_count()?;
                self.sequence += 1;
                self.publish_overlay_update();
                return Ok(interrupted);
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
        let discarded = match self.runtime.discard_recording(id) {
            Ok(discarded) => discarded,
            Err(error) => {
                self.workflow.apply(WorkflowSignal::Interrupted {
                    job_id: id,
                    at: JobStage::Captured,
                })?;
                self.active_job = None;
                self.active_recording = None;
                self.recoverable_count = self.attention_recovery_count()?;
                self.sequence += 1;
                self.publish_overlay_update();
                return Err(error.into());
            }
        };
        self.workflow
            .apply(WorkflowSignal::DiscardCommitted { job_id: id })?;
        self.active_job = None;
        self.active_recording = None;
        self.recoverable_count = self.attention_recovery_count()?;
        self.sequence += 1;
        self.publish_overlay_update();
        tracing::info!(job_id = %id, "dictation discarded");
        Ok(discarded)
    }

    /// Handles a kernel-observed recorder exit. A stale notification from a
    /// normal stop is ignored; an active unexpected exit preserves whatever
    /// audio was finalized for explicit recovery instead of guessing that the
    /// dictation was complete.
    pub fn recorder_exited(&mut self, id: JobId) -> Result<Option<RecordingJob>, DaemonError> {
        if self.active_job != Some(id) {
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
        if self.active_job.is_some() {
            self.preserve_active_recording(
                "AgentDictate shut down before this dictation completed; audio was preserved",
            )?;
        }
        Ok(())
    }

    pub fn retry_transcription(&mut self, id: JobId) -> Result<RecordingJob, DaemonError> {
        let result =
            self.runtime
                .retry_transcription(id, &mut self.transcriber, &mut self.deliverer)?;
        self.workflow = Workflow::new();
        if result.stage == JobStage::Delivered
            && let Err(error) = self.runtime.record_delivered_session(id, &self.settings)
        {
            tracing::error!(job_id = %id, %error, "could not record retried transcription history");
        }
        if result.stage == JobStage::Delivered {
            self.cleanup_delivered_audio(&result);
        }
        self.last_transcript = Some(result.final_text.clone());
        self.recoverable_count = self.attention_recovery_count()?;
        self.sequence += 1;
        self.publish_overlay_update();
        Ok(result)
    }

    pub fn retry_delivery(&mut self, id: JobId) -> Result<RecordingJob, DaemonError> {
        let result = self.runtime.retry_delivery(id, &mut self.deliverer)?;
        self.workflow = Workflow::new();
        if result.stage == JobStage::Delivered
            && let Err(error) = self.runtime.record_delivered_session(id, &self.settings)
        {
            tracing::error!(job_id = %id, %error, "could not record retried delivery history");
        }
        if result.stage == JobStage::Delivered {
            self.cleanup_delivered_audio(&result);
        }
        self.last_transcript = Some(result.final_text.clone());
        self.recoverable_count = self.attention_recovery_count()?;
        self.sequence += 1;
        self.publish_overlay_update();
        Ok(result)
    }

    pub fn delete_recovery(&mut self, id: JobId) -> Result<RecordingJob, DaemonError> {
        let result = self.runtime.delete_recovery(id)?;
        self.workflow = Workflow::new();
        self.recoverable_count = self.attention_recovery_count()?;
        self.sequence += 1;
        self.publish_overlay_update();
        Ok(result)
    }

    pub fn workspace_snapshot(&self) -> Result<WorkspaceSnapshot, RuntimeError> {
        let recoveries = self
            .runtime
            .recovery_entries()?
            .into_iter()
            .map(|entry| RecoverySnapshot {
                job_id: entry.job_id,
                stage: entry.stage,
                updated_at: entry.updated_at,
                duration_seconds: entry.duration_seconds,
                raw_transcript: entry.raw_transcript,
                final_text: entry.final_text,
                error_message: entry.error_message,
                audio_present: entry.audio_present,
                delivery_ambiguous: entry.delivery_status == DeliveryStatus::Ambiguous,
            })
            .collect();
        let history_page = self.history_page_snapshot(HistoryPageRequest::default())?;
        let replacements = self.runtime.replacement_rules()?;
        let usage = self.usage_snapshot()?;
        Ok(WorkspaceSnapshot {
            recoveries,
            recent_history: history_page.rows.clone(),
            history: history_page.rows,
            history_total: history_page.total_matches,
            history_has_more: history_page.next_cursor.is_some(),
            history_next_cursor: history_page.next_cursor,
            history_search: history_page.search,
            replacements,
            usage,
            model_catalog: Default::default(),
        })
    }

    pub fn history_page_snapshot(
        &self,
        request: HistoryPageRequest,
    ) -> Result<HistoryPageSnapshot, RuntimeError> {
        let history_query = HistoryQuery {
            search: request.search.clone(),
            limit: request.page_size,
            after: request
                .after
                .as_ref()
                .map(|cursor| HistoryCursor::from_opaque(cursor.as_str())),
            ..HistoryQuery::default()
        };
        let (page, cursor_restarted) = match self.runtime.history_page(history_query) {
            Ok(page) => (page, false),
            Err(RuntimeError::InvalidHistoryCursor(_)) if request.after.is_some() => (
                self.runtime.history_page(HistoryQuery {
                    search: request.search.clone(),
                    limit: request.page_size,
                    ..HistoryQuery::default()
                })?,
                true,
            ),
            Err(error) => return Err(error),
        };
        Ok(HistoryPageSnapshot {
            search: request.search,
            total_matches: page.total_matches,
            cursor_restarted,
            next_cursor: page
                .next_cursor
                .map(|cursor| HistoryPageCursor::new(cursor.into_opaque())),
            rows: page
                .matches
                .into_iter()
                .map(|matched| HistorySnapshot {
                    id: matched.entry.id,
                    created_at: matched.entry.created_at,
                    preview_text: matched.preview,
                    word_count: matched.entry.final_word_count,
                    duration_seconds: matched.entry.duration_seconds,
                })
                .collect(),
        })
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

    pub fn history(&self, id: i64) -> Result<Option<HistoryEntry>, RuntimeError> {
        self.runtime.history(id)
    }

    #[must_use]
    pub fn snapshot(&self) -> AppSnapshot {
        AppSnapshot {
            sequence: self.sequence,
            workflow: self.workflow.snapshot(),
            hotkey: self.hotkey.clone(),
            recoverable_count: self.recoverable_count,
            last_transcript: self.last_transcript.clone(),
        }
    }

    pub fn set_hotkey_readiness(&mut self, readiness: HotkeyReadiness) {
        self.hotkey = readiness;
        self.sequence += 1;
        self.publish_overlay_update();
    }

    pub fn set_overlay_sender(&mut self, sender: Sender<OverlayUpdate>) {
        self.overlay_sender = Some(sender);
        self.publish_overlay_update();
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
        self.sequence += 1;
        self.publish_overlay_update();
    }

    pub fn sync_pricing(&mut self, settings: &Settings) -> Result<(), RuntimeError> {
        self.runtime.sync_pricing(settings)
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

    fn publish_overlay_update(&self) {
        if let Some(sender) = &self.overlay_sender {
            let _ = sender.send(self.overlay_update());
        }
    }

    fn recover_after_capture_checkpoint_failure(&mut self, id: JobId, primary: &RuntimeError) {
        self.active_job = None;
        self.active_recording = None;

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

        let interrupted = WorkflowSignal::Interrupted {
            job_id: id,
            at: JobStage::Interrupted,
        };
        if let Err(workflow_error) = self.workflow.apply(interrupted) {
            tracing::warn!(
                job_id = %id,
                %workflow_error,
                "rebuilding workflow after capture-checkpoint failure"
            );
            self.workflow = Workflow::new();
            let _ = self
                .workflow
                .apply(WorkflowSignal::StartRequested { job_id: id });
            let _ = self.workflow.apply(interrupted);
        }
        self.recoverable_count = self.runtime.recovery_entries().map_or_else(
            |recovery_error| {
                tracing::error!(
                    job_id = %id,
                    %recovery_error,
                    "could not recount recoverable recordings"
                );
                self.recoverable_count.max(1)
            },
            |jobs| jobs.len(),
        );
        self.sequence += 1;
        self.publish_overlay_update();
    }

    fn preserve_active_recording(
        &mut self,
        reason: &'static str,
    ) -> Result<RecordingJob, DaemonError> {
        let id = self.active_job.ok_or(DaemonError::NotRecording)?;
        let job = self.runtime.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
        let capture = match self.recorder.finish(&job) {
            Ok(capture) => capture,
            Err(error) => {
                let interrupted = self.runtime.interrupt_job(
                    id,
                    JobStage::Recording,
                    format!("{reason}; recording could not be finalized: {error}"),
                )?;
                self.workflow.apply(WorkflowSignal::Interrupted {
                    job_id: id,
                    at: JobStage::Interrupted,
                })?;
                self.active_job = None;
                self.active_recording = None;
                self.recoverable_count = self.attention_recovery_count()?;
                self.sequence += 1;
                self.publish_overlay_update();
                return Ok(interrupted);
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
            .interrupt_job(id, JobStage::Captured, reason.to_owned())?;
        self.workflow.apply(WorkflowSignal::Interrupted {
            job_id: id,
            at: JobStage::Interrupted,
        })?;
        self.active_job = None;
        self.active_recording = None;
        self.recoverable_count = self.attention_recovery_count()?;
        self.sequence += 1;
        self.publish_overlay_update();
        Ok(interrupted)
    }

    fn attention_recovery_count(&self) -> Result<usize, RuntimeError> {
        Ok(self.runtime.recovery_entries()?.len())
    }

    fn overlay_update(&self) -> OverlayUpdate {
        OverlayUpdate {
            workflow: self.workflow.snapshot(),
            active_recording: self.active_recording.clone(),
        }
    }

    fn cleanup_delivered_audio(&self, job: &RecordingJob) {
        if self.settings.preserve_temp_audio {
            return;
        }
        match fs::remove_file(&job.audio_path) {
            Ok(()) => tracing::debug!(job_id = %job.id, "removed delivered recording audio"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(job_id = %job.id, %error, "could not remove delivered recording audio");
            }
        }
    }

    fn usage_snapshot(&self) -> Result<UsageSnapshot, RuntimeError> {
        let summary = self.runtime.usage_summary()?;
        let sessions = self.runtime.usage_series(30, UsageMetric::Sessions)?;
        let words = self.runtime.usage_series(30, UsageMetric::Words)?;
        let audio = self.runtime.usage_series(30, UsageMetric::AudioMinutes)?;
        let costs = self.runtime.usage_series(30, UsageMetric::EstimatedCost)?;
        let activity = sessions
            .into_iter()
            .zip(words)
            .zip(audio)
            .zip(costs)
            .map(|(((sessions, words), audio), cost)| UsageDaySnapshot {
                date: sessions.date,
                totals: UsageTotalsSnapshot {
                    dictations: sessions.value.round().max(0.0) as u64,
                    words: words.value.round().max(0.0) as u64,
                    audio_seconds: (audio.value * 60.0).max(0.0),
                    estimated_cost: cost.value.max(0.0),
                },
            })
            .collect::<Vec<_>>();
        let last_30_days = sum_usage(activity.iter().map(|day| day.totals));
        let last_7_days = sum_usage(activity.iter().rev().take(7).map(|day| day.totals));
        let weekly_activity = self
            .runtime
            .usage_weekly_series()?
            .into_iter()
            .map(|week| UsageDaySnapshot {
                date: week.week_start,
                totals: UsageTotalsSnapshot {
                    dictations: week.total_sessions,
                    words: week.total_words,
                    audio_seconds: week.total_audio_seconds,
                    estimated_cost: week.estimated_total_cost,
                },
            })
            .collect();
        Ok(UsageSnapshot {
            last_7_days,
            last_30_days,
            all_time: usage_totals(summary.all_time),
            activity,
            weekly_activity,
        })
    }
}

fn usage_totals(aggregate: UsageAggregate) -> UsageTotalsSnapshot {
    UsageTotalsSnapshot {
        dictations: aggregate.total_sessions,
        words: aggregate.total_words,
        audio_seconds: aggregate.total_audio_seconds,
        estimated_cost: aggregate.estimated_total_cost,
    }
}

fn sum_usage(values: impl Iterator<Item = UsageTotalsSnapshot>) -> UsageTotalsSnapshot {
    values.fold(UsageTotalsSnapshot::default(), |mut total, value| {
        total.dictations += value.dictations;
        total.words += value.words;
        total.audio_seconds += value.audio_seconds;
        total.estimated_cost += value.estimated_cost;
        total
    })
}
