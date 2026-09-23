use std::cell::RefCell;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use agentdictate_core::apply_replacements;
use chrono::Utc;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

use crate::history::serialize_replacements;
use crate::history_search;
use crate::schema::{SCHEMA, row_to_job, stage_name, state_for_stage, timestamp};
use crate::startup_cleanup::recovery_delete_path;
use crate::{
    Deliverer, DeliveryDisposition, DeliveryGate, DeliveryMethod, DeliveryStatus, ExternalError,
    HeadlessDeliveryGate, JobId, JobStage, Recorder, RecordingJob, RecordingRequest,
    ReplacementRule, RuntimeError, Transcriber,
};

pub struct Runtime {
    pub(crate) connection: Connection,
    pub(crate) history_search_cache: RefCell<history_search::SearchCache>,
}

impl Runtime {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RuntimeError> {
        let path = path.as_ref();
        let mut connection = Connection::open(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        configure_writer(&mut connection)?;
        connection.execute_batch(SCHEMA)?;
        add_missing_columns(&connection)?;
        connection.execute_batch(INDEXES)?;
        if let Err(error) = history_search::ensure_schema(&mut connection)
            && !history_search::is_search_schema_unavailable(&error)
        {
            return Err(error);
        }
        remove_blank_replacements(&connection)?;
        rename_committed_deliveries(&connection)?;
        reconcile_ambiguous_deliveries(&connection)?;
        reconcile_interrupted_jobs(&connection)?;
        Ok(Self {
            connection,
            history_search_cache: RefCell::new(history_search::SearchCache::default()),
        })
    }

    /// Opens a read-only view without running startup reconciliation.
    pub fn open_observer(path: impl AsRef<Path>) -> Result<Self, RuntimeError> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(Self {
            connection,
            history_search_cache: RefCell::new(history_search::SearchCache::default()),
        })
    }

    /// Opens a write connection for a worker owned by an already-running
    /// daemon. Unlike `open`, this never performs crash reconciliation that
    /// could reinterpret the live daemon's active recording as abandoned.
    pub fn open_background_writer(path: impl AsRef<Path>) -> Result<Self, RuntimeError> {
        let mut connection = Connection::open(path)?;
        configure_writer(&mut connection)?;
        Ok(Self {
            connection,
            history_search_cache: RefCell::new(history_search::SearchCache::default()),
        })
    }

    /// Completes the deferred full-text backfill after essential listeners are
    /// ready. Search remains available through a literal fallback beforehand.
    /// Live daemon callers must use `HistoryIndexMaintenance` so recording can
    /// interrupt this otherwise monolithic transaction.
    pub fn ensure_history_search_index(&mut self) -> Result<(), RuntimeError> {
        history_search::ensure_index(&mut self.connection, &self.history_search_cache)
    }

    pub fn start_recording(
        &mut self,
        request: RecordingRequest,
        recorder: &mut impl Recorder,
    ) -> Result<RecordingJob, RuntimeError> {
        let id = request.id;
        let now = timestamp(Utc::now());
        self.connection.execute(
            r#"
            INSERT INTO dictation_jobs (
                runtime_id, started_at, updated_at, state, stage, audio_path,
                transcription_model, processing_options
            ) VALUES (?1, ?2, ?3, 'active', 'starting', ?4, ?5, ?6)
            "#,
            params![
                id.to_string(),
                timestamp(request.started_at),
                now,
                request.audio_path.to_string_lossy(),
                request.transcription_model,
                request
                    .options
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
            ],
        )?;
        let starting = self.job(id)?.expect("inserted job must be readable");

        if let Err(error) = recorder.start(&starting) {
            let _ = self.update_stage(id, JobStage::Interrupted, Some(error.to_string()));
            return Err(error.into());
        }

        if let Err(checkpoint_error) = self.update_stage(id, JobStage::Recording, None) {
            let compensation = recorder.abort_start(&starting);
            let mut recovery_message = format!(
                "recording stopped because its durable checkpoint failed: {checkpoint_error}"
            );
            if let Err(compensation_error) = compensation {
                recovery_message.push_str(&format!(
                    "; recorder compensation also failed: {compensation_error}"
                ));
            }
            let _ = self.update_stage(id, JobStage::Interrupted, Some(recovery_message));
            return Err(checkpoint_error);
        }
        let job = self.job(id)?.expect("updated job must be readable");
        Ok(job)
    }

    pub fn capture_recording(
        &mut self,
        id: JobId,
        duration_seconds: f64,
    ) -> Result<RecordingJob, RuntimeError> {
        let recording = self.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
        if recording.stage != JobStage::Recording {
            return Err(RuntimeError::InvalidStage {
                job_id: id,
                expected: JobStage::Recording,
                actual: recording.stage,
            });
        }
        let updated = self.connection.execute(
            r#"
            UPDATE dictation_jobs
            SET state = 'captured', stage = 'captured', duration_seconds = ?1,
                updated_at = ?2, error_message = NULL
            WHERE runtime_id = ?3
            "#,
            params![duration_seconds, timestamp(Utc::now()), id.to_string()],
        )?;
        if updated == 0 {
            return Err(RuntimeError::JobNotFound(id));
        }
        let job = self.job(id)?.expect("updated job must be readable");
        Ok(job)
    }

    /// Moves an in-flight job to a recoverable terminal state. `at` is an
    /// optimistic concurrency guard so a late platform error cannot interrupt
    /// a job that has already advanced to a safer checkpoint.
    pub fn interrupt_job(
        &mut self,
        id: JobId,
        at: JobStage,
        error_message: impl Into<String>,
    ) -> Result<RecordingJob, RuntimeError> {
        let current = self.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
        if current.stage != at {
            return Err(RuntimeError::InvalidStage {
                job_id: id,
                expected: at,
                actual: current.stage,
            });
        }
        self.update_stage(id, JobStage::Interrupted, Some(error_message.into()))?;
        let interrupted = self.job(id)?.expect("updated job must be readable");
        Ok(interrupted)
    }

    /// Permanently discards a recording after its audio has reached the
    /// durable captured checkpoint. With `keep_audio` (the "Preserve
    /// temporary audio" setting) only the job is deleted and the WAV stays,
    /// like the audio of a completed dictation. Otherwise the shared recovery
    /// deletion path moves the audio into quarantine before deleting the job
    /// row, so a failed delete never strands a retryable row without its
    /// only audio copy.
    pub fn discard_recording(
        &mut self,
        id: JobId,
        keep_audio: bool,
    ) -> Result<RecordingJob, RuntimeError> {
        let current = self.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
        if current.stage != JobStage::Captured {
            return Err(RuntimeError::InvalidStage {
                job_id: id,
                expected: JobStage::Captured,
                actual: current.stage,
            });
        }
        if !keep_audio {
            return self.delete_recovery(id);
        }
        self.connection.execute(
            "DELETE FROM dictation_jobs WHERE runtime_id = ?1",
            [id.to_string()],
        )?;
        let deleted = RecordingJob {
            stage: JobStage::Deleted,
            updated_at: Utc::now(),
            error_message: None,
            ..current
        };
        Ok(deleted)
    }

    /// Transcribes a captured recording and pastes the result.
    pub fn process_captured(
        &mut self,
        id: JobId,
        transcriber: &mut impl Transcriber,
        delivery_gate: &mut impl DeliveryGate,
        deliverer: &mut impl Deliverer,
    ) -> Result<RecordingJob, RuntimeError> {
        self.process(
            id,
            DeliveryMethod::Paste,
            transcriber,
            delivery_gate,
            deliverer,
        )
    }

    fn process(
        &mut self,
        id: JobId,
        method: DeliveryMethod,
        transcriber: &mut impl Transcriber,
        delivery_gate: &mut impl DeliveryGate,
        deliverer: &mut impl Deliverer,
    ) -> Result<RecordingJob, RuntimeError> {
        let captured = self.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
        if captured.stage != JobStage::Captured {
            return Err(RuntimeError::InvalidStage {
                job_id: id,
                expected: JobStage::Captured,
                actual: captured.stage,
            });
        }
        self.update_stage(id, JobStage::Transcribing, None)?;
        self.transcribe_and_deliver(id, method, transcriber, delivery_gate, deliverer)
            .map_err(|error| self.fail_job(id, error))
    }

    /// Everything after the `Transcribing` checkpoint. Callers route its
    /// errors through `fail_job` so no failure can strand the job in flight.
    fn transcribe_and_deliver(
        &mut self,
        id: JobId,
        method: DeliveryMethod,
        transcriber: &mut impl Transcriber,
        delivery_gate: &mut impl DeliveryGate,
        deliverer: &mut impl Deliverer,
    ) -> Result<RecordingJob, RuntimeError> {
        let transcribing = self.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
        let transcript = match transcriber.transcribe(&transcribing) {
            Ok(transcript) => transcript,
            Err(ExternalError::NoSpeech) => {
                // Nothing to deliver or recover, so the job leaves the
                // in-flight table. The caller removes its audio.
                self.connection.execute(
                    "DELETE FROM dictation_jobs WHERE runtime_id = ?1",
                    [id.to_string()],
                )?;
                let finished = RecordingJob {
                    stage: JobStage::NoSpeech,
                    updated_at: Utc::now(),
                    ..transcribing
                };
                return Ok(finished);
            }
            Err(error) => {
                // If this write fails too, `fail_job` records the failure.
                let _ = self.update_stage(id, JobStage::Failed, Some(error.to_string()));
                return Err(error.into());
            }
        };
        // Checkpoint the paid-for text first, so a later failure leaves it
        // in Recovery instead of needing another transcription.
        self.connection.execute(
            r#"
            UPDATE dictation_jobs
            SET raw_transcript = ?1, transcription_model = ?2, updated_at = ?3
            WHERE runtime_id = ?4
            "#,
            params![
                transcript.text,
                transcript.model,
                timestamp(Utc::now()),
                id.to_string()
            ],
        )?;
        let replacement_rules = match &transcribing.options {
            Some(options) => options.replacements.clone(),
            None => self.replacement_rules()?,
        };
        let mut replacement_result = apply_replacements(&transcript.text, &replacement_rules)
            .map_err(|error| {
                ExternalError::new(format!("replacement processing failed: {error}"))
            })?;
        if let Some(options) = &transcribing.options {
            let normalized = agentdictate_core::normalize_vocabulary(
                &replacement_result.text,
                &options.vocabulary,
            );
            replacement_result.text = normalized.text;
            replacement_result.applied.extend(normalized.applied);
        }
        let replacements_applied = serialize_replacements(&replacement_result.applied)?;

        self.connection.execute(
            r#"
            UPDATE dictation_jobs
            SET state = 'captured', stage = 'ready_to_deliver', updated_at = ?1,
                final_text = ?2, replacements_applied = ?3, error_message = NULL,
                delivery_status = 'not_attempted'
            WHERE runtime_id = ?4
            "#,
            params![
                timestamp(Utc::now()),
                replacement_result.text,
                replacements_applied,
                id.to_string(),
            ],
        )?;
        let ready = self.job(id)?.expect("updated job must be readable");

        self.deliver_ready(ready, method, delivery_gate, deliverer)
    }

    pub fn replacement_rules(&self) -> Result<Vec<ReplacementRule>, RuntimeError> {
        let mut statement = self.connection.prepare(
            r#"
            SELECT id, source_phrase, replacement_phrase, enabled,
                   case_sensitive, whole_word_only
            FROM replacement_mappings
            ORDER BY id ASC
            "#,
        )?;
        Ok(statement
            .query_map([], |row| {
                Ok(ReplacementRule {
                    id: Some(row.get(0)?),
                    source_phrase: row.get(1)?,
                    replacement_phrase: row.get(2)?,
                    enabled: row.get(3)?,
                    case_sensitive: row.get(4)?,
                    whole_word_only: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn create_replacement(
        &mut self,
        mut rule: ReplacementRule,
    ) -> Result<ReplacementRule, RuntimeError> {
        rule.source_phrase = rule.source_phrase.trim().to_owned();
        if rule.source_phrase.is_empty() {
            return Err(RuntimeError::InvalidReplacementSource);
        }
        let now = timestamp(Utc::now());
        self.connection.execute(
            r#"
            INSERT INTO replacement_mappings (
                source_phrase, replacement_phrase, enabled, case_sensitive,
                whole_word_only, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
            "#,
            params![
                rule.source_phrase,
                rule.replacement_phrase,
                rule.enabled,
                rule.case_sensitive,
                rule.whole_word_only,
                now,
            ],
        )?;
        rule.id = Some(self.connection.last_insert_rowid());
        Ok(rule)
    }

    pub fn update_replacement(
        &mut self,
        mut rule: ReplacementRule,
    ) -> Result<ReplacementRule, RuntimeError> {
        let id = rule.id.ok_or(RuntimeError::MissingReplacementId)?;
        rule.source_phrase = rule.source_phrase.trim().to_owned();
        if rule.source_phrase.is_empty() {
            return Err(RuntimeError::InvalidReplacementSource);
        }
        let updated = self.connection.execute(
            r#"
            UPDATE replacement_mappings
            SET source_phrase = ?1, replacement_phrase = ?2, enabled = ?3,
                case_sensitive = ?4, whole_word_only = ?5, updated_at = ?6
            WHERE id = ?7
            "#,
            params![
                rule.source_phrase,
                rule.replacement_phrase,
                rule.enabled,
                rule.case_sensitive,
                rule.whole_word_only,
                timestamp(Utc::now()),
                id,
            ],
        )?;
        if updated == 0 {
            return Err(RuntimeError::ReplacementNotFound(id));
        }
        Ok(rule)
    }

    pub fn delete_replacement(&mut self, id: i64) -> Result<bool, RuntimeError> {
        Ok(self
            .connection
            .execute("DELETE FROM replacement_mappings WHERE id = ?1", [id])?
            > 0)
    }

    /// Transcribes a Recovery item again after an explicit user action and
    /// copies the result. Like `retry_delivery`, it never pastes: the user
    /// asked from AgentDictate's own window, which has the focus.
    pub fn retry_transcription(
        &mut self,
        id: JobId,
        transcriber: &mut impl Transcriber,
        deliverer: &mut impl Deliverer,
    ) -> Result<RecordingJob, RuntimeError> {
        let current = self.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
        if current.delivery_status == DeliveryStatus::Ambiguous {
            return Err(RuntimeError::OperationNotAllowed {
                operation: "retry transcription for",
                job_id: id,
                stage: current.stage,
                reason: "the previous delivery may already have reached the focused application",
            });
        }
        if !matches!(
            current.stage,
            JobStage::Captured | JobStage::Interrupted | JobStage::Failed
        ) {
            return Err(RuntimeError::OperationNotAllowed {
                operation: "retry transcription for",
                job_id: id,
                stage: current.stage,
                reason: "the job is not in a recoverable transcription state",
            });
        }
        self.connection.execute(
            r#"
            UPDATE dictation_jobs
            SET state = 'captured', stage = 'captured', updated_at = ?1,
                delivery_status = 'not_attempted', error_message = NULL
            WHERE runtime_id = ?2
            "#,
            params![timestamp(Utc::now()), id.to_string()],
        )?;
        // Copying cannot reach transient AgentDictate UI, so no gate is needed.
        self.process(
            id,
            DeliveryMethod::CopyOnly,
            transcriber,
            &mut HeadlessDeliveryGate,
            deliverer,
        )
    }

    /// Re-attempts only the delivery step after an explicit user action, by
    /// copying the stored transcript. It never pastes: the user asked from
    /// AgentDictate's own window, which has the focus. This is intentionally
    /// separate from startup recovery: an ambiguous prior injection is never
    /// retried automatically because doing so could paste duplicate text.
    pub fn retry_delivery(
        &mut self,
        id: JobId,
        deliverer: &mut impl Deliverer,
    ) -> Result<RecordingJob, RuntimeError> {
        let current = self.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
        if current.delivery_status == DeliveryStatus::Attempting {
            return Err(RuntimeError::OperationNotAllowed {
                operation: "retry delivery for",
                job_id: id,
                stage: current.stage,
                reason: "the previous paste attempt has no durable outcome yet",
            });
        }
        if current.final_text.trim().is_empty()
            || matches!(
                current.stage,
                JobStage::Starting
                    | JobStage::Recording
                    | JobStage::Captured
                    | JobStage::Transcribing
                    | JobStage::Delivered
                    | JobStage::Deleted
            )
        {
            return Err(RuntimeError::OperationNotAllowed {
                operation: "retry delivery for",
                job_id: id,
                stage: current.stage,
                reason: "the job does not have a recoverable stored transcript",
            });
        }
        self.connection.execute(
            r#"
            UPDATE dictation_jobs
            SET state = 'captured', stage = 'ready_to_deliver', updated_at = ?1,
                delivery_status = 'not_attempted', error_message = NULL
            WHERE runtime_id = ?2
            "#,
            params![timestamp(Utc::now()), id.to_string()],
        )?;
        let ready = self.job(id)?.expect("updated job must be readable");
        self.deliver_ready(
            ready,
            DeliveryMethod::CopyOnly,
            &mut HeadlessDeliveryGate,
            deliverer,
        )
        .map_err(|error| self.fail_job(id, error))
    }

    /// Deletes explicit recovery data, text and audio, without exposing a
    /// crash window where the database still offers a retry after the only
    /// audio copy is gone: the audio moves to quarantine, then the job row is
    /// deleted, then the quarantine file is unlinked.
    pub fn delete_recovery(&mut self, id: JobId) -> Result<RecordingJob, RuntimeError> {
        let current = self.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
        if matches!(
            current.stage,
            JobStage::Starting
                | JobStage::Recording
                | JobStage::Transcribing
                | JobStage::Delivered
                | JobStage::Deleted
        ) {
            return Err(RuntimeError::OperationNotAllowed {
                operation: "delete recovery for",
                job_id: id,
                stage: current.stage,
                reason: "the job is active, delivered, or already deleted",
            });
        }
        let quarantine_path = recovery_delete_path(&current.audio_path, id);
        let quarantined = match fs::rename(&current.audio_path, &quarantine_path) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => quarantine_path.exists(),
            Err(error) => return Err(error.into()),
        };
        if let Err(error) = self.connection.execute(
            "DELETE FROM dictation_jobs WHERE runtime_id = ?1",
            [id.to_string()],
        ) {
            if quarantined && !current.audio_path.exists() {
                fs::rename(&quarantine_path, &current.audio_path)?;
            }
            return Err(error.into());
        }
        let deleted = RecordingJob {
            stage: JobStage::Deleted,
            updated_at: Utc::now(),
            error_message: None,
            ..current
        };
        if quarantined {
            // The job row is already gone. A rare unlink failure leaves a
            // deterministic quarantine file that startup reconciliation
            // removes, never a falsely retryable job without audio.
            let _ = fs::remove_file(quarantine_path);
        }
        Ok(deleted)
    }

    fn deliver_ready(
        &mut self,
        ready: RecordingJob,
        method: DeliveryMethod,
        delivery_gate: &mut impl DeliveryGate,
        deliverer: &mut impl Deliverer,
    ) -> Result<RecordingJob, RuntimeError> {
        if let Err(error) = delivery_gate.confirm_ready() {
            self.connection.execute(
                r#"
                UPDATE dictation_jobs
                SET updated_at = ?1, error_message = ?2
                WHERE runtime_id = ?3
                "#,
                params![
                    timestamp(Utc::now()),
                    format!("delivery blocked before paste: {error}"),
                    ready.id.to_string(),
                ],
            )?;
            return Err(RuntimeError::DeliveryBlocked(error));
        }
        self.connection.execute(
            r#"
            UPDATE dictation_jobs
            SET delivery_status = 'attempting', updated_at = ?1
            WHERE runtime_id = ?2
            "#,
            params![timestamp(Utc::now()), ready.id.to_string()],
        )?;

        let disposition = match deliverer.deliver(&ready, method) {
            Ok(disposition) => disposition,
            Err(error) => {
                self.mark_delivery_ambiguous(
                    ready.id,
                    false,
                    format!("delivery result is ambiguous: {error}"),
                )?;
                return Err(error.into());
            }
        };
        match disposition {
            DeliveryDisposition::Submitted {
                copied_to_clipboard,
                paste_triggered,
            } => {
                self.connection.execute(
                    r#"
                    UPDATE dictation_jobs
                    SET state = 'delivered', stage = 'delivered', updated_at = ?1,
                        copied_to_clipboard = ?2, paste_triggered = ?3,
                        delivery_status = 'submitted', error_message = NULL
                    WHERE runtime_id = ?4
                    "#,
                    params![
                        timestamp(Utc::now()),
                        copied_to_clipboard,
                        paste_triggered,
                        ready.id.to_string(),
                    ],
                )?;
            }
            DeliveryDisposition::Ambiguous {
                copied_to_clipboard,
            } => self.mark_delivery_ambiguous(
                ready.id,
                copied_to_clipboard,
                "delivery may have reached the focused application".to_owned(),
            )?,
            DeliveryDisposition::NotSent {
                copied_to_clipboard,
                reason,
            } => {
                // The job stays ready to deliver, so the user can try again.
                self.connection.execute(
                    r#"
                    UPDATE dictation_jobs
                    SET updated_at = ?1, copied_to_clipboard = ?2,
                        delivery_status = 'not_attempted', error_message = ?3
                    WHERE runtime_id = ?4
                    "#,
                    params![
                        timestamp(Utc::now()),
                        copied_to_clipboard,
                        reason,
                        ready.id.to_string(),
                    ],
                )?;
            }
        }
        let result = self.job(ready.id)?.expect("updated job must be readable");
        Ok(result)
    }

    /// Records `error` on a job that a failed step left in flight, applying
    /// the daemon-start rules to this one job: transcribing becomes failed
    /// and keeps its raw transcript, and a started paste
    /// attempt becomes ambiguous so it is never replayed. A job at a safe
    /// checkpoint is left as it is. Returns the error to propagate; if even
    /// this write fails, the error says so and the next daemon start
    /// reconciles the job instead.
    fn fail_job(&self, id: JobId, error: RuntimeError) -> RuntimeError {
        let recorded = self.connection.execute(
            r#"
            UPDATE dictation_jobs
            SET state = 'failed', stage = 'failed', updated_at = ?1, error_message = ?2,
                delivery_status = CASE delivery_status
                    WHEN 'attempting' THEN 'ambiguous'
                    ELSE delivery_status
                END
            WHERE runtime_id = ?3
              AND (stage = 'transcribing' OR delivery_status = 'attempting')
            "#,
            params![timestamp(Utc::now()), error.to_string(), id.to_string()],
        );
        match recorded {
            Ok(_) => error,
            Err(record_error) => RuntimeError::FailureNotRecorded {
                error: Box::new(error),
                record_error,
            },
        }
    }

    fn mark_delivery_ambiguous(
        &self,
        id: JobId,
        copied_to_clipboard: bool,
        error_message: String,
    ) -> Result<(), RuntimeError> {
        self.connection.execute(
            r#"
            UPDATE dictation_jobs
            SET state = 'failed', stage = 'failed', updated_at = ?1,
                copied_to_clipboard = ?2, delivery_status = 'ambiguous',
                error_message = ?3
            WHERE runtime_id = ?4
            "#,
            params![
                timestamp(Utc::now()),
                copied_to_clipboard,
                error_message,
                id.to_string(),
            ],
        )?;
        Ok(())
    }

    pub fn job(&self, id: JobId) -> Result<Option<RecordingJob>, RuntimeError> {
        load_job(&self.connection, id)
    }

    pub fn recoverable_jobs(&self) -> Result<Vec<RecordingJob>, RuntimeError> {
        let mut statement = self.connection.prepare(
            r#"
            SELECT runtime_id, started_at, updated_at, stage, audio_path,
                   duration_seconds, transcription_model, raw_transcript,
                   final_text, copied_to_clipboard, paste_triggered,
                   delivery_status, error_message, processing_options
            FROM dictation_jobs
            WHERE state NOT IN ('delivered', 'deleted', 'no_speech')
            ORDER BY updated_at DESC, id DESC
            "#,
        )?;
        let rows = statement
            .query_map([], row_to_job)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().collect()
    }

    fn update_stage(
        &self,
        id: JobId,
        stage: JobStage,
        error_message: Option<String>,
    ) -> Result<(), RuntimeError> {
        self.connection.execute(
            r#"
            UPDATE dictation_jobs
            SET state = ?1, stage = ?2, updated_at = ?3, error_message = ?4
            WHERE runtime_id = ?5
            "#,
            params![
                state_for_stage(stage),
                stage_name(stage),
                timestamp(Utc::now()),
                error_message,
                id.to_string(),
            ],
        )?;
        Ok(())
    }
}

/// Configures every connection that writes. WAL lets readers proceed while
/// another connection writes, and `synchronous = NORMAL` drops the per-commit
/// fsync: a process crash loses nothing, and a power cut can only lose the
/// most recent commits, in order. Transactions begin IMMEDIATE so one that
/// reads before it writes waits for a concurrent writer instead of failing
/// with "database is locked".
fn configure_writer(connection: &mut Connection) -> rusqlite::Result<()> {
    connection.execute_batch(
        "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;",
    )?;
    connection.set_transaction_behavior(TransactionBehavior::Immediate);
    Ok(())
}

/// Reads one job row. Takes a connection so a transaction can use it too.
pub(crate) fn load_job(
    connection: &Connection,
    id: JobId,
) -> Result<Option<RecordingJob>, RuntimeError> {
    connection
        .query_row(
            r#"
            SELECT runtime_id, started_at, updated_at, stage, audio_path,
                   duration_seconds, transcription_model, raw_transcript,
                   final_text, copied_to_clipboard, paste_triggered,
                   delivery_status, error_message, processing_options
            FROM dictation_jobs
            WHERE runtime_id = ?1
            "#,
            [id.to_string()],
            row_to_job,
        )
        .optional()?
        .map_or(Ok(None), |job| job.map(Some))
}

/// Columns added after the first Rust release, for databases created
/// before them.
const ADDED_COLUMNS: &[(&str, &str, &str)] = &[
    ("dictation_jobs", "runtime_id", "TEXT"),
    (
        "dictation_jobs",
        "delivery_status",
        "TEXT NOT NULL DEFAULT 'not_attempted'",
    ),
    ("dictation_jobs", "processing_options", "TEXT"),
    (
        "dictation_jobs",
        "transcription_provider",
        "TEXT NOT NULL DEFAULT 'openai_api'",
    ),
    ("dictation_jobs", "cleaned_transcript", "TEXT"),
    (
        "dictation_jobs",
        "replacements_applied",
        "TEXT NOT NULL DEFAULT '[]'",
    ),
    ("dictation_jobs", "cleanup_error", "TEXT"),
    (
        "dictation_sessions",
        "transcription_provider",
        "TEXT NOT NULL DEFAULT 'openai_api'",
    ),
    ("dictation_sessions", "runtime_job_id", "TEXT"),
];

/// Unique lookups the added columns need. `ALTER TABLE` cannot add a
/// `UNIQUE` column, so older databases get these indexes instead.
const INDEXES: &str = r#"
CREATE UNIQUE INDEX IF NOT EXISTS idx_dictation_jobs_runtime_id ON dictation_jobs(runtime_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_runtime_job_id ON dictation_sessions(runtime_job_id);
"#;

fn add_missing_columns(connection: &Connection) -> rusqlite::Result<()> {
    for (table, column, declaration) in ADDED_COLUMNS {
        let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
        let exists = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|existing| existing == column);
        if !exists {
            connection.execute(
                &format!("ALTER TABLE {table} ADD COLUMN {column} {declaration}"),
                [],
            )?;
        }
    }
    Ok(())
}

fn remove_blank_replacements(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute(
        "DELETE FROM replacement_mappings WHERE trim(source_phrase) = ''",
        [],
    )?;
    Ok(())
}

/// Until 2026-08-21 a completed paste was stored as `committed`.
fn rename_committed_deliveries(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute(
        "UPDATE dictation_jobs SET delivery_status = 'submitted' WHERE delivery_status = 'committed'",
        [],
    )?;
    Ok(())
}

fn reconcile_ambiguous_deliveries(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute(
        r#"
        UPDATE dictation_jobs
        SET state = 'failed', stage = 'failed', delivery_status = 'ambiguous',
            updated_at = ?1,
            error_message = 'delivery was interrupted after the attempt began'
        WHERE delivery_status = 'attempting'
           OR (
               state = 'delivering'
               AND stage = 'delivering'
               AND delivery_status = 'not_attempted'
           )
        "#,
        [timestamp(Utc::now())],
    )?;
    Ok(())
}

fn reconcile_interrupted_jobs(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute(
        r#"
        UPDATE dictation_jobs
        SET state = 'interrupted', stage = 'interrupted', updated_at = ?1,
            error_message = COALESCE(
                error_message,
                'AgentDictate stopped before this dictation completed'
            )
        WHERE stage IN ('starting', 'recording', 'transcribing', 'cleaning')
        "#,
        [timestamp(Utc::now())],
    )?;
    Ok(())
}
