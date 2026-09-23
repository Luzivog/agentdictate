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
    transcription_provider TEXT NOT NULL DEFAULT 'openai_api',
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

-- Databases from before 2026-09-23 also hold an `external_dictation_imports`
-- ledger from the retired ChatGPT desktop import, which nothing reads, and the
-- `replacement_mappings` rules of the retired Replacements feature, which
-- `Runtime::retire_replacement_rules` moves into vocabulary once.

-- Caches that usage and pricing no longer keep.
DROP TABLE IF EXISTS daily_stats;
DROP TABLE IF EXISTS pricing_settings;

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
    transcription_provider TEXT NOT NULL DEFAULT 'openai_api',
    raw_transcript TEXT NOT NULL DEFAULT '',
    final_text TEXT NOT NULL DEFAULT '',
    copied_to_clipboard INTEGER NOT NULL DEFAULT 0,
    paste_triggered INTEGER NOT NULL DEFAULT 0,
    error_message TEXT,
    runtime_id TEXT UNIQUE,
    delivery_status TEXT NOT NULL DEFAULT 'not_attempted',
    cleaned_transcript TEXT,
    replacements_applied TEXT NOT NULL DEFAULT '[]',
    cleanup_error TEXT,
    processing_options TEXT
);

CREATE INDEX IF NOT EXISTS idx_dictation_jobs_state ON dictation_jobs(state);
CREATE INDEX IF NOT EXISTS idx_dictation_jobs_updated_at ON dictation_jobs(updated_at);
"#;

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
        // The retired cleanup step was part of processing, like transcribing.
        "transcribing" | "cleaning" => Ok(JobStage::Transcribing),
        "ready_to_deliver" => Ok(JobStage::ReadyToDeliver),
        "delivered" => Ok(JobStage::Delivered),
        "no_speech" => Ok(JobStage::NoSpeech),
        // The Rust app never writes these two stages. Startup reconciles a
        // stored `delivering` row to an ambiguous failure.
        "interrupted" | "canceled" => Ok(JobStage::Interrupted),
        "failed" | "delivering" => Ok(JobStage::Failed),
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
