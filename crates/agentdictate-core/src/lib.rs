//! Platform-independent AgentDictate domain types and workflow state.

use std::{collections::BTreeMap, fmt, str::FromStr};

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReplacementRule {
    pub id: Option<i64>,
    pub source_phrase: String,
    pub replacement_phrase: String,
    pub enabled: bool,
    pub case_sensitive: bool,
    pub whole_word_only: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AppliedReplacement {
    pub rule_id: Option<i64>,
    pub source_phrase: String,
    pub replacement_phrase: String,
    pub count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReplacementResult {
    pub text: String,
    pub applied: Vec<AppliedReplacement>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecoverySnapshot {
    pub job_id: JobId,
    pub stage: JobStage,
    pub updated_at: DateTime<Utc>,
    pub duration_seconds: f64,
    pub raw_transcript: String,
    pub final_text: String,
    pub error_message: Option<String>,
    pub audio_present: bool,
    pub delivery_ambiguous: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistorySnapshot {
    pub id: i64,
    pub created_at: DateTime<Utc>,
    #[serde(alias = "final_text")]
    pub preview_text: String,
    pub word_count: u64,
    pub duration_seconds: f64,
}

pub const DEFAULT_HISTORY_PAGE_SIZE: usize = 10;

/// Opaque continuation token returned by the daemon for a specific history query.
///
/// Clients must round-trip this value unchanged rather than inspecting or constructing
/// database pagination state themselves.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HistoryPageCursor(String);

impl HistoryPageCursor {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HistoryPageRequest {
    pub search: String,
    #[serde(alias = "limit")]
    pub page_size: usize,
    #[serde(default)]
    pub after: Option<HistoryPageCursor>,
}

impl Default for HistoryPageRequest {
    fn default() -> Self {
        Self {
            search: String::new(),
            page_size: DEFAULT_HISTORY_PAGE_SIZE,
            after: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HistoryPageSnapshot {
    pub search: String,
    pub total_matches: u64,
    /// True when an expired opaque cursor was safely restarted at page one.
    #[serde(default)]
    pub cursor_restarted: bool,
    #[serde(default)]
    pub next_cursor: Option<HistoryPageCursor>,
    pub rows: Vec<HistorySnapshot>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageTotalsSnapshot {
    pub dictations: u64,
    pub words: u64,
    pub audio_seconds: f64,
    pub estimated_cost: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageDaySnapshot {
    pub date: NaiveDate,
    pub totals: UsageTotalsSnapshot,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub last_7_days: UsageTotalsSnapshot,
    pub last_30_days: UsageTotalsSnapshot,
    pub all_time: UsageTotalsSnapshot,
    pub activity: Vec<UsageDaySnapshot>,
    pub weekly_activity: Vec<UsageDaySnapshot>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCatalogOrigin {
    Account,
    Bundled,
    Current,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCatalogSupport {
    Confirmed,
    Unverified,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Default,
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ReasoningEffort {
    #[must_use]
    pub const fn settings_value(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    #[must_use]
    pub const fn openai_value(self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            effort => Some(effort.settings_value()),
        }
    }

    #[must_use]
    pub fn from_settings_value(value: &str) -> Option<Self> {
        match value {
            "default" => Some(Self::Default),
            "none" => Some(Self::None),
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::Xhigh),
            "max" => Some(Self::Max),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModelCatalogEntry {
    pub id: String,
    pub origin: ModelCatalogOrigin,
    pub support: ModelCatalogSupport,
    #[serde(default)]
    pub reasoning_efforts: Vec<ReasoningEffort>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCatalogFallback {
    Cached,
    Builtin,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ModelCatalogStatus {
    Live {
        refreshed_at: DateTime<Utc>,
    },
    Cached {
        refreshed_at: DateTime<Utc>,
    },
    #[default]
    Builtin,
    Failed {
        fallback: ModelCatalogFallback,
        message: String,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModelCatalogSnapshot {
    pub transcription_models: Vec<ModelCatalogEntry>,
    pub cleanup_models: Vec<ModelCatalogEntry>,
    pub status: ModelCatalogStatus,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub recoveries: Vec<RecoverySnapshot>,
    #[serde(default)]
    pub recent_history: Vec<HistorySnapshot>,
    pub history: Vec<HistorySnapshot>,
    pub history_total: u64,
    pub history_has_more: bool,
    #[serde(default)]
    pub history_next_cursor: Option<HistoryPageCursor>,
    pub history_search: String,
    pub replacements: Vec<ReplacementRule>,
    pub usage: UsageSnapshot,
    #[serde(default)]
    pub model_catalog: ModelCatalogSnapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CostEstimate {
    pub transcription_cost: f64,
    pub cleanup_cost: f64,
    pub total_cost: f64,
    pub cleanup_input_tokens: u64,
    pub cleanup_output_tokens: u64,
}

#[must_use]
pub fn estimate_tokens(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    ((text.len() as f64 / 4.0).round_ties_even() as u64).max(1)
}

#[must_use]
pub fn estimate_session_cost(
    duration_seconds: f64,
    raw_transcript: &str,
    cleaned_transcript: Option<&str>,
    cleanup_enabled: bool,
    transcription_price_per_minute: f64,
    cleanup_input_price_per_1m_tokens: f64,
    cleanup_output_price_per_1m_tokens: f64,
) -> CostEstimate {
    let transcription_cost =
        (duration_seconds.max(0.0) / 60.0) * transcription_price_per_minute.max(0.0);
    let (cleanup_cost, cleanup_input_tokens, cleanup_output_tokens) = if cleanup_enabled
        && let Some(cleaned) = cleaned_transcript
    {
        let input_tokens = estimate_tokens(raw_transcript);
        let output_tokens = estimate_tokens(cleaned);
        let cost = input_tokens as f64 / 1_000_000.0 * cleanup_input_price_per_1m_tokens.max(0.0)
            + output_tokens as f64 / 1_000_000.0 * cleanup_output_price_per_1m_tokens.max(0.0);
        (cost, input_tokens, output_tokens)
    } else {
        (0.0, 0, 0)
    };
    CostEstimate {
        transcription_cost,
        cleanup_cost,
        total_cost: transcription_cost + cleanup_cost,
        cleanup_input_tokens,
        cleanup_output_tokens,
    }
}

/// Applies enabled rules in stored order, matching the existing Python behavior.
pub fn apply_replacements(
    text: &str,
    rules: &[ReplacementRule],
) -> Result<ReplacementResult, regex::Error> {
    let mut text = text.to_owned();
    let mut applied = Vec::new();
    let word_character = regex::Regex::new(r"^\w$")?;
    for rule in rules {
        if !rule.enabled || rule.source_phrase.is_empty() {
            continue;
        }
        let expression = regex::RegexBuilder::new(&regex::escape(&rule.source_phrase))
            .case_insensitive(!rule.case_sensitive)
            .build()?;
        let matches = expression
            .find_iter(&text)
            .filter(|matched| {
                !rule.whole_word_only
                    || (!neighbor_is_word(&text, matched.start(), true, &word_character)
                        && !neighbor_is_word(&text, matched.end(), false, &word_character))
            })
            .map(|matched| (matched.start(), matched.end()))
            .collect::<Vec<_>>();
        let count = matches.len();
        if count == 0 {
            continue;
        }
        let mut replaced = String::with_capacity(text.len());
        let mut cursor = 0;
        for (start, end) in matches {
            replaced.push_str(&text[cursor..start]);
            replaced.push_str(&rule.replacement_phrase);
            cursor = end;
        }
        replaced.push_str(&text[cursor..]);
        text = replaced;
        applied.push(AppliedReplacement {
            rule_id: rule.id,
            source_phrase: rule.source_phrase.clone(),
            replacement_phrase: rule.replacement_phrase.clone(),
            count,
        });
    }
    Ok(ReplacementResult { text, applied })
}

fn neighbor_is_word(
    text: &str,
    byte_index: usize,
    before: bool,
    word_character: &regex::Regex,
) -> bool {
    let neighbor = if before {
        text[..byte_index].chars().next_back()
    } else {
        text[byte_index..].chars().next()
    };
    neighbor.is_some_and(|character| {
        let mut encoded = [0; 4];
        word_character.is_match(character.encode_utf8(&mut encoded))
    })
}

pub const DEFAULT_CLEANUP_PROMPT: &str = "Clean up this dictation into a clear prompt. Preserve intent and technical terms. Fix punctuation and obvious filler. Do not add new requirements.";
pub const PROTOCOL_VERSION: u16 = 2;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClientCommand {
    pub protocol_version: u16,
    #[serde(flatten)]
    pub kind: ClientCommandKind,
}

impl ClientCommand {
    const fn with_kind(kind: ClientCommandKind) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            kind,
        }
    }

    #[must_use]
    pub const fn start_recording(request_id: u64) -> Self {
        Self::with_kind(ClientCommandKind::StartRecording { request_id })
    }

    #[must_use]
    pub const fn get_snapshot(request_id: u64) -> Self {
        Self::with_kind(ClientCommandKind::GetSnapshot { request_id })
    }

    #[must_use]
    pub const fn get_workspace(request_id: u64) -> Self {
        Self::with_kind(ClientCommandKind::GetWorkspace { request_id })
    }

    /// Returns the current cached/bundled catalog immediately while asking
    /// the daemon to refresh account availability in the background.
    #[must_use]
    pub const fn refresh_model_catalog(request_id: u64) -> Self {
        Self::with_kind(ClientCommandKind::RefreshModelCatalog { request_id })
    }

    #[must_use]
    pub fn get_history_page(
        request_id: u64,
        search: impl Into<String>,
        page_size: usize,
        after: Option<HistoryPageCursor>,
    ) -> Self {
        Self::with_kind(ClientCommandKind::GetHistoryPage {
            request_id,
            request: HistoryPageRequest {
                search: search.into(),
                page_size,
                after,
            },
        })
    }

    #[must_use]
    pub const fn stop_recording(request_id: u64) -> Self {
        Self::with_kind(ClientCommandKind::StopRecording { request_id })
    }

    #[must_use]
    pub const fn cancel(request_id: u64) -> Self {
        Self::with_kind(ClientCommandKind::Cancel { request_id })
    }

    #[doc(hidden)]
    #[must_use]
    pub const fn recorder_exited(request_id: u64, job_id: JobId) -> Self {
        Self::with_kind(ClientCommandKind::RecorderExited { request_id, job_id })
    }

    #[must_use]
    pub const fn retry_transcription(request_id: u64, job_id: JobId) -> Self {
        Self::with_kind(ClientCommandKind::RetryTranscription { request_id, job_id })
    }

    #[must_use]
    pub const fn retry_delivery(request_id: u64, job_id: JobId) -> Self {
        Self::with_kind(ClientCommandKind::RetryDelivery { request_id, job_id })
    }

    #[must_use]
    pub const fn delete_recovery(request_id: u64, job_id: JobId) -> Self {
        Self::with_kind(ClientCommandKind::DeleteRecovery { request_id, job_id })
    }

    #[must_use]
    pub fn create_replacement(request_id: u64, rule: ReplacementRule) -> Self {
        Self::with_kind(ClientCommandKind::CreateReplacement { request_id, rule })
    }

    #[must_use]
    pub fn update_replacement(request_id: u64, rule: ReplacementRule) -> Self {
        Self::with_kind(ClientCommandKind::UpdateReplacement { request_id, rule })
    }

    #[must_use]
    pub const fn delete_replacement(request_id: u64, id: i64) -> Self {
        Self::with_kind(ClientCommandKind::DeleteReplacement { request_id, id })
    }

    #[must_use]
    pub const fn delete_history(request_id: u64, id: i64) -> Self {
        Self::with_kind(ClientCommandKind::DeleteHistory { request_id, id })
    }

    #[must_use]
    pub const fn clear_history(request_id: u64) -> Self {
        Self::with_kind(ClientCommandKind::ClearHistory { request_id })
    }

    #[must_use]
    pub const fn copy_transcript(request_id: u64, id: i64) -> Self {
        Self::with_kind(ClientCommandKind::CopyTranscript { request_id, id })
    }

    #[must_use]
    pub const fn quit(request_id: u64) -> Self {
        Self::with_kind(ClientCommandKind::Quit { request_id })
    }

    #[doc(hidden)]
    #[must_use]
    pub fn hotkey_status_changed(request_id: u64, readiness: HotkeyReadiness) -> Self {
        Self::with_kind(ClientCommandKind::HotkeyStatusChanged {
            request_id,
            readiness,
        })
    }

    #[must_use]
    pub fn update_settings(request_id: u64, settings: &Settings) -> Self {
        Self::with_kind(ClientCommandKind::UpdateSettings {
            request_id,
            settings: Box::new(SettingsSnapshot::from(settings).values),
        })
    }

    #[must_use]
    pub fn set_api_key(request_id: u64, api_key: impl Into<String>) -> Self {
        Self::with_kind(ClientCommandKind::SetApiKey {
            request_id,
            api_key: SecretString(api_key.into()),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum ClientCommandKind {
    GetSnapshot {
        request_id: u64,
    },
    GetWorkspace {
        request_id: u64,
    },
    RefreshModelCatalog {
        request_id: u64,
    },
    GetHistoryPage {
        request_id: u64,
        request: HistoryPageRequest,
    },
    StartRecording {
        request_id: u64,
    },
    StopRecording {
        request_id: u64,
    },
    Cancel {
        request_id: u64,
    },
    RecorderExited {
        request_id: u64,
        job_id: JobId,
    },
    RetryTranscription {
        request_id: u64,
        job_id: JobId,
    },
    RetryDelivery {
        request_id: u64,
        job_id: JobId,
    },
    DeleteRecovery {
        request_id: u64,
        job_id: JobId,
    },
    CreateReplacement {
        request_id: u64,
        rule: ReplacementRule,
    },
    UpdateReplacement {
        request_id: u64,
        rule: ReplacementRule,
    },
    DeleteReplacement {
        request_id: u64,
        id: i64,
    },
    DeleteHistory {
        request_id: u64,
        id: i64,
    },
    ClearHistory {
        request_id: u64,
    },
    CopyTranscript {
        request_id: u64,
        id: i64,
    },
    UpdateSettings {
        request_id: u64,
        settings: Box<Settings>,
    },
    SetApiKey {
        request_id: u64,
        api_key: SecretString,
    },
    HotkeyStatusChanged {
        request_id: u64,
        readiness: HotkeyReadiness,
    },
    Quit {
        request_id: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HotkeyReadiness {
    Starting,
    Ready,
    Unavailable { message: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AppSnapshot {
    pub sequence: u64,
    pub workflow: WorkflowSnapshot,
    pub hotkey: HotkeyReadiness,
    pub recoverable_count: usize,
    pub last_transcript: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerMessage {
    pub protocol_version: u16,
    #[serde(flatten)]
    pub kind: ServerMessageKind,
}

impl ServerMessage {
    #[must_use]
    pub fn snapshot(request_id: u64, snapshot: AppSnapshot, settings: &Settings) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            kind: ServerMessageKind::Snapshot {
                request_id,
                snapshot,
                settings: Box::new(SettingsSnapshot::from(settings)),
            },
        }
    }

    #[must_use]
    pub fn workspace(request_id: u64, workspace: WorkspaceSnapshot) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            kind: ServerMessageKind::Workspace {
                request_id,
                workspace: Box::new(workspace),
            },
        }
    }

    #[must_use]
    pub fn history_page(request_id: u64, page: HistoryPageSnapshot) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            kind: ServerMessageKind::HistoryPage {
                request_id,
                page: Box::new(page),
            },
        }
    }

    #[must_use]
    pub fn command_rejected(request_id: u64, error: impl Into<String>) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            kind: ServerMessageKind::CommandRejected {
                request_id,
                error: error.into(),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "message", rename_all = "snake_case")]
pub enum ServerMessageKind {
    Snapshot {
        request_id: u64,
        snapshot: AppSnapshot,
        settings: Box<SettingsSnapshot>,
    },
    Workspace {
        request_id: u64,
        workspace: Box<WorkspaceSnapshot>,
    },
    HistoryPage {
        request_id: u64,
        page: Box<HistoryPageSnapshot>,
    },
    CommandRejected {
        request_id: u64,
        error: String,
    },
}

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

/// Stable identifier shared by persisted jobs, runtime events, and UI snapshots.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(Uuid);

impl JobId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for JobId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for JobId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value).map(Self)
    }
}

/// Durable checkpoints written before and after externally visible effects.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStage {
    Starting,
    Recording,
    Captured,
    Transcribing,
    Cleaning,
    ReadyToDeliver,
    Delivering,
    Delivered,
    Interrupted,
    Failed,
    Canceled,
    Deleted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingStage {
    Transcribing,
    Cleaning,
    ReadyToDeliver,
    Delivering,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum WorkflowPhase {
    Ready,
    Starting {
        job_id: JobId,
    },
    Recording {
        job_id: JobId,
    },
    Stopping {
        job_id: JobId,
    },
    Processing {
        job_id: JobId,
        stage: ProcessingStage,
    },
    NeedsAttention {
        job_id: JobId,
        at: JobStage,
    },
}

impl WorkflowPhase {
    const fn job_id(self) -> Option<JobId> {
        match self {
            Self::Ready => None,
            Self::Starting { job_id }
            | Self::Recording { job_id }
            | Self::Stopping { job_id }
            | Self::Processing { job_id, .. }
            | Self::NeedsAttention { job_id, .. } => Some(job_id),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "signal", rename_all = "snake_case")]
pub enum WorkflowSignal {
    StartRequested {
        job_id: JobId,
    },
    FirstAudioFrameWritten {
        job_id: JobId,
    },
    StopRequested,
    /// The explicit user discard reached its durable `Deleted` checkpoint.
    DiscardCommitted {
        job_id: JobId,
    },
    CaptureFinalized {
        job_id: JobId,
    },
    TranscriptStored {
        job_id: JobId,
    },
    TranscriptStoredForCleanup {
        job_id: JobId,
    },
    CleanupStored {
        job_id: JobId,
    },
    DeliveryStarted {
        job_id: JobId,
    },
    DeliveryCommitted {
        job_id: JobId,
    },
    Interrupted {
        job_id: JobId,
        at: JobStage,
    },
    RetryDeliveryRequested {
        job_id: JobId,
    },
}

impl WorkflowSignal {
    const fn job_id(self) -> Option<JobId> {
        match self {
            Self::StopRequested => None,
            Self::StartRequested { job_id }
            | Self::FirstAudioFrameWritten { job_id }
            | Self::DiscardCommitted { job_id }
            | Self::CaptureFinalized { job_id }
            | Self::TranscriptStored { job_id }
            | Self::TranscriptStoredForCleanup { job_id }
            | Self::CleanupStored { job_id }
            | Self::DeliveryStarted { job_id }
            | Self::DeliveryCommitted { job_id }
            | Self::Interrupted { job_id, .. }
            | Self::RetryDeliveryRequested { job_id } => Some(job_id),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowSnapshot {
    pub phase: WorkflowPhase,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum WorkflowError {
    #[error("workflow signal is not valid while {phase:?}")]
    InvalidSignal { phase: WorkflowPhase },
    #[error("workflow signal belongs to job {received}, not active job {expected}")]
    JobMismatch { expected: JobId, received: JobId },
}

/// Validates lifecycle transitions before the runtime performs the next effect.
pub struct Workflow {
    snapshot: WorkflowSnapshot,
}

impl Workflow {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            snapshot: WorkflowSnapshot {
                phase: WorkflowPhase::Ready,
            },
        }
    }

    #[must_use]
    pub const fn snapshot(&self) -> WorkflowSnapshot {
        self.snapshot
    }

    pub fn apply(&mut self, signal: WorkflowSignal) -> Result<WorkflowSnapshot, WorkflowError> {
        let starts_after_recovery = matches!(
            (self.snapshot.phase, signal),
            (
                WorkflowPhase::NeedsAttention { .. },
                WorkflowSignal::StartRequested { .. }
            )
        );
        if let (Some(expected), Some(received)) = (self.snapshot.phase.job_id(), signal.job_id())
            && expected != received
            && !starts_after_recovery
        {
            return Err(WorkflowError::JobMismatch { expected, received });
        }
        let next_phase = match (self.snapshot.phase, signal) {
            (WorkflowPhase::Ready, WorkflowSignal::StartRequested { job_id }) => {
                WorkflowPhase::Starting { job_id }
            }
            (WorkflowPhase::NeedsAttention { .. }, WorkflowSignal::StartRequested { job_id }) => {
                WorkflowPhase::Starting { job_id }
            }
            (
                WorkflowPhase::Starting { job_id: expected },
                WorkflowSignal::FirstAudioFrameWritten { job_id },
            ) if expected == job_id => WorkflowPhase::Recording { job_id },
            (
                WorkflowPhase::Recording { job_id } | WorkflowPhase::Starting { job_id },
                WorkflowSignal::StopRequested,
            ) => WorkflowPhase::Stopping { job_id },
            (
                WorkflowPhase::Starting { job_id: expected }
                | WorkflowPhase::Recording { job_id: expected }
                | WorkflowPhase::Stopping { job_id: expected },
                WorkflowSignal::DiscardCommitted { job_id },
            ) if expected == job_id => WorkflowPhase::Ready,
            (
                WorkflowPhase::Stopping { job_id: expected },
                WorkflowSignal::CaptureFinalized { job_id },
            ) if expected == job_id => WorkflowPhase::Processing {
                job_id,
                stage: ProcessingStage::Transcribing,
            },
            (
                WorkflowPhase::Processing {
                    job_id: expected,
                    stage: ProcessingStage::Transcribing,
                },
                WorkflowSignal::TranscriptStored { job_id },
            ) if expected == job_id => WorkflowPhase::Processing {
                job_id,
                stage: ProcessingStage::ReadyToDeliver,
            },
            (
                WorkflowPhase::Processing {
                    job_id: expected,
                    stage: ProcessingStage::Transcribing,
                },
                WorkflowSignal::TranscriptStoredForCleanup { job_id },
            ) if expected == job_id => WorkflowPhase::Processing {
                job_id,
                stage: ProcessingStage::Cleaning,
            },
            (
                WorkflowPhase::Processing {
                    job_id: expected,
                    stage: ProcessingStage::Cleaning,
                },
                WorkflowSignal::CleanupStored { job_id },
            ) if expected == job_id => WorkflowPhase::Processing {
                job_id,
                stage: ProcessingStage::ReadyToDeliver,
            },
            (
                WorkflowPhase::Processing {
                    job_id: expected,
                    stage: ProcessingStage::ReadyToDeliver,
                },
                WorkflowSignal::DeliveryStarted { job_id },
            ) if expected == job_id => WorkflowPhase::Processing {
                job_id,
                stage: ProcessingStage::Delivering,
            },
            (
                WorkflowPhase::Processing {
                    job_id: expected,
                    stage: ProcessingStage::Delivering,
                },
                WorkflowSignal::DeliveryCommitted { job_id },
            ) if expected == job_id => WorkflowPhase::Ready,
            (
                WorkflowPhase::Starting { job_id: expected }
                | WorkflowPhase::Recording { job_id: expected }
                | WorkflowPhase::Stopping { job_id: expected }
                | WorkflowPhase::Processing {
                    job_id: expected, ..
                },
                WorkflowSignal::Interrupted { job_id, at },
            ) if expected == job_id => WorkflowPhase::NeedsAttention { job_id, at },
            (
                WorkflowPhase::NeedsAttention {
                    job_id: expected, ..
                },
                WorkflowSignal::RetryDeliveryRequested { job_id },
            ) if expected == job_id => WorkflowPhase::Processing {
                job_id,
                stage: ProcessingStage::ReadyToDeliver,
            },
            (phase, _) => return Err(WorkflowError::InvalidSignal { phase }),
        };
        self.snapshot.phase = next_phase;
        Ok(self.snapshot)
    }
}

impl Default for Workflow {
    fn default() -> Self {
        Self::new()
    }
}
