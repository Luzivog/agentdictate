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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistorySnapshot {
    pub id: i64,
    pub created_at: DateTime<Utc>,
    pub preview_text: String,
    pub word_count: u64,
    pub duration_seconds: f64,
}

pub const DEFAULT_HISTORY_PAGE_SIZE: usize = 20;
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
    pub recent_history: Vec<HistorySnapshot>,
    pub history: Vec<HistorySnapshot>,
    pub history_total: u64,
    pub history_has_more: bool,
    pub history_next_cursor: Option<HistoryPageCursor>,
    pub history_search: String,
    pub replacements: Vec<ReplacementRule>,
    pub usage: UsageSnapshot,
}
