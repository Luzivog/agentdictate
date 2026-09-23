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

/// A saved History row as stored, for what the History page does not show.
#[derive(Debug)]
pub(crate) struct StoredHistory {
    pub(crate) job_id: Option<String>,
    pub(crate) raw_transcript: String,
    pub(crate) final_text: String,
    pub(crate) replacements_applied: serde_json::Value,
    pub(crate) copied_to_clipboard: bool,
    pub(crate) paste_triggered: bool,
    pub(crate) raw_word_count: u64,
    pub(crate) final_word_count: u64,
    pub(crate) final_character_count: u64,
    pub(crate) estimated_total_cost: f64,
}

/// Reads every History row with its usage session, newest first.
pub(crate) fn stored_history(database: &Path) -> Vec<StoredHistory> {
    let connection = rusqlite::Connection::open(database).unwrap();
    let mut statement = connection
        .prepare(
            r#"
            SELECT s.runtime_job_id, h.raw_transcript, h.final_text,
                   h.replacements_applied, h.copied_to_clipboard, h.paste_triggered,
                   s.raw_word_count, s.final_word_count, s.final_character_count,
                   s.estimated_total_cost
            FROM transcript_history h
            JOIN dictation_sessions s ON s.id = h.session_id
            ORDER BY h.created_at DESC, h.id DESC
            "#,
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok(StoredHistory {
                job_id: row.get(0)?,
                raw_transcript: row.get(1)?,
                final_text: row.get(2)?,
                replacements_applied: serde_json::from_str(&row.get::<_, String>(3)?).unwrap(),
                copied_to_clipboard: row.get(4)?,
                paste_triggered: row.get(5)?,
                raw_word_count: row.get(6)?,
                final_word_count: row.get(7)?,
                final_character_count: row.get(8)?,
                estimated_total_cost: row.get(9)?,
            })
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}
