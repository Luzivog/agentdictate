use std::path::PathBuf;
use std::str::FromStr;

use chrono::{DateTime, SecondsFormat, Utc};

use crate::{DeliveryStatus, JobId, JobStage, RecordingJob, RuntimeError};

pub(crate) fn row_to_job(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<RecordingJob, RuntimeError>> {
    let runtime_id: String = row.get(0)?;
    let started_at: String = row.get(1)?;
    let updated_at: String = row.get(2)?;
    let stage: String = row.get(3)?;
    Ok((|| {
        Ok(RecordingJob {
            options: row
                .get::<_, Option<String>>(13)?
                .map(|s| serde_json::from_str(&s))
                .transpose()?,
            id: JobId::from_str(&runtime_id)
                .map_err(|_| RuntimeError::InvalidJobId(runtime_id.clone()))?,
            started_at: parse_timestamp(&started_at)?,
            updated_at: parse_timestamp(&updated_at)?,
            stage: parse_stage(&stage)?,
            audio_path: PathBuf::from(row.get::<_, String>(4)?),
            duration_seconds: row.get(5)?,
            transcription_model: row.get(6)?,
            raw_transcript: row.get(7)?,
            final_text: row.get(8)?,
            copied_to_clipboard: row.get(9)?,
            paste_triggered: row.get(10)?,
            delivery_status: parse_delivery_status(&row.get::<_, String>(11)?)?,
            error_message: row.get(12)?,
        })
    })())
}

fn parse_delivery_status(value: &str) -> Result<DeliveryStatus, RuntimeError> {
    match value {
        "not_attempted" => Ok(DeliveryStatus::NotAttempted),
        "attempting" => Ok(DeliveryStatus::Attempting),
        "submitted" => Ok(DeliveryStatus::Submitted),
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

pub(crate) fn stage_name(stage: JobStage) -> &'static str {
    match stage {
        JobStage::Starting => "starting",
        JobStage::Recording => "recording",
        JobStage::Captured => "captured",
        JobStage::Transcribing => "transcribing",
        JobStage::ReadyToDeliver => "ready_to_deliver",
        JobStage::Delivered => "delivered",
        JobStage::NoSpeech => "no_speech",
        JobStage::Interrupted => "interrupted",
        JobStage::Failed => "failed",
        JobStage::Deleted => "deleted",
    }
}

fn parse_stage(value: &str) -> Result<JobStage, RuntimeError> {
    match value {
        "starting" => Ok(JobStage::Starting),
        "recording" => Ok(JobStage::Recording),
        "captured" => Ok(JobStage::Captured),
        "transcribing" => Ok(JobStage::Transcribing),
        "ready_to_deliver" => Ok(JobStage::ReadyToDeliver),
        "delivered" => Ok(JobStage::Delivered),
        "no_speech" => Ok(JobStage::NoSpeech),
        "interrupted" => Ok(JobStage::Interrupted),
        "failed" => Ok(JobStage::Failed),
        "deleted" => Ok(JobStage::Deleted),
        other => Err(RuntimeError::InvalidJobId(format!(
            "unknown stage {other:?}"
        ))),
    }
}

pub(crate) fn state_for_stage(stage: JobStage) -> &'static str {
    match stage {
        JobStage::Delivered => "delivered",
        JobStage::NoSpeech => "no_speech",
        JobStage::Deleted => "deleted",
        JobStage::Interrupted => "interrupted",
        JobStage::Failed => "failed",
        JobStage::Starting | JobStage::Recording => "active",
        JobStage::Captured | JobStage::Transcribing | JobStage::ReadyToDeliver => "captured",
    }
}
