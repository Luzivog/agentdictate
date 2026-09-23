use std::path::Path;

use agentdictate_core::{HistoryPageRequest, HistorySnapshot, JobId};
use agentdictate_runtime::{ExternalError, Recorder, RecordingJob, RecordingRequest, Runtime};
use chrono::{SecondsFormat, TimeDelta, TimeZone, Utc};
use rusqlite::{Connection, params};

pub(crate) struct ReadyRecorder;

impl Recorder for ReadyRecorder {
    fn start(&mut self, _job: &RecordingJob) -> Result<(), ExternalError> {
        Ok(())
    }
}

pub(crate) fn request(audio_path: &Path, transcription_model: &str) -> RecordingRequest {
    RecordingRequest {
        id: JobId::new(),
        options: None,
        audio_path: audio_path.to_owned(),
        started_at: Utc.with_ymd_and_hms(2026, 8, 18, 12, 0, 0).unwrap(),
        transcription_model: transcription_model.to_owned(),
    }
}

/// A stored timestamp `days` before now.
pub(crate) fn days_ago(days: i64) -> String {
    (Utc::now() - TimeDelta::days(days)).to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Inserts a job row in `state` and `stage`, last changed at `updated_at`.
pub(crate) fn insert_job(
    connection: &Connection,
    audio_path: &Path,
    state: &str,
    stage: &str,
    updated_at: &str,
) -> JobId {
    let id = JobId::new();
    connection
        .execute(
            r#"
            INSERT INTO dictation_jobs (
                started_at, updated_at, state, stage, audio_path,
                duration_seconds, transcription_model, raw_transcript, final_text,
                delivery_status, runtime_id
            ) VALUES (?1, ?1, ?2, ?3, ?4, 12.5, 'gpt-transcribe', 'raw words', 'final words',
                      'not_attempted', ?5)
            "#,
            params![
                updated_at,
                state,
                stage,
                audio_path.to_string_lossy(),
                id.to_string()
            ],
        )
        .unwrap();
    id
}

/// Every History row the History page lists, newest first.
pub(crate) fn history_rows(runtime: &Runtime) -> Vec<HistorySnapshot> {
    runtime
        .history_page(&HistoryPageRequest {
            page_size: 100,
            ..HistoryPageRequest::default()
        })
        .unwrap()
        .rows
}

/// A recorded dictation as stored, for what the History page does not show.
#[derive(Debug)]
pub(crate) struct StoredDictation {
    pub(crate) job_id: Option<String>,
    pub(crate) raw_text: Option<String>,
    pub(crate) final_text: Option<String>,
    pub(crate) vocabulary_corrections: Option<serde_json::Value>,
    pub(crate) word_count: u64,
    pub(crate) character_count: u64,
    pub(crate) estimated_cost: f64,
}

/// Reads every recorded dictation, newest first.
pub(crate) fn stored_dictations(database: &Path) -> Vec<StoredDictation> {
    let connection = rusqlite::Connection::open(database).unwrap();
    let mut statement = connection
        .prepare(
            r#"
            SELECT job_id, raw_text, final_text, vocabulary_corrections,
                   word_count, character_count, estimated_cost
            FROM dictations
            ORDER BY ended_at DESC, id DESC
            "#,
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok(StoredDictation {
                job_id: row.get(0)?,
                raw_text: row.get(1)?,
                final_text: row.get(2)?,
                vocabulary_corrections: row
                    .get::<_, Option<String>>(3)?
                    .map(|json| serde_json::from_str(&json).unwrap()),
                word_count: row.get(4)?,
                character_count: row.get(5)?,
                estimated_cost: row.get(6)?,
            })
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}
