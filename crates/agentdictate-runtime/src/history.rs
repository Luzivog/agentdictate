use agentdictate_core::{AppliedReplacement, JobId, JobStage, Settings, estimate_session_cost};
use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::{OptionalExtension, params};
use std::sync::OnceLock;

use crate::{Runtime, RuntimeError, parse_timestamp, timestamp};

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct HistoryEntry {
    pub id: i64,
    pub session_id: i64,
    pub job_id: Option<JobId>,
    pub created_at: DateTime<Utc>,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub duration_seconds: f64,
    pub transcription_model: String,
    pub cleanup_enabled: bool,
    pub cleanup_model: Option<String>,
    pub cleanup_style: Option<String>,
    pub raw_transcript: String,
    pub cleaned_transcript: Option<String>,
    pub final_text: String,
    pub replacements_applied: Vec<AppliedReplacement>,
    pub copied_to_clipboard: bool,
    pub paste_triggered: bool,
    pub raw_word_count: u64,
    pub final_word_count: u64,
    pub final_character_count: u64,
    pub estimated_transcription_cost: f64,
    pub estimated_cleanup_cost: f64,
    pub estimated_total_cost: f64,
    pub success: bool,
    pub error_message: Option<String>,
    pub cleanup_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct HistoryQuery {
    pub search: String,
    pub day: Option<NaiveDate>,
    pub limit: usize,
    pub after: Option<HistoryCursor>,
}

/// Storage-side continuation state. Decoded from the protocol's opaque
/// `HistoryPageCursor` at the IPC boundary so persistence never trusts
/// client-constructed pagination state.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct HistoryCursor(String);

impl HistoryCursor {
    #[must_use]
    pub fn from_opaque(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_opaque(self) -> String {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct HistoryMatch {
    pub entry: HistoryEntry,
    pub preview: String,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct HistoryPage {
    pub matches: Vec<HistoryMatch>,
    pub total_matches: u64,
    pub next_cursor: Option<HistoryCursor>,
}

impl Default for HistoryQuery {
    fn default() -> Self {
        Self {
            search: String::new(),
            day: None,
            limit: 250,
            after: None,
        }
    }
}

impl HistoryQuery {
    /// Returns every matching row. The bounded default remains appropriate for
    /// ordinary search/list callers; workspace bootstrap uses this explicit
    /// query so older saved transcripts never become unreachable.
    #[must_use]
    pub fn all() -> Self {
        Self {
            limit: usize::MAX,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn explicit_all_history_query_does_not_silently_stop_at_default_limit() {
        let directory = tempdir().unwrap();
        let mut runtime = Runtime::open(directory.path().join("history.sqlite")).unwrap();
        runtime.ensure_history_search_index().unwrap();
        let transaction = runtime.connection.transaction().unwrap();
        for index in 0..251 {
            transaction
                .execute(
                    r#"
                    INSERT INTO dictation_sessions (
                        started_at, ended_at, duration_seconds, transcription_model,
                        raw_word_count, final_word_count, final_character_count
                    ) VALUES (?1, ?1, 1, 'test-model', 1, 1, 8)
                    "#,
                    [format!(
                        "2026-08-18T12:{:02}:{:02}Z",
                        index / 60,
                        index % 60
                    )],
                )
                .unwrap();
            let session_id = transaction.last_insert_rowid();
            transaction
                .execute(
                    r#"
                    INSERT INTO transcript_history (
                        session_id, created_at, raw_transcript, final_text
                    ) VALUES (?1, ?2, 'complete body', ?3)
                    "#,
                    params![
                        session_id,
                        format!("2026-08-18T12:{:02}:{:02}Z", index / 60, index % 60),
                        format!("entry {index}"),
                    ],
                )
                .unwrap();
        }
        transaction.commit().unwrap();

        assert_eq!(
            runtime.list_history(HistoryQuery::default()).unwrap().len(),
            250
        );
        assert_eq!(
            runtime.list_history(HistoryQuery::all()).unwrap().len(),
            251
        );
        let page = runtime
            .history_page(HistoryQuery {
                limit: usize::MAX,
                ..HistoryQuery::default()
            })
            .unwrap();
        assert_eq!(page.matches.len(), 100);
        assert_eq!(page.total_matches, 251);
        assert!(page.next_cursor.is_some());
    }
}

impl Runtime {
    pub fn backfill_delivered_sessions(
        &mut self,
        settings: &Settings,
    ) -> Result<usize, RuntimeError> {
        if !settings.save_history {
            return Ok(0);
        }
        let mut statement = self.connection.prepare(
            "SELECT runtime_id FROM dictation_jobs WHERE stage = 'delivered' ORDER BY id ASC",
        )?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        let mut inserted = 0;
        for value in ids {
            let id = value
                .parse::<JobId>()
                .map_err(|_| RuntimeError::InvalidJobId(value.clone()))?;
            let existed = self.history_for_job(id)?.is_some();
            if self.record_delivered_session(id, settings)?.is_some() && !existed {
                inserted += 1;
            }
        }
        Ok(inserted)
    }

    /// Adds one Python-compatible history row for a committed delivery.
    /// The persisted job id is unique, making repeated calls crash-safe.
    pub fn record_delivered_session(
        &mut self,
        job_id: JobId,
        settings: &Settings,
    ) -> Result<Option<HistoryEntry>, RuntimeError> {
        if !settings.save_history {
            return Ok(None);
        }
        if let Some(existing) = self.history_for_job(job_id)? {
            return Ok(Some(existing));
        }
        let job = self.job(job_id)?.ok_or(RuntimeError::JobNotFound(job_id))?;
        if job.stage != JobStage::Delivered {
            return Err(RuntimeError::InvalidStage {
                job_id,
                expected: JobStage::Delivered,
                actual: job.stage,
            });
        }
        let (stored_cleaned, replacements_json, cleanup_error): (
            Option<String>,
            String,
            Option<String>,
        ) = self.connection.query_row(
            r#"
                SELECT cleaned_transcript, replacements_applied, cleanup_error
                FROM dictation_jobs
                WHERE runtime_id = ?1
                "#,
            [job_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let replacements_applied = deserialize_replacements(&replacements_json)?;
        let cleanup_enabled = settings.cleanup_enabled && stored_cleaned.is_some();
        let cleaned_transcript = cleanup_enabled.then_some(stored_cleaned).flatten();
        let cleanup_model = cleanup_enabled
            .then(|| settings.active_cleanup_model().to_owned())
            .filter(|model| !model.is_empty());
        let cleanup_style = cleanup_enabled.then(|| settings.cleanup_style.clone());
        let transcription_price = settings
            .transcription_prices
            .get(&job.transcription_model)
            .map_or(0.0, |price| price.price_per_audio_minute);
        let cleanup_price = cleanup_model
            .as_ref()
            .and_then(|model| settings.cleanup_prices.get(model));
        let cost = estimate_session_cost(
            job.duration_seconds,
            &job.raw_transcript,
            cleaned_transcript.as_deref(),
            cleanup_enabled,
            transcription_price,
            cleanup_price.map_or(0.0, |price| price.input_price_per_1m_tokens),
            cleanup_price.map_or(0.0, |price| price.output_price_per_1m_tokens),
        );
        let day = job.started_at.date_naive();
        let replacements_json = serialize_replacements(&replacements_applied)?;
        let transaction = self.connection.transaction()?;
        transaction.execute(
            r#"
            INSERT INTO dictation_sessions (
                started_at, ended_at, duration_seconds, transcription_model,
                cleanup_enabled, cleanup_model, cleanup_style, raw_word_count,
                final_word_count, final_character_count,
                estimated_transcription_cost, estimated_cleanup_cost,
                estimated_total_cost, success, error_message, runtime_job_id
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                1, NULL, ?14
            )
            "#,
            params![
                timestamp(job.started_at),
                timestamp(job.updated_at),
                job.duration_seconds,
                job.transcription_model,
                cleanup_enabled,
                cleanup_model,
                cleanup_style,
                word_count(&job.raw_transcript),
                word_count(&job.final_text),
                job.final_text.chars().count() as u64,
                cost.transcription_cost,
                cost.cleanup_cost,
                cost.total_cost,
                job_id.to_string(),
            ],
        )?;
        let session_id = transaction.last_insert_rowid();
        transaction.execute(
            r#"
            INSERT INTO transcript_history (
                session_id, created_at, raw_transcript, cleaned_transcript,
                final_text, replacements_applied, copied_to_clipboard,
                paste_triggered, cleanup_error
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            params![
                session_id,
                timestamp(job.updated_at),
                job.raw_transcript,
                cleaned_transcript,
                job.final_text,
                replacements_json,
                job.copied_to_clipboard,
                job.paste_triggered,
                cleanup_error,
            ],
        )?;
        recompute_daily_stats(&transaction, day)?;
        transaction.commit()?;
        self.history_search_cache.borrow_mut().invalidate();
        self.history_for_job(job_id)
    }

    pub fn list_history(&self, query: HistoryQuery) -> Result<Vec<HistoryEntry>, RuntimeError> {
        self.query_history_rows(&query, query.limit)
    }

    pub fn history_page(&self, query: HistoryQuery) -> Result<HistoryPage, RuntimeError> {
        crate::history_search::history_page(&self.connection, &self.history_search_cache, query)
    }

    fn query_history_rows(
        &self,
        query: &HistoryQuery,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>, RuntimeError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let (term, day) = history_query_parameters(query);
        let mut statement = self.connection.prepare(&format!(
            "{}\n             WHERE (?1 = '' OR h.raw_transcript LIKE ?1 ESCAPE '\\' OR h.cleaned_transcript LIKE ?1 ESCAPE '\\' OR h.final_text LIKE ?1 ESCAPE '\\')\n               AND (?2 = '' OR substr(h.created_at, 1, 10) = ?2)\n             ORDER BY h.created_at DESC, h.id DESC\n             LIMIT ?3",
            history_select()
        ))?;
        let rows = statement
            .query_map(
                params![term, day, i64::try_from(limit).unwrap_or(i64::MAX)],
                row_to_history,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().collect()
    }

    pub fn history(&self, id: i64) -> Result<Option<HistoryEntry>, RuntimeError> {
        self.query_one_history("h.id = ?1", id)
    }

    pub fn delete_history(&mut self, id: i64) -> Result<bool, RuntimeError> {
        let row: Option<(i64, String)> = self
            .connection
            .query_row(
                r#"
                SELECT h.session_id, s.started_at
                FROM transcript_history h
                JOIN dictation_sessions s ON s.id = h.session_id
                WHERE h.id = ?1
                "#,
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((session_id, started_at)) = row else {
            return Ok(false);
        };
        let day = parse_timestamp(&started_at)?.date_naive();
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM dictation_sessions WHERE id = ?1", [session_id])?;
        recompute_daily_stats(&transaction, day)?;
        transaction.commit()?;
        self.history_search_cache.borrow_mut().invalidate();
        Ok(true)
    }

    pub fn clear_history(&mut self) -> Result<(), RuntimeError> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM transcript_history", [])?;
        transaction.execute("DELETE FROM dictation_sessions", [])?;
        transaction.execute("DELETE FROM daily_stats", [])?;
        transaction.commit()?;
        self.history_search_cache.borrow_mut().invalidate();
        Ok(())
    }

    fn history_for_job(&self, id: JobId) -> Result<Option<HistoryEntry>, RuntimeError> {
        self.query_one_history("s.runtime_job_id = ?1", id.to_string())
    }

    fn query_one_history(
        &self,
        predicate: &str,
        value: impl rusqlite::ToSql,
    ) -> Result<Option<HistoryEntry>, RuntimeError> {
        self.connection
            .query_row(
                &format!("{} WHERE {predicate}", history_select()),
                [value],
                row_to_history,
            )
            .optional()?
            .map_or(Ok(None), |entry| entry.map(Some))
    }
}

fn history_query_parameters(query: &HistoryQuery) -> (String, String) {
    let day = query.day.map_or_else(String::new, |day| day.to_string());
    let search = query.search.trim();
    let term = if search.is_empty() {
        String::new()
    } else {
        let escaped = search
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        format!("%{escaped}%")
    };
    (term, day)
}

pub(crate) fn history_select() -> &'static str {
    r#"
    SELECT
        h.id, h.session_id, s.runtime_job_id, h.created_at,
        s.started_at, s.ended_at, s.duration_seconds, s.transcription_model,
        s.cleanup_enabled, s.cleanup_model, s.cleanup_style,
        h.raw_transcript, h.cleaned_transcript, h.final_text,
        h.replacements_applied, h.copied_to_clipboard, h.paste_triggered,
        s.raw_word_count, s.final_word_count, s.final_character_count,
        s.estimated_transcription_cost, s.estimated_cleanup_cost,
        s.estimated_total_cost, s.success, s.error_message, h.cleanup_error
    FROM transcript_history h
    JOIN dictation_sessions s ON s.id = h.session_id
    "#
}

pub(crate) fn row_to_history(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<HistoryEntry, RuntimeError>> {
    let job_id: Option<String> = row.get(2)?;
    let created_at: String = row.get(3)?;
    let started_at: String = row.get(4)?;
    let ended_at: String = row.get(5)?;
    let replacements_applied: String = row.get(14)?;
    Ok((|| {
        Ok(HistoryEntry {
            id: row.get(0)?,
            session_id: row.get(1)?,
            job_id: job_id
                .map(|id| id.parse().map_err(|_| RuntimeError::InvalidJobId(id)))
                .transpose()?,
            created_at: parse_timestamp(&created_at)?,
            started_at: parse_timestamp(&started_at)?,
            ended_at: parse_timestamp(&ended_at)?,
            duration_seconds: row.get(6)?,
            transcription_model: row.get(7)?,
            cleanup_enabled: row.get(8)?,
            cleanup_model: row.get(9)?,
            cleanup_style: row.get(10)?,
            raw_transcript: row.get(11)?,
            cleaned_transcript: row.get(12)?,
            final_text: row.get(13)?,
            replacements_applied: deserialize_replacements(&replacements_applied)?,
            copied_to_clipboard: row.get(15)?,
            paste_triggered: row.get(16)?,
            raw_word_count: row.get(17)?,
            final_word_count: row.get(18)?,
            final_character_count: row.get(19)?,
            estimated_transcription_cost: row.get(20)?,
            estimated_cleanup_cost: row.get(21)?,
            estimated_total_cost: row.get(22)?,
            success: row.get(23)?,
            error_message: row.get(24)?,
            cleanup_error: row.get(25)?,
        })
    })())
}

pub(super) fn recompute_daily_stats(
    connection: &rusqlite::Connection,
    day: NaiveDate,
) -> Result<(), RuntimeError> {
    let day = day.to_string();
    let aggregate: (u64, u64, f64, f64, f64, f64) = connection.query_row(
        r#"
        SELECT COUNT(*), COALESCE(SUM(final_word_count), 0),
               COALESCE(SUM(duration_seconds), 0),
               COALESCE(SUM(estimated_transcription_cost), 0),
               COALESCE(SUM(estimated_cleanup_cost), 0),
               COALESCE(SUM(estimated_total_cost), 0)
        FROM dictation_sessions
        WHERE substr(started_at, 1, 10) = ?1
        "#,
        [&day],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        },
    )?;
    let average_wpm = if aggregate.2 > 0.0 {
        aggregate.1 as f64 / (aggregate.2 / 60.0)
    } else {
        0.0
    };
    connection.execute(
        r#"
        INSERT INTO daily_stats (
            date, total_sessions, total_words, total_audio_seconds,
            average_wpm, estimated_transcription_cost,
            estimated_cleanup_cost, estimated_total_cost
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        ON CONFLICT(date) DO UPDATE SET
            total_sessions = excluded.total_sessions,
            total_words = excluded.total_words,
            total_audio_seconds = excluded.total_audio_seconds,
            average_wpm = excluded.average_wpm,
            estimated_transcription_cost = excluded.estimated_transcription_cost,
            estimated_cleanup_cost = excluded.estimated_cleanup_cost,
            estimated_total_cost = excluded.estimated_total_cost
        "#,
        params![
            day,
            aggregate.0,
            aggregate.1,
            aggregate.2,
            average_wpm,
            aggregate.3,
            aggregate.4,
            aggregate.5,
        ],
    )?;
    Ok(())
}

fn word_count(text: &str) -> u64 {
    static WORD_EXPRESSION: OnceLock<regex::Regex> = OnceLock::new();
    WORD_EXPRESSION
        .get_or_init(|| {
            regex::Regex::new(r"[A-Za-z0-9_]+(?:[-'][A-Za-z0-9_]+)?")
                .expect("word-count expression is valid")
        })
        .find_iter(text)
        .count() as u64
}

#[derive(serde::Deserialize, serde::Serialize)]
struct StoredReplacement {
    #[serde(alias = "rule_id")]
    id: Option<i64>,
    source_phrase: String,
    replacement_phrase: String,
    count: usize,
}

pub(crate) fn serialize_replacements(
    replacements: &[AppliedReplacement],
) -> Result<String, serde_json::Error> {
    serde_json::to_string(
        &replacements
            .iter()
            .map(|replacement| StoredReplacement {
                id: replacement.rule_id,
                source_phrase: replacement.source_phrase.clone(),
                replacement_phrase: replacement.replacement_phrase.clone(),
                count: replacement.count,
            })
            .collect::<Vec<_>>(),
    )
}

fn deserialize_replacements(value: &str) -> Result<Vec<AppliedReplacement>, serde_json::Error> {
    serde_json::from_str::<Vec<StoredReplacement>>(value).map(|stored| {
        stored
            .into_iter()
            .map(|replacement| AppliedReplacement {
                rule_id: replacement.id.filter(|id| *id != 0),
                source_phrase: replacement.source_phrase,
                replacement_phrase: replacement.replacement_phrase,
                count: replacement.count,
            })
            .collect()
    })
}
