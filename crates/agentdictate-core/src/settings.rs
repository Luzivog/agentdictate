use std::fmt;

use serde::{Deserialize, Serialize};

/// The OpenAI model every dictation uses. `Settings::transcription_model` can
/// override it from config.json to try a newer model; the app has no picker.
pub const TRANSCRIPTION_MODEL: &str = "gpt-transcribe";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum TranscriptionProvider {
    #[default]
    #[serde(rename = "openai_api")]
    OpenAiApi,
    #[serde(rename = "chatgpt_subscription")]
    ChatGptSubscription,
}

impl TranscriptionProvider {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiApi => "openai_api",
            Self::ChatGptSubscription => "chatgpt_subscription",
        }
    }

    /// Returns the Platform API transcription price for this route.
    /// The ChatGPT route does not send a Platform API billing credential.
    #[must_use]
    pub const fn marginal_price_per_audio_minute(self, openai_api_price: f64) -> f64 {
        match self {
            Self::OpenAiApi => openai_api_price,
            Self::ChatGptSubscription => 0.0,
        }
    }
}

impl std::str::FromStr for TranscriptionProvider {
    type Err = ParseTranscriptionProviderError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "openai_api" => Ok(Self::OpenAiApi),
            "chatgpt_subscription" => Ok(Self::ChatGptSubscription),
            _ => Err(ParseTranscriptionProviderError(value.to_owned())),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseTranscriptionProviderError(String);

impl fmt::Display for ParseTranscriptionProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown transcription provider {:?}", self.0)
    }
}

impl std::error::Error for ParseTranscriptionProviderError {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub openai_api_key: String,
    pub transcription_provider: TranscriptionProvider,
    #[serde(deserialize_with = "deserialize_transcription_model")]
    pub transcription_model: String,
    pub language: String,
    pub transcription_prompt: String,
    pub vocabulary: Vec<crate::VocabularyEntry>,
    pub project_context: String,
    pub dictation_mode: crate::DictationMode,
    pub streaming_enabled: bool,
    pub hotkey: String,
    pub recording_mode: String,
    pub max_recording_seconds: u32,
    pub sound_feedback: bool,
    pub start_sound: bool,
    pub stop_sound: bool,
    pub audio_ducking_enabled: bool,
    pub audio_ducking_volume_percent: u8,
    pub audio_ducking_fade_out_ms: u32,
    pub audio_ducking_fade_in_ms: u32,
    pub start_on_login: bool,
    pub show_tray_icon: bool,
    pub minimize_to_tray_on_close: bool,
    pub launch_window_on_startup: bool,
    pub restore_clipboard_after_paste: bool,
    pub debug_mode: bool,
    pub preserve_temp_audio: bool,
    pub save_history: bool,
    pub paste_shortcut: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            openai_api_key: String::new(),
            transcription_provider: TranscriptionProvider::OpenAiApi,
            transcription_model: TRANSCRIPTION_MODEL.into(),
            language: String::new(),
            transcription_prompt: String::new(),
            vocabulary: Vec::new(),
            project_context: String::new(),
            dictation_mode: crate::DictationMode::Dictate,
            streaming_enabled: false,
            hotkey: "Ctrl+Space".into(),
            recording_mode: "toggle".into(),
            max_recording_seconds: 300,
            sound_feedback: false,
            start_sound: false,
            stop_sound: false,
            audio_ducking_enabled: true,
            audio_ducking_volume_percent: 15,
            audio_ducking_fade_out_ms: 600,
            audio_ducking_fade_in_ms: 600,
            start_on_login: true,
            show_tray_icon: true,
            minimize_to_tray_on_close: true,
            launch_window_on_startup: false,
            restore_clipboard_after_paste: false,
            debug_mode: false,
            preserve_temp_audio: false,
            save_history: true,
            paste_shortcut: "Automatic".into(),
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
