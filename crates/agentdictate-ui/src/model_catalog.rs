use agentdictate_core::{
    ModelCatalogEntry, ModelCatalogFallback, ModelCatalogOrigin, ModelCatalogSnapshot,
    ModelCatalogStatus, ModelCatalogSupport, ReasoningEffort,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelCatalogOptionViewModel {
    pub id: String,
    pub label: String,
    reasoning_efforts: Vec<ReasoningEffort>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReasoningOptionViewModel {
    pub label: &'static str,
    pub value: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelCatalogStatusViewModel {
    pub source: ModelCatalogStatusSource,
    pub label: &'static str,
    pub detail: String,
    pub is_error: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelCatalogStatusSource {
    Live,
    Cached,
    Builtin,
}

impl ModelCatalogStatusViewModel {
    #[must_use]
    pub fn selector(&self) -> &'static str {
        match self.source {
            ModelCatalogStatusSource::Live => "settings-model-catalog-live",
            ModelCatalogStatusSource::Cached => "settings-model-catalog-cached",
            ModelCatalogStatusSource::Builtin => "settings-model-catalog-builtin",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelCatalogViewModel {
    pub transcription_models: Vec<ModelCatalogOptionViewModel>,
    pub cleanup_models: Vec<ModelCatalogOptionViewModel>,
    pub status: ModelCatalogStatusViewModel,
}

impl Default for ModelCatalogViewModel {
    fn default() -> Self {
        Self::from(ModelCatalogSnapshot::default())
    }
}

impl ModelCatalogViewModel {
    #[must_use]
    pub fn reasoning_options_for(&self, model_id: &str) -> Vec<ReasoningOptionViewModel> {
        self.cleanup_models
            .iter()
            .find(|model| model.id == model_id)
            .map(|model| model.reasoning_efforts.as_slice())
            .filter(|efforts| !efforts.is_empty())
            .unwrap_or(&[ReasoningEffort::Default])
            .iter()
            .copied()
            .map(reasoning_option)
            .collect()
    }

    /// Keeps a reasoning choice only when the newly selected model advertises
    /// it. Explicit model changes fall back to Default instead of sending an
    /// unsupported value to OpenAI.
    #[must_use]
    pub fn normalized_reasoning_effort(&self, model_id: &str, requested: &str) -> String {
        let options = self.reasoning_options_for(model_id);
        options
            .iter()
            .find(|option| option.value == requested)
            .or_else(|| options.iter().find(|option| option.value == "default"))
            .or_else(|| options.first())
            .map_or_else(|| "default".to_owned(), |option| option.value.to_owned())
    }
}

impl From<ModelCatalogSnapshot> for ModelCatalogViewModel {
    fn from(snapshot: ModelCatalogSnapshot) -> Self {
        Self {
            transcription_models: snapshot
                .transcription_models
                .into_iter()
                .map(ModelCatalogOptionViewModel::from)
                .collect(),
            cleanup_models: snapshot
                .cleanup_models
                .into_iter()
                .map(ModelCatalogOptionViewModel::from)
                .collect(),
            status: status_view_model(snapshot.status),
        }
    }
}

impl From<ModelCatalogEntry> for ModelCatalogOptionViewModel {
    fn from(entry: ModelCatalogEntry) -> Self {
        let label = match (entry.origin, entry.support) {
            (ModelCatalogOrigin::Current, _) => {
                format!("{} — current custom model", entry.id)
            }
            (_, ModelCatalogSupport::Unverified) => {
                format!("{} — compatibility unverified", entry.id)
            }
            _ => entry.id.clone(),
        };
        Self {
            id: entry.id,
            label,
            reasoning_efforts: entry.reasoning_efforts,
        }
    }
}

fn reasoning_option(effort: ReasoningEffort) -> ReasoningOptionViewModel {
    let label = match effort {
        ReasoningEffort::Default => "Default",
        ReasoningEffort::None => "None",
        ReasoningEffort::Minimal => "Minimal",
        ReasoningEffort::Low => "Low",
        ReasoningEffort::Medium => "Medium",
        ReasoningEffort::High => "High",
        ReasoningEffort::Xhigh => "XHigh",
        ReasoningEffort::Max => "Max",
    };
    ReasoningOptionViewModel {
        label,
        value: effort.settings_value(),
    }
}

fn status_view_model(status: ModelCatalogStatus) -> ModelCatalogStatusViewModel {
    match status {
        ModelCatalogStatus::Live { .. } => ModelCatalogStatusViewModel {
            source: ModelCatalogStatusSource::Live,
            label: "Models from OpenAI",
            detail: "Available models are up to date".to_owned(),
            is_error: false,
        },
        ModelCatalogStatus::Cached { .. } => ModelCatalogStatusViewModel {
            source: ModelCatalogStatusSource::Cached,
            label: "Using cached models",
            detail: "Refreshing from OpenAI in the background".to_owned(),
            is_error: false,
        },
        ModelCatalogStatus::Builtin => ModelCatalogStatusViewModel {
            source: ModelCatalogStatusSource::Builtin,
            label: "Using built-in models",
            detail: "Connect OpenAI to load the models available to your account".to_owned(),
            is_error: false,
        },
        ModelCatalogStatus::Failed { fallback, message } => ModelCatalogStatusViewModel {
            source: match fallback {
                ModelCatalogFallback::Cached => ModelCatalogStatusSource::Cached,
                ModelCatalogFallback::Builtin => ModelCatalogStatusSource::Builtin,
            },
            label: match fallback {
                ModelCatalogFallback::Cached => "Using cached models",
                ModelCatalogFallback::Builtin => "Using built-in models",
            },
            detail: message,
            is_error: true,
        },
    }
}
