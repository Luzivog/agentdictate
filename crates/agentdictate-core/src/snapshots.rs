use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::replacements::ReplacementRule;
use crate::workflow::{JobId, JobStage};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecoverySnapshot {
    pub job_id: JobId,
    pub stage: JobStage,
    pub updated_at: DateTime<Utc>,
    pub duration_seconds: f64,
    pub raw_transcript: String,
    pub final_text: String,
    pub error_message: Option<String>,
    pub audio_present: bool,
    pub delivery_ambiguous: bool,
}

/// One History row. A page carries every row's whole text (transcripts are
/// a few KB at most) so the window can expand a row without another request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistorySnapshot {
    pub id: i64,
    pub created_at: DateTime<Utc>,
    /// One line to list: the start of the text, or the part around a
    /// search match.
    pub preview_text: String,
    pub text: String,
    pub word_count: u64,
    pub duration_seconds: f64,
}

/// Rows in a first History page. The workspace's page also fills the
/// overview's recent list.
pub const DEFAULT_HISTORY_PAGE_SIZE: usize = 30;
pub const HISTORY_CONTINUATION_PAGE_SIZE: usize = 50;

/// Opaque continuation token returned by the daemon for a specific history query.
///
/// Clients must round-trip this value unchanged rather than inspecting or constructing
/// database pagination state themselves.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HistoryPageCursor(String);

impl HistoryPageCursor {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HistoryPageRequest {
    pub search: String,
    pub page_size: usize,
    pub after: Option<HistoryPageCursor>,
}

impl Default for HistoryPageRequest {
    fn default() -> Self {
        Self {
            search: String::new(),
            page_size: DEFAULT_HISTORY_PAGE_SIZE,
            after: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HistoryPageSnapshot {
    pub search: String,
    pub total_matches: u64,
    /// True when an expired opaque cursor was safely restarted at page one.
    pub cursor_restarted: bool,
    pub next_cursor: Option<HistoryPageCursor>,
    pub rows: Vec<HistorySnapshot>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageTotalsSnapshot {
    pub dictations: u64,
    pub words: u64,
    pub audio_seconds: f64,
    pub estimated_cost: f64,
}

impl std::ops::Add for UsageTotalsSnapshot {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            dictations: self.dictations + other.dictations,
            words: self.words + other.words,
            audio_seconds: self.audio_seconds + other.audio_seconds,
            estimated_cost: self.estimated_cost + other.estimated_cost,
        }
    }
}

impl std::ops::AddAssign for UsageTotalsSnapshot {
    fn add_assign(&mut self, other: Self) {
        *self = *self + other;
    }
}

impl std::iter::Sum for UsageTotalsSnapshot {
    fn sum<I: Iterator<Item = Self>>(totals: I) -> Self {
        totals.fold(Self::default(), std::ops::Add::add)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageDaySnapshot {
    pub date: NaiveDate,
    pub totals: UsageTotalsSnapshot,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub last_7_days: UsageTotalsSnapshot,
    pub last_30_days: UsageTotalsSnapshot,
    pub all_time: UsageTotalsSnapshot,
    pub activity: Vec<UsageDaySnapshot>,
    pub weekly_activity: Vec<UsageDaySnapshot>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub overlay_unavailable: bool,
    pub recoveries: Vec<RecoverySnapshot>,
    /// The newest transcripts, unfiltered.
    pub history: HistoryPageSnapshot,
    pub replacements: Vec<ReplacementRule>,
    pub usage: UsageSnapshot,
}
