use std::collections::BTreeMap;

use agentdictate_core::{UsageDaySnapshot, UsageSnapshot, UsageTotalsSnapshot};
use chrono::{Datelike, Days, Local, NaiveDate};

use crate::{Runtime, RuntimeError};

/// Days in the overview's recent activity chart.
const ACTIVITY_DAYS: u64 = 30;

impl Runtime {
    /// Usage totals and activity by local calendar day, from one pass over
    /// the recorded sessions.
    pub fn usage(&self) -> Result<UsageSnapshot, RuntimeError> {
        let (days, undated) = self.totals_by_local_day()?;
        Ok(summarize(&days, undated, Local::now().date_naive()))
    }

    /// Sums sessions by the local calendar day they started on. Sessions
    /// whose start SQLite cannot read are summed separately, so they still
    /// count toward the all-time totals.
    fn totals_by_local_day(
        &self,
    ) -> Result<
        (
            BTreeMap<NaiveDate, UsageTotalsSnapshot>,
            UsageTotalsSnapshot,
        ),
        RuntimeError,
    > {
        let mut statement = self.connection.prepare(
            r#"
            SELECT date(started_at, 'localtime'), COUNT(*),
                   COALESCE(SUM(final_word_count), 0),
                   COALESCE(SUM(duration_seconds), 0),
                   COALESCE(SUM(estimated_total_cost), 0)
            FROM dictation_sessions
            GROUP BY 1
            "#,
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    UsageTotalsSnapshot {
                        dictations: row.get(1)?,
                        words: row.get(2)?,
                        audio_seconds: row.get(3)?,
                        estimated_cost: row.get(4)?,
                    },
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut days = BTreeMap::new();
        let mut undated = UsageTotalsSnapshot::default();
        for (date, totals) in rows {
            let Some(date) = date else {
                undated += totals;
                continue;
            };
            let parsed = NaiveDate::parse_from_str(&date, "%Y-%m-%d")
                .map_err(|source| RuntimeError::InvalidUsageDate { date, source })?;
            days.insert(parsed, totals);
        }
        Ok((days, undated))
    }
}

/// Builds the overview's totals from per-day totals: the last 30 and 7
/// days ending `today`, one entry per day of the last 30 (empty days
/// included), all time, and Monday-based weeks from the first active week
/// through the current one, keeping empty weeks so the chart never
/// compresses inactive time.
fn summarize(
    days: &BTreeMap<NaiveDate, UsageTotalsSnapshot>,
    undated: UsageTotalsSnapshot,
    today: NaiveDate,
) -> UsageSnapshot {
    let activity = (0..ACTIVITY_DAYS)
        .rev()
        .filter_map(|days_ago| today.checked_sub_days(Days::new(days_ago)))
        .map(|date| UsageDaySnapshot {
            date,
            totals: days.get(&date).copied().unwrap_or_default(),
        })
        .collect::<Vec<_>>();
    let mut weeks = BTreeMap::<NaiveDate, UsageTotalsSnapshot>::new();
    for (date, totals) in days {
        *weeks.entry(monday_of(*date)).or_default() += *totals;
    }
    let mut weekly_activity = Vec::new();
    if let Some(first_week) = weeks.keys().next().copied() {
        let mut week = first_week;
        while week <= monday_of(today) {
            weekly_activity.push(UsageDaySnapshot {
                date: week,
                totals: weeks.get(&week).copied().unwrap_or_default(),
            });
            let Some(next) = week.checked_add_days(Days::new(7)) else {
                break;
            };
            week = next;
        }
    }
    UsageSnapshot {
        last_7_days: activity.iter().rev().take(7).map(|day| day.totals).sum(),
        last_30_days: activity.iter().map(|day| day.totals).sum(),
        all_time: days.values().copied().sum::<UsageTotalsSnapshot>() + undated,
        activity,
        weekly_activity,
    }
}

fn monday_of(date: NaiveDate) -> NaiveDate {
    date.checked_sub_days(Days::new(date.weekday().num_days_from_monday().into()))
        .unwrap_or(date)
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use rusqlite::params;
    use tempfile::tempdir;

    use super::*;

    fn totals(dictations: u64) -> UsageTotalsSnapshot {
        UsageTotalsSnapshot {
            dictations,
            words: dictations * 10,
            audio_seconds: dictations as f64 * 6.0,
            estimated_cost: dictations as f64 * 0.01,
        }
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn totals_cover_recent_windows_all_time_and_every_week() {
        // 2026-09-23 is a Wednesday.
        let today = date("2026-09-23");
        let days = BTreeMap::from([
            (date("2026-08-01"), totals(1)),
            (date("2026-08-30"), totals(2)),
            (date("2026-09-16"), totals(4)),
            (date("2026-09-17"), totals(8)),
            (date("2026-09-22"), totals(16)),
        ]);

        let usage = summarize(&days, totals(32), today);

        assert_eq!(usage.activity.len(), 30);
        assert_eq!(usage.activity[0].date, date("2026-08-25"));
        assert_eq!(usage.activity[29].date, today);
        assert_eq!(usage.activity[28].totals, totals(16));
        assert_eq!(usage.last_7_days.dictations, 8 + 16);
        assert_eq!(usage.last_30_days.dictations, 2 + 4 + 8 + 16);
        assert_eq!(usage.all_time.dictations, 63);
        assert_eq!(usage.all_time.words, 630);
        let weeks = &usage.weekly_activity;
        assert_eq!(weeks.first().unwrap().date, date("2026-07-27"));
        assert_eq!(weeks.last().unwrap().date, date("2026-09-21"));
        assert_eq!(weeks.len(), 9);
        assert_eq!(weeks[1].totals, UsageTotalsSnapshot::default());
        assert_eq!(weeks[7].totals.dictations, 4 + 8);
    }

    #[test]
    fn no_sessions_have_no_weeks_and_empty_days() {
        let usage = summarize(
            &BTreeMap::new(),
            UsageTotalsSnapshot::default(),
            date("2026-09-23"),
        );

        assert!(usage.weekly_activity.is_empty());
        assert_eq!(usage.activity.len(), 30);
        assert_eq!(usage.all_time, UsageTotalsSnapshot::default());
    }

    #[test]
    fn sessions_count_on_their_local_calendar_day() {
        let directory = tempdir().unwrap();
        let runtime = Runtime::open(directory.path().join("usage.sqlite")).unwrap();
        // One minute apart across UTC midnight: two days in UTC, one day in
        // most local time zones.
        let started = ["2026-08-17T23:59:30Z", "2026-08-18T00:00:30Z"];
        for started_at in started {
            runtime
                .connection
                .execute(
                    r#"
                    INSERT INTO dictation_sessions (
                        started_at, ended_at, transcription_model, final_word_count
                    ) VALUES (?1, ?1, 'gpt-transcribe', 3)
                    "#,
                    params![started_at],
                )
                .unwrap();
        }

        let (days, undated) = runtime.totals_by_local_day().unwrap();

        let mut expected = BTreeMap::<NaiveDate, u64>::new();
        for started_at in started {
            let local_day = DateTime::parse_from_rfc3339(started_at)
                .unwrap()
                .with_timezone(&Local)
                .date_naive();
            *expected.entry(local_day).or_default() += 1;
        }
        let counted = days
            .iter()
            .map(|(day, totals)| (*day, totals.dictations))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(counted, expected);
        assert_eq!(undated, UsageTotalsSnapshot::default());
    }
}
