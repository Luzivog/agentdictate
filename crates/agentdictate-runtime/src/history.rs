use agentdictate_core::{
    HistoryPageCursor, HistoryPageRequest, HistoryPageSnapshot, HistorySnapshot, JobId, JobStage,
    KeepTranscripts, Settings, count_words_ascii_history, transcription_price_per_minute,
};
use chrono::Utc;
use rusqlite::{OptionalExtension, Transaction, params};

use crate::runtime::load_job;
use crate::{RecordingJob, Runtime, RuntimeError, parse_timestamp, timestamp};

/// Rows in one History page at most.
const MAX_PAGE_SIZE: usize = 100;
/// Characters of a transcript that a History row previews.
const PREVIEW_CHARACTERS: usize = 160;

impl Runtime {
    /// Moves a delivered job out of the in-flight job table. One transaction
    /// records its dictation, with its usage numbers always and its text
    /// unless `keep_transcripts` is `Never`, and deletes the job row, so the
    /// text survives only where History keeps it. Completing an already
    /// completed job changes nothing. Then, with the job done, it applies the
    /// retention rules: text older than `keep_transcripts` allows and expired
    /// Recovery items are deleted.
    pub fn complete_delivered(
        &mut self,
        job_id: JobId,
        settings: &Settings,
    ) -> Result<(), RuntimeError> {
        self.complete_delivered_job(job_id, settings)?;
        self.apply_retention(settings.keep_transcripts, Utc::now())?;
        Ok(())
    }

    /// Returns whether this call recorded the job's dictation. It is false
    /// when the job was already completed, or when its dictation was recorded
    /// before completed job rows were deleted.
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
                "SELECT 1 FROM dictations WHERE job_id = ?1",
                [job_id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !already_recorded {
            let keep_text = settings.keep_transcripts != KeepTranscripts::Never;
            record_dictation(&transaction, &job, keep_text)?;
        }
        transaction.execute(
            "DELETE FROM dictation_jobs WHERE runtime_id = ?1",
            [job_id.to_string()],
        )?;
        transaction.commit()?;
        Ok(!already_recorded)
    }

    /// Returns one page of History, newest first: the kept transcripts whose
    /// final text contains `request.search`, ignoring ASCII case, or every
    /// kept transcript for a blank search. Pages continue after the opaque
    /// cursor (ended_at, id) of the previous page's last row, so rows saved
    /// in the meantime never shift a page. A malformed cursor restarts at the
    /// first page and says so.
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
            r"SELECT COUNT(*) FROM dictations WHERE final_text LIKE ?1 ESCAPE '\'",
            [&pattern],
            |row| row.get(0),
        )?;
        let mut statement = self.connection.prepare(
            r"
            SELECT id, ended_at, final_text, word_count, duration_seconds
            FROM dictations
            WHERE final_text LIKE ?1 ESCAPE '\'
              AND (?2 IS NULL OR ended_at < ?2 OR (ended_at = ?2 AND id < ?3))
            ORDER BY ended_at DESC, id DESC
            LIMIT ?4
            ",
        )?;
        let mut rows = statement
            .query_map(
                params![
                    pattern,
                    after.as_ref().map(|cursor| &cursor.ended_at),
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
        let next_cursor = rows.last().filter(|_| has_more).map(|(id, ended_at, ..)| {
            PageCursor {
                ended_at: ended_at.clone(),
                id: *id,
            }
            .encode()
        });
        let rows = rows
            .into_iter()
            .map(|(id, ended_at, final_text, word_count, duration_seconds)| {
                Ok(HistorySnapshot {
                    id,
                    created_at: parse_timestamp(&ended_at)?,
                    preview_text: preview(&final_text, search),
                    text: final_text,
                    word_count,
                    duration_seconds,
                })
            })
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
                "SELECT final_text FROM dictations WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?
            .flatten())
    }

    /// Deletes one History entry with its usage numbers, from the disk too.
    pub fn delete_history(&mut self, id: i64) -> Result<bool, RuntimeError> {
        let deleted = self
            .connection
            .execute("DELETE FROM dictations WHERE id = ?1", [id])?;
        self.truncate_write_ahead_log();
        Ok(deleted > 0)
    }

    /// Deletes every dictation, text and usage numbers, from the disk too.
    pub fn clear_history(&mut self) -> Result<(), RuntimeError> {
        self.connection.execute("DELETE FROM dictations", [])?;
        self.truncate_write_ahead_log();
        Ok(())
    }
}

/// Inserts the dictation row of a delivered job: its usage numbers, and its
/// text only when `keep_text`. The raw transcript is stored only when
/// vocabulary changed it, and the corrections only when there were any.
fn record_dictation(
    transaction: &Transaction<'_>,
    job: &RecordingJob,
    keep_text: bool,
) -> Result<(), RuntimeError> {
    let corrections: String = transaction.query_row(
        "SELECT replacements_applied FROM dictation_jobs WHERE runtime_id = ?1",
        [job.id.to_string()],
        |row| row.get(0),
    )?;
    // Priced once, at today's price: that is what these minutes cost.
    let cost = job.duration_seconds.max(0.0) / 60.0
        * transcription_price_per_minute(&job.transcription_model);
    let (final_text, raw_text, corrections) = if keep_text {
        (
            Some(job.final_text.as_str()),
            Some(job.raw_transcript.as_str()).filter(|raw| *raw != job.final_text),
            Some(corrections).filter(|corrections| corrections != "[]"),
        )
    } else {
        (None, None, None)
    };
    transaction.execute(
        r#"
        INSERT INTO dictations (
            job_id, source, started_at, ended_at, duration_seconds,
            transcription_provider, transcription_model, word_count,
            character_count, estimated_cost, final_text, raw_text,
            vocabulary_corrections
        ) VALUES (?1, 'agentdictate', ?2, ?3, ?4, 'openai_api', ?5, ?6, ?7, ?8, ?9, ?10, ?11)
        "#,
        params![
            job.id.to_string(),
            timestamp(job.started_at),
            timestamp(job.updated_at),
            job.duration_seconds,
            job.transcription_model,
            count_words_ascii_history(&job.final_text),
            job.final_text.chars().count() as u64,
            cost,
            final_text,
            raw_text,
            corrections,
        ],
    )?;
    Ok(())
}

/// Where a History page ends: the stored `ended_at` text, exactly as the
/// keyset comparison orders it, and the row id that breaks ties.
struct PageCursor {
    ended_at: String,
    id: i64,
}

impl PageCursor {
    fn encode(&self) -> HistoryPageCursor {
        HistoryPageCursor::new(format!("{}|{}", self.ended_at, self.id))
    }

    fn decode(cursor: &HistoryPageCursor) -> Option<Self> {
        let (ended_at, id) = cursor.as_str().rsplit_once('|')?;
        Some(Self {
            ended_at: ended_at.to_owned(),
            id: id.parse().ok()?,
        })
        .filter(|cursor| !cursor.ended_at.is_empty())
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
                    INSERT INTO dictations (
                        started_at, ended_at, duration_seconds, transcription_provider,
                        transcription_model, word_count, character_count, estimated_cost,
                        final_text
                    ) VALUES (?1, ?1, 1, 'openai_api', 'test-model', 2, 7, 0, ?2)
                    "#,
                    params![at, format!("entry {index}")],
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
