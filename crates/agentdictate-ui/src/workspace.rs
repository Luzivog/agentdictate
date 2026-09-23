use std::sync::Arc;

use crate::{
    HistoryViewModel, RecoveryStage, ReplacementDraft, ReplacementsViewModel, TranscriptViewModel,
    UsagePeriod, UsageViewModel,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceAction {
    RetryRecovery { id: String, stage: RecoveryStage },
    DeleteRecovery { id: String },
    CopyTranscript { id: i64 },
    SearchHistory { query: String },
    LoadMoreHistory,
    CreateReplacement { draft: ReplacementDraft },
    UpdateReplacement { id: i64, draft: ReplacementDraft },
    SetReplacementEnabled { id: i64, enabled: bool },
    DeleteReplacement { id: i64 },
    SelectUsagePeriod(UsagePeriod),
}

impl WorkspaceAction {
    /// What to tell the user once this action succeeds, if anything.
    pub const fn success_feedback(&self) -> Option<&'static str> {
        match self {
            // Recovery never pastes: its buttons sit in this window, which
            // has the focus, so it only copies and the user pastes.
            Self::RetryRecovery { .. } => Some("Copied — press Ctrl+V where you want it"),
            Self::DeleteRecovery { .. }
            | Self::CopyTranscript { .. }
            | Self::SearchHistory { .. }
            | Self::LoadMoreHistory
            | Self::CreateReplacement { .. }
            | Self::UpdateReplacement { .. }
            | Self::SetReplacementEnabled { .. }
            | Self::DeleteReplacement { .. }
            | Self::SelectUsagePeriod(_) => None,
        }
    }

    pub fn selector(&self) -> String {
        match self {
            Self::RetryRecovery { id, .. } => format!("history-retry-recovery-{id}"),
            Self::DeleteRecovery { id } => format!("history-delete-recovery-{id}"),
            Self::CopyTranscript { id } => format!("history-copy-transcript-{id}"),
            Self::SearchHistory { .. } => "history-search".to_owned(),
            Self::LoadMoreHistory => "history-load-more".to_owned(),
            Self::CreateReplacement { .. } => "replacement-save-new".to_owned(),
            Self::UpdateReplacement { id, .. } => format!("replacement-save-{id}"),
            Self::SetReplacementEnabled { id, .. } => format!("replacement-toggle-{id}"),
            Self::DeleteReplacement { id } => format!("replacement-delete-{id}"),
            Self::SelectUsagePeriod(period) => format!("usage-period-{}", period.slug()),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorkspaceViewModel {
    pub overlay_unavailable: bool,
    pub history: HistoryViewModel,
    pub recent_transcripts: Vec<TranscriptViewModel>,
    pub replacements: ReplacementsViewModel,
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
        replacements: ReplacementsViewModel,
        usage: UsageViewModel,
    ) -> Self {
        Self {
            overlay_unavailable: false,
            history,
            recent_transcripts,
            replacements,
            usage,
        }
    }

    #[must_use]
    pub fn with_overlay_unavailable(mut self, unavailable: bool) -> Self {
        self.overlay_unavailable = unavailable;
        self
    }
}
