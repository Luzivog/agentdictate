//! What the Setup screen needs from the rest of AgentDictate, and when the
//! window opens on it.

use std::sync::Arc;

use agentdictate_core::{ApiKeyCheck, HotkeyReadiness, MicrophoneCheck, Readiness};

use crate::UiActionError;

/// What the Setup screen asks of AgentDictate. Each call blocks, so the
/// window makes it off the UI thread.
pub trait SetupActions: Send + Sync {
    /// Asks OpenAI whether `api_key` works, or the saved key when it is
    /// `None`. Saves nothing.
    fn check_api_key(&self, api_key: Option<String>) -> Result<ApiKeyCheck, UiActionError>;

    /// Gives AgentDictate keyboard and paste access once the user enters
    /// their password, then returns the daemon's readiness: with the access
    /// in place, or as it still is when that needs a new login.
    fn grant_access(&self) -> Result<Readiness, UiActionError>;

    /// Listens to the microphone for a few seconds, calling `level` with how
    /// loud it is, from 0 to 100, as it hears it.
    fn test_microphone(&self, level: &mut dyn FnMut(u8)) -> Result<MicrophoneCheck, UiActionError>;
}

pub type SetupSink = Arc<dyn SetupActions>;

/// Whether the window opens on Setup: dictation can't work yet because the
/// API key is missing, or AgentDictate can't read the shortcut or paste.
#[must_use]
pub fn needs_setup(readiness: &Readiness) -> bool {
    !readiness.transcription_key || !has_input_access(readiness)
}

/// Whether AgentDictate can read the shortcut and paste. The shortcut
/// listener may still be starting.
#[must_use]
pub fn has_input_access(readiness: &Readiness) -> bool {
    readiness.desktop.paste_access
        && !matches!(readiness.shortcut, HotkeyReadiness::Unavailable { .. })
}
