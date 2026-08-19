use agentdictate_core::{
    ModelCatalogEntry, ModelCatalogFallback, ModelCatalogOrigin, ModelCatalogSnapshot,
    ModelCatalogStatus, ModelCatalogSupport, ReasoningEffort,
};
use agentdictate_ui::ModelCatalogViewModel;

fn entry(
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

#[test]
fn catalog_choices_label_unverified_and_current_models_honestly() {
    let catalog = ModelCatalogViewModel::from(ModelCatalogSnapshot {
        transcription_models: vec![
            entry(
                "gpt-4o-transcribe",
                ModelCatalogOrigin::Account,
                ModelCatalogSupport::Confirmed,
                Vec::new(),
            ),
            entry(
                "future-audio-model",
                ModelCatalogOrigin::Account,
                ModelCatalogSupport::Unverified,
                Vec::new(),
            ),
            entry(
                "my-private-transcriber",
                ModelCatalogOrigin::Current,
                ModelCatalogSupport::Unverified,
                Vec::new(),
            ),
        ],
        cleanup_models: Vec::new(),
        status: ModelCatalogStatus::Builtin,
    });

    let choices = &catalog.transcription_models;
    assert_eq!(choices[0].label, "gpt-4o-transcribe");
    assert_eq!(
        choices[1].label,
        "future-audio-model — compatibility unverified"
    );
    assert_eq!(
        choices[2].label,
        "my-private-transcriber — current custom model"
    );
}

#[test]
fn cleanup_reasoning_choices_follow_the_selected_model_capabilities() {
    let catalog = ModelCatalogViewModel::from(ModelCatalogSnapshot {
        transcription_models: Vec::new(),
        cleanup_models: vec![entry(
            "gpt-reasoner",
            ModelCatalogOrigin::Account,
            ModelCatalogSupport::Confirmed,
            vec![
                ReasoningEffort::Default,
                ReasoningEffort::Low,
                ReasoningEffort::High,
                ReasoningEffort::Max,
            ],
        )],
        status: ModelCatalogStatus::Builtin,
    });

    let supported = catalog.reasoning_options_for("gpt-reasoner");
    assert_eq!(
        supported
            .iter()
            .map(|choice| choice.value)
            .collect::<Vec<_>>(),
        ["default", "low", "high", "max"]
    );
    assert_eq!(
        catalog.reasoning_options_for("unknown-cleanup-model")[0].value,
        "default"
    );
    assert_eq!(
        catalog.reasoning_options_for("unknown-cleanup-model").len(),
        1
    );
    assert_eq!(
        catalog.normalized_reasoning_effort("gpt-reasoner", "high"),
        "high"
    );
    assert_eq!(
        catalog.normalized_reasoning_effort("unknown-cleanup-model", "high"),
        "default"
    );
}

#[test]
fn failed_refresh_reports_the_fallback_that_remains_available() {
    let catalog = ModelCatalogViewModel::from(ModelCatalogSnapshot {
        transcription_models: Vec::new(),
        cleanup_models: Vec::new(),
        status: ModelCatalogStatus::Failed {
            fallback: ModelCatalogFallback::Cached,
            message: "OpenAI is temporarily unavailable".to_owned(),
        },
    });

    assert_eq!(catalog.status.label, "Using cached models");
    assert_eq!(catalog.status.detail, "OpenAI is temporarily unavailable");
    assert!(catalog.status.is_error);
}
