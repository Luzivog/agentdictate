use serde::{Deserialize, Serialize};

use crate::hotkey::Hotkey;
use crate::settings::{SecretString, SettingChange, Settings, SettingsSnapshot};
use crate::snapshots::{
    HistoryPageCursor, HistoryPageRequest, HistoryPageSnapshot, WorkspaceSnapshot,
};
use crate::workflow::{JobId, WorkflowSnapshot};

pub const PROTOCOL_VERSION: u16 = 11;

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

    /// Asks the daemon to capture the next shortcut pressed on any keyboard.
    /// The reply is [`ServerMessageKind::HotkeyCaptured`].
    #[must_use]
    pub const fn capture_hotkey(request_id: u64) -> Self {
        Self::with_kind(ClientCommandKind::CaptureHotkey { request_id })
    }

    /// Ends a pending shortcut capture; it replies `Cancelled`.
    #[must_use]
    pub const fn cancel_hotkey_capture(request_id: u64) -> Self {
        Self::with_kind(ClientCommandKind::CancelHotkeyCapture { request_id })
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

    /// Changes one setting; the daemon keeps every other setting it holds.
    #[must_use]
    pub const fn change_setting(request_id: u64, change: SettingChange) -> Self {
        Self::with_kind(ClientCommandKind::ChangeSetting { request_id, change })
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
            ClientCommandKind::GetHistoryPage { .. } => ClientCommandTag::GetHistoryPage,
            ClientCommandKind::StartRecording { .. } => ClientCommandTag::StartRecording,
            ClientCommandKind::StopRecording { .. } => ClientCommandTag::StopRecording,
            ClientCommandKind::Cancel { .. } => ClientCommandTag::Cancel,
            ClientCommandKind::RecorderExited { .. } => ClientCommandTag::RecorderExited,
            ClientCommandKind::RetryTranscription { .. } => ClientCommandTag::RetryTranscription,
            ClientCommandKind::RetryDelivery { .. } => ClientCommandTag::RetryDelivery,
            ClientCommandKind::DeleteRecovery { .. } => ClientCommandTag::DeleteRecovery,
            ClientCommandKind::DeleteHistory { .. } => ClientCommandTag::DeleteHistory,
            ClientCommandKind::ClearHistory { .. } => ClientCommandTag::ClearHistory,
            ClientCommandKind::CopyTranscript { .. } => ClientCommandTag::CopyTranscript,
            ClientCommandKind::ChangeSetting { .. } => ClientCommandTag::ChangeSetting,
            ClientCommandKind::SetApiKey { .. } => ClientCommandTag::SetApiKey,
            ClientCommandKind::HotkeyStatusChanged { .. } => ClientCommandTag::HotkeyStatusChanged,
            ClientCommandKind::CaptureHotkey { .. } => ClientCommandTag::CaptureHotkey,
            ClientCommandKind::CancelHotkeyCapture { .. } => ClientCommandTag::CancelHotkeyCapture,
            ClientCommandKind::Quit { .. } => ClientCommandTag::Quit,
        }
    }
}

/// Data-less discriminator for every command carried by [`ClientCommandKind`].
///
/// This is separate from the payload-bearing wire enum so existing command
/// construction and pattern matching remain unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientCommandTag {
    GetSnapshot,
    GetWorkspace,
    GetHistoryPage,
    StartRecording,
    StopRecording,
    Cancel,
    RecorderExited,
    RetryTranscription,
    RetryDelivery,
    DeleteRecovery,
    DeleteHistory,
    ClearHistory,
    CopyTranscript,
    ChangeSetting,
    SetApiKey,
    HotkeyStatusChanged,
    CaptureHotkey,
    CancelHotkeyCapture,
    Quit,
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
    ChangeSetting {
        request_id: u64,
        change: SettingChange,
    },
    SetApiKey {
        request_id: u64,
        api_key: SecretString,
    },
    HotkeyStatusChanged {
        request_id: u64,
        readiness: HotkeyReadiness,
    },
    CaptureHotkey {
        request_id: u64,
    },
    CancelHotkeyCapture {
        request_id: u64,
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

/// How a shortcut capture ended.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum HotkeyCaptureOutcome {
    Captured {
        hotkey: Hotkey,
    },
    /// Esc was pressed, or the capture was cancelled or replaced.
    Cancelled,
    /// No shortcut was pressed in time.
    TimedOut,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AppSnapshot {
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
    pub const fn hotkey_captured(request_id: u64, outcome: HotkeyCaptureOutcome) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            kind: ServerMessageKind::HotkeyCaptured {
                request_id,
                outcome,
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
    HotkeyCaptured {
        request_id: u64,
        outcome: HotkeyCaptureOutcome,
    },
    CommandRejected {
        request_id: u64,
        error: String,
    },
}
