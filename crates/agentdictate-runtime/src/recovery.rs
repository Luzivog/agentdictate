use std::path::PathBuf;

use agentdictate_core::{JobId, JobStage};
use chrono::{DateTime, Utc};

use crate::{DeliveryStatus, Runtime, RuntimeError};

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct RecoveryEntry {
    pub job_id: JobId,
    pub stage: JobStage,
    pub updated_at: DateTime<Utc>,
    pub duration_seconds: f64,
    pub raw_transcript: String,
    pub final_text: String,
    pub error_message: Option<String>,
    pub audio_path: PathBuf,
    pub audio_present: bool,
    pub delivery_status: DeliveryStatus,
}

impl Runtime {
    pub fn recovery_entries(&self) -> Result<Vec<RecoveryEntry>, RuntimeError> {
        Ok(self
            .recoverable_jobs()?
            .into_iter()
            .filter(|job| {
                matches!(
                    job.stage,
                    JobStage::Captured
                        | JobStage::ReadyToDeliver
                        | JobStage::Interrupted
                        | JobStage::Failed
                        | JobStage::Canceled
                )
            })
            .map(|job| RecoveryEntry {
                job_id: job.id,
                stage: job.stage,
                updated_at: job.updated_at,
                duration_seconds: job.duration_seconds,
                raw_transcript: job.raw_transcript,
                final_text: job.final_text,
                error_message: job.error_message,
                audio_present: job.audio_path.is_file(),
                audio_path: job.audio_path,
                delivery_status: job.delivery_status,
            })
            .collect())
    }
}
