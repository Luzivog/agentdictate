use std::fmt::Display;

use agentdictate_core::{Settings, TranscriptionProvider};
use thiserror::Error;

macro_rules! settings_fields {
    ($callback:ident) => {
        $callback! {
            select {
                transcription_provider: TranscriptionProvider {
                    from: copied,
                    apply: value(copied),
                    options: plain(transcription_provider_options),
                    searchable: false,
                    read: provider,
                },
                transcription_model: String {
                    from: cloned,
                    apply: validate_draft(validated_transcription_model),
                    options: catalog(transcription_model_options),
                    searchable: true,
                    read: string,
                },
                language: String {
                    from: cloned,
                    apply: value(trimmed),
                    options: plain(language_options),
                    searchable: true,
                    read: string,
                },
                cleanup_model: String {
                    from: cloned,
                    apply: validate_field(validated_cleanup_model),
                    options: catalog(cleanup_model_options),
                    searchable: true,
                    read: string,
                },
                cleanup_style: String {
                    from: cloned,
                    apply: value(trimmed),
                    options: plain(cleanup_style_options),
                    searchable: false,
                    read: string,
                },
                recording_mode: String {
                    from: cloned,
                    apply: validate_field(validated_recording_mode),
                    options: plain(recording_mode_options),
                    searchable: false,
                    read: string,
                },
                paste_shortcut: String {
                    from: cloned,
                    apply: validate_field(validated_paste_shortcut),
                    options: plain(paste_shortcut_options),
                    searchable: false,
                    read: string,
                },
            }
            dependent_select {
                cleanup_reasoning_effort: String {
                    from: cloned,
                    apply: value(trimmed),
                    depends_on: cleanup_model,
                    options: reasoning_effort_options,
                },
            }
            input {
                custom_transcription_model: String {
                    from: cloned,
                    apply: validate_draft(validated_custom_transcription_model),
                    placeholder: "Custom OpenAI model",
                },
                custom_cleanup_model: String {
                    from: cloned,
                    apply: validate_draft(validated_custom_cleanup_model),
                    placeholder: "Custom cleanup model",
                },
            }
            text_area {
                transcription_prompt: String {
                    from: cloned,
                    apply: value(trimmed),
                    placeholder: "Names and technical context",
                    rows: 2..=5,
                },
                cleanup_prompt: String {
                    from: cloned,
                    apply: value(trimmed),
                    placeholder: "Cleanup instructions",
                    rows: 3..=6,
                },
            }
            number {
                max_recording_seconds: String {
                    from: stringified,
                    apply: validate_field(parsed_max_recording_seconds),
                    placeholder: "300",
                    maximum: u64::from(u32::MAX),
                },
                audio_ducking_volume_percent: String {
                    from: stringified,
                    apply: validate_field(validated_ducking_volume),
                    placeholder: "15",
                    maximum: 100,
                },
                audio_ducking_fade_out_ms: String {
                    from: stringified,
                    apply: validate_field(parsed_ducking_fade_out_ms),
                    placeholder: "600",
                    maximum: u64::from(u32::MAX),
                },
                audio_ducking_fade_in_ms: String {
                    from: stringified,
                    apply: validate_field(parsed_ducking_fade_in_ms),
                    placeholder: "600",
                    maximum: u64::from(u32::MAX),
                },
            }
            draft_only {
                cleanup_enabled: bool {
                    from: copied,
                    apply: value(copied),
                },
                audio_ducking_enabled: bool {
                    from: copied,
                    apply: value(copied),
                },
                start_on_login: bool {
                    from: copied,
                    apply: value(copied),
                },
                save_history: bool {
                    from: copied,
                    apply: value(copied),
                },
                preserve_temp_audio: bool {
                    from: copied,
                    apply: value(copied),
                },
            }
            shortcut {
                hotkey: String {
                    from: cloned,
                    apply: validate_field(validated_hotkey),
                },
            }
        }
    };
}

#[cfg_attr(not(feature = "desktop"), allow(unused_imports))]
pub(crate) use settings_fields;

macro_rules! from_settings_field {
    ($settings:ident, $field:ident, $converter:ident) => {
        $converter(&$settings.$field)
    };
}

macro_rules! apply_settings_field {
    ($draft:ident, $field:ident, value($converter:ident)) => {
        $converter(&$draft.$field)
    };
    ($draft:ident, $field:ident, validate_field($validator:ident)) => {
        $validator(&$draft.$field)?
    };
    ($draft:ident, $field:ident, validate_draft($validator:ident)) => {
        $validator($draft)?
    };
}

macro_rules! define_settings_draft {
    (
        select {
            $(
                $select_field:ident: $select_type:ty {
                    from: $select_from:ident,
                    apply: $select_apply_kind:ident($select_apply:ident),
                    $($select_control:tt)*
                },
            )*
        }
        dependent_select {
            $(
                $dependent_field:ident: $dependent_type:ty {
                    from: $dependent_from:ident,
                    apply: $dependent_apply_kind:ident($dependent_apply:ident),
                    $($dependent_control:tt)*
                },
            )*
        }
        input {
            $(
                $input_field:ident: $input_type:ty {
                    from: $input_from:ident,
                    apply: $input_apply_kind:ident($input_apply:ident),
                    $($input_control:tt)*
                },
            )*
        }
        text_area {
            $(
                $text_area_field:ident: $text_area_type:ty {
                    from: $text_area_from:ident,
                    apply: $text_area_apply_kind:ident($text_area_apply:ident),
                    $($text_area_control:tt)*
                },
            )*
        }
        number {
            $(
                $number_field:ident: $number_type:ty {
                    from: $number_from:ident,
                    apply: $number_apply_kind:ident($number_apply:ident),
                    $($number_control:tt)*
                },
            )*
        }
        draft_only {
            $(
                $draft_only_field:ident: $draft_only_type:ty {
                    from: $draft_only_from:ident,
                    apply: $draft_only_apply_kind:ident($draft_only_apply:ident),
                },
            )*
        }
        shortcut {
            $(
                $shortcut_field:ident: $shortcut_type:ty {
                    from: $shortcut_from:ident,
                    apply: $shortcut_apply_kind:ident($shortcut_apply:ident),
                },
            )*
        }
    ) => {
        /// Values edited in the settings form before validation.
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct SettingsDraft {
            $(pub $select_field: $select_type,)*
            $(pub $dependent_field: $dependent_type,)*
            $(pub $input_field: $input_type,)*
            $(pub $text_area_field: $text_area_type,)*
            $(pub $number_field: $number_type,)*
            $(pub $draft_only_field: $draft_only_type,)*
            $(pub $shortcut_field: $shortcut_type,)*
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
                let mut updated = current.clone();
                $(updated.$select_field = apply_settings_field!(self, $select_field, $select_apply_kind($select_apply));)*
                $(updated.$dependent_field = apply_settings_field!(self, $dependent_field, $dependent_apply_kind($dependent_apply));)*
                $(updated.$input_field = apply_settings_field!(self, $input_field, $input_apply_kind($input_apply));)*
                $(updated.$text_area_field = apply_settings_field!(self, $text_area_field, $text_area_apply_kind($text_area_apply));)*
                $(updated.$number_field = apply_settings_field!(self, $number_field, $number_apply_kind($number_apply));)*
                $(updated.$draft_only_field = apply_settings_field!(self, $draft_only_field, $draft_only_apply_kind($draft_only_apply));)*
                $(updated.$shortcut_field = apply_settings_field!(self, $shortcut_field, $shortcut_apply_kind($shortcut_apply));)*
                Ok(updated)
            }
        }

        impl From<&Settings> for SettingsDraft {
            fn from(settings: &Settings) -> Self {
                Self {
                    $($select_field: from_settings_field!(settings, $select_field, $select_from),)*
                    $($dependent_field: from_settings_field!(settings, $dependent_field, $dependent_from),)*
                    $($input_field: from_settings_field!(settings, $input_field, $input_from),)*
                    $($text_area_field: from_settings_field!(settings, $text_area_field, $text_area_from),)*
                    $($number_field: from_settings_field!(settings, $number_field, $number_from),)*
                    $($draft_only_field: from_settings_field!(settings, $draft_only_field, $draft_only_from),)*
                    $($shortcut_field: from_settings_field!(settings, $shortcut_field, $shortcut_from),)*
                }
            }
        }
    };
}

settings_fields!(define_settings_draft);

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

fn copied<T: Copy>(value: &T) -> T {
    *value
}

fn cloned<T: Clone>(value: &T) -> T {
    value.clone()
}

fn stringified(value: &impl Display) -> String {
    value.to_string()
}

fn trimmed(value: &str) -> String {
    value.trim().to_owned()
}

fn validated_transcription_model(draft: &SettingsDraft) -> Result<String, SettingsDraftError> {
    if draft.transcription_provider == TranscriptionProvider::OpenAiApi {
        required("Transcription model", &draft.transcription_model)
    } else {
        Ok(trimmed(&draft.transcription_model))
    }
}

fn validated_custom_transcription_model(
    draft: &SettingsDraft,
) -> Result<String, SettingsDraftError> {
    if draft.transcription_provider == TranscriptionProvider::OpenAiApi
        && draft.transcription_model.trim() == "Custom"
    {
        required(
            "Custom transcription model",
            &draft.custom_transcription_model,
        )
    } else {
        Ok(trimmed(&draft.custom_transcription_model))
    }
}

fn validated_cleanup_model(value: &str) -> Result<String, SettingsDraftError> {
    required("Cleanup model", value)
}

fn validated_custom_cleanup_model(draft: &SettingsDraft) -> Result<String, SettingsDraftError> {
    if draft.cleanup_model.trim() == "Custom" {
        required("Custom cleanup model", &draft.custom_cleanup_model)
    } else {
        Ok(trimmed(&draft.custom_cleanup_model))
    }
}

fn validated_hotkey(value: &str) -> Result<String, SettingsDraftError> {
    required("Global shortcut", value)
}

fn validated_recording_mode(value: &str) -> Result<String, SettingsDraftError> {
    let recording_mode = value.trim().to_ascii_lowercase();
    if matches!(recording_mode.as_str(), "toggle" | "hold") {
        Ok(recording_mode)
    } else {
        Err(SettingsDraftError::InvalidRecordingMode)
    }
}

fn parsed_max_recording_seconds(value: &str) -> Result<u32, SettingsDraftError> {
    parse_number("Maximum recording seconds", value)
}

fn validated_ducking_volume(value: &str) -> Result<u8, SettingsDraftError> {
    let volume = parse_number("Ducked volume", value)?;
    if volume > 100 {
        Err(SettingsDraftError::DuckedVolumeOutOfRange)
    } else {
        Ok(volume)
    }
}

fn parsed_ducking_fade_out_ms(value: &str) -> Result<u32, SettingsDraftError> {
    parse_number("Fade out", value)
}

fn parsed_ducking_fade_in_ms(value: &str) -> Result<u32, SettingsDraftError> {
    parse_number("Fade in", value)
}

fn validated_paste_shortcut(value: &str) -> Result<String, SettingsDraftError> {
    required("Paste shortcut", value)
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
