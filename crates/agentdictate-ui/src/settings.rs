//! What the Settings and Words screens ask the daemon to save.

use std::fmt;

use agentdictate_core::SettingChange;

/// One settings edit the window sends to the daemon. Changes apply as the
/// user makes them; there is no Save button.
#[derive(Clone, PartialEq, Eq)]
pub enum SettingsRequest {
    Change(SettingChange),
    /// Saves a new OpenAI API key.
    SetApiKey(String),
    /// Ends a shortcut capture that is waiting for a key press.
    CancelHotkeyCapture,
}

impl fmt::Debug for SettingsRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Change(change) => formatter.debug_tuple("Change").field(change).finish(),
            Self::SetApiKey(_) => formatter.write_str("SetApiKey([REDACTED])"),
            Self::CancelHotkeyCapture => formatter.write_str("CancelHotkeyCapture"),
        }
    }
}
