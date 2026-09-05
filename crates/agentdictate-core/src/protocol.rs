use serde::{Deserialize, Serialize};

use crate::replacements::ReplacementRule;
use crate::settings::{SecretString, Settings, SettingsSnapshot};
use crate::snapshots::{
    HistoryPageCursor, HistoryPageRequest, HistoryPageSnapshot, WorkspaceSnapshot,
};
use crate::workflow::{JobId, WorkflowSnapshot};

pub const PROTOCOL_VERSION: u16 = 4;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClientCommand {
    pub protocol_version: u16,
    #[serde(flatten)]
    pub kind: ClientCommandKind,
}

impl ClientCommand {
    const fn with_kind(kind: ClientCommandKind) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            kind,
        }
    }

    #[must_use]
    pub const fn start_recording(request_id: u64) -> Self {
        Self::with_kind(ClientCommandKind::StartRecording {
            request_id,
            mode: None,
        })
    }

    /// Overrides output mode for this recording without changing saved settings.
    pub const fn start_recording_in_mode(request_id: u64, mode: crate::DictationMode) -> Self {
        Self::with_kind(ClientCommandKind::StartRecording {
            request_id,
            mode: Some(mode),
        })
    }

    #[must_use]
    pub const fn get_snapshot(request_id: u64) -> Self {
        Self::with_kind(ClientCommandKind::GetSnapshot { request_id })
    }

    #[must_use]
    pub const fn get_workspace(request_id: u64) -> Self {
        Self::with_kind(ClientCommandKind::GetWorkspace { request_id })
    }

    /// Returns the current cached/bundled catalog immediately while asking
    /// the daemon to refresh account availability in the background.
    #[must_use]
    pub const fn refresh_model_catalog(request_id: u64) -> Self {
        Self::with_kind(ClientCommandKind::RefreshModelCatalog { request_id })
    }

    #[must_use]
    pub fn get_history_page(
        request_id: u64,
        search: impl Into<String>,
        page_size: usize,
        after: Option<HistoryPageCursor>,
    ) -> Self {
        Self::with_kind(ClientCommandKind::GetHistoryPage {
            request_id,
            request: HistoryPageRequest {
                search: search.into(),
                page_size,
                after,
            },
        })
    }

    #[must_use]
    pub const fn stop_recording(request_id: u64) -> Self {
        Self::with_kind(ClientCommandKind::StopRecording { request_id })
    }

    #[must_use]
    pub const fn cancel(request_id: u64) -> Self {
        Self::with_kind(ClientCommandKind::Cancel { request_id })
    }

    #[doc(hidden)]
    #[must_use]
    pub const fn recorder_exited(request_id: u64, job_id: JobId) -> Self {
        Self::with_kind(ClientCommandKind::RecorderExited { request_id, job_id })
    }

    #[must_use]
    pub const fn retry_transcription(request_id: u64, job_id: JobId) -> Self {
        Self::with_kind(ClientCommandKind::RetryTranscription { request_id, job_id })
    }

    #[must_use]
    pub const fn retry_delivery(request_id: u64, job_id: JobId) -> Self {
        Self::with_kind(ClientCommandKind::RetryDelivery { request_id, job_id })
    }

    #[must_use]
    pub const fn delete_recovery(request_id: u64, job_id: JobId) -> Self {
        Self::with_kind(ClientCommandKind::DeleteRecovery { request_id, job_id })
    }

    #[must_use]
    pub fn create_replacement(request_id: u64, rule: ReplacementRule) -> Self {
        Self::with_kind(ClientCommandKind::CreateReplacement { request_id, rule })
    }

    #[must_use]
    pub fn update_replacement(request_id: u64, rule: ReplacementRule) -> Self {
        Self::with_kind(ClientCommandKind::UpdateReplacement { request_id, rule })
    }

    #[must_use]
    pub const fn delete_replacement(request_id: u64, id: i64) -> Self {
        Self::with_kind(ClientCommandKind::DeleteReplacement { request_id, id })
    }

    #[must_use]
    pub const fn delete_history(request_id: u64, id: i64) -> Self {
        Self::with_kind(ClientCommandKind::DeleteHistory { request_id, id })
    }

    #[must_use]
    pub const fn clear_history(request_id: u64) -> Self {
        Self::with_kind(ClientCommandKind::ClearHistory { request_id })
    }

    #[must_use]
    pub const fn copy_transcript(request_id: u64, id: i64) -> Self {
        Self::with_kind(ClientCommandKind::CopyTranscript { request_id, id })
    }

    #[must_use]
    pub const fn quit(request_id: u64) -> Self {
        Self::with_kind(ClientCommandKind::Quit { request_id })
    }

    #[doc(hidden)]
    #[must_use]
    pub fn hotkey_status_changed(request_id: u64, readiness: HotkeyReadiness) -> Self {
        Self::with_kind(ClientCommandKind::HotkeyStatusChanged {
            request_id,
            readiness,
        })
    }

    #[must_use]
    pub fn update_settings(request_id: u64, settings: &Settings) -> Self {
        Self::with_kind(ClientCommandKind::UpdateSettings {
            request_id,
            settings: Box::new(SettingsSnapshot::from(settings).values),
        })
    }

    #[must_use]
    pub fn set_api_key(request_id: u64, api_key: impl Into<String>) -> Self {
        Self::with_kind(ClientCommandKind::SetApiKey {
            request_id,
            api_key: SecretString(api_key.into()),
        })
    }

    /// Returns the data-less command tag without cloning command payloads.
    #[must_use]
    pub const fn kind(&self) -> ClientCommandTag {
        match &self.kind {
            ClientCommandKind::GetSnapshot { .. } => ClientCommandTag::GetSnapshot,
            ClientCommandKind::GetWorkspace { .. } => ClientCommandTag::GetWorkspace,
            ClientCommandKind::RefreshModelCatalog { .. } => ClientCommandTag::RefreshModelCatalog,
            ClientCommandKind::GetHistoryPage { .. } => ClientCommandTag::GetHistoryPage,
            ClientCommandKind::StartRecording { .. } => ClientCommandTag::StartRecording,
            ClientCommandKind::StopRecording { .. } => ClientCommandTag::StopRecording,
            ClientCommandKind::Cancel { .. } => ClientCommandTag::Cancel,
            ClientCommandKind::RecorderExited { .. } => ClientCommandTag::RecorderExited,
            ClientCommandKind::RetryTranscription { .. } => ClientCommandTag::RetryTranscription,
            ClientCommandKind::RetryDelivery { .. } => ClientCommandTag::RetryDelivery,
            ClientCommandKind::DeleteRecovery { .. } => ClientCommandTag::DeleteRecovery,
            ClientCommandKind::CreateReplacement { .. } => ClientCommandTag::CreateReplacement,
            ClientCommandKind::UpdateReplacement { .. } => ClientCommandTag::UpdateReplacement,
            ClientCommandKind::DeleteReplacement { .. } => ClientCommandTag::DeleteReplacement,
            ClientCommandKind::DeleteHistory { .. } => ClientCommandTag::DeleteHistory,
            ClientCommandKind::ClearHistory { .. } => ClientCommandTag::ClearHistory,
            ClientCommandKind::CopyTranscript { .. } => ClientCommandTag::CopyTranscript,
            ClientCommandKind::UpdateSettings { .. } => ClientCommandTag::UpdateSettings,
            ClientCommandKind::SetApiKey { .. } => ClientCommandTag::SetApiKey,
            ClientCommandKind::HotkeyStatusChanged { .. } => ClientCommandTag::HotkeyStatusChanged,
            ClientCommandKind::Quit { .. } => ClientCommandTag::Quit,
        }
    }
}

/// Data-less discriminator for every command carried by [`ClientCommandKind`].
///
/// This is separate from the payload-bearing wire enum so existing command
/// construction and pattern matching remain unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ClientCommandTag {
    GetSnapshot,
    GetWorkspace,
    RefreshModelCatalog,
    GetHistoryPage,
    StartRecording,
    StopRecording,
    Cancel,
    RecorderExited,
    RetryTranscription,
    RetryDelivery,
    DeleteRecovery,
    CreateReplacement,
    UpdateReplacement,
    DeleteReplacement,
    DeleteHistory,
    ClearHistory,
    CopyTranscript,
    UpdateSettings,
    SetApiKey,
    HotkeyStatusChanged,
    Quit,
}

impl ClientCommandTag {
    /// Every command tag in wire-enum declaration order.
    pub const ALL: &'static [Self] = &[
        Self::GetSnapshot,
        Self::GetWorkspace,
        Self::RefreshModelCatalog,
        Self::GetHistoryPage,
        Self::StartRecording,
        Self::StopRecording,
        Self::Cancel,
        Self::RecorderExited,
        Self::RetryTranscription,
        Self::RetryDelivery,
        Self::DeleteRecovery,
        Self::CreateReplacement,
        Self::UpdateReplacement,
        Self::DeleteReplacement,
        Self::DeleteHistory,
        Self::ClearHistory,
        Self::CopyTranscript,
        Self::UpdateSettings,
        Self::SetApiKey,
        Self::HotkeyStatusChanged,
        Self::Quit,
    ];
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum ClientCommandKind {
    GetSnapshot {
        request_id: u64,
    },
    GetWorkspace {
        request_id: u64,
    },
    RefreshModelCatalog {
        request_id: u64,
    },
    GetHistoryPage {
        request_id: u64,
        request: HistoryPageRequest,
    },
    StartRecording {
        request_id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<crate::DictationMode>,
    },
    StopRecording {
        request_id: u64,
    },
    Cancel {
        request_id: u64,
    },
    RecorderExited {
        request_id: u64,
        job_id: JobId,
    },
    RetryTranscription {
        request_id: u64,
        job_id: JobId,
    },
    RetryDelivery {
        request_id: u64,
        job_id: JobId,
    },
    DeleteRecovery {
        request_id: u64,
        job_id: JobId,
    },
    CreateReplacement {
        request_id: u64,
        rule: ReplacementRule,
    },
    UpdateReplacement {
        request_id: u64,
        rule: ReplacementRule,
    },
    DeleteReplacement {
        request_id: u64,
        id: i64,
    },
    DeleteHistory {
        request_id: u64,
        id: i64,
    },
    ClearHistory {
        request_id: u64,
    },
    CopyTranscript {
        request_id: u64,
        id: i64,
    },
    UpdateSettings {
        request_id: u64,
        settings: Box<Settings>,
    },
    SetApiKey {
        request_id: u64,
        api_key: SecretString,
    },
    HotkeyStatusChanged {
        request_id: u64,
        readiness: HotkeyReadiness,
    },
    Quit {
        request_id: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    const VARIANT_COUNT: usize = ClientCommandTag::Quit as usize + 1;

    const fn tag_index(tag: ClientCommandTag) -> usize {
        match tag {
            ClientCommandTag::GetSnapshot => 0,
            ClientCommandTag::GetWorkspace => 1,
            ClientCommandTag::RefreshModelCatalog => 2,
            ClientCommandTag::GetHistoryPage => 3,
            ClientCommandTag::StartRecording => 4,
            ClientCommandTag::StopRecording => 5,
            ClientCommandTag::Cancel => 6,
            ClientCommandTag::RecorderExited => 7,
            ClientCommandTag::RetryTranscription => 8,
            ClientCommandTag::RetryDelivery => 9,
            ClientCommandTag::DeleteRecovery => 10,
            ClientCommandTag::CreateReplacement => 11,
            ClientCommandTag::UpdateReplacement => 12,
            ClientCommandTag::DeleteReplacement => 13,
            ClientCommandTag::DeleteHistory => 14,
            ClientCommandTag::ClearHistory => 15,
            ClientCommandTag::CopyTranscript => 16,
            ClientCommandTag::UpdateSettings => 17,
            ClientCommandTag::SetApiKey => 18,
            ClientCommandTag::HotkeyStatusChanged => 19,
            ClientCommandTag::Quit => 20,
        }
    }

    #[test]
    fn command_tags_list_every_variant_once() {
        assert_eq!(ClientCommandTag::ALL.len(), VARIANT_COUNT);
        let mut seen = [false; VARIANT_COUNT];
        for tag in ClientCommandTag::ALL {
            let seen = &mut seen[tag_index(*tag)];
            assert!(!*seen, "duplicate command tag: {tag:?}");
            *seen = true;
        }
        assert!(seen.into_iter().all(|present| present));
    }

    #[test]
    fn commands_report_their_data_less_tag() {
        assert_eq!(
            ClientCommand::start_recording(7).kind(),
            ClientCommandTag::StartRecording
        );
        assert_eq!(
            ClientCommand::get_history_page(8, "needle", 20, None).kind(),
            ClientCommandTag::GetHistoryPage
        );
        assert_eq!(
            ClientCommand::set_api_key(9, "secret").kind(),
            ClientCommandTag::SetApiKey
        );
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HotkeyReadiness {
    Starting,
    Ready,
    Unavailable { message: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AppSnapshot {
    pub sequence: u64,
    pub workflow: WorkflowSnapshot,
    pub hotkey: HotkeyReadiness,
    pub recoverable_count: usize,
    pub last_transcript: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerMessage {
    pub protocol_version: u16,
    #[serde(flatten)]
    pub kind: ServerMessageKind,
}

impl ServerMessage {
    #[must_use]
    pub fn snapshot(request_id: u64, snapshot: AppSnapshot, settings: &Settings) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            kind: ServerMessageKind::Snapshot {
                request_id,
                snapshot,
                settings: Box::new(SettingsSnapshot::from(settings)),
            },
        }
    }

    #[must_use]
    pub fn workspace(request_id: u64, workspace: WorkspaceSnapshot) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            kind: ServerMessageKind::Workspace {
                request_id,
                workspace: Box::new(workspace),
            },
        }
    }

    #[must_use]
    pub fn history_page(request_id: u64, page: HistoryPageSnapshot) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            kind: ServerMessageKind::HistoryPage {
                request_id,
                page: Box::new(page),
            },
        }
    }

    #[must_use]
    pub fn command_rejected(request_id: u64, error: impl Into<String>) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            kind: ServerMessageKind::CommandRejected {
                request_id,
                error: error.into(),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "message", rename_all = "snake_case")]
pub enum ServerMessageKind {
    Snapshot {
        request_id: u64,
        snapshot: AppSnapshot,
        settings: Box<SettingsSnapshot>,
    },
    Workspace {
        request_id: u64,
        workspace: Box<WorkspaceSnapshot>,
    },
    HistoryPage {
        request_id: u64,
        page: Box<HistoryPageSnapshot>,
    },
    CommandRejected {
        request_id: u64,
        error: String,
    },
}
