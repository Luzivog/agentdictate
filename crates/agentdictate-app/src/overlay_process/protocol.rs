use std::path::PathBuf;

use agentdictate_core::WorkflowSnapshot;
use agentdictate_ui::{ActiveRecordingPresentation, OverlayPresentation};
use serde::{Deserialize, Serialize};

pub(super) const OVERLAY_HELPER_ARGUMENT: &str = "--overlay-helper";

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(super) enum OverlayHelperStatus {
    WindowCreated,
    FrameSubmitted,
    Error { message: String },
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
}

impl OverlayUpdate {
    pub fn presentation(&self) -> OverlayPresentation {
        OverlayPresentation {
            workflow: self.workflow,
            active_recording: self.active_recording.as_ref().map(|recording| {
                ActiveRecordingPresentation {
                    audio_path: recording.audio_path.clone(),
                    started_at_unix_millis: recording.started_at_unix_millis,
                }
            }),
        }
    }
}
