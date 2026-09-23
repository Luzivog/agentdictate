use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use chrono::Utc;
use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior, params};

use crate::migrations::migrate;
use crate::schema::{row_to_job, stage_name, state_for_stage, timestamp};
use crate::startup_cleanup::recovery_delete_path;
use crate::{
    Deliverer, DeliveryDisposition, DeliveryGate, DeliveryMethod, DeliveryStatus, JobId, JobStage,
    Recorder, RecordingJob, RecordingRequest, RuntimeError, StoredTranscript, Transcript,
    TranscriptionOutcome,
};

pub struct Runtime {
    pub(crate) connection: Connection,
}

impl Runtime {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RuntimeError> {
        let path = path.as_ref();
        let mut connection = Connection::open(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        configure_writer(&mut connection)?;
        migrate(&mut connection, path)?;
        reconcile_ambiguous_deliveries(&connection)?;
        reconcile_interrupted_jobs(&connection)?;
        Ok(Self { connection })
    }

    /// Opens the daemon's database like `open`. A file SQLite cannot read as
    /// a database is moved aside to `<file>.corrupt-<unix time>`, with its
    /// `-wal` and `-shm` files, and a fresh database takes its place, so
    /// dictation keeps working. Returns where the unreadable file went.
    pub fn open_or_set_aside(
        path: impl AsRef<Path>,
    ) -> Result<(Self, Option<PathBuf>), RuntimeError> {
        let path = path.as_ref();
        match Self::open(path) {
            Ok(runtime) => Ok((runtime, None)),
            Err(RuntimeError::Database(rusqlite::Error::SqliteFailure(failure, _)))
                if matches!(
                    failure.code,
                    ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
                ) =>
            {
                let set_aside = suffixed(path, &format!(".corrupt-{}", Utc::now().timestamp()));
                for companion in ["", "-wal", "-shm"] {
                    match fs::rename(suffixed(path, companion), suffixed(&set_aside, companion)) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                Ok((Self::open(path)?, Some(set_aside)))
            }
            Err(error) => Err(error),
        }
    }

    /// Opens a read-only view without running startup reconciliation.
    pub fn open_observer(path: impl AsRef<Path>) -> Result<Self, RuntimeError> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(Self { connection })
    }

    /// Opens a write connection for a worker owned by an already-running
    /// daemon. Unlike `open`, this never performs crash reconciliation that
    /// could reinterpret the live daemon's active recording as abandoned.
    pub fn open_background_writer(path: impl AsRef<Path>) -> Result<Self, RuntimeError> {
        let mut connection = Connection::open(path)?;
        configure_writer(&mut connection)?;
        Ok(Self { connection })
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

    /// Moves a captured job to `transcribing`: the last checkpoint before its
    /// audio is transcribed, away from the daemon lock. Only a job that is
    /// durably transcribing can have a transcript stored.
    pub fn begin_transcription(&mut self, id: JobId) -> Result<RecordingJob, RuntimeError> {
        let captured = self.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
        if captured.stage != JobStage::Captured {
            return Err(RuntimeError::InvalidStage {
                job_id: id,
                expected: JobStage::Captured,
                actual: captured.stage,
            });
        }
        self.update_stage(id, JobStage::Transcribing, None)?;
        Ok(self.job(id)?.expect("updated job must be readable"))
    }

    /// Moves a Recovery item back to `transcribing` after an explicit
    /// "Transcribe again". A raw transcript an earlier attempt stored is
    /// kept, so it is not paid for twice. A job whose paste may already have
    /// reached an application is refused.
    pub fn prepare_transcription_retry(&mut self, id: JobId) -> Result<RecordingJob, RuntimeError> {
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
            SET state = 'captured', stage = 'transcribing', updated_at = ?1,
                delivery_status = 'not_attempted', error_message = NULL
            WHERE runtime_id = ?2
            "#,
            params![timestamp(Utc::now()), id.to_string()],
        )?;
        Ok(self.job(id)?.expect("updated job must be readable"))
    }

    /// Records the result of transcribing a job, but only while the job is
    /// still `transcribing`: a late result for a job that was deleted or
    /// reconciled meanwhile changes nothing. Text is checkpointed raw first,
    /// then with the job's own vocabulary applied, as ready to deliver;
    /// `note` becomes the ready job's message. A failed write fails the job,
    /// keeping any raw text, so it never stays in flight.
    pub fn store_transcript(
        &mut self,
        id: JobId,
        outcome: TranscriptionOutcome,
        note: Option<&str>,
    ) -> Result<StoredTranscript, RuntimeError> {
        let Some(transcribing) = self
            .job(id)?
            .filter(|job| job.stage == JobStage::Transcribing)
        else {
            return Ok(StoredTranscript::Stale);
        };
        let stored = match outcome {
            TranscriptionOutcome::Text(transcript) => self
                .store_text(&transcribing, &transcript, note)
                .map(StoredTranscript::Ready),
            TranscriptionOutcome::NoSpeech => {
                // Nothing to deliver or recover, so the job leaves the
                // in-flight table. The caller removes its audio.
                self.connection
                    .execute(
                        "DELETE FROM dictation_jobs WHERE runtime_id = ?1",
                        [id.to_string()],
                    )
                    .map(|_| {
                        StoredTranscript::NoSpeech(RecordingJob {
                            stage: JobStage::NoSpeech,
                            updated_at: Utc::now(),
                            ..transcribing
                        })
                    })
                    .map_err(Into::into)
            }
            TranscriptionOutcome::Failed { message } => self
                .update_stage(id, JobStage::Failed, Some(message))
                .map(|()| {
                    StoredTranscript::Failed(self.job(id).ok().flatten().unwrap_or(transcribing))
                }),
        };
        stored.map_err(|error| self.fail_job(id, error))
    }

    fn store_text(
        &mut self,
        transcribing: &RecordingJob,
        transcript: &Transcript,
        note: Option<&str>,
    ) -> Result<RecordingJob, RuntimeError> {
        let id = transcribing.id;
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
        // Jobs from before options were stored have no vocabulary to apply.
        let vocabulary = transcribing
            .options
            .as_ref()
            .map_or(&[][..], |options| &options.vocabulary[..]);
        let normalized = agentdictate_core::normalize_vocabulary(&transcript.text, vocabulary);
        let corrections = serde_json::to_string(&normalized.corrections)?;
        self.connection.execute(
            r#"
            UPDATE dictation_jobs
            SET state = 'captured', stage = 'ready_to_deliver', updated_at = ?1,
                final_text = ?2, replacements_applied = ?3, error_message = ?4,
                delivery_status = 'not_attempted'
            WHERE runtime_id = ?5
            "#,
            params![
                timestamp(Utc::now()),
                normalized.text,
                corrections,
                note,
                id.to_string(),
            ],
        )?;
        Ok(self.job(id)?.expect("updated job must be readable"))
    }

    /// Resets a stored transcript for an explicit "Paste again", which only
    /// copies it. This is intentionally separate from startup recovery: an
    /// ambiguous prior injection is never retried automatically because
    /// doing so could paste duplicate text.
    pub fn prepare_delivery_retry(&mut self, id: JobId) -> Result<RecordingJob, RuntimeError> {
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
        Ok(self.job(id)?.expect("updated job must be readable"))
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

    /// Delivers a ready transcript once: the gate must confirm first, then
    /// `attempting` is checkpointed before the deliverer runs, so a crash
    /// mid-paste reconciles to ambiguous and is never pasted again. A
    /// failure leaves the job retryable or ambiguous, never in flight.
    pub fn deliver_ready(
        &mut self,
        ready: RecordingJob,
        method: DeliveryMethod,
        delivery_gate: &mut impl DeliveryGate,
        deliverer: &mut impl Deliverer,
    ) -> Result<RecordingJob, RuntimeError> {
        let id = ready.id;
        self.deliver(ready, method, delivery_gate, deliverer)
            .map_err(|error| self.fail_job(id, error))
    }

    fn deliver(
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

/// `path` with `suffix` appended to its file name.
fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}

/// Configures every connection that writes. WAL lets readers proceed while
/// another connection writes, and `synchronous = NORMAL` drops the per-commit
/// fsync: a process crash loses nothing, and a power cut can only lose the
/// most recent commits, in order. `secure_delete` overwrites deleted content
/// with zeros, so deleted and expired text does not linger in free space.
/// Transactions begin IMMEDIATE so one that reads before it writes waits for
/// a concurrent writer instead of failing with "database is locked".
fn configure_writer(connection: &mut Connection) -> rusqlite::Result<()> {
    connection.execute_batch(
        "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA secure_delete = ON;",
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

fn reconcile_ambiguous_deliveries(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute(
        r#"
        UPDATE dictation_jobs
        SET state = 'failed', stage = 'failed', delivery_status = 'ambiguous',
            updated_at = ?1,
            error_message = 'delivery was interrupted after the attempt began'
        WHERE delivery_status = 'attempting'
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
        WHERE stage IN ('starting', 'recording', 'transcribing')
        "#,
        [timestamp(Utc::now())],
    )?;
    Ok(())
}
