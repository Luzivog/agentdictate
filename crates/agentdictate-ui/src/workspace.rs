use std::sync::Arc;

use agentdictate_core::{AppSnapshot, WorkspaceSnapshot};
use chrono::{DateTime, TimeZone};

use crate::{HistoryViewModel, RecoveryStage, TranscriptViewModel, UsagePeriod, UsageViewModel};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceAction {
    RetryRecovery { id: String, stage: RecoveryStage },
    DeleteRecovery { id: String },
    CopyTranscript { id: i64 },
    DeleteTranscript { id: i64 },
    ClearHistory,
    SearchHistory { query: String },
    LoadMoreHistory,
    SelectUsagePeriod(UsagePeriod),
}

impl WorkspaceAction {
    /// What to tell the user once this action succeeds, if anything.
    pub const fn success_feedback(&self) -> Option<&'static str> {
        match self {
            // Recovery never pastes: its buttons sit in this window, which
            // has the focus, so it only copies and the user pastes.
            Self::RetryRecovery { .. } => Some("Copied — press Ctrl+V where you want it"),
            // Deleting all history happens in Settings, which has no list
            // to show the result.
            Self::ClearHistory => Some("All history deleted"),
            Self::DeleteRecovery { .. }
            | Self::CopyTranscript { .. }
            | Self::DeleteTranscript { .. }
            | Self::SearchHistory { .. }
            | Self::LoadMoreHistory
            | Self::SelectUsagePeriod(_) => None,
        }
    }

    pub fn selector(&self) -> String {
        match self {
            Self::RetryRecovery { id, .. } => format!("history-retry-recovery-{id}"),
            Self::DeleteRecovery { id } => format!("history-delete-recovery-{id}"),
            Self::CopyTranscript { id } => format!("history-copy-transcript-{id}"),
            Self::DeleteTranscript { id } => format!("history-delete-transcript-{id}"),
            Self::ClearHistory => "settings-delete-all-history".to_owned(),
            Self::SearchHistory { .. } => "history-search".to_owned(),
            Self::LoadMoreHistory => "history-load-more".to_owned(),
            Self::SelectUsagePeriod(period) => format!("usage-period-{}", period.slug()),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorkspaceViewModel {
    pub overlay_unavailable: bool,
    /// Where an unreadable history database was moved when the daemon
    /// started, for a notice that History starts over.
    pub history_set_aside: Option<String>,
    /// The daemon or its database is from a newer AgentDictate than this
    /// window, which asks to be reopened instead of misreading either.
    pub window_outdated: bool,
    pub history: HistoryViewModel,
    pub recent_transcripts: Vec<TranscriptViewModel>,
    pub usage: UsageViewModel,
}

/// Executes one workspace action and returns the fresh presentation snapshot
/// that replaces all workspace route data atomically.
pub type UiActionError = Box<dyn std::error::Error + Send + Sync + 'static>;

pub type WorkspaceActionSink =
    Arc<dyn Fn(WorkspaceAction) -> Result<WorkspaceViewModel, UiActionError> + Send + Sync>;

impl WorkspaceViewModel {
    pub fn new(
        history: HistoryViewModel,
        recent_transcripts: Vec<TranscriptViewModel>,
        usage: UsageViewModel,
    ) -> Self {
        Self {
            overlay_unavailable: false,
            history_set_aside: None,
            window_outdated: false,
            history,
            recent_transcripts,
            usage,
        }
    }

    /// Presents what the window read from the database, with usage for
    /// `period` and times on `now`'s clock.
    pub fn from_snapshot<Tz: TimeZone>(
        snapshot: &WorkspaceSnapshot,
        period: UsagePeriod,
        now: &DateTime<Tz>,
    ) -> Self
    where
        Tz::Offset: std::fmt::Display,
    {
        Self::new(
            HistoryViewModel::from_snapshots(&snapshot.recoveries, &snapshot.history, now),
            snapshot
                .recent
                .rows
                .iter()
                .map(|entry| TranscriptViewModel::from_snapshot(entry, now))
                .collect(),
            UsageViewModel::from_snapshot(&snapshot.usage, period),
        )
    }

    /// Adds the notices of the daemon's status snapshot.
    #[must_use]
    pub fn with_status(self, status: &AppSnapshot) -> Self {
        self.with_overlay_unavailable(status.overlay_unavailable)
            .with_history_set_aside(
                status
                    .history_set_aside
                    .as_ref()
                    .map(|path| path.display().to_string()),
            )
    }

    #[must_use]
    pub fn with_window_outdated(mut self, outdated: bool) -> Self {
        self.window_outdated = outdated;
        self
    }

    #[must_use]
    pub fn with_overlay_unavailable(mut self, unavailable: bool) -> Self {
        self.overlay_unavailable = unavailable;
        self
    }

    #[must_use]
    pub fn with_history_set_aside(mut self, set_aside: Option<String>) -> Self {
        self.history_set_aside = set_aside;
        self
    }
}
