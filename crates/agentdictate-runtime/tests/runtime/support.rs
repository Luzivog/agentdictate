use std::path::Path;

use agentdictate_core::{HistoryPageRequest, HistorySnapshot, JobId};
use agentdictate_runtime::{ExternalError, Recorder, RecordingJob, RecordingRequest, Runtime};
use chrono::{TimeZone, Utc};

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
