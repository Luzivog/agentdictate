use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::hotkey::Hotkey;
use crate::settings::{SecretString, SettingChange, Settings, SettingsSnapshot};
use crate::workflow::{JobId, WorkflowSnapshot};

/// The IPC wire format's version. The settings window reads History, usage
/// and Recovery from the database itself; IPC carries commands and the
/// status snapshot.
pub const PROTOCOL_VERSION: u16 = 15;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClientCommand {
    pub protocol_version: u16,
    #[serde(flatten)]
    pub kind: ClientCommandKind,
}

impl From<ClientCommandKind> for ClientCommand {
    fn from(kind: ClientCommandKind) -> Self {
        Self::new(kind)
    }
}

impl ClientCommand {
    #[must_use]
    pub const fn new(kind: ClientCommandKind) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            kind,
        }
    }

    #[must_use]
    pub const fn start_recording() -> Self {
        Self::new(ClientCommandKind::StartRecording { mode: None })
    }

    /// Overrides output mode for this recording without changing saved settings.
    #[must_use]
    pub const fn start_recording_in_mode(mode: crate::DictationMode) -> Self {
        Self::new(ClientCommandKind::StartRecording { mode: Some(mode) })
    }

    /// Changes one setting; the daemon keeps every other setting it holds.
    #[must_use]
    pub const fn change_setting(change: SettingChange) -> Self {
        Self::new(ClientCommandKind::ChangeSetting { change })
    }

    #[must_use]
    pub fn set_api_key(api_key: impl Into<String>) -> Self {
        Self::new(ClientCommandKind::SetApiKey {
            api_key: SecretString(api_key.into()),
        })
    }
}

/// Every command a client can send. A reply always answers the command just
/// sent on the same connection, so commands carry no request id. Unless a
/// variant says otherwise, success replies with the status snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum ClientCommandKind {
    GetSnapshot,
    StartRecording {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<crate::DictationMode>,
    },
    /// Stops the recording; its transcription continues after the reply.
    StopRecording,
    /// Discards a recording, or stops waiting for a transcription, whose
    /// result then waits in Recovery.
    Cancel,
    RetryTranscription {
        job_id: JobId,
    },
    RetryDelivery {
        job_id: JobId,
    },
    DeleteRecovery {
        job_id: JobId,
    },
    DeleteHistory {
        id: i64,
    },
    ClearHistory,
    CopyTranscript {
        id: i64,
    },
    /// Captures the next shortcut pressed on any keyboard; the reply is
    /// [`ServerMessageKind::HotkeyCaptured`].
    CaptureHotkey,
    /// Ends a pending shortcut capture; it replies `Cancelled`.
    CancelHotkeyCapture,
    ChangeSetting {
        change: SettingChange,
    },
    SetApiKey {
        api_key: SecretString,
    },
    Quit,
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

/// The daemon's status: what the database does not hold.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AppSnapshot {
    pub workflow: WorkflowSnapshot,
    pub hotkey: HotkeyReadiness,
    pub recoverable_count: usize,
    pub last_transcript: Option<String>,
    /// The recording overlay could not be shown; dictation still works.
    pub overlay_unavailable: bool,
    /// Where the daemon moved a history database it could not read when it
    /// started; a fresh one replaced it. Reported until the daemon restarts.
    pub history_set_aside: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerMessage {
    pub protocol_version: u16,
    #[serde(flatten)]
    pub kind: ServerMessageKind,
}

impl ServerMessage {
    const fn new(kind: ServerMessageKind) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            kind,
        }
    }

    #[must_use]
    pub fn snapshot(snapshot: AppSnapshot, settings: &Settings) -> Self {
        Self::new(ServerMessageKind::Snapshot {
            snapshot,
            settings: Box::new(SettingsSnapshot::from(settings)),
        })
    }

    #[must_use]
    pub const fn hotkey_captured(outcome: HotkeyCaptureOutcome) -> Self {
        Self::new(ServerMessageKind::HotkeyCaptured { outcome })
    }

    #[must_use]
    pub fn command_rejected(error: impl Into<String>) -> Self {
        Self::new(ServerMessageKind::CommandRejected {
            error: error.into(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "message", rename_all = "snake_case")]
pub enum ServerMessageKind {
    Snapshot {
        snapshot: AppSnapshot,
        settings: Box<SettingsSnapshot>,
    },
    HotkeyCaptured {
        outcome: HotkeyCaptureOutcome,
    },
    CommandRejected {
        error: String,
    },
}
