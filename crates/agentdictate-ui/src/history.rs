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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryStage {
    Transcription,
    Delivery,
}

impl RecoveryStage {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Transcription => "Needs transcription",
            Self::Delivery => "Ready to paste",
        }
    }

    pub const fn primary_action_label(self) -> &'static str {
        match self {
            Self::Transcription => "Transcribe again",
            Self::Delivery => "Paste again",
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
}

impl RecoveryItemViewModel {
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
    pub text: String,
    pub word_count: u64,
    pub duration: String,
}

impl TranscriptViewModel {
    pub fn new(
        id: i64,
        created_at: impl Into<String>,
        text: impl Into<String>,
        word_count: u64,
        duration: impl Into<String>,
    ) -> Self {
        Self {
            id,
            created_at: created_at.into(),
            text: text.into(),
            word_count,
            duration: duration.into(),
        }
    }

    pub fn preview(&self) -> String {
        const MAX_CHARACTERS: usize = 120;
        let mut characters = self.text.chars();
        let preview = characters.by_ref().take(MAX_CHARACTERS).collect::<String>();
        if characters.next().is_some() {
            format!("{preview}…")
        } else {
            preview
        }
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
    use chrono::{FixedOffset, TimeZone, Utc};

    use super::format_history_time;

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
}
