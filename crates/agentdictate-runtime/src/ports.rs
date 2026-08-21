use std::path::PathBuf;

use agentdictate_core::{JobId, JobStage};
use chrono::{DateTime, Utc};
use thiserror::Error;

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
