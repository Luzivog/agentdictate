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
    /// "About your work": names, topics and jargon the user often mentions,
    /// sent to the model as context. It absorbed the retired
    /// `project_context` setting; see `load_settings`.
    pub transcription_prompt: String,
    pub vocabulary: Vec<crate::VocabularyEntry>,
    pub dictation_mode: crate::DictationMode,
    pub streaming_enabled: bool,
    #[serde(deserialize_with = "crate::hotkey::deserialize_hotkey")]
    pub hotkey: crate::Hotkey,
    pub recording_mode: RecordingMode,
    pub max_recording_seconds: u32,
    pub audio_ducking_enabled: bool,
    pub audio_ducking_volume_percent: u8,
    pub audio_ducking_fade_out_ms: u32,
    pub audio_ducking_fade_in_ms: u32,
    pub start_on_login: bool,
    pub show_tray_icon: bool,
    pub preserve_temp_audio: bool,
    #[serde(
        alias = "save_history",
        deserialize_with = "deserialize_keep_transcripts"
    )]
    pub keep_transcripts: KeepTranscripts,
    pub paste_shortcut: PasteShortcut,
}

/// The languages the settings window offers, as (stored code, name). The
/// empty code detects the language; "en,fr" hints both.
pub const LANGUAGES: [(&str, &str); 15] = [
    ("", "Detect automatically"),
    ("en", "English"),
    ("en,fr", "English and French"),
    ("fr", "French"),
    ("es", "Spanish"),
    ("de", "German"),
    ("pt", "Portuguese"),
    ("it", "Italian"),
    ("nl", "Dutch"),
    ("pl", "Polish"),
    ("ar", "Arabic"),
    ("zh", "Chinese"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("hi", "Hindi"),
];

/// One edit to one setting. The settings window sends one as each control
/// changes, and the daemon applies it to the settings it holds, so edits
/// from two windows, or from Settings and Words, never undo each other.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "setting", content = "value", rename_all = "snake_case")]
pub enum SettingChange {
    Language(String),
    Hotkey(crate::Hotkey),
    RecordingMode(RecordingMode),
    AudioDuckingEnabled(bool),
    KeepTranscripts(KeepTranscripts),
    StartOnLogin(bool),
    TranscriptionPrompt(String),
    DictationMode(crate::DictationMode),
    PasteShortcut(PasteShortcut),
    MaxRecordingSeconds(u32),
    AudioDuckingVolumePercent(u8),
    PreserveTempAudio(bool),
    Vocabulary(Vec<crate::VocabularyEntry>),
}

impl SettingChange {
    /// Writes this change into `settings`, or says why its value is refused.
    /// The daemon also checks that it can listen for a new hotkey.
    pub fn apply(self, settings: &mut Settings) -> Result<(), SettingsError> {
        match self {
            Self::Language(language) => {
                let language = language.trim();
                if !LANGUAGES.iter().any(|(code, _)| *code == language) {
                    return Err(SettingsError::UnknownLanguage);
                }
                language.clone_into(&mut settings.language);
            }
            Self::Hotkey(hotkey) => settings.hotkey = hotkey,
            Self::RecordingMode(mode) => settings.recording_mode = mode,
            Self::AudioDuckingEnabled(enabled) => settings.audio_ducking_enabled = enabled,
            Self::KeepTranscripts(keep) => settings.keep_transcripts = keep,
            Self::StartOnLogin(start) => settings.start_on_login = start,
            Self::TranscriptionPrompt(prompt) => {
                prompt.trim().clone_into(&mut settings.transcription_prompt);
            }
            Self::DictationMode(mode) => settings.dictation_mode = mode,
            Self::PasteShortcut(shortcut) => settings.paste_shortcut = shortcut,
            Self::MaxRecordingSeconds(seconds) => settings.max_recording_seconds = seconds,
            Self::AudioDuckingVolumePercent(percent) => {
                if percent > 100 {
                    return Err(SettingsError::DuckedVolumeOutOfRange);
                }
                settings.audio_ducking_volume_percent = percent;
            }
            Self::PreserveTempAudio(preserve) => settings.preserve_temp_audio = preserve,
            Self::Vocabulary(vocabulary) => {
                crate::validate_vocabulary(&vocabulary)?;
                settings.vocabulary = vocabulary;
            }
        }
        Ok(())
    }
}

/// Why a `SettingChange` or a stored choice was refused.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SettingsError {
    #[error("Choose a language from the list")]
    UnknownLanguage,
    #[error("Ducked volume must be between 0 and 100")]
    DuckedVolumeOutOfRange,
    #[error(transparent)]
    Vocabulary(#[from] crate::VocabularyError),
    #[error("Recording mode must be toggle or hold")]
    UnknownRecordingMode,
    #[error("Paste shortcut must be automatic, standard, or terminal")]
    UnknownPasteShortcut,
    #[error("Keep transcripts must be never, 30_days, or forever")]
    UnknownKeepTranscripts,
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

/// How long History keeps the text of a finished dictation. Its usage
/// numbers (duration, words, model, cost) are kept either way.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum KeepTranscripts {
    /// No text is stored after the paste, and stored text is deleted.
    #[serde(rename = "never")]
    Never,
    /// Text is deleted 30 days after its dictation.
    #[serde(rename = "30_days")]
    Days30,
    #[default]
    #[serde(rename = "forever")]
    Forever,
}

impl KeepTranscripts {
    /// The stored name, as config.json and the settings form spell it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::Days30 => "30_days",
            Self::Forever => "forever",
        }
    }

    /// How long text stays after its dictation ends, or `None` for no limit.
    #[must_use]
    pub const fn limit(self) -> Option<chrono::TimeDelta> {
        match self {
            Self::Never => Some(chrono::TimeDelta::zero()),
            Self::Days30 => Some(chrono::TimeDelta::days(30)),
            Self::Forever => None,
        }
    }
}

impl std::str::FromStr for KeepTranscripts {
    type Err = SettingsError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        [Self::Never, Self::Days30, Self::Forever]
            .into_iter()
            .find(|keep| keep.as_str() == value)
            .ok_or(SettingsError::UnknownKeepTranscripts)
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
            dictation_mode: crate::DictationMode::Dictate,
            streaming_enabled: false,
            hotkey: crate::Hotkey::default(),
            recording_mode: RecordingMode::Toggle,
            max_recording_seconds: 300,
            audio_ducking_enabled: true,
            audio_ducking_volume_percent: 15,
            audio_ducking_fade_out_ms: 600,
            audio_ducking_fade_in_ms: 600,
            start_on_login: true,
            show_tray_icon: true,
            preserve_temp_audio: false,
            keep_transcripts: KeepTranscripts::Forever,
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

/// Reads `keep_transcripts`, or the `save_history` switch it replaced: on
/// kept transcripts forever, off kept none.
fn deserialize_keep_transcripts<'de, D>(deserializer: D) -> Result<KeepTranscripts, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Stored {
        Choice(KeepTranscripts),
        SaveHistory(bool),
    }
    Ok(match Stored::deserialize(deserializer)? {
        Stored::Choice(keep) => keep,
        Stored::SaveHistory(true) => KeepTranscripts::Forever,
        Stored::SaveHistory(false) => KeepTranscripts::Never,
    })
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
