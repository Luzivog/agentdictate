//! Durable dictation orchestration, persistence, networking, and IPC.

use std::cell::RefCell;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::mpsc::{Receiver, Sender, channel};

use agentdictate_core::apply_replacements;
pub use agentdictate_core::{
    AppSnapshot, ClientCommand, ClientCommandKind, HotkeyReadiness, JobId, JobStage,
    ReplacementRule, ServerMessage, ServerMessageKind, Settings, Workflow, WorkflowPhase,
    WorkflowSignal,
};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use thiserror::Error;

mod ipc;
pub use ipc::{IpcClient, IpcError, IpcHandler, IpcServer};
mod history;
pub use history::{HistoryCursor, HistoryEntry, HistoryMatch, HistoryPage, HistoryQuery};
mod history_search;
mod maintenance_priority;
pub use maintenance_priority::{HistoryIndexMaintenance, RecordingPriorityGuard};
mod pricing;
mod recovery;
pub use recovery::RecoveryEntry;
mod usage;
pub use usage::{UsageAggregate, UsageMetric, UsagePoint, UsageSummary, UsageWeek};

const SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS dictation_sessions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at TEXT NOT NULL,
    ended_at TEXT NOT NULL,
    duration_seconds REAL NOT NULL DEFAULT 0,
    transcription_model TEXT NOT NULL,
    cleanup_enabled INTEGER NOT NULL DEFAULT 0,
    cleanup_model TEXT,
    cleanup_style TEXT,
    raw_word_count INTEGER NOT NULL DEFAULT 0,
    final_word_count INTEGER NOT NULL DEFAULT 0,
    final_character_count INTEGER NOT NULL DEFAULT 0,
    estimated_transcription_cost REAL NOT NULL DEFAULT 0,
    estimated_cleanup_cost REAL NOT NULL DEFAULT 0,
    estimated_total_cost REAL NOT NULL DEFAULT 0,
    success INTEGER NOT NULL DEFAULT 1,
    error_message TEXT,
    runtime_job_id TEXT UNIQUE
);

CREATE TABLE IF NOT EXISTS transcript_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id INTEGER NOT NULL REFERENCES dictation_sessions(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    raw_transcript TEXT NOT NULL DEFAULT '',
    cleaned_transcript TEXT,
    final_text TEXT NOT NULL DEFAULT '',
    replacements_applied TEXT NOT NULL DEFAULT '[]',
    copied_to_clipboard INTEGER NOT NULL DEFAULT 0,
    paste_triggered INTEGER NOT NULL DEFAULT 0,
    cleanup_error TEXT
);

CREATE TABLE IF NOT EXISTS replacement_mappings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_phrase TEXT NOT NULL,
    replacement_phrase TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    case_sensitive INTEGER NOT NULL DEFAULT 0,
    whole_word_only INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS daily_stats (
    date TEXT PRIMARY KEY,
    total_sessions INTEGER NOT NULL DEFAULT 0,
    total_words INTEGER NOT NULL DEFAULT 0,
    total_audio_seconds REAL NOT NULL DEFAULT 0,
    average_wpm REAL NOT NULL DEFAULT 0,
    estimated_transcription_cost REAL NOT NULL DEFAULT 0,
    estimated_cleanup_cost REAL NOT NULL DEFAULT 0,
    estimated_total_cost REAL NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS pricing_settings (
    model_name TEXT NOT NULL,
    model_type TEXT NOT NULL,
    input_price_per_1m_tokens REAL NOT NULL DEFAULT 0,
    output_price_per_1m_tokens REAL NOT NULL DEFAULT 0,
    price_per_audio_minute REAL NOT NULL DEFAULT 0,
    currency TEXT NOT NULL DEFAULT 'USD',
    updated_at TEXT NOT NULL,
    PRIMARY KEY (model_name, model_type)
);

CREATE INDEX IF NOT EXISTS idx_sessions_started_at ON dictation_sessions(started_at);
CREATE INDEX IF NOT EXISTS idx_history_created_at ON transcript_history(created_at);
CREATE INDEX IF NOT EXISTS idx_history_session_id ON transcript_history(session_id);

CREATE TABLE IF NOT EXISTS dictation_jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    state TEXT NOT NULL,
    stage TEXT NOT NULL,
    audio_path TEXT NOT NULL UNIQUE,
    duration_seconds REAL NOT NULL DEFAULT 0,
    transcription_model TEXT NOT NULL DEFAULT '',
    raw_transcript TEXT NOT NULL DEFAULT '',
    final_text TEXT NOT NULL DEFAULT '',
    copied_to_clipboard INTEGER NOT NULL DEFAULT 0,
    paste_triggered INTEGER NOT NULL DEFAULT 0,
    error_message TEXT,
    runtime_id TEXT UNIQUE,
    delivery_status TEXT NOT NULL DEFAULT 'not_attempted',
    cleaned_transcript TEXT,
    replacements_applied TEXT NOT NULL DEFAULT '[]',
    cleanup_error TEXT
);

CREATE INDEX IF NOT EXISTS idx_dictation_jobs_state ON dictation_jobs(state);
CREATE INDEX IF NOT EXISTS idx_dictation_jobs_updated_at ON dictation_jobs(updated_at);
"#;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("database operation failed: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("invalid persisted job id: {0}")]
    InvalidJobId(String),
    #[error("invalid history cursor: {0}")]
    InvalidHistoryCursor(String),
    #[error("dictation job {0} was not found")]
    JobNotFound(JobId),
    #[error("replacement mapping {0} was not found")]
    ReplacementNotFound(i64),
    #[error("replacement mapping id is required for updates")]
    MissingReplacementId,
    #[error("replacement source phrase cannot be blank")]
    InvalidReplacementSource,
    #[error("dictation job {job_id} is {actual:?}, expected {expected:?}")]
    InvalidStage {
        job_id: JobId,
        expected: JobStage,
        actual: JobStage,
    },
    #[error("cannot {operation} dictation job {job_id} while it is {stage:?}: {reason}")]
    OperationNotAllowed {
        operation: &'static str,
        job_id: JobId,
        stage: JobStage,
        reason: &'static str,
    },
    #[error("external operation failed: {0}")]
    External(#[from] ExternalError),
    #[error("settings I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("settings JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("daily usage contains an invalid date {date}: {source}")]
    InvalidUsageDate {
        date: String,
        source: chrono::ParseError,
    },
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("{message}")]
pub struct ExternalError {
    message: String,
}

impl ExternalError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl From<RuntimeError> for ExternalError {
    fn from(error: RuntimeError) -> Self {
        Self::new(error.to_string())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordingJob {
    pub id: JobId,
    /// Stable SQLite identifier used by the legacy Python application.
    pub legacy_id: i64,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub stage: JobStage,
    pub audio_path: PathBuf,
    pub duration_seconds: f64,
    pub transcription_model: String,
    pub raw_transcript: String,
    pub final_text: String,
    pub copied_to_clipboard: bool,
    pub paste_triggered: bool,
    pub delivery_status: DeliveryStatus,
    pub error_message: Option<String>,
    pub cleanup_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordingRequest {
    pub audio_path: PathBuf,
    pub started_at: DateTime<Utc>,
    pub transcription_model: String,
}

pub trait Recorder {
    fn start(&mut self, job: &RecordingJob) -> Result<(), ExternalError>;

    /// Compensates a successful `start` when the following durable Recording
    /// checkpoint cannot be written. Concrete recorders with external state
    /// must stop that state before returning success.
    fn abort_start(&mut self, _job: &RecordingJob) -> Result<(), ExternalError> {
        Err(ExternalError::new(
            "recorder does not support start compensation",
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Transcript {
    pub raw: String,
    pub final_text: String,
    pub cleaned_text: Option<String>,
    pub cleanup_error: Option<String>,
}

pub trait Transcriber {
    fn transcribe(&mut self, job: &RecordingJob) -> Result<Transcript, ExternalError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryDisposition {
    Committed {
        copied_to_clipboard: bool,
        paste_triggered: bool,
    },
    Ambiguous {
        copied_to_clipboard: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    NotAttempted,
    Attempting,
    Committed,
    Ambiguous,
}

pub trait Deliverer {
    fn deliver(&mut self, job: &RecordingJob) -> Result<DeliveryDisposition, ExternalError>;
}

#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeEvent {
    JobUpdated(RecordingJob),
}

pub struct Runtime {
    connection: Connection,
    subscribers: Vec<Sender<RuntimeEvent>>,
    history_search_cache: RefCell<history_search::SearchCache>,
}

pub fn load_settings(path: impl AsRef<Path>) -> Result<Settings, RuntimeError> {
    let path = path.as_ref();
    if !path.exists() {
        let settings = Settings::default();
        save_settings(path, &settings)?;
        return Ok(settings);
    }

    let contents = fs::read_to_string(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    let mut settings: Settings = serde_json::from_str(&contents)?;
    if settings.repair_pricing_defaults() {
        save_settings(path, &settings)?;
    }
    Ok(settings)
}

pub fn save_settings(path: impl AsRef<Path>, settings: &Settings) -> Result<(), RuntimeError> {
    let path = path.as_ref();
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    let mut contents = serde_json::to_string_pretty(settings)?;
    contents.push('\n');
    temporary.write_all(contents.as_bytes())?;
    temporary.as_file_mut().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

impl Runtime {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RuntimeError> {
        let path = path.as_ref();
        let mut connection = Connection::open(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        connection.execute_batch(SCHEMA)?;
        ensure_runtime_id_column(&connection)?;
        ensure_delivery_status_column(&connection)?;
        ensure_history_columns(&connection)?;
        if let Err(error) = history_search::ensure_schema(&mut connection)
            && !history_search::is_search_schema_unavailable(&error)
        {
            return Err(error);
        }
        remove_blank_replacements(&connection)?;
        backfill_runtime_ids(&connection)?;
        reconcile_legacy_python_stages(&connection)?;
        reconcile_ambiguous_deliveries(&connection)?;
        reconcile_interrupted_jobs(&connection)?;
        reconcile_recovery_deletions(&connection)?;
        Ok(Self {
            connection,
            subscribers: Vec::new(),
            history_search_cache: RefCell::new(history_search::SearchCache::default()),
        })
    }

    /// Opens a read-only view without running startup reconciliation.
    pub fn open_observer(path: impl AsRef<Path>) -> Result<Self, RuntimeError> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(Self {
            connection,
            subscribers: Vec::new(),
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

    /// Subscriptions are observers only. Dropping one never changes job lifecycle.
    pub fn subscribe(&mut self) -> Receiver<RuntimeEvent> {
        let (sender, receiver) = channel();
        self.subscribers.push(sender);
        receiver
    }

    pub fn start_recording(
        &mut self,
        request: RecordingRequest,
        recorder: &mut impl Recorder,
    ) -> Result<RecordingJob, RuntimeError> {
        let id = JobId::new();
        let now = timestamp(Utc::now());
        self.connection.execute(
            r#"
            INSERT INTO dictation_jobs (
                runtime_id, started_at, updated_at, state, stage, audio_path,
                transcription_model
            ) VALUES (?1, ?2, ?3, 'active', 'starting', ?4, ?5)
            "#,
            params![
                id.to_string(),
                timestamp(request.started_at),
                now,
                request.audio_path.to_string_lossy(),
                request.transcription_model,
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
        self.publish(RuntimeEvent::JobUpdated(job.clone()));
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
        self.publish(RuntimeEvent::JobUpdated(job.clone()));
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
        self.publish(RuntimeEvent::JobUpdated(interrupted.clone()));
        Ok(interrupted)
    }

    /// Permanently discards a recording after its audio has reached the
    /// durable captured checkpoint. The shared recovery deletion path moves
    /// the audio into quarantine before committing `Deleted`, so a failed
    /// checkpoint never strands a retryable row without its only audio copy.
    pub fn discard_recording(&mut self, id: JobId) -> Result<RecordingJob, RuntimeError> {
        let current = self.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
        if current.stage != JobStage::Captured {
            return Err(RuntimeError::InvalidStage {
                job_id: id,
                expected: JobStage::Captured,
                actual: current.stage,
            });
        }
        self.delete_recovery(id)
    }

    pub fn process_captured(
        &mut self,
        id: JobId,
        transcriber: &mut impl Transcriber,
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
        let transcribing = self.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
        let transcript = match transcriber.transcribe(&transcribing) {
            Ok(transcript) => transcript,
            Err(error) => {
                self.update_stage(id, JobStage::Failed, Some(error.to_string()))?;
                return Err(error.into());
            }
        };
        let replacement_rules = self.replacement_rules()?;
        let replacement_result = apply_replacements(&transcript.final_text, &replacement_rules)
            .map_err(|error| {
                ExternalError::new(format!("replacement processing failed: {error}"))
            })?;
        let replacements_applied = history::serialize_replacements(&replacement_result.applied)?;

        self.connection.execute(
            r#"
            UPDATE dictation_jobs
            SET state = 'captured', stage = 'ready_to_deliver', updated_at = ?1,
                raw_transcript = ?2, cleaned_transcript = ?3, final_text = ?4,
                replacements_applied = ?5, cleanup_error = ?6, error_message = NULL,
                delivery_status = 'not_attempted'
            WHERE runtime_id = ?7
            "#,
            params![
                timestamp(Utc::now()),
                transcript.raw,
                transcript.cleaned_text,
                replacement_result.text,
                replacements_applied,
                transcript.cleanup_error,
                id.to_string(),
            ],
        )?;
        let ready = self.job(id)?.expect("updated job must be readable");
        self.publish(RuntimeEvent::JobUpdated(ready.clone()));

        self.deliver_ready(ready, deliverer)
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
            JobStage::Captured | JobStage::Interrupted | JobStage::Failed | JobStage::Canceled
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
        let captured = self.job(id)?.expect("updated job must be readable");
        self.publish(RuntimeEvent::JobUpdated(captured));
        self.process_captured(id, transcriber, deliverer)
    }

    /// Re-attempts only the delivery step after an explicit user action. This
    /// is intentionally separate from startup recovery: an ambiguous prior
    /// injection is never retried automatically because doing so could paste
    /// duplicate text.
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
                    | JobStage::Cleaning
                    | JobStage::Delivering
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
        self.publish(RuntimeEvent::JobUpdated(ready.clone()));
        self.deliver_ready(ready, deliverer)
    }

    /// Deletes explicit recovery data without exposing a crash window where
    /// the database still offers a retry after the only audio copy is gone.
    pub fn delete_recovery(&mut self, id: JobId) -> Result<RecordingJob, RuntimeError> {
        let current = self.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
        if matches!(
            current.stage,
            JobStage::Starting
                | JobStage::Recording
                | JobStage::Transcribing
                | JobStage::Cleaning
                | JobStage::Delivering
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
        if let Err(error) = self.update_stage(id, JobStage::Deleted, None) {
            if quarantined && !current.audio_path.exists() {
                fs::rename(&quarantine_path, &current.audio_path)?;
            }
            return Err(error);
        }
        let deleted = self.job(id)?.expect("updated job must be readable");
        self.publish(RuntimeEvent::JobUpdated(deleted.clone()));
        if quarantined {
            // The durable row is already deleted. A rare unlink failure leaves
            // a deterministic quarantine file that startup reconciliation can
            // finish, never a falsely retryable job without audio.
            let _ = fs::remove_file(quarantine_path);
        }
        Ok(deleted)
    }

    /// Resumes only deliveries for which no injection attempt was started.
    /// Attempts left in-flight by a crash are reconciled to `Ambiguous` on open.
    pub fn resume_safe_deliveries(
        &mut self,
        deliverer: &mut impl Deliverer,
    ) -> Result<Vec<RecordingJob>, RuntimeError> {
        let mut statement = self.connection.prepare(
            r#"
            SELECT runtime_id
            FROM dictation_jobs
            WHERE stage = 'ready_to_deliver' AND delivery_status = 'not_attempted'
            ORDER BY updated_at ASC, id ASC
            "#,
        )?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        let mut results = Vec::with_capacity(ids.len());
        for value in ids {
            let id =
                JobId::from_str(&value).map_err(|_| RuntimeError::InvalidJobId(value.clone()))?;
            let ready = self.job(id)?.ok_or(RuntimeError::JobNotFound(id))?;
            results.push(self.deliver_ready(ready, deliverer)?);
        }
        Ok(results)
    }

    fn deliver_ready(
        &mut self,
        ready: RecordingJob,
        deliverer: &mut impl Deliverer,
    ) -> Result<RecordingJob, RuntimeError> {
        self.connection.execute(
            r#"
            UPDATE dictation_jobs
            SET delivery_status = 'attempting', updated_at = ?1
            WHERE runtime_id = ?2
            "#,
            params![timestamp(Utc::now()), ready.id.to_string()],
        )?;

        let disposition = match deliverer.deliver(&ready) {
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
            DeliveryDisposition::Committed {
                copied_to_clipboard,
                paste_triggered,
            } => {
                self.connection.execute(
                    r#"
                    UPDATE dictation_jobs
                    SET state = 'delivered', stage = 'delivered', updated_at = ?1,
                        copied_to_clipboard = ?2, paste_triggered = ?3,
                        delivery_status = 'committed', error_message = NULL
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
        }
        let result = self.job(ready.id)?.expect("updated job must be readable");
        self.publish(RuntimeEvent::JobUpdated(result.clone()));
        Ok(result)
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
        self.connection
            .query_row(
                r#"
                SELECT id, runtime_id, started_at, updated_at, stage, audio_path,
                       duration_seconds, transcription_model, raw_transcript,
                       final_text, copied_to_clipboard, paste_triggered,
                       delivery_status, error_message, cleanup_error
                FROM dictation_jobs
                WHERE runtime_id = ?1
                "#,
                [id.to_string()],
                row_to_job,
            )
            .optional()?
            .map_or(Ok(None), |job| job.map(Some))
    }

    pub fn recoverable_jobs(&self) -> Result<Vec<RecordingJob>, RuntimeError> {
        let mut statement = self.connection.prepare(
            r#"
            SELECT id, runtime_id, started_at, updated_at, stage, audio_path,
                   duration_seconds, transcription_model, raw_transcript,
                   final_text, copied_to_clipboard, paste_triggered,
                   delivery_status, error_message, cleanup_error
            FROM dictation_jobs
            WHERE state NOT IN ('delivered', 'deleted')
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

    fn publish(&mut self, event: RuntimeEvent) {
        self.subscribers
            .retain(|subscriber| subscriber.send(event.clone()).is_ok());
    }
}

fn ensure_runtime_id_column(connection: &Connection) -> rusqlite::Result<()> {
    let mut statement = connection.prepare("PRAGMA table_info(dictation_jobs)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|column| column == "runtime_id") {
        connection.execute("ALTER TABLE dictation_jobs ADD COLUMN runtime_id TEXT", [])?;
    }
    connection.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_dictation_jobs_runtime_id ON dictation_jobs(runtime_id)",
        [],
    )?;
    Ok(())
}

fn recovery_delete_path(audio_path: &Path, id: JobId) -> PathBuf {
    audio_path.with_file_name(format!(".agentdictate-delete-{id}.pending"))
}

fn reconcile_recovery_deletions(connection: &Connection) -> Result<(), RuntimeError> {
    let mut statement = connection.prepare(
        "SELECT runtime_id, audio_path, stage FROM dictation_jobs WHERE audio_path != ''",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                PathBuf::from(row.get::<_, String>(1)?),
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for (runtime_id, audio_path, stage) in rows {
        let id = JobId::from_str(&runtime_id)
            .map_err(|_| RuntimeError::InvalidJobId(runtime_id.clone()))?;
        let quarantine_path = recovery_delete_path(&audio_path, id);
        if !quarantine_path.exists() {
            continue;
        }
        if stage == "deleted" || audio_path.exists() {
            match fs::remove_file(&quarantine_path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        } else {
            fs::rename(quarantine_path, audio_path)?;
        }
    }
    Ok(())
}

fn ensure_delivery_status_column(connection: &Connection) -> rusqlite::Result<()> {
    let mut statement = connection.prepare("PRAGMA table_info(dictation_jobs)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|column| column == "delivery_status") {
        connection.execute(
            "ALTER TABLE dictation_jobs ADD COLUMN delivery_status TEXT NOT NULL DEFAULT 'not_attempted'",
            [],
        )?;
    }
    Ok(())
}

fn ensure_history_columns(connection: &Connection) -> rusqlite::Result<()> {
    ensure_column(connection, "dictation_jobs", "cleaned_transcript", "TEXT")?;
    ensure_column(
        connection,
        "dictation_jobs",
        "replacements_applied",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    ensure_column(connection, "dictation_sessions", "runtime_job_id", "TEXT")?;
    ensure_column(connection, "dictation_jobs", "cleanup_error", "TEXT")?;
    connection.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_runtime_job_id ON dictation_sessions(runtime_job_id)",
        [],
    )?;
    Ok(())
}

fn ensure_column(
    connection: &Connection,
    table: &str,
    column: &str,
    declaration: &str,
) -> rusqlite::Result<()> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|existing| existing == column) {
        connection.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {declaration}"),
            [],
        )?;
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

fn backfill_runtime_ids(connection: &Connection) -> rusqlite::Result<()> {
    let mut statement = connection
        .prepare("SELECT id FROM dictation_jobs WHERE runtime_id IS NULL OR runtime_id = ''")?;
    let ids = statement
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for legacy_id in ids {
        connection.execute(
            "UPDATE dictation_jobs SET runtime_id = ?1 WHERE id = ?2",
            params![JobId::new().to_string(), legacy_id],
        )?;
    }
    Ok(())
}

fn reconcile_legacy_python_stages(connection: &Connection) -> rusqlite::Result<()> {
    let now = timestamp(Utc::now());
    connection.execute(
        r#"
        UPDATE dictation_jobs
        SET state = 'captured', stage = 'ready_to_deliver', updated_at = ?1,
            delivery_status = 'not_attempted', error_message = NULL
        WHERE state = 'transcribed'
          AND stage = 'transcribed'
          AND final_text != ''
        "#,
        [&now],
    )?;
    connection.execute(
        r#"
        UPDATE dictation_jobs
        SET state = 'interrupted', stage = 'interrupted', updated_at = ?1,
            error_message = COALESCE(
                error_message,
                'AgentDictate stopped before this dictation completed'
            )
        WHERE stage IN ('transcribed', 'cleanup', 'replacements')
        "#,
        [&now],
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

fn row_to_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<RecordingJob, RuntimeError>> {
    let legacy_id = row.get(0)?;
    let runtime_id: String = row.get(1)?;
    let started_at: String = row.get(2)?;
    let updated_at: String = row.get(3)?;
    let stage: String = row.get(4)?;
    Ok((|| {
        Ok(RecordingJob {
            id: JobId::from_str(&runtime_id)
                .map_err(|_| RuntimeError::InvalidJobId(runtime_id.clone()))?,
            legacy_id,
            started_at: parse_timestamp(&started_at)?,
            updated_at: parse_timestamp(&updated_at)?,
            stage: parse_stage(&stage)?,
            audio_path: PathBuf::from(row.get::<_, String>(5)?),
            duration_seconds: row.get(6)?,
            transcription_model: row.get(7)?,
            raw_transcript: row.get(8)?,
            final_text: row.get(9)?,
            copied_to_clipboard: row.get(10)?,
            paste_triggered: row.get(11)?,
            delivery_status: parse_delivery_status(&row.get::<_, String>(12)?)?,
            error_message: row.get(13)?,
            cleanup_error: row.get(14)?,
        })
    })())
}

fn parse_delivery_status(value: &str) -> Result<DeliveryStatus, RuntimeError> {
    match value {
        "not_attempted" => Ok(DeliveryStatus::NotAttempted),
        "attempting" => Ok(DeliveryStatus::Attempting),
        "committed" => Ok(DeliveryStatus::Committed),
        "ambiguous" => Ok(DeliveryStatus::Ambiguous),
        other => Err(RuntimeError::InvalidJobId(format!(
            "unknown delivery status {other:?}"
        ))),
    }
}

pub(crate) fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, RuntimeError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| RuntimeError::InvalidJobId(format!("invalid timestamp {value:?}")))
}

pub(crate) fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn stage_name(stage: JobStage) -> &'static str {
    match stage {
        JobStage::Starting => "starting",
        JobStage::Recording => "recording",
        JobStage::Captured => "captured",
        JobStage::Transcribing => "transcribing",
        JobStage::Cleaning => "cleaning",
        JobStage::ReadyToDeliver => "ready_to_deliver",
        JobStage::Delivering => "delivering",
        JobStage::Delivered => "delivered",
        JobStage::Interrupted => "interrupted",
        JobStage::Failed => "failed",
        JobStage::Canceled => "canceled",
        JobStage::Deleted => "deleted",
    }
}

fn parse_stage(value: &str) -> Result<JobStage, RuntimeError> {
    match value {
        "starting" => Ok(JobStage::Starting),
        "recording" => Ok(JobStage::Recording),
        "captured" => Ok(JobStage::Captured),
        "transcribing" => Ok(JobStage::Transcribing),
        "cleaning" => Ok(JobStage::Cleaning),
        "ready_to_deliver" => Ok(JobStage::ReadyToDeliver),
        "delivering" => Ok(JobStage::Delivering),
        "delivered" => Ok(JobStage::Delivered),
        "interrupted" => Ok(JobStage::Interrupted),
        "failed" => Ok(JobStage::Failed),
        "canceled" => Ok(JobStage::Canceled),
        "deleted" => Ok(JobStage::Deleted),
        other => Err(RuntimeError::InvalidJobId(format!(
            "unknown stage {other:?}"
        ))),
    }
}

fn state_for_stage(stage: JobStage) -> &'static str {
    match stage {
        JobStage::Delivered => "delivered",
        JobStage::Deleted => "deleted",
        JobStage::Interrupted => "interrupted",
        JobStage::Failed => "failed",
        JobStage::Canceled => "canceled",
        JobStage::Starting | JobStage::Recording => "active",
        _ => "captured",
    }
}
