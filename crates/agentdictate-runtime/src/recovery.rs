use agentdictate_core::{JobStage, RecoverySnapshot};

use crate::retention::RECOVERY_LIFETIME;
use crate::{DeliveryStatus, Runtime, RuntimeError};

impl Runtime {
    /// The dictations the user can retry or delete in Recovery, newest
    /// first, each with when it expires. Jobs still recording or processing
    /// are not listed.
    pub fn recoveries(&self) -> Result<Vec<RecoverySnapshot>, RuntimeError> {
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
                )
            })
            .map(|job| RecoverySnapshot {
                job_id: job.id,
                stage: job.stage,
                updated_at: job.updated_at,
                expires_at: job.updated_at + RECOVERY_LIFETIME,
                duration_seconds: job.duration_seconds,
                raw_transcript: job.raw_transcript,
                final_text: job.final_text,
                error_message: job.error_message,
                audio_present: job.audio_path.is_file(),
                delivery_ambiguous: job.delivery_status == DeliveryStatus::Ambiguous,
            })
            .collect())
    }
}
