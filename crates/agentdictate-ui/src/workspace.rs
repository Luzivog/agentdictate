use std::sync::Arc;

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
            history,
            recent_transcripts,
            usage,
        }
    }

    #[must_use]
    pub fn with_overlay_unavailable(mut self, unavailable: bool) -> Self {
        self.overlay_unavailable = unavailable;
        self
    }
}
