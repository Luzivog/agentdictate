use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

pub const DEFAULT_CLEANUP_PROMPT: &str = "Clean up this dictation into a clear prompt. Preserve intent and technical terms. Fix punctuation and obvious filler. Do not add new requirements.";

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
pub struct TranscriptionPrice {
    pub model_name: String,
    pub price_per_audio_minute: f64,
    pub currency: String,
}

impl Default for TranscriptionPrice {
    fn default() -> Self {
        Self {
            model_name: String::new(),
            price_per_audio_minute: 0.0,
            currency: "USD".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CleanupPrice {
    pub model_name: String,
    pub input_price_per_1m_tokens: f64,
    pub output_price_per_1m_tokens: f64,
    pub currency: String,
}

impl Default for CleanupPrice {
    fn default() -> Self {
        Self {
            model_name: String::new(),
            input_price_per_1m_tokens: 0.0,
            output_price_per_1m_tokens: 0.0,
            currency: "USD".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub openai_api_key: String,
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
    pub max_recording_seconds: u32,
    pub sound_feedback: bool,
    pub start_sound: bool,
    pub stop_sound: bool,
    pub audio_ducking_enabled: bool,
    pub audio_ducking_volume_percent: u8,
    pub audio_ducking_fade_ms: u32,
    pub start_on_login: bool,
    pub show_tray_icon: bool,
    pub minimize_to_tray_on_close: bool,
    pub launch_window_on_startup: bool,
    pub restore_clipboard_after_paste: bool,
    pub debug_mode: bool,
    pub preserve_temp_audio: bool,
    pub save_history: bool,
    pub paste_shortcut: String,
    pub currency: String,
    pub transcription_prices: BTreeMap<String, TranscriptionPrice>,
    pub cleanup_prices: BTreeMap<String, CleanupPrice>,
}

impl Settings {
    #[must_use]
    pub fn active_transcription_model(&self) -> &str {
        if self.transcription_model == "Custom" {
            self.custom_transcription_model.trim()
        } else {
            &self.transcription_model
        }
    }

    #[must_use]
    pub fn active_cleanup_model(&self) -> &str {
        if self.cleanup_model == "Custom" {
            self.custom_cleanup_model.trim()
        } else {
            &self.cleanup_model
        }
    }

    /// Repairs the historical all-zero pricing file while preserving any
    /// deliberate non-zero customizations and unknown future models.
    pub fn repair_pricing_defaults(&mut self) -> bool {
        let transcription_defaults = default_transcription_prices();
        let all_transcription_prices_zero = transcription_defaults.keys().all(|model| {
            self.transcription_prices
                .get(model)
                .is_none_or(|price| price.price_per_audio_minute <= 0.0)
        });
        let mut changed = false;
        if all_transcription_prices_zero {
            self.transcription_prices = transcription_defaults;
            changed = true;
        } else {
            for (model, price) in transcription_defaults {
                if let std::collections::btree_map::Entry::Vacant(entry) =
                    self.transcription_prices.entry(model)
                {
                    entry.insert(price);
                    changed = true;
                }
            }
        }

        let cleanup_defaults = default_cleanup_prices();
        let all_cleanup_prices_zero = cleanup_defaults.keys().all(|model| {
            self.cleanup_prices.get(model).is_none_or(|price| {
                price.input_price_per_1m_tokens <= 0.0 && price.output_price_per_1m_tokens <= 0.0
            })
        });
        if all_cleanup_prices_zero {
            self.cleanup_prices = cleanup_defaults;
            changed = true;
        } else {
            for (model, price) in cleanup_defaults {
                if let std::collections::btree_map::Entry::Vacant(entry) =
                    self.cleanup_prices.entry(model)
                {
                    entry.insert(price);
                    changed = true;
                }
            }
        }
        changed
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            openai_api_key: String::new(),
            transcription_provider: TranscriptionProvider::OpenAiApi,
            transcription_model: "gpt-transcribe".into(),
            custom_transcription_model: String::new(),
            language: String::new(),
            transcription_prompt: String::new(),
            cleanup_enabled: true,
            cleanup_model: "gpt-5.4-nano".into(),
            custom_cleanup_model: String::new(),
            cleanup_reasoning_effort: "default".into(),
            cleanup_style: "Light cleanup".into(),
            cleanup_prompt: DEFAULT_CLEANUP_PROMPT.into(),
            hotkey: "Ctrl+Space".into(),
            recording_mode: "toggle".into(),
            max_recording_seconds: 300,
            sound_feedback: false,
            start_sound: false,
            stop_sound: false,
            audio_ducking_enabled: true,
            audio_ducking_volume_percent: 15,
            audio_ducking_fade_ms: 1_000,
            start_on_login: true,
            show_tray_icon: true,
            minimize_to_tray_on_close: true,
            launch_window_on_startup: false,
            restore_clipboard_after_paste: false,
            debug_mode: false,
            preserve_temp_audio: false,
            save_history: true,
            paste_shortcut: "Automatic".into(),
            currency: "USD".into(),
            transcription_prices: default_transcription_prices(),
            cleanup_prices: default_cleanup_prices(),
        }
    }
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

fn default_transcription_prices() -> BTreeMap<String, TranscriptionPrice> {
    [
        ("gpt-transcribe", 0.0045),
        ("gpt-4o-transcribe", 0.006),
        ("gpt-4o-mini-transcribe", 0.003),
        ("whisper-1", 0.006),
    ]
    .into_iter()
    .map(|(model, price)| {
        (
            model.into(),
            TranscriptionPrice {
                model_name: model.into(),
                price_per_audio_minute: price,
                currency: "USD".into(),
            },
        )
    })
    .collect()
}

fn default_cleanup_prices() -> BTreeMap<String, CleanupPrice> {
    [
        ("gpt-5.4-nano", 0.05, 0.40),
        ("gpt-5.4-mini", 0.25, 2.00),
        ("gpt-5.5", 1.25, 10.00),
    ]
    .into_iter()
    .map(|(model, input, output)| {
        (
            model.into(),
            CleanupPrice {
                model_name: model.into(),
                input_price_per_1m_tokens: input,
                output_price_per_1m_tokens: output,
                currency: "USD".into(),
            },
        )
    })
    .collect()
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
