use agentdictate_core::{
    HistoryPageSnapshot, HistorySnapshot, JobStage, RecoverySnapshot, format_duration_clock,
};
use chrono::{DateTime, Datelike, TimeZone, Utc};

/// Formats when a dictation happened in `now`'s time zone, the way History
/// and Home list it: "Today 14:32", "Yesterday 09:05", "Mon 14:32" within the
/// last week, then "Sep 3", adding the year only when it isn't `now`'s.
pub fn format_history_time<Tz: TimeZone>(at: DateTime<Utc>, now: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let local = at.with_timezone(&now.timezone());
    let format = match (now.date_naive() - local.date_naive()).num_days() {
        0 => "Today %H:%M",
        1 => "Yesterday %H:%M",
        2..=6 => "%a %H:%M",
        _ if local.year() == now.year() => "%b %-d",
        _ => "%b %-d, %Y",
    };
    local.format(format).to_string()
}

/// Says how long until a Recovery item is deleted, rounded to the nearest
/// hour below 36 hours and to the nearest day above: "Expires in 7 days",
/// "Expires in 24 hours", "Expires in less than an hour".
pub fn format_expiry<Tz: TimeZone>(expires_at: DateTime<Utc>, now: &DateTime<Tz>) -> String {
    const HOUR: i64 = 60;
    const DAY: i64 = 24 * HOUR;
    let minutes = (expires_at - now.with_timezone(&Utc)).num_minutes();
    if minutes < HOUR {
        "Expires in less than an hour".to_owned()
    } else if minutes < 36 * HOUR {
        let hours = (minutes + HOUR / 2) / HOUR;
        format!(
            "Expires in {hours} hour{}",
            if hours == 1 { "" } else { "s" }
        )
    } else {
        format!("Expires in {} days", (minutes + DAY / 2) / DAY)
    }
}

/// What a Recovery item needs, which decides its label and its button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryStage {
    Transcription,
    Delivery,
    /// Discarded with Esc; transcribing it anyway is the user's choice.
    Cancelled,
}

impl RecoveryStage {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Transcription => "Needs transcription",
            Self::Delivery => "Ready to paste",
            Self::Cancelled => "Cancelled — transcribe anyway?",
        }
    }

    pub const fn primary_action_label(self) -> &'static str {
        match self {
            Self::Transcription => "Transcribe again",
            Self::Delivery => "Paste again",
            Self::Cancelled => "Transcribe",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryItemViewModel {
    pub id: String,
    pub stage: RecoveryStage,
    pub captured_at: String,
    pub duration: String,
    pub error: String,
    pub transcript_preview: Option<String>,
    /// When the item is deleted unless the user acts, such as "Expires in
    /// 7 days".
    pub expires: Option<String>,
}

impl RecoveryItemViewModel {
    /// Presents a Recovery item with times on `now`'s clock. Stored text
    /// that failed to paste offers "Paste again"; anything else is
    /// transcribed again.
    pub fn from_snapshot<Tz: TimeZone>(entry: &RecoverySnapshot, now: &DateTime<Tz>) -> Self
    where
        Tz::Offset: std::fmt::Display,
    {
        let has_text = !entry.final_text.trim().is_empty();
        let stage = match entry.stage {
            JobStage::Cancelled => RecoveryStage::Cancelled,
            JobStage::ReadyToDeliver | JobStage::Failed if has_text => RecoveryStage::Delivery,
            _ if has_text && entry.delivery_ambiguous => RecoveryStage::Delivery,
            _ => RecoveryStage::Transcription,
        };
        Self {
            expires: Some(format_expiry(entry.expires_at, now)),
            ..Self::new(
                entry.job_id.to_string(),
                stage,
                format_history_time(entry.updated_at, now),
                format_duration_clock(entry.duration_seconds),
                entry
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "Recording saved safely".to_owned()),
                has_text.then(|| entry.final_text.clone()),
            )
        }
    }

    pub fn new(
        id: impl Into<String>,
        stage: RecoveryStage,
        captured_at: impl Into<String>,
        duration: impl Into<String>,
        error: impl Into<String>,
        transcript_preview: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            stage,
            captured_at: captured_at.into(),
            duration: duration.into(),
            error: error.into(),
            transcript_preview,
            expires: None,
        }
    }

    pub const fn primary_action_label(&self) -> &'static str {
        self.stage.primary_action_label()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptViewModel {
    pub id: i64,
    pub created_at: String,
    /// The whole transcript, shown when its row is expanded.
    pub text: String,
    /// The one line shown while its row is collapsed.
    pub preview: String,
    pub word_count: u64,
    pub duration: String,
}

impl TranscriptViewModel {
    /// A transcript whose collapsed line is the start of `text`.
    pub fn new(
        id: i64,
        created_at: impl Into<String>,
        text: impl Into<String>,
        word_count: u64,
        duration: impl Into<String>,
    ) -> Self {
        const PREVIEW_CHARACTERS: usize = 120;
        let text = text.into();
        let mut characters = text.chars();
        let mut preview = characters
            .by_ref()
            .take(PREVIEW_CHARACTERS)
            .collect::<String>();
        if characters.next().is_some() {
            preview.push('…');
        }
        Self {
            id,
            created_at: created_at.into(),
            text,
            preview,
            word_count,
            duration: duration.into(),
        }
    }

    /// Replaces the collapsed line, e.g. with the excerpt around a search
    /// match.
    #[must_use]
    pub fn with_preview(mut self, preview: impl Into<String>) -> Self {
        self.preview = preview.into();
        self
    }

    /// Presents a History row with its time on `now`'s clock.
    pub fn from_snapshot<Tz: TimeZone>(entry: &HistorySnapshot, now: &DateTime<Tz>) -> Self
    where
        Tz::Offset: std::fmt::Display,
    {
        Self::new(
            entry.id,
            format_history_time(entry.created_at, now),
            entry.text.clone(),
            entry.word_count,
            format_duration_clock(entry.duration_seconds),
        )
        .with_preview(entry.preview_text.clone())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryViewModel {
    pub title: &'static str,
    pub detail: String,
    pub item_count: u64,
    pub items: Vec<RecoveryItemViewModel>,
}

impl RecoveryViewModel {
    pub fn from_item_count(item_count: u64) -> Self {
        let detail = match item_count {
            0 => "No recordings need your attention".to_owned(),
            1 => "1 recording needs your attention".to_owned(),
            count => format!("{count} recordings need your attention"),
        };

        Self {
            title: "Recovery",
            detail,
            item_count,
            items: Vec::new(),
        }
    }

    pub fn from_items(items: Vec<RecoveryItemViewModel>) -> Self {
        let mut model = Self::from_item_count(items.len() as u64);
        model.items = items;
        model
    }

    pub const fn has_items(&self) -> bool {
        self.item_count > 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryViewModel {
    pub transcript_count: u64,
    pub search: String,
    pub has_more: bool,
    pub recovery: RecoveryViewModel,
    pub transcripts: Vec<TranscriptViewModel>,
}

impl HistoryViewModel {
    pub fn new(transcript_count: u64, recoverable_recording_count: u64) -> Self {
        Self {
            transcript_count,
            search: String::new(),
            has_more: false,
            recovery: RecoveryViewModel::from_item_count(recoverable_recording_count),
            transcripts: Vec::new(),
        }
    }

    /// Presents Recovery and one History page with times on `now`'s clock.
    pub fn from_snapshots<Tz: TimeZone>(
        recoveries: &[RecoverySnapshot],
        page: &HistoryPageSnapshot,
        now: &DateTime<Tz>,
    ) -> Self
    where
        Tz::Offset: std::fmt::Display,
    {
        Self::from_page(
            recoveries
                .iter()
                .map(|entry| RecoveryItemViewModel::from_snapshot(entry, now))
                .collect(),
            page.rows
                .iter()
                .map(|entry| TranscriptViewModel::from_snapshot(entry, now))
                .collect(),
            page.total_matches,
            page.search.clone(),
            page.next_cursor.is_some(),
        )
    }

    pub fn from_page(
        recovery_items: Vec<RecoveryItemViewModel>,
        transcripts: Vec<TranscriptViewModel>,
        transcript_count: u64,
        search: String,
        has_more: bool,
    ) -> Self {
        Self {
            transcript_count,
            search,
            has_more,
            recovery: RecoveryViewModel::from_items(recovery_items),
            transcripts,
        }
    }
}

impl Default for HistoryViewModel {
    fn default() -> Self {
        Self::new(0, 0)
    }
}

#[cfg(test)]
mod tests {
    use chrono::{FixedOffset, TimeDelta, TimeZone, Utc};

    use super::{format_expiry, format_history_time};

    /// Wednesday 23 September 2026, 10:00 at UTC+2.
    fn now() -> chrono::DateTime<FixedOffset> {
        FixedOffset::east_opt(2 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 9, 23, 10, 0, 0)
            .unwrap()
    }

    fn format_utc(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> String {
        let at = Utc
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .unwrap();
        format_history_time(at, &now())
    }

    #[test]
    fn days_are_counted_on_the_local_calendar() {
        // 23:30 UTC on the 22nd is already 01:30 on the 23rd at UTC+2.
        assert_eq!(format_utc(2026, 9, 22, 23, 30), "Today 01:30");
        assert_eq!(format_utc(2026, 9, 22, 21, 59), "Yesterday 23:59");
        assert_eq!(format_utc(2026, 9, 22, 7, 5), "Yesterday 09:05");
    }

    #[test]
    fn the_last_week_uses_weekdays_and_older_entries_use_dates() {
        assert_eq!(format_utc(2026, 9, 21, 12, 32), "Mon 14:32");
        assert_eq!(format_utc(2026, 9, 17, 12, 32), "Thu 14:32");
        // Seven days back would repeat today's weekday, so it gets a date.
        assert_eq!(format_utc(2026, 9, 16, 12, 32), "Sep 16");
        assert_eq!(format_utc(2026, 1, 3, 12, 0), "Jan 3");
        assert_eq!(format_utc(2025, 12, 30, 12, 0), "Dec 30, 2025");
    }

    #[test]
    fn a_timestamp_from_a_later_day_shows_its_date() {
        assert_eq!(format_utc(2026, 9, 24, 12, 0), "Sep 24");
    }

    #[test]
    fn expiry_counts_hours_within_a_day_and_a_half_and_days_after() {
        let expiring_in = |hours: i64, minutes: i64| {
            let now = now();
            let expires_at =
                now.with_timezone(&Utc) + TimeDelta::hours(hours) + TimeDelta::minutes(minutes);
            format_expiry(expires_at, &now)
        };

        assert_eq!(expiring_in(7 * 24, 0), "Expires in 7 days");
        assert_eq!(expiring_in(6 * 24, -1), "Expires in 6 days");
        assert_eq!(expiring_in(36, 0), "Expires in 2 days");
        assert_eq!(expiring_in(35, 59), "Expires in 36 hours");
        assert_eq!(expiring_in(24, 0), "Expires in 24 hours");
        assert_eq!(expiring_in(1, 0), "Expires in 1 hour");
        assert_eq!(expiring_in(0, 59), "Expires in less than an hour");
        assert_eq!(expiring_in(-2, 0), "Expires in less than an hour");
    }
}
