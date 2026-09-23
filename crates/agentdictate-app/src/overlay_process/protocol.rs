use std::path::PathBuf;

use agentdictate_core::{DictationNotice, WorkflowSnapshot};
use agentdictate_ui::{ActiveRecordingPresentation, OverlayPresentation, OverlayState};
use serde::{Deserialize, Serialize};

pub(super) const OVERLAY_HELPER_ARGUMENT: &str = "--overlay-helper";

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(super) enum OverlayHelperStatus {
    /// The window exists. `override_redirect` is the X server's answer to
    /// whether the window is unmanaged, so the window manager can never focus
    /// it. A helper that omits the field counts as unconfirmed.
    WindowCreated {
        #[serde(default)]
        override_redirect: bool,
    },
    FrameSubmitted,
    Error {
        message: String,
    },
}

/// Serializable recording metadata for the private daemon-to-overlay pipe.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActiveRecordingUpdate {
    pub audio_path: PathBuf,
    pub started_at_unix_millis: i64,
}

/// Event-driven status update consumed by the short-lived overlay helper.
///
/// This is intentionally separate from the public IPC `AppSnapshot`: only the
/// helper receives the temporary audio path that it samples on its own ticks.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OverlayUpdate {
    pub workflow: WorkflowSnapshot,
    pub active_recording: Option<ActiveRecordingUpdate>,
    /// Sent once, with the update that ends a dictation that was not pasted.
    /// The presenter keeps it up for `OVERLAY_NOTICE_HOLD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<DictationNotice>,
}

impl OverlayUpdate {
    pub fn state(&self) -> OverlayState {
        self.presentation().state()
    }

    pub fn presentation(&self) -> OverlayPresentation {
        OverlayPresentation {
            workflow: self.workflow,
            active_recording: self.active_recording.as_ref().map(|recording| {
                ActiveRecordingPresentation {
                    audio_path: recording.audio_path.clone(),
                    started_at_unix_millis: recording.started_at_unix_millis,
                }
            }),
            notice: self.notice,
        }
    }
}
