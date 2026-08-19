use std::collections::BTreeMap;

use chrono::{Datelike, Days, NaiveDate, Utc};
use rusqlite::{OptionalExtension, params};

use crate::{Runtime, RuntimeError};

#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct UsageAggregate {
    pub total_sessions: u64,
    pub total_words: u64,
    pub total_audio_seconds: f64,
    pub estimated_transcription_cost: f64,
    pub estimated_cleanup_cost: f64,
    pub estimated_total_cost: f64,
    pub average_wpm: f64,
    pub average_words_per_session: f64,
    pub average_duration_per_session: f64,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct UsageSummary {
    pub all_time: UsageAggregate,
    pub today: UsageAggregate,
    pub week: UsageAggregate,
    pub month: UsageAggregate,
    pub most_used_transcription_model: Option<String>,
    pub most_used_cleanup_model: Option<String>,
    pub cleanup_mode_usage_count: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageMetric {
    Words,
    AudioMinutes,
    Sessions,
    EstimatedCost,
    AverageWpm,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct UsagePoint {
    pub date: NaiveDate,
    pub value: f64,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct UsageWeek {
    pub week_start: NaiveDate,
    pub total_sessions: u64,
    pub total_words: u64,
    pub total_audio_seconds: f64,
    pub estimated_total_cost: f64,
}

impl Runtime {
    pub fn usage_summary(&self) -> Result<UsageSummary, RuntimeError> {
        self.usage_summary_on(Utc::now().date_naive())
    }

    pub fn usage_series(
        &self,
        days: usize,
        metric: UsageMetric,
    ) -> Result<Vec<UsagePoint>, RuntimeError> {
        self.usage_series_ending(days, metric, Utc::now().date_naive())
    }

    /// Returns the complete all-time activity timeline in Monday-based weeks.
    /// Empty weeks between the first activity and today are retained so the
    /// chart never visually compresses inactive time.
    pub fn usage_weekly_series(&self) -> Result<Vec<UsageWeek>, RuntimeError> {
        let mut statement = self.connection.prepare(
            r#"
            SELECT date, total_sessions, total_words, total_audio_seconds,
                   estimated_total_cost
            FROM daily_stats
            ORDER BY date ASC
            "#,
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, u64>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, f64>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut weekly = BTreeMap::<NaiveDate, UsageWeek>::new();
        for (date, sessions, words, audio_seconds, cost) in rows {
            let parsed = NaiveDate::parse_from_str(&date, "%Y-%m-%d").map_err(|source| {
                RuntimeError::InvalidUsageDate {
                    date: date.clone(),
                    source,
                }
            })?;
            let week_start = monday_of(parsed);
            let bucket = weekly.entry(week_start).or_insert(UsageWeek {
                week_start,
                total_sessions: 0,
                total_words: 0,
                total_audio_seconds: 0.0,
                estimated_total_cost: 0.0,
            });
            bucket.total_sessions = bucket.total_sessions.saturating_add(sessions);
            bucket.total_words = bucket.total_words.saturating_add(words);
            bucket.total_audio_seconds += audio_seconds.max(0.0);
            bucket.estimated_total_cost += cost.max(0.0);
        }

        let Some(first_week) = weekly.first_key_value().map(|(date, _)| *date) else {
            return Ok(Vec::new());
        };
        let current_week = monday_of(Utc::now().date_naive());
        let mut result = Vec::new();
        let mut cursor = first_week;
        while cursor <= current_week {
            result.push(weekly.remove(&cursor).unwrap_or(UsageWeek {
                week_start: cursor,
                total_sessions: 0,
                total_words: 0,
                total_audio_seconds: 0.0,
                estimated_total_cost: 0.0,
            }));
            let Some(next) = cursor.checked_add_days(Days::new(7)) else {
                break;
            };
            cursor = next;
        }
        Ok(result)
    }

    fn usage_summary_on(&self, today: NaiveDate) -> Result<UsageSummary, RuntimeError> {
        let week_start = today
            .checked_sub_days(Days::new(today.weekday().num_days_from_monday().into()))
            .unwrap_or(today);
        let month_start = today.with_day(1).unwrap_or(today);
        let all_time = aggregate(&self.connection, None)?;
        let today_usage = aggregate(&self.connection, Some(today))?;
        let week = aggregate(&self.connection, Some(week_start))?;
        let month = aggregate(&self.connection, Some(month_start))?;
        let most_used_transcription_model = most_used_model(
            &self.connection,
            "transcription_model",
            "transcription_model != ''",
        )?;
        let most_used_cleanup_model = most_used_model(
            &self.connection,
            "cleanup_model",
            "cleanup_enabled = 1 AND cleanup_model IS NOT NULL AND cleanup_model != ''",
        )?;
        let cleanup_mode_usage_count = self.connection.query_row(
            "SELECT COUNT(*) FROM dictation_sessions WHERE cleanup_enabled = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(UsageSummary {
            all_time,
            today: today_usage,
            week,
            month,
            most_used_transcription_model,
            most_used_cleanup_model,
            cleanup_mode_usage_count,
        })
    }

    fn usage_series_ending(
        &self,
        days: usize,
        metric: UsageMetric,
        end: NaiveDate,
    ) -> Result<Vec<UsagePoint>, RuntimeError> {
        if days == 0 {
            return Ok(Vec::new());
        }
        let start = end
            .checked_sub_days(Days::new((days - 1) as u64))
            .unwrap_or(end);
        let mut statement = self.connection.prepare(
            r#"
            SELECT date, total_sessions, total_words, total_audio_seconds,
                   average_wpm, estimated_total_cost
            FROM daily_stats
            WHERE date >= ?1 AND date <= ?2
            ORDER BY date ASC
            "#,
        )?;
        let stored = statement
            .query_map(params![start.to_string(), end.to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    (
                        row.get::<_, f64>(1)?,
                        row.get::<_, f64>(2)?,
                        row.get::<_, f64>(3)?,
                        row.get::<_, f64>(4)?,
                        row.get::<_, f64>(5)?,
                    ),
                ))
            })?
            .collect::<rusqlite::Result<std::collections::BTreeMap<_, _>>>()?;
        let mut result = Vec::with_capacity(days);
        for offset in 0..days {
            let date = start
                .checked_add_days(Days::new(offset as u64))
                .unwrap_or(end);
            let value = stored
                .get(&date.to_string())
                .map_or(0.0, |row| match metric {
                    UsageMetric::Sessions => row.0,
                    UsageMetric::Words => row.1,
                    UsageMetric::AudioMinutes => row.2 / 60.0,
                    UsageMetric::AverageWpm => row.3,
                    UsageMetric::EstimatedCost => row.4,
                });
            result.push(UsagePoint { date, value });
        }
        Ok(result)
    }
}

fn monday_of(date: NaiveDate) -> NaiveDate {
    date.checked_sub_days(Days::new(date.weekday().num_days_from_monday().into()))
        .unwrap_or(date)
}

fn aggregate(
    connection: &rusqlite::Connection,
    since: Option<NaiveDate>,
) -> Result<UsageAggregate, RuntimeError> {
    let since = since.map_or_else(String::new, |date| date.to_string());
    let values: (u64, u64, f64, f64, f64, f64) = connection.query_row(
        r#"
        SELECT COUNT(*), COALESCE(SUM(final_word_count), 0),
               COALESCE(SUM(duration_seconds), 0),
               COALESCE(SUM(estimated_transcription_cost), 0),
               COALESCE(SUM(estimated_cleanup_cost), 0),
               COALESCE(SUM(estimated_total_cost), 0)
        FROM dictation_sessions
        WHERE (?1 = '' OR substr(started_at, 1, 10) >= ?1)
        "#,
        [&since],
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
    let average_wpm = if values.2 > 0.0 {
        values.1 as f64 / (values.2 / 60.0)
    } else {
        0.0
    };
    let session_divisor = values.0 as f64;
    Ok(UsageAggregate {
        total_sessions: values.0,
        total_words: values.1,
        total_audio_seconds: values.2,
        estimated_transcription_cost: values.3,
        estimated_cleanup_cost: values.4,
        estimated_total_cost: values.5,
        average_wpm,
        average_words_per_session: if values.0 == 0 {
            0.0
        } else {
            values.1 as f64 / session_divisor
        },
        average_duration_per_session: if values.0 == 0 {
            0.0
        } else {
            values.2 / session_divisor
        },
    })
}

fn most_used_model(
    connection: &rusqlite::Connection,
    column: &str,
    predicate: &str,
) -> Result<Option<String>, RuntimeError> {
    Ok(connection
        .query_row(
            &format!(
                "SELECT {column} FROM dictation_sessions WHERE {predicate} \
                 GROUP BY {column} ORDER BY COUNT(*) DESC, {column} ASC LIMIT 1"
            ),
            [],
            |row| row.get(0),
        )
        .optional()?)
}
