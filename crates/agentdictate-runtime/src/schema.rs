use std::path::PathBuf;
use std::str::FromStr;

use chrono::{DateTime, SecondsFormat, Utc};

use crate::{DeliveryStatus, JobId, JobStage, RecordingJob, RuntimeError};

pub(crate) const SCHEMA: &str = r#"
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

pub(crate) fn row_to_job(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<RecordingJob, RuntimeError>> {
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

pub(crate) fn stage_name(stage: JobStage) -> &'static str {
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

pub(crate) fn state_for_stage(stage: JobStage) -> &'static str {
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
