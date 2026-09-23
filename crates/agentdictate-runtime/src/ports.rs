use std::path::PathBuf;

use agentdictate_core::{FailureKind, JobId, JobStage};
use chrono::{DateTime, Utc};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("database operation failed: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("invalid persisted job id: {0}")]
    InvalidJobId(String),
    #[error("dictation job {0} was not found")]
    JobNotFound(JobId),
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
    #[error("delivery was blocked before the paste attempt: {0}")]
    DeliveryBlocked(#[source] DeliveryGateError),
    #[error("settings I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("settings JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("daily usage contains an invalid date {date}: {source}")]
    InvalidUsageDate {
        date: String,
        source: chrono::ParseError,
    },
    #[error(
        "the database is from a newer AgentDictate (schema version {version}; this one knows {latest})"
    )]
    NewerDatabase { version: i64, latest: usize },
    #[error("{error}; the failure could not be recorded either: {record_error}")]
    FailureNotRecorded {
        error: Box<RuntimeError>,
        record_error: rusqlite::Error,
    },
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ExternalError {
    /// `kind` is what a user is told when this failure ends a dictation.
    #[error("{message}")]
    Failure { kind: FailureKind, message: String },
    #[error("No speech detected.")]
    NoSpeech,
}

impl ExternalError {
    /// A failure with no kind more specific than `Unexpected`.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self::of_kind(FailureKind::Unexpected, message)
    }

    #[must_use]
    pub fn of_kind(kind: FailureKind, message: impl Into<String>) -> Self {
        Self::Failure {
            kind,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> FailureKind {
        match self {
            Self::Failure { kind, .. } => *kind,
            Self::NoSpeech => FailureKind::NoSpeech,
        }
    }
}

/// Why a job stopped short, kept with it for Recovery: `kind` is what the
/// user is told, and `message` the detail for the log.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobFailure {
    pub kind: FailureKind,
    pub message: String,
}

impl JobFailure {
    #[must_use]
    pub fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl From<ExternalError> for JobFailure {
    fn from(error: ExternalError) -> Self {
        Self::new(error.kind(), error.to_string())
    }
}

impl From<RuntimeError> for ExternalError {
    fn from(error: RuntimeError) -> Self {
        Self::new(error.to_string())
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("{message}")]
pub struct DeliveryGateError {
    message: String,
}

impl DeliveryGateError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordingJob {
    pub options: Option<agentdictate_core::DictationOptions>,
    pub id: JobId,
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
    /// Why the job stopped short, when it did. Jobs from before failures
    /// were typed, and notes such as a cancelled job's, have none.
    pub failure: Option<FailureKind>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordingRequest {
    /// Chosen by the caller, so it can show the job as starting while the
    /// recorder is still coming up.
    pub id: JobId,
    pub options: Option<agentdictate_core::DictationOptions>,
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

/// Speech-to-text output for one recording.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Transcript {
    pub text: String,
    /// The model that produced `text`, which a live session can change from
    /// the job's requested model. It is stored with the job.
    pub model: String,
}

/// What one transcription attempt produced. It is built away from the
/// database, and `Runtime::store_transcript` records it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TranscriptionOutcome {
    Text(Transcript),
    /// The audio was quiet: there is nothing to deliver or recover.
    NoSpeech,
    /// The attempt failed; the job keeps its audio for Recovery.
    Failed(JobFailure),
}

/// The job as `Runtime::store_transcript` left it.
#[derive(Clone, Debug, PartialEq)]
pub enum StoredTranscript {
    /// Ready to deliver, with delivery not yet attempted.
    Ready(RecordingJob),
    /// Removed from the in-flight jobs; its audio can go.
    NoSpeech(RecordingJob),
    /// Failed and kept for Recovery.
    Failed(RecordingJob),
    /// The job had already left `transcribing` (it was deleted, or a restart
    /// reconciled it), so nothing was written.
    Stale,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeliveryDisposition {
    Submitted {
        copied_to_clipboard: bool,
        paste_triggered: bool,
        /// Whether an application requested the text right after the paste
        /// chord: the target's acknowledgement that the paste landed.
        /// Always false when no chord was sent.
        consumed: bool,
    },
    /// A paste shortcut was attempted but its outcome is unknown, so the
    /// text may already be in the focused application.
    Ambiguous { copied_to_clipboard: bool },
    /// Delivery failed before any paste shortcut was sent, so nothing
    /// reached the focused application and it is safe to try again.
    NotSent {
        copied_to_clipboard: bool,
        reason: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    NotAttempted,
    Attempting,
    Submitted,
    Ambiguous,
}

/// How a ready transcript reaches the user.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryMethod {
    /// Publish to the clipboard, then send exactly one paste shortcut to the
    /// focused application.
    Paste,
    /// Publish to the clipboard only; the user pastes where they want it.
    CopyOnly,
}

pub trait Deliverer {
    fn deliver(
        &mut self,
        job: &RecordingJob,
        method: DeliveryMethod,
    ) -> Result<DeliveryDisposition, ExternalError>;

    /// The window a paste would reach now, as far as it can be told apart
    /// from another one.
    fn observe_focus(&mut self) -> ObservedFocus {
        ObservedFocus::Unknown
    }

    /// Waits, briefly, for the user to let go of the keys of a shortcut that
    /// asked for a paste, so they cannot mix into the paste chord.
    fn wait_for_released_keys(&mut self) {}
}

/// The focused window, as far as the desktop lets AgentDictate tell one
/// window from another.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservedFocus {
    /// An X11 or XWayland window, by its id.
    X11(u32),
    /// A native Wayland window. Wayland does not say which one.
    Wayland,
    /// The focus could not be read.
    Unknown,
}

impl ObservedFocus {
    /// Whether the focus observably moved from `self` to `now`. Two native
    /// Wayland windows cannot be told apart, and an unknown focus proves
    /// nothing.
    #[must_use]
    pub fn moved_to(self, now: Self) -> bool {
        match (self, now) {
            (Self::Unknown, _) | (_, Self::Unknown) | (Self::Wayland, Self::Wayland) => false,
            (before, now) => before != now,
        }
    }
}

/// Confirms that transient AgentDictate UI cannot receive the upcoming paste.
/// Returning success is the prerequisite for persisting `Attempting` and
/// invoking the delivery adapter.
pub trait DeliveryGate {
    fn confirm_ready(&mut self) -> Result<(), DeliveryGateError>;
}

/// Explicit delivery gate for processes that never launched an overlay.
#[derive(Clone, Copy, Debug, Default)]
pub struct HeadlessDeliveryGate;

impl DeliveryGate for HeadlessDeliveryGate {
    fn confirm_ready(&mut self) -> Result<(), DeliveryGateError> {
        Ok(())
    }
}
