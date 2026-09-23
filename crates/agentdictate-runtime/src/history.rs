use agentdictate_core::{
    HistoryPageCursor, HistoryPageRequest, HistoryPageSnapshot, HistorySnapshot, JobId, JobStage,
    Settings, count_words_ascii_history, transcription_price_per_minute,
};
use rusqlite::{OptionalExtension, Transaction, params};

use crate::runtime::load_job;
use crate::{RecordingJob, Runtime, RuntimeError, parse_timestamp, timestamp};

/// Rows in one History page at most.
const MAX_PAGE_SIZE: usize = 100;
/// Characters of a transcript that a History row previews.
const PREVIEW_CHARACTERS: usize = 160;

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
        Ok(!already_recorded)
    }

    /// Returns one page of History, newest first: the transcripts whose
    /// final text contains `request.search`, ignoring ASCII case, or every
    /// transcript for a blank search. Pages continue after the opaque cursor
    /// (created_at, id) of the previous page's last row, so rows saved in the
    /// meantime never shift a page. A malformed cursor restarts at the first
    /// page and says so.
    pub fn history_page(
        &self,
        request: &HistoryPageRequest,
    ) -> Result<HistoryPageSnapshot, RuntimeError> {
        let search = request.search.trim();
        let pattern = format!("%{}%", escape_like(search));
        let limit = request.page_size.clamp(1, MAX_PAGE_SIZE);
        let after = request.after.as_ref().map(PageCursor::decode);
        let cursor_restarted = matches!(after, Some(None));
        let after = after.flatten();
        let total_matches = self.connection.query_row(
            r"SELECT COUNT(*) FROM transcript_history WHERE final_text LIKE ?1 ESCAPE '\'",
            [&pattern],
            |row| row.get(0),
        )?;
        let mut statement = self.connection.prepare(
            r"
            SELECT h.id, h.created_at, h.final_text, s.final_word_count, s.duration_seconds
            FROM transcript_history h
            JOIN dictation_sessions s ON s.id = h.session_id
            WHERE h.final_text LIKE ?1 ESCAPE '\'
              AND (?2 IS NULL OR h.created_at < ?2 OR (h.created_at = ?2 AND h.id < ?3))
            ORDER BY h.created_at DESC, h.id DESC
            LIMIT ?4
            ",
        )?;
        let mut rows = statement
            .query_map(
                params![
                    pattern,
                    after.as_ref().map(|cursor| &cursor.created_at),
                    after.as_ref().map_or(0, |cursor| cursor.id),
                    // One extra row tells whether another page follows.
                    i64::try_from(limit + 1).unwrap_or(i64::MAX),
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, u64>(3)?,
                        row.get::<_, f64>(4)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let has_more = rows.len() > limit;
        rows.truncate(limit);
        let next_cursor = rows
            .last()
            .filter(|_| has_more)
            .map(|(id, created_at, ..)| {
                PageCursor {
                    created_at: created_at.clone(),
                    id: *id,
                }
                .encode()
            });
        let rows = rows
            .into_iter()
            .map(
                |(id, created_at, final_text, word_count, duration_seconds)| {
                    Ok(HistorySnapshot {
                        id,
                        created_at: parse_timestamp(&created_at)?,
                        preview_text: preview(&final_text, search),
                        text: final_text,
                        word_count,
                        duration_seconds,
                    })
                },
            )
            .collect::<Result<Vec<_>, RuntimeError>>()?;
        Ok(HistoryPageSnapshot {
            search: request.search.clone(),
            total_matches,
            cursor_restarted,
            next_cursor,
            rows,
        })
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
        Ok(deleted > 0)
    }

    pub fn clear_history(&mut self) -> Result<(), RuntimeError> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM transcript_history", [])?;
        transaction.execute("DELETE FROM dictation_sessions", [])?;
        transaction.commit()?;
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

/// Where a History page ends: the stored `created_at` text, exactly as
/// the keyset comparison orders it, and the row id that breaks ties.
struct PageCursor {
    created_at: String,
    id: i64,
}

impl PageCursor {
    fn encode(&self) -> HistoryPageCursor {
        HistoryPageCursor::new(format!("{}|{}", self.created_at, self.id))
    }

    fn decode(cursor: &HistoryPageCursor) -> Option<Self> {
        let (created_at, id) = cursor.as_str().rsplit_once('|')?;
        Some(Self {
            created_at: created_at.to_owned(),
            id: id.parse().ok()?,
        })
        .filter(|cursor| !cursor.created_at.is_empty())
    }
}

/// Escapes LIKE's wildcards so the user's text matches literally.
fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// The first `PREVIEW_CHARACTERS` of `text`, or for a search, the window
/// that shows its first match.
fn preview(text: &str, search: &str) -> String {
    let characters = text.chars().collect::<Vec<_>>();
    if characters.len() <= PREVIEW_CHARACTERS {
        return text.to_owned();
    }
    let start = match_start(text, search).map_or(0, |index| index.saturating_sub(36));
    let end = (start + PREVIEW_CHARACTERS).min(characters.len());
    let mut value = String::new();
    if start > 0 {
        value.push('…');
    }
    value.extend(&characters[start..end]);
    if end < characters.len() {
        value.push('…');
    }
    value
}

/// The character index where `search` first occurs in `text`, ignoring
/// ASCII case as SQLite's LIKE does. A blank search has no match.
fn match_start(text: &str, search: &str) -> Option<usize> {
    if search.is_empty() {
        return None;
    }
    text.char_indices().position(|(byte, _)| {
        text.as_bytes()[byte..]
            .get(..search.len())
            .is_some_and(|window| window.eq_ignore_ascii_case(search.as_bytes()))
    })
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn history_pages_hold_at_most_one_hundred_rows() {
        let directory = tempdir().unwrap();
        let mut runtime = Runtime::open(directory.path().join("history.sqlite")).unwrap();
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

    #[test]
    fn a_long_transcript_previews_the_window_around_its_first_match() {
        let text = format!("{}The NEEDLE is here.", "x".repeat(300));

        let shown = preview(&text, "needle");

        assert!(shown.starts_with('…'));
        assert!(shown.contains("The NEEDLE is here."));
        assert!(preview(&text, "").starts_with("xxx"));
    }
}
