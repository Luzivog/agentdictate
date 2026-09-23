use std::fmt;

use serde::{Deserialize, Serialize};

/// The OpenAI model every dictation uses. `Settings::transcription_model` can
/// override it from config.json to try a newer model; the app has no picker.
pub const TRANSCRIPTION_MODEL: &str = "gpt-transcribe";

/// The user's configuration, stored in config.json. Missing keys take their
/// defaults and unknown keys, such as retired settings, are ignored. The
/// retired `transcription_provider` key is one: every dictation uses the
/// OpenAI API.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub openai_api_key: String,
    #[serde(deserialize_with = "deserialize_transcription_model")]
    pub transcription_model: String,
    pub language: String,
    pub transcription_prompt: String,
    pub vocabulary: Vec<crate::VocabularyEntry>,
    pub project_context: String,
    pub dictation_mode: crate::DictationMode,
    pub streaming_enabled: bool,
    pub hotkey: String,
    pub recording_mode: RecordingMode,
    pub max_recording_seconds: u32,
    pub audio_ducking_enabled: bool,
    pub audio_ducking_volume_percent: u8,
    pub audio_ducking_fade_out_ms: u32,
    pub audio_ducking_fade_in_ms: u32,
    pub start_on_login: bool,
    pub show_tray_icon: bool,
    pub preserve_temp_audio: bool,
    pub save_history: bool,
    pub paste_shortcut: PasteShortcut,
}

impl Settings {
    /// Rejects values that cannot work together. Loading config.json does
    /// not validate, so a hand edit never stops the daemon from starting.
    pub fn validate(&self) -> Result<(), SettingsError> {
        if self.audio_ducking_volume_percent > 100 {
            return Err(SettingsError::DuckedVolumeOutOfRange);
        }
        Ok(())
    }
}

/// Why `Settings::validate` rejected a change.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SettingsError {
    #[error("Ducked volume must be between 0 and 100")]
    DuckedVolumeOutOfRange,
    #[error("Recording mode must be toggle or hold")]
    UnknownRecordingMode,
    #[error("Paste shortcut must be automatic, standard, or terminal")]
    UnknownPasteShortcut,
}

/// How the global shortcut records: press it once to start and again to
/// stop, or hold it down while speaking.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingMode {
    #[default]
    #[serde(alias = "Toggle")]
    Toggle,
    #[serde(alias = "Hold")]
    Hold,
}

impl RecordingMode {
    /// The stored name, as config.json and the settings form spell it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Toggle => "toggle",
            Self::Hold => "hold",
        }
    }
}

impl std::str::FromStr for RecordingMode {
    type Err = SettingsError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        [Self::Toggle, Self::Hold]
            .into_iter()
            .find(|mode| mode.as_str() == value)
            .ok_or(SettingsError::UnknownRecordingMode)
    }
}

/// The keys that paste a transcript. `Automatic` picks them per target
/// app; the other two always send Ctrl+V or Ctrl+Shift+V. The aliases are
/// the labels that older settings windows stored.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PasteShortcut {
    #[default]
    #[serde(alias = "Automatic")]
    Automatic,
    #[serde(alias = "Standard (Ctrl+V)", alias = "Ctrl+V")]
    Standard,
    #[serde(alias = "Terminal (Ctrl+Shift+V)", alias = "Ctrl+Shift+V")]
    Terminal,
}

impl PasteShortcut {
    /// The stored name, as config.json and the settings form spell it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Standard => "standard",
            Self::Terminal => "terminal",
        }
    }
}

impl std::str::FromStr for PasteShortcut {
    type Err = SettingsError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        [Self::Automatic, Self::Standard, Self::Terminal]
            .into_iter()
            .find(|shortcut| shortcut.as_str() == value)
            .ok_or(SettingsError::UnknownPasteShortcut)
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            openai_api_key: String::new(),
            transcription_model: TRANSCRIPTION_MODEL.into(),
            language: String::new(),
            transcription_prompt: String::new(),
            vocabulary: Vec::new(),
            project_context: String::new(),
            dictation_mode: crate::DictationMode::Dictate,
            streaming_enabled: false,
            hotkey: "Ctrl+Space".into(),
            recording_mode: RecordingMode::Toggle,
            max_recording_seconds: 300,
            audio_ducking_enabled: true,
            audio_ducking_volume_percent: 15,
            audio_ducking_fade_out_ms: 600,
            audio_ducking_fade_in_ms: 600,
            start_on_login: true,
            show_tray_icon: true,
            preserve_temp_audio: false,
            save_history: true,
            paste_shortcut: PasteShortcut::Automatic,
        }
    }
}

/// Reads a stored model override. Models OpenAI shuts down on 2027-02-26
/// (whisper-1 and the gpt-4o transcribe family), a blank value, and the old
/// picker's "Custom" sentinel all mean the built-in model.
fn deserialize_transcription_model<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let model = String::deserialize(deserializer)?;
    let model = model.trim();
    let retired = matches!(model, "" | "Custom" | "whisper-1")
        || model.starts_with("gpt-4o-transcribe")
        || model.starts_with("gpt-4o-mini-transcribe");
    Ok(if retired { TRANSCRIPTION_MODEL } else { model }.to_owned())
}

/// Settings projection safe to send to presentation processes and diagnostics.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SettingsSnapshot {
    pub values: Settings,
    pub has_api_key: bool,
}

impl From<&Settings> for SettingsSnapshot {
    fn from(settings: &Settings) -> Self {
        let has_api_key = !settings.openai_api_key.trim().is_empty();
        let mut values = settings.clone();
        values.openai_api_key.clear();
        Self {
            values,
            has_api_key,
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretString(pub(crate) String);

impl SecretString {
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}
