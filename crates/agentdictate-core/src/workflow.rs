use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Stable identifier shared by persisted jobs, runtime events, and UI snapshots.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(Uuid);

impl JobId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for JobId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for JobId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value).map(Self)
    }
}

/// Durable checkpoints written before and after externally visible effects.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStage {
    Starting,
    Recording,
    Captured,
    Transcribing,
    Cleaning,
    ReadyToDeliver,
    Delivering,
    Delivered,
    NoSpeech,
    Interrupted,
    Failed,
    Canceled,
    Deleted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingStage {
    Transcribing,
    Cleaning,
    ReadyToDeliver,
    Delivering,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum WorkflowPhase {
    Ready,
    Starting {
        job_id: JobId,
    },
    Recording {
        job_id: JobId,
    },
    Stopping {
        job_id: JobId,
    },
    Processing {
        job_id: JobId,
        stage: ProcessingStage,
    },
    NeedsAttention {
        job_id: JobId,
        at: JobStage,
    },
}

impl WorkflowPhase {
    const fn job_id(self) -> Option<JobId> {
        match self {
            Self::Ready => None,
            Self::Starting { job_id }
            | Self::Recording { job_id }
            | Self::Stopping { job_id }
            | Self::Processing { job_id, .. }
            | Self::NeedsAttention { job_id, .. } => Some(job_id),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "signal", rename_all = "snake_case")]
pub enum WorkflowSignal {
    StartRequested {
        job_id: JobId,
    },
    FirstAudioFrameWritten {
        job_id: JobId,
    },
    StopRequested,
    /// The explicit user discard reached its durable `Deleted` checkpoint.
    DiscardCommitted {
        job_id: JobId,
    },
    CaptureFinalized {
        job_id: JobId,
    },
    NoSpeechDetected {
        job_id: JobId,
    },
    TranscriptStored {
        job_id: JobId,
    },
    TranscriptStoredForCleanup {
        job_id: JobId,
    },
    CleanupStored {
        job_id: JobId,
    },
    DeliveryStarted {
        job_id: JobId,
    },
    DeliverySubmitted {
        job_id: JobId,
    },
    Interrupted {
        job_id: JobId,
        at: JobStage,
    },
    RetryDeliveryRequested {
        job_id: JobId,
    },
}

impl WorkflowSignal {
    const fn job_id(self) -> Option<JobId> {
        match self {
            Self::StopRequested => None,
            Self::StartRequested { job_id }
            | Self::FirstAudioFrameWritten { job_id }
            | Self::DiscardCommitted { job_id }
            | Self::CaptureFinalized { job_id }
            | Self::NoSpeechDetected { job_id }
            | Self::TranscriptStored { job_id }
            | Self::TranscriptStoredForCleanup { job_id }
            | Self::CleanupStored { job_id }
            | Self::DeliveryStarted { job_id }
            | Self::DeliverySubmitted { job_id }
            | Self::Interrupted { job_id, .. }
            | Self::RetryDeliveryRequested { job_id } => Some(job_id),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowSnapshot {
    pub phase: WorkflowPhase,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum WorkflowError {
    #[error("workflow signal is not valid while {phase:?}")]
    InvalidSignal { phase: WorkflowPhase },
    #[error("workflow signal belongs to job {received}, not active job {expected}")]
    JobMismatch { expected: JobId, received: JobId },
}

/// Validates lifecycle transitions before the runtime performs the next effect.
pub struct Workflow {
    snapshot: WorkflowSnapshot,
}

impl Workflow {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            snapshot: WorkflowSnapshot {
                phase: WorkflowPhase::Ready,
            },
        }
    }

    #[must_use]
    pub const fn snapshot(&self) -> WorkflowSnapshot {
        self.snapshot
    }

    pub fn apply(&mut self, signal: WorkflowSignal) -> Result<WorkflowSnapshot, WorkflowError> {
        let starts_after_recovery = matches!(
            (self.snapshot.phase, signal),
            (
                WorkflowPhase::NeedsAttention { .. },
                WorkflowSignal::StartRequested { .. }
            )
        );
        if let (Some(expected), Some(received)) = (self.snapshot.phase.job_id(), signal.job_id())
            && expected != received
            && !starts_after_recovery
        {
            return Err(WorkflowError::JobMismatch { expected, received });
        }
        let next_phase = match (self.snapshot.phase, signal) {
            (
                WorkflowPhase::Processing {
                    job_id: expected,
                    stage: ProcessingStage::Transcribing,
                },
                WorkflowSignal::NoSpeechDetected { job_id },
            ) if expected == job_id => WorkflowPhase::Ready,

            (WorkflowPhase::Ready, WorkflowSignal::StartRequested { job_id }) => {
                WorkflowPhase::Starting { job_id }
            }
            (WorkflowPhase::NeedsAttention { .. }, WorkflowSignal::StartRequested { job_id }) => {
                WorkflowPhase::Starting { job_id }
            }
            (
                WorkflowPhase::Starting { job_id: expected },
                WorkflowSignal::FirstAudioFrameWritten { job_id },
            ) if expected == job_id => WorkflowPhase::Recording { job_id },
            (
                WorkflowPhase::Recording { job_id } | WorkflowPhase::Starting { job_id },
                WorkflowSignal::StopRequested,
            ) => WorkflowPhase::Stopping { job_id },
            (
                WorkflowPhase::Starting { job_id: expected }
                | WorkflowPhase::Recording { job_id: expected }
                | WorkflowPhase::Stopping { job_id: expected },
                WorkflowSignal::DiscardCommitted { job_id },
            ) if expected == job_id => WorkflowPhase::Ready,
            (
                WorkflowPhase::Stopping { job_id: expected },
                WorkflowSignal::CaptureFinalized { job_id },
            ) if expected == job_id => WorkflowPhase::Processing {
                job_id,
                stage: ProcessingStage::Transcribing,
            },
            (
                WorkflowPhase::Processing {
                    job_id: expected,
                    stage: ProcessingStage::Transcribing,
                },
                WorkflowSignal::TranscriptStored { job_id },
            ) if expected == job_id => WorkflowPhase::Processing {
                job_id,
                stage: ProcessingStage::ReadyToDeliver,
            },
            (
                WorkflowPhase::Processing {
                    job_id: expected,
                    stage: ProcessingStage::Transcribing,
                },
                WorkflowSignal::TranscriptStoredForCleanup { job_id },
            ) if expected == job_id => WorkflowPhase::Processing {
                job_id,
                stage: ProcessingStage::Cleaning,
            },
            (
                WorkflowPhase::Processing {
                    job_id: expected,
                    stage: ProcessingStage::Cleaning,
                },
                WorkflowSignal::CleanupStored { job_id },
            ) if expected == job_id => WorkflowPhase::Processing {
                job_id,
                stage: ProcessingStage::ReadyToDeliver,
            },
            (
                WorkflowPhase::Processing {
                    job_id: expected,
                    stage: ProcessingStage::ReadyToDeliver,
                },
                WorkflowSignal::DeliveryStarted { job_id },
            ) if expected == job_id => WorkflowPhase::Processing {
                job_id,
                stage: ProcessingStage::Delivering,
            },
            (
                WorkflowPhase::Processing {
                    job_id: expected,
                    stage: ProcessingStage::Delivering,
                },
                WorkflowSignal::DeliverySubmitted { job_id },
            ) if expected == job_id => WorkflowPhase::Ready,
            (
                WorkflowPhase::Starting { job_id: expected }
                | WorkflowPhase::Recording { job_id: expected }
                | WorkflowPhase::Stopping { job_id: expected }
                | WorkflowPhase::Processing {
                    job_id: expected, ..
                },
                WorkflowSignal::Interrupted { job_id, at },
            ) if expected == job_id => WorkflowPhase::NeedsAttention { job_id, at },
            (
                WorkflowPhase::NeedsAttention {
                    job_id: expected, ..
                },
                WorkflowSignal::RetryDeliveryRequested { job_id },
            ) if expected == job_id => WorkflowPhase::Processing {
                job_id,
                stage: ProcessingStage::ReadyToDeliver,
            },
            (phase, _) => return Err(WorkflowError::InvalidSignal { phase }),
        };
        self.snapshot.phase = next_phase;
        Ok(self.snapshot)
    }
}

impl Default for Workflow {
    fn default() -> Self {
        Self::new()
    }
}
