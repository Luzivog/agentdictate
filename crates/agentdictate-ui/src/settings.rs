use agentdictate_core::{Settings, TranscriptionProvider};
use thiserror::Error;

/// Values edited in the settings form before validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsDraft {
    pub transcription_provider: TranscriptionProvider,
    pub transcription_model: String,
    pub custom_transcription_model: String,
    pub language: String,
    pub transcription_prompt: String,
    pub cleanup_enabled: bool,
    pub cleanup_model: String,
    pub custom_cleanup_model: String,
    pub cleanup_reasoning_effort: String,
    pub cleanup_style: String,
    pub cleanup_prompt: String,
    pub hotkey: String,
    pub recording_mode: String,
    pub max_recording_seconds: String,
    pub audio_ducking_enabled: bool,
    pub audio_ducking_volume_percent: String,
    pub paste_shortcut: String,
    pub start_on_login: bool,
    pub save_history: bool,
    pub preserve_temp_audio: bool,
}

impl SettingsDraft {
    #[must_use]
    pub fn is_dirty_against(&self, persisted: &Settings) -> bool {
        self != &Self::from(persisted)
    }

    pub fn discard_changes(&mut self, persisted: &Settings) {
        *self = Self::from(persisted);
    }

    pub fn apply_to(&self, current: &Settings) -> Result<Settings, SettingsDraftError> {
        let transcription_model = if self.transcription_provider == TranscriptionProvider::OpenAiApi
        {
            required("Transcription model", &self.transcription_model)?
        } else {
            self.transcription_model.trim().to_owned()
        };
        let cleanup_model = required("Cleanup model", &self.cleanup_model)?;
        let custom_transcription_model = if self.transcription_provider
            == TranscriptionProvider::OpenAiApi
            && transcription_model == "Custom"
        {
            required(
                "Custom transcription model",
                &self.custom_transcription_model,
            )?
        } else {
            self.custom_transcription_model.trim().to_owned()
        };
        let custom_cleanup_model = if cleanup_model == "Custom" {
            required("Custom cleanup model", &self.custom_cleanup_model)?
        } else {
            self.custom_cleanup_model.trim().to_owned()
        };
        let hotkey = required("Global shortcut", &self.hotkey)?;
        let paste_shortcut = required("Paste shortcut", &self.paste_shortcut)?;
        let recording_mode = self.recording_mode.trim().to_ascii_lowercase();
        if !matches!(recording_mode.as_str(), "toggle" | "hold") {
            return Err(SettingsDraftError::InvalidRecordingMode);
        }
        let max_recording_seconds =
            parse_number::<u32>("Maximum recording seconds", &self.max_recording_seconds)?;
        let audio_ducking_volume_percent =
            parse_number::<u8>("Ducked volume", &self.audio_ducking_volume_percent)?;
        if audio_ducking_volume_percent > 100 {
            return Err(SettingsDraftError::DuckedVolumeOutOfRange);
        }

        Ok(Settings {
            transcription_provider: self.transcription_provider,
            transcription_model,
            custom_transcription_model,
            language: self.language.trim().to_owned(),
            transcription_prompt: self.transcription_prompt.trim().to_owned(),
            cleanup_enabled: self.cleanup_enabled,
            cleanup_model,
            custom_cleanup_model,
            cleanup_reasoning_effort: self.cleanup_reasoning_effort.trim().to_owned(),
            cleanup_style: self.cleanup_style.trim().to_owned(),
            cleanup_prompt: self.cleanup_prompt.trim().to_owned(),
            hotkey,
            recording_mode,
            max_recording_seconds,
            audio_ducking_enabled: self.audio_ducking_enabled,
            audio_ducking_volume_percent,
            paste_shortcut,
            start_on_login: self.start_on_login,
            save_history: self.save_history,
            preserve_temp_audio: self.preserve_temp_audio,
            ..current.clone()
        })
    }
}

impl From<&Settings> for SettingsDraft {
    fn from(settings: &Settings) -> Self {
        Self {
            transcription_provider: settings.transcription_provider,
            transcription_model: settings.transcription_model.clone(),
            custom_transcription_model: settings.custom_transcription_model.clone(),
            language: settings.language.clone(),
            transcription_prompt: settings.transcription_prompt.clone(),
            cleanup_enabled: settings.cleanup_enabled,
            cleanup_model: settings.cleanup_model.clone(),
            custom_cleanup_model: settings.custom_cleanup_model.clone(),
            cleanup_reasoning_effort: settings.cleanup_reasoning_effort.clone(),
            cleanup_style: settings.cleanup_style.clone(),
            cleanup_prompt: settings.cleanup_prompt.clone(),
            hotkey: settings.hotkey.clone(),
            recording_mode: settings.recording_mode.clone(),
            max_recording_seconds: settings.max_recording_seconds.to_string(),
            audio_ducking_enabled: settings.audio_ducking_enabled,
            audio_ducking_volume_percent: settings.audio_ducking_volume_percent.to_string(),
            paste_shortcut: settings.paste_shortcut.clone(),
            start_on_login: settings.start_on_login,
            save_history: settings.save_history,
            preserve_temp_audio: settings.preserve_temp_audio,
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SettingsDraftError {
    #[error("{0} cannot be blank")]
    Required(&'static str),
    #[error("Recording mode must be toggle or hold")]
    InvalidRecordingMode,
    #[error("{field} must be a whole number")]
    InvalidNumber { field: &'static str },
    #[error("Ducked volume must be between 0 and 100")]
    DuckedVolumeOutOfRange,
}

fn required(field: &'static str, value: &str) -> Result<String, SettingsDraftError> {
    let value = value.trim();
    if value.is_empty() {
        Err(SettingsDraftError::Required(field))
    } else {
        Ok(value.to_owned())
    }
}

fn parse_number<T>(field: &'static str, value: &str) -> Result<T, SettingsDraftError>
where
    T: std::str::FromStr,
{
    value
        .trim()
        .parse()
        .map_err(|_| SettingsDraftError::InvalidNumber { field })
}
