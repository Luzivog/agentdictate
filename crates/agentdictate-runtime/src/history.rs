use agentdictate_core::{
    HistoryPageCursor, HistoryPageRequest, HistoryPageSnapshot, JobId, JobStage, Settings,
    count_words_ascii_history, transcription_price_per_minute,
};
use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::{OptionalExtension, Transaction, params};

use crate::runtime::load_job;
use crate::{RecordingJob, Runtime, RuntimeError, parse_timestamp, timestamp};

/// One saved transcript, as History search reads it.
pub(crate) struct HistoryRow {
    pub(crate) id: i64,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) final_text: String,
    pub(crate) word_count: u64,
    pub(crate) duration_seconds: f64,
}

/// A History search as the search engine runs it. `day` narrows it to one
/// UTC day; no caller sets it.
pub(crate) struct HistoryQuery {
    pub(crate) search: String,
    pub(crate) day: Option<NaiveDate>,
    pub(crate) limit: usize,
    pub(crate) after: Option<HistoryPageCursor>,
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn history_pages_hold_at_most_one_hundred_rows() {
        let directory = tempdir().unwrap();
        let mut runtime = Runtime::open(directory.path().join("history.sqlite")).unwrap();
        runtime.ensure_history_search_index().unwrap();
        let transaction = runtime.connection.transaction().unwrap();
        for index in 0..101 {
            let at = format!("2026-08-18T12:{:02}:{:02}Z", index / 60, index % 60);
            transaction
                .execute(
                    r#"
                    INSERT INTO dictation_sessions (
                        started_at, ended_at, duration_seconds, transcription_model,
                        final_word_count
                    ) VALUES (?1, ?1, 1, 'test-model', 2)
                    "#,
                    [&at],
                )
                .unwrap();
            transaction
                .execute(
                    "INSERT INTO transcript_history (session_id, created_at, final_text)
                     VALUES (?1, ?2, ?3)",
                    params![
                        transaction.last_insert_rowid(),
                        at,
                        format!("entry {index}")
                    ],
                )
                .unwrap();
        }
        transaction.commit().unwrap();

        let page = runtime
            .history_page(&HistoryPageRequest {
                page_size: usize::MAX,
                ..HistoryPageRequest::default()
            })
            .unwrap();

        assert_eq!(page.rows.len(), 100);
        assert_eq!(page.total_matches, 101);
        assert!(page.next_cursor.is_some());
    }
}

impl Runtime {
    /// Moves a delivered job out of the in-flight job table. One transaction
    /// records its usage session (numbers only, always), saves the transcript
    /// to History when `save_history` is on, and deletes the job row, so the
    /// text survives only where History keeps it. Completing an already
    /// completed job changes nothing.
    pub fn complete_delivered(
        &mut self,
        job_id: JobId,
        settings: &Settings,
    ) -> Result<(), RuntimeError> {
        self.complete_delivered_job(job_id, settings).map(|_| ())
    }

    /// Returns whether this call recorded the job's usage session. It is
    /// false when the job was already completed, or when its session was
    /// recorded before completed job rows were deleted.
    pub(crate) fn complete_delivered_job(
        &mut self,
        job_id: JobId,
        settings: &Settings,
    ) -> Result<bool, RuntimeError> {
        let transaction = self.connection.transaction()?;
        let Some(job) = load_job(&transaction, job_id)? else {
            return Ok(false);
        };
        if job.stage != JobStage::Delivered {
            return Err(RuntimeError::InvalidStage {
                job_id,
                expected: JobStage::Delivered,
                actual: job.stage,
            });
        }
        let already_recorded = transaction
            .query_row(
                "SELECT 1 FROM dictation_sessions WHERE runtime_job_id = ?1",
                [job_id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !already_recorded {
            record_session(&transaction, &job, settings)?;
        }
        transaction.execute(
            "DELETE FROM dictation_jobs WHERE runtime_id = ?1",
            [job_id.to_string()],
        )?;
        transaction.commit()?;
        if !already_recorded {
            self.history_search_cache.borrow_mut().invalidate();
        }
        Ok(!already_recorded)
    }

    /// Returns one page of History, newest first. An expired continuation
    /// cursor restarts the search at its first page and says so.
    pub fn history_page(
        &self,
        request: &HistoryPageRequest,
    ) -> Result<HistoryPageSnapshot, RuntimeError> {
        let query = |after: Option<HistoryPageCursor>| HistoryQuery {
            search: request.search.clone(),
            day: None,
            limit: request.page_size,
            after,
        };
        let search = |query| {
            crate::history_search::history_page(&self.connection, &self.history_search_cache, query)
        };
        match search(query(request.after.clone())) {
            Err(RuntimeError::InvalidHistoryCursor(_)) if request.after.is_some() => {
                Ok(HistoryPageSnapshot {
                    cursor_restarted: true,
                    ..search(query(None))?
                })
            }
            page => page,
        }
    }

    /// The full final text of one History entry, for copying.
    pub fn transcript_text(&self, id: i64) -> Result<Option<String>, RuntimeError> {
        Ok(self
            .connection
            .query_row(
                "SELECT final_text FROM transcript_history WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Deletes one History entry and its usage session.
    pub fn delete_history(&mut self, id: i64) -> Result<bool, RuntimeError> {
        let deleted = self.connection.execute(
            "DELETE FROM dictation_sessions
             WHERE id = (SELECT session_id FROM transcript_history WHERE id = ?1)",
            [id],
        )?;
        if deleted > 0 {
            self.history_search_cache.borrow_mut().invalidate();
        }
        Ok(deleted > 0)
    }

    pub fn clear_history(&mut self) -> Result<(), RuntimeError> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM transcript_history", [])?;
        transaction.execute("DELETE FROM dictation_sessions", [])?;
        transaction.commit()?;
        self.history_search_cache.borrow_mut().invalidate();
        Ok(())
    }
}

/// Inserts the usage session for a delivered job, plus its History row when
/// `save_history` is on. Sessions hold numbers only, never transcript text.
fn record_session(
    transaction: &Transaction<'_>,
    job: &RecordingJob,
    settings: &Settings,
) -> Result<(), RuntimeError> {
    let corrections: String = transaction.query_row(
        "SELECT replacements_applied FROM dictation_jobs WHERE runtime_id = ?1",
        [job.id.to_string()],
        |row| row.get(0),
    )?;
    // Priced once, at today's price: that is what these minutes cost.
    let cost = job.duration_seconds.max(0.0) / 60.0
        * transcription_price_per_minute(&job.transcription_model);
    transaction.execute(
        r#"
        INSERT INTO dictation_sessions (
            started_at, ended_at, duration_seconds, transcription_model,
            raw_word_count, final_word_count, final_character_count,
            estimated_transcription_cost, estimated_total_cost, success,
            error_message, runtime_job_id
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, 1, NULL, ?9)
        "#,
        params![
            timestamp(job.started_at),
            timestamp(job.updated_at),
            job.duration_seconds,
            job.transcription_model,
            count_words_ascii_history(&job.raw_transcript),
            count_words_ascii_history(&job.final_text),
            job.final_text.chars().count() as u64,
            cost,
            job.id.to_string(),
        ],
    )?;
    let session_id = transaction.last_insert_rowid();
    if settings.save_history {
        transaction.execute(
            r#"
            INSERT INTO transcript_history (
                session_id, created_at, raw_transcript, final_text,
                replacements_applied, copied_to_clipboard, paste_triggered
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![
                session_id,
                timestamp(job.updated_at),
                job.raw_transcript,
                job.final_text,
                corrections,
                job.copied_to_clipboard,
                job.paste_triggered,
            ],
        )?;
    }
    Ok(())
}

pub(crate) fn history_select() -> &'static str {
    r#"
    SELECT h.id, h.created_at, h.final_text, s.final_word_count, s.duration_seconds
    FROM transcript_history h
    JOIN dictation_sessions s ON s.id = h.session_id
    "#
}

pub(crate) fn row_to_history(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<HistoryRow, RuntimeError>> {
    let created_at: String = row.get(1)?;
    let id = row.get(0)?;
    let final_text = row.get(2)?;
    let word_count = row.get(3)?;
    let duration_seconds = row.get(4)?;
    Ok(parse_timestamp(&created_at).map(|created_at| HistoryRow {
        id,
        created_at,
        final_text,
        word_count,
        duration_seconds,
    }))
}
