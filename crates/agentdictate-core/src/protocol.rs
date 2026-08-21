use serde::{Deserialize, Serialize};

use crate::replacements::ReplacementRule;
use crate::settings::{SecretString, Settings, SettingsSnapshot};
use crate::snapshots::{
    HistoryPageCursor, HistoryPageRequest, HistoryPageSnapshot, WorkspaceSnapshot,
};
use crate::workflow::{JobId, WorkflowSnapshot};

pub const PROTOCOL_VERSION: u16 = 2;

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
        Self::with_kind(ClientCommandKind::StartRecording { request_id })
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
