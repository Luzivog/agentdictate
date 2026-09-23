use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::HotkeyReadiness;

/// Whether dictation can work now, as the daemon sees it. Home shows one
/// line when everything is in place, or the most important thing to fix.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Readiness {
    pub shortcut: HotkeyReadiness,
    /// Whether an OpenAI API key is saved.
    pub transcription_key: bool,
    pub desktop: DesktopReadiness,
}

/// Before the daemon has reported: the shortcut is still starting, and
/// nothing is known to be missing.
impl Default for Readiness {
    fn default() -> Self {
        Self {
            shortcut: HotkeyReadiness::Starting,
            transcription_key: true,
            desktop: DesktopReadiness::default(),
        }
    }
}

/// What the desktop provides for dictation, as the daemon last checked it.
/// The default is a desktop with everything in place.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DesktopReadiness {
    /// Whether AgentDictate can type the paste shortcut: `/dev/uinput` is
    /// writable.
    pub paste_access: bool,
    /// Set when any user or app on this computer can read the keyboards or
    /// type into apps: input devices are world-accessible.
    pub exposed_input: Option<ExposedInput>,
    /// Programs AgentDictate uses that are not installed.
    pub missing_tools: Vec<MissingTool>,
}

impl Default for DesktopReadiness {
    fn default() -> Self {
        Self {
            paste_access: true,
            exposed_input: None,
            missing_tools: Vec::new(),
        }
    }
}

/// World-accessible input devices, and the udev rule that makes them so.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExposedInput {
    /// The rule file, when one was found; AgentDictate's own rule never
    /// grants that access.
    pub rule: Option<PathBuf>,
}

/// A program AgentDictate runs.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingTool {
    /// Records the microphone; nothing works without it.
    PwRecord,
    /// Compresses uploads; without it they are sent uncompressed.
    Ffmpeg,
    /// Lowers other sounds while you dictate.
    Pactl,
}

impl MissingTool {
    pub const ALL: [Self; 3] = [Self::PwRecord, Self::Ffmpeg, Self::Pactl];

    #[must_use]
    pub const fn program(self) -> &'static str {
        match self {
            Self::PwRecord => "pw-record",
            Self::Ffmpeg => "ffmpeg",
            Self::Pactl => "pactl",
        }
    }
}
