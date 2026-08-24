use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};

use agentdictate_core::{
    ModelCatalogEntry, ModelCatalogFallback, ModelCatalogOrigin, ModelCatalogSnapshot,
    ModelCatalogStatus, ModelCatalogSupport, ReasoningEffort,
};
use agentdictate_runtime::write_atomic;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const BUNDLED_MODEL_IDS: &[&str] = &[
    "gpt-transcribe",
    "gpt-4o-transcribe",
    "gpt-4o-mini-transcribe",
    "whisper-1",
    "gpt-5.4-nano",
    "gpt-5.4-mini",
    "gpt-5.5",
];

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(crate) enum ModelCatalogError {
    #[error("OpenAI authentication failed. Check your API key.")]
    Authentication,
    #[error("OpenAI is temporarily rate limiting model discovery.")]
    RateLimited,
    #[error("OpenAI model discovery is temporarily unavailable.")]
    Unavailable,
    #[error("OpenAI returned an invalid model list.")]
    InvalidResponse,
}

pub(crate) trait ModelCatalogSource: Send + Sync {
    fn list_model_ids(&self, api_key: &str) -> Result<Vec<String>, ModelCatalogError>;
}

pub(crate) struct ReqwestModelCatalogSource {
    client: reqwest::blocking::Client,
    api_base: String,
}

impl ReqwestModelCatalogSource {
    fn new() -> Self {
        Self::with_api_base("https://api.openai.com/v1")
    }

    fn with_api_base(api_base: impl Into<String>) -> Self {
        Self {
            client: reqwest::blocking::Client::builder()
                .connect_timeout(Duration::from_secs(20))
                .timeout(Duration::from_secs(60))
                .build()
                .expect("the rustls HTTP client must be constructible"),
            api_base: api_base.into().trim_end_matches('/').to_owned(),
        }
    }
}

#[derive(Deserialize)]
struct OpenAiModelList {
    data: Vec<OpenAiModel>,
}

#[derive(Deserialize)]
struct OpenAiModel {
    id: String,
}

impl ModelCatalogSource for ReqwestModelCatalogSource {
    fn list_model_ids(&self, api_key: &str) -> Result<Vec<String>, ModelCatalogError> {
        let response = self
            .client
            .get(format!("{}/models", self.api_base))
            .bearer_auth(api_key)
            .send()
            .map_err(|_| ModelCatalogError::Unavailable)?;
        match response.status() {
            status if status.is_success() => {}
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                return Err(ModelCatalogError::Authentication);
            }
            reqwest::StatusCode::TOO_MANY_REQUESTS => {
                return Err(ModelCatalogError::RateLimited);
            }
            _ => return Err(ModelCatalogError::Unavailable),
        }
        let list = response
            .json::<OpenAiModelList>()
            .map_err(|_| ModelCatalogError::InvalidResponse)?;
        Ok(list.data.into_iter().map(|model| model.id).collect())
    }
}

struct ModelCatalogState {
    model_ids: Vec<String>,
    origin: ModelCatalogOrigin,
    status: ModelCatalogStatus,
    credential_fingerprint: String,
    generation: u64,
}

#[derive(Clone)]
pub(crate) struct ModelCatalog {
    state: Arc<RwLock<ModelCatalogState>>,
    source: Arc<dyn ModelCatalogSource>,
    cache_file: PathBuf,
}

impl ModelCatalog {
    pub(crate) fn open(cache_directory: &Path, api_key: &str) -> Self {
        Self::with_source(
            cache_directory.join("model-catalog.json"),
            Arc::new(ReqwestModelCatalogSource::new()),
            api_key,
        )
    }

    fn with_source(
        cache_file: PathBuf,
        source: Arc<dyn ModelCatalogSource>,
        api_key: &str,
    ) -> Self {
        let state = fallback_state(&cache_file, api_key);
        Self {
            state: Arc::new(RwLock::new(state)),
            source,
            cache_file,
        }
    }

    pub(crate) fn snapshot(
        &self,
        current_transcription_model: &str,
        current_cleanup_model: &str,
    ) -> ModelCatalogSnapshot {
        let state = self
            .state
            .read()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut snapshot = snapshot_from_model_ids_with_origin(
            state.model_ids.iter(),
            current_transcription_model,
            current_cleanup_model,
            state.origin,
        );
        snapshot.status = state.status.clone();
        snapshot
    }

    pub(crate) fn refresh_in_background(
        &self,
        api_key: &str,
    ) -> io::Result<std::thread::JoinHandle<()>> {
        let api_key = api_key.trim().to_owned();
        let fingerprint = credential_fingerprint(&api_key);
        let generation = {
            let mut state = self
                .state
                .write()
                .unwrap_or_else(|poison| poison.into_inner());
            if state.credential_fingerprint != fingerprint {
                let next_generation = state.generation.saturating_add(1);
                *state = fallback_state(&self.cache_file, &api_key);
                state.generation = next_generation;
            } else {
                state.generation = state.generation.saturating_add(1);
            }
            state.generation
        };
        let state = Arc::clone(&self.state);
        let source = Arc::clone(&self.source);
        let cache_file = self.cache_file.clone();
        std::thread::Builder::new()
            .name("agentdictate-model-catalog".into())
            .spawn(move || {
                if api_key.is_empty() {
                    return;
                }
                let result = source.list_model_ids(&api_key);
                let refreshed_at = Utc::now();
                let mut state = state.write().unwrap_or_else(|poison| poison.into_inner());
                if state.generation != generation || state.credential_fingerprint != fingerprint {
                    return;
                }
                match result {
                    Ok(mut model_ids) => {
                        model_ids.retain(|model| !model.trim().is_empty());
                        model_ids.sort_unstable();
                        model_ids.dedup();
                        let cached = CachedCatalog {
                            credential_fingerprint: fingerprint,
                            refreshed_at,
                            model_ids: model_ids.clone(),
                            failure: None,
                        };
                        let cache_result = cache_bytes(&cached)
                            .and_then(|bytes| write_atomic(&cache_file, &bytes, 0o600));
                        if let Err(error) = cache_result {
                            tracing::warn!(%error, "could not persist OpenAI model catalog");
                        }
                        state.model_ids = model_ids;
                        state.origin = ModelCatalogOrigin::Account;
                        state.status = ModelCatalogStatus::Live { refreshed_at };
                    }
                    Err(error) => {
                        let fallback = if state.origin == ModelCatalogOrigin::Bundled {
                            ModelCatalogFallback::Builtin
                        } else {
                            ModelCatalogFallback::Cached
                        };
                        let message = error.to_string();
                        let previous_refresh = match &state.status {
                            ModelCatalogStatus::Live { refreshed_at }
                            | ModelCatalogStatus::Cached { refreshed_at } => *refreshed_at,
                            ModelCatalogStatus::Builtin | ModelCatalogStatus::Failed { .. } => {
                                refreshed_at
                            }
                        };
                        let cached = CachedCatalog {
                            credential_fingerprint: fingerprint,
                            refreshed_at: previous_refresh,
                            model_ids: state.model_ids.clone(),
                            failure: Some(CachedCatalogFailure {
                                fallback,
                                message: message.clone(),
                            }),
                        };
                        let cache_result = cache_bytes(&cached)
                            .and_then(|bytes| write_atomic(&cache_file, &bytes, 0o600));
                        if let Err(cache_error) = cache_result {
                            tracing::warn!(%cache_error, "could not persist model discovery failure");
                        }
                        state.status = ModelCatalogStatus::Failed {
                            fallback,
                            message,
                        };
                    }
                }
            })
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct CachedCatalog {
    credential_fingerprint: String,
    refreshed_at: DateTime<Utc>,
    model_ids: Vec<String>,
    #[serde(default)]
    failure: Option<CachedCatalogFailure>,
}

#[derive(Debug, Deserialize, Serialize)]
struct CachedCatalogFailure {
    fallback: ModelCatalogFallback,
    message: String,
}

fn fallback_state(cache_file: &Path, api_key: &str) -> ModelCatalogState {
    let fingerprint = credential_fingerprint(api_key.trim());
    let builtin = |status| ModelCatalogState {
        model_ids: BUNDLED_MODEL_IDS
            .iter()
            .map(|model| (*model).to_owned())
            .collect(),
        origin: ModelCatalogOrigin::Bundled,
        status,
        credential_fingerprint: fingerprint.clone(),
        generation: 0,
    };
    let bytes = match std::fs::read(cache_file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return builtin(ModelCatalogStatus::Builtin);
        }
        Err(_) => {
            return builtin(ModelCatalogStatus::Failed {
                fallback: ModelCatalogFallback::Builtin,
                message: "The saved model list could not be read.".into(),
            });
        }
    };
    let cached = match serde_json::from_slice::<CachedCatalog>(&bytes) {
        Ok(cached) => cached,
        Err(_) => {
            return builtin(ModelCatalogStatus::Failed {
                fallback: ModelCatalogFallback::Builtin,
                message: "The saved model list is invalid.".into(),
            });
        }
    };
    if cached.credential_fingerprint != fingerprint {
        return builtin(ModelCatalogStatus::Builtin);
    }
    let (origin, status) = cached.failure.map_or_else(
        || {
            (
                ModelCatalogOrigin::Account,
                ModelCatalogStatus::Cached {
                    refreshed_at: cached.refreshed_at,
                },
            )
        },
        |failure| {
            (
                match failure.fallback {
                    ModelCatalogFallback::Cached => ModelCatalogOrigin::Account,
                    ModelCatalogFallback::Builtin => ModelCatalogOrigin::Bundled,
                },
                ModelCatalogStatus::Failed {
                    fallback: failure.fallback,
                    message: failure.message,
                },
            )
        },
    );
    ModelCatalogState {
        model_ids: cached.model_ids,
        origin,
        status,
        credential_fingerprint: fingerprint,
        generation: 0,
    }
}

fn credential_fingerprint(api_key: &str) -> String {
    let digest = Sha256::digest(format!("agentdictate-model-catalog\0{api_key}").as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn cache_bytes(catalog: &CachedCatalog) -> io::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(catalog).map_err(io::Error::other)?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(test)]
fn snapshot_from_model_ids(
    model_ids: impl IntoIterator<Item = impl AsRef<str>>,
    current_transcription_model: &str,
    current_cleanup_model: &str,
) -> ModelCatalogSnapshot {
    snapshot_from_model_ids_with_origin(
        model_ids,
        current_transcription_model,
        current_cleanup_model,
        ModelCatalogOrigin::Account,
    )
}

fn snapshot_from_model_ids_with_origin(
    model_ids: impl IntoIterator<Item = impl AsRef<str>>,
    current_transcription_model: &str,
    current_cleanup_model: &str,
    origin: ModelCatalogOrigin,
) -> ModelCatalogSnapshot {
    let mut transcription_models = BTreeMap::new();
    let mut cleanup_models = BTreeMap::new();
    for id in model_ids {
        let id = id.as_ref().trim();
        if id.is_empty() {
            continue;
        }
        if let Some(support) = transcription_support(id) {
            transcription_models.insert(
                id.to_owned(),
                catalog_entry(id, origin, support, Vec::new()),
            );
        }
        if is_cleanup_candidate(id) {
            let support = if is_confirmed_cleanup_model(id) {
                ModelCatalogSupport::Confirmed
            } else {
                ModelCatalogSupport::Unverified
            };
            cleanup_models.insert(
                id.to_owned(),
                catalog_entry(id, origin, support, reasoning_efforts(id)),
            );
        }
    }
    preserve_current(
        &mut transcription_models,
        current_transcription_model,
        Vec::new(),
    );
    preserve_current(
        &mut cleanup_models,
        current_cleanup_model,
        reasoning_efforts(current_cleanup_model),
    );
    ModelCatalogSnapshot {
        transcription_models: transcription_models.into_values().collect(),
        cleanup_models: cleanup_models.into_values().collect(),
        ..ModelCatalogSnapshot::default()
    }
}

fn preserve_current(
    models: &mut BTreeMap<String, ModelCatalogEntry>,
    current: &str,
    reasoning_efforts: Vec<ReasoningEffort>,
) {
    let current = current.trim();
    if current.is_empty() || current == "Custom" || models.contains_key(current) {
        return;
    }
    models.insert(
        current.to_owned(),
        catalog_entry(
            current,
            ModelCatalogOrigin::Current,
            ModelCatalogSupport::Unverified,
            reasoning_efforts,
        ),
    );
}

fn catalog_entry(
    id: &str,
    origin: ModelCatalogOrigin,
    support: ModelCatalogSupport,
    reasoning_efforts: Vec<ReasoningEffort>,
) -> ModelCatalogEntry {
    ModelCatalogEntry {
        id: id.to_owned(),
        origin,
        support,
        reasoning_efforts,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TranscriptionProfile {
    AgentDictateGpt,
    OpenAiGpt,
    Standard,
}

pub(crate) fn transcription_profile(model: &str) -> Option<TranscriptionProfile> {
    let model = model.trim().to_ascii_lowercase();
    if model == "gpt-transcribe" {
        Some(TranscriptionProfile::AgentDictateGpt)
    } else if !model.contains("diarize")
        && (model == "gpt-4o-transcribe"
            || model.starts_with("gpt-4o-transcribe-")
            || model == "gpt-4o-mini-transcribe"
            || model.starts_with("gpt-4o-mini-transcribe-"))
    {
        Some(TranscriptionProfile::OpenAiGpt)
    } else if model == "whisper-1" {
        Some(TranscriptionProfile::Standard)
    } else {
        None
    }
}

fn transcription_support(model: &str) -> Option<ModelCatalogSupport> {
    if transcription_profile(model).is_some() {
        return Some(ModelCatalogSupport::Confirmed);
    }
    let model = model.trim().to_ascii_lowercase();
    let incompatible = [
        "audio",
        "tts",
        "text-to-speech",
        "text_to_speech",
        "realtime",
        "diarize",
        "embedding",
        "moderation",
        "dall-e",
        "image",
        "search",
        "computer-use",
        "codex",
    ];
    if incompatible.iter().any(|part| model.contains(part)) {
        return None;
    }
    let looks_like_speech_model = model.contains("transcrib")
        || model.contains("transcript")
        || model.contains("speech")
        || model.starts_with("whisper-")
        || model.starts_with("stt-")
        || model.ends_with("-stt")
        || model.contains("-stt-");
    looks_like_speech_model.then_some(ModelCatalogSupport::Unverified)
}

fn is_cleanup_candidate(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    let specialized = [
        "transcribe",
        "transcript",
        "whisper",
        "speech",
        "-stt",
        "realtime",
        "embedding",
        "moderation",
        "dall-e",
        "image",
        "tts",
        "audio",
        "search",
        "computer-use",
        "codex",
    ];
    !specialized.iter().any(|part| model.contains(part))
        && (model.starts_with("gpt-")
            || model.starts_with("o1")
            || model.starts_with("o3")
            || model.starts_with("o4"))
}

fn is_confirmed_cleanup_model(model: &str) -> bool {
    matches!(model, "gpt-5.4-nano" | "gpt-5.4-mini" | "gpt-5.5")
}

fn reasoning_efforts(model: &str) -> Vec<ReasoningEffort> {
    use ReasoningEffort::{Default, High, Low, Medium, Xhigh};
    match model {
        "gpt-5.5" => vec![Default, Low, Medium, High, Xhigh],
        "gpt-5.4-nano" | "gpt-5.4-mini" => vec![Default, Low, Medium, High],
        _ => vec![Default],
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        io::{Read, Write},
        net::TcpListener,
        os::unix::fs::PermissionsExt,
        sync::{Arc, Mutex, mpsc},
    };

    use agentdictate_core::{
        ModelCatalogOrigin, ModelCatalogStatus, ModelCatalogSupport, ReasoningEffort,
    };
    use tempfile::tempdir;

    use super::{
        ModelCatalog, ModelCatalogError, ModelCatalogSource, ReqwestModelCatalogSource,
        snapshot_from_model_ids,
    };

    struct NeverFetch;

    impl ModelCatalogSource for NeverFetch {
        fn list_model_ids(&self, _api_key: &str) -> Result<Vec<String>, ModelCatalogError> {
            panic!("opening the catalog must not perform network work")
        }
    }

    struct FixedModels(&'static [&'static str]);

    impl ModelCatalogSource for FixedModels {
        fn list_model_ids(&self, _api_key: &str) -> Result<Vec<String>, ModelCatalogError> {
            Ok(self.0.iter().map(|model| (*model).to_owned()).collect())
        }
    }

    struct ControlledModels {
        next: Mutex<VecDeque<mpsc::Receiver<Result<Vec<String>, ModelCatalogError>>>>,
        started: mpsc::Sender<()>,
    }

    impl ModelCatalogSource for ControlledModels {
        fn list_model_ids(&self, _api_key: &str) -> Result<Vec<String>, ModelCatalogError> {
            let receiver = self.next.lock().unwrap().pop_front().unwrap();
            self.started.send(()).unwrap();
            receiver.recv().unwrap()
        }
    }

    #[test]
    fn account_catalog_only_exposes_compatible_speech_and_general_text_models() {
        let snapshot = snapshot_from_model_ids(
            [
                "tts-1",
                "tts-transcribe-bridge",
                "gpt-audio-transcribe-preview",
                "text-embedding-3-small",
                "omni-moderation-latest",
                "gpt-4o-realtime-preview",
                "gpt-transcribe-realtime-preview",
                "gpt-transcribe-diarize",
                "gpt-4o-mini-transcribe",
                "gpt-5-transcribe",
                "gpt-6-speech",
                "whisper-1",
                "gpt-5.5",
                "gpt-6",
            ],
            "private-transcriber",
            "private-cleaner",
        );

        assert_eq!(
            snapshot
                .transcription_models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            [
                "gpt-4o-mini-transcribe",
                "gpt-5-transcribe",
                "gpt-6-speech",
                "private-transcriber",
                "whisper-1",
            ]
        );
        let future_transcriber = &snapshot.transcription_models[1];
        assert_eq!(future_transcriber.origin, ModelCatalogOrigin::Account);
        assert_eq!(future_transcriber.support, ModelCatalogSupport::Unverified);
        assert_eq!(
            snapshot.transcription_models[2].support,
            ModelCatalogSupport::Unverified
        );
        let private_transcriber = &snapshot.transcription_models[3];
        assert_eq!(private_transcriber.origin, ModelCatalogOrigin::Current);
        assert_eq!(private_transcriber.support, ModelCatalogSupport::Unverified);

        assert_eq!(
            snapshot
                .cleanup_models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["gpt-5.5", "gpt-6", "private-cleaner"]
        );
        assert_eq!(
            snapshot.cleanup_models[0].reasoning_efforts,
            [
                ReasoningEffort::Default,
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
                ReasoningEffort::Xhigh,
            ]
        );
        assert_eq!(
            snapshot.cleanup_models[1].reasoning_efforts,
            [ReasoningEffort::Default]
        );
        assert_eq!(
            snapshot.cleanup_models[2].reasoning_efforts,
            [ReasoningEffort::Default]
        );
    }

    #[test]
    fn opening_without_a_matching_cache_returns_bundled_choices_and_preserves_current_values() {
        let directory = tempdir().unwrap();
        let catalog = ModelCatalog::with_source(
            directory.path().join("model-catalog.json"),
            Arc::new(NeverFetch),
            "sk-account-a",
        );

        let snapshot = catalog.snapshot("company-transcriber", "company-cleaner");

        assert_eq!(snapshot.status, ModelCatalogStatus::Builtin);
        assert!(snapshot.transcription_models.iter().any(|model| {
            model.id == "gpt-transcribe" && model.origin == ModelCatalogOrigin::Bundled
        }));
        assert!(snapshot.transcription_models.iter().any(|model| {
            model.id == "company-transcriber" && model.origin == ModelCatalogOrigin::Current
        }));
        assert!(snapshot.cleanup_models.iter().any(|model| {
            model.id == "company-cleaner"
                && model.origin == ModelCatalogOrigin::Current
                && model.reasoning_efforts == [ReasoningEffort::Default]
        }));
    }

    #[test]
    fn successful_refresh_is_live_then_loads_as_an_account_scoped_cache_without_storing_the_key() {
        let directory = tempdir().unwrap();
        let cache_file = directory.path().join("model-catalog.json");
        let catalog = ModelCatalog::with_source(
            cache_file.clone(),
            Arc::new(FixedModels(&["gpt-6", "gpt-4o-mini-transcribe"])),
            "sk-account-a",
        );

        catalog
            .refresh_in_background("sk-account-a")
            .unwrap()
            .join()
            .unwrap();

        let live = catalog.snapshot("gpt-4o-mini-transcribe", "gpt-6");
        assert!(matches!(live.status, ModelCatalogStatus::Live { .. }));
        assert_eq!(live.cleanup_models[0].id, "gpt-6");
        let persisted = std::fs::read_to_string(&cache_file).unwrap();
        assert!(!persisted.contains("sk-account-a"));
        assert!(persisted.contains("gpt-6"));
        assert!(persisted.ends_with('\n'));
        assert_eq!(
            std::fs::metadata(&cache_file).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let cached =
            ModelCatalog::with_source(cache_file.clone(), Arc::new(NeverFetch), "sk-account-a")
                .snapshot("gpt-4o-mini-transcribe", "gpt-6");
        assert!(matches!(cached.status, ModelCatalogStatus::Cached { .. }));
        assert_eq!(cached.cleanup_models[0].id, "gpt-6");

        let other_account =
            ModelCatalog::with_source(cache_file, Arc::new(NeverFetch), "sk-account-b")
                .snapshot("gpt-transcribe", "gpt-5.4-nano");
        assert_eq!(other_account.status, ModelCatalogStatus::Builtin);
        assert!(
            !other_account
                .cleanup_models
                .iter()
                .any(|model| model.id == "gpt-6")
        );
    }

    #[test]
    fn successful_refresh_surfaces_future_transcription_models_as_unverified() {
        let directory = tempdir().unwrap();
        let catalog = ModelCatalog::with_source(
            directory.path().join("model-catalog.json"),
            Arc::new(FixedModels(&[
                "gpt-6-transcribe",
                "gpt-audio-transcribe-preview",
            ])),
            "sk-account-a",
        );

        catalog
            .refresh_in_background("sk-account-a")
            .unwrap()
            .join()
            .unwrap();

        let snapshot = catalog.snapshot("gpt-transcribe", "gpt-5.4-nano");
        let discovered = snapshot
            .transcription_models
            .iter()
            .find(|model| model.id == "gpt-6-transcribe")
            .expect("future transcription model should remain selectable");
        assert_eq!(discovered.origin, ModelCatalogOrigin::Account);
        assert_eq!(discovered.support, ModelCatalogSupport::Unverified);
        assert!(
            !snapshot
                .transcription_models
                .iter()
                .any(|model| model.id == "gpt-audio-transcribe-preview")
        );
    }

    #[test]
    fn a_slow_old_refresh_cannot_replace_a_newer_catalog_or_its_cache() {
        let directory = tempdir().unwrap();
        let cache_file = directory.path().join("model-catalog.json");
        let (first_sender, first_receiver) = mpsc::channel();
        let (second_sender, second_receiver) = mpsc::channel();
        let (started_sender, started_receiver) = mpsc::channel();
        let source = ControlledModels {
            next: Mutex::new(VecDeque::from([first_receiver, second_receiver])),
            started: started_sender,
        };
        let catalog =
            ModelCatalog::with_source(cache_file.clone(), Arc::new(source), "sk-account-a");

        let first = catalog.refresh_in_background("sk-account-a").unwrap();
        started_receiver.recv().unwrap();
        let second = catalog.refresh_in_background("sk-account-a").unwrap();
        started_receiver.recv().unwrap();
        second_sender.send(Ok(vec!["gpt-6".into()])).unwrap();
        second.join().unwrap();
        first_sender.send(Ok(vec!["gpt-5.5".into()])).unwrap();
        first.join().unwrap();

        let snapshot = catalog.snapshot("gpt-transcribe", "gpt-6");
        assert_eq!(
            snapshot
                .cleanup_models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["gpt-6"]
        );
        let cache = std::fs::read_to_string(cache_file).unwrap();
        assert!(cache.contains("gpt-6"));
        assert!(!cache.contains("gpt-5.5"));
    }

    #[test]
    fn refresh_failure_keeps_the_cache_and_exposes_only_a_sanitized_typed_error() {
        let directory = tempdir().unwrap();
        let cache_file = directory.path().join("model-catalog.json");
        let catalog = ModelCatalog::with_source(
            cache_file.clone(),
            Arc::new(FixedModels(&["gpt-6"])),
            "sk-secret-value",
        );
        catalog
            .refresh_in_background("sk-secret-value")
            .unwrap()
            .join()
            .unwrap();
        let failing = ModelCatalog {
            source: Arc::new(FailingModels(ModelCatalogError::Authentication)),
            ..catalog.clone()
        };

        failing
            .refresh_in_background("sk-secret-value")
            .unwrap()
            .join()
            .unwrap();

        let snapshot = failing.snapshot("gpt-transcribe", "gpt-6");
        assert_eq!(
            snapshot.status,
            ModelCatalogStatus::Failed {
                fallback: agentdictate_core::ModelCatalogFallback::Cached,
                message: "OpenAI authentication failed. Check your API key.".into(),
            }
        );
        assert!(
            snapshot
                .cleanup_models
                .iter()
                .any(|model| model.id == "gpt-6")
        );
        assert!(!format!("{:?}", snapshot).contains("sk-secret-value"));
        let reopened =
            ModelCatalog::with_source(cache_file, Arc::new(NeverFetch), "sk-secret-value")
                .snapshot("gpt-transcribe", "gpt-6");
        assert_eq!(reopened.status, snapshot.status);
        assert!(
            reopened
                .cleanup_models
                .iter()
                .any(|model| model.id == "gpt-6")
        );
    }

    struct FailingModels(ModelCatalogError);

    impl ModelCatalogSource for FailingModels {
        fn list_model_ids(&self, _api_key: &str) -> Result<Vec<String>, ModelCatalogError> {
            Err(self.0.clone())
        }
    }

    #[test]
    fn openai_source_fetches_the_account_model_ids_without_leaking_transport_details() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (request_sender, request_receiver) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let bytes = stream.read(&mut request).unwrap();
            request_sender
                .send(String::from_utf8_lossy(&request[..bytes]).into_owned())
                .unwrap();
            let body =
                r#"{"object":"list","data":[{"id":"gpt-6"},{"id":"gpt-4o-mini-transcribe"}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let source = ReqwestModelCatalogSource::with_api_base(format!("http://{address}/v1"));

        let models = source.list_model_ids("sk-account-a").unwrap();

        let request = request_receiver.recv().unwrap();
        server.join().unwrap();
        assert_eq!(models, ["gpt-6", "gpt-4o-mini-transcribe"]);
        assert!(request.starts_with("GET /v1/models HTTP/1.1"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer sk-account-a")
        );
    }

    #[test]
    fn openai_source_maps_remote_failures_to_sanitized_domain_errors() {
        for (status, body, expected) in [
            (
                "401 Unauthorized",
                r#"{"error":{"message":"key sk-secret-value rejected"}}"#,
                ModelCatalogError::Authentication,
            ),
            (
                "429 Too Many Requests",
                r#"{"error":{"message":"account-specific quota detail"}}"#,
                ModelCatalogError::RateLimited,
            ),
            (
                "500 Internal Server Error",
                "private upstream trace",
                ModelCatalogError::Unavailable,
            ),
            ("200 OK", "not json", ModelCatalogError::InvalidResponse),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2048];
                let _ = stream.read(&mut request).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            });
            let source = ReqwestModelCatalogSource::with_api_base(format!("http://{address}/v1"));

            let error = source.list_model_ids("sk-secret-value").unwrap_err();

            server.join().unwrap();
            assert_eq!(error, expected);
            assert!(!error.to_string().contains("sk-secret-value"));
            assert!(!error.to_string().contains("private upstream trace"));
        }
    }

    #[test]
    fn corrupt_cache_falls_back_to_bundled_choices_with_a_sanitized_status() {
        let directory = tempdir().unwrap();
        let cache_file = directory.path().join("model-catalog.json");
        std::fs::write(&cache_file, b"sk-secret-value: definitely not json").unwrap();

        let snapshot =
            ModelCatalog::with_source(cache_file, Arc::new(NeverFetch), "sk-secret-value")
                .snapshot("gpt-transcribe", "gpt-5.4-nano");

        assert_eq!(
            snapshot.status,
            ModelCatalogStatus::Failed {
                fallback: agentdictate_core::ModelCatalogFallback::Builtin,
                message: "The saved model list is invalid.".into(),
            }
        );
        assert!(!format!("{snapshot:?}").contains("sk-secret-value"));
    }
}
