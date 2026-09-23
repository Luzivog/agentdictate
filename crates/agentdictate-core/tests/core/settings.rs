use agentdictate_core::{Settings, SettingsSnapshot, TRANSCRIPTION_MODEL, TranscriptionProvider};

#[test]
fn existing_python_settings_load_with_new_defaults_and_ignore_unknown_fields() {
    let settings: Settings = serde_json::from_str(
        r#"{
            "transcription_model": "Custom",
            "custom_transcription_model": "my-transcriber",
            "cleanup_enabled": true,
            "hotkey": "Ctrl+Space",
            "future_python_field": "ignored"
        }"#,
    )
    .unwrap();

    assert_eq!(settings.transcription_model, TRANSCRIPTION_MODEL);
    assert_eq!(
        settings.transcription_provider,
        TranscriptionProvider::OpenAiApi
    );
    assert_eq!(settings.max_recording_seconds, 300);
    assert_eq!(settings.audio_ducking_volume_percent, 15);
    assert_eq!(settings.audio_ducking_fade_out_ms, 600);
    assert_eq!(settings.audio_ducking_fade_in_ms, 600);
}

#[test]
fn retired_transcription_models_load_as_the_built_in_model() {
    let model = |stored: &str| {
        serde_json::from_value::<Settings>(serde_json::json!({ "transcription_model": stored }))
            .unwrap()
            .transcription_model
    };

    for retired in [
        "",
        "whisper-1",
        "gpt-4o-transcribe",
        "gpt-4o-mini-transcribe",
        "gpt-4o-transcribe-diarize",
        "gpt-4o-mini-transcribe-2025-12-15",
    ] {
        assert_eq!(model(retired), TRANSCRIPTION_MODEL, "{retired:?}");
    }
    assert_eq!(model(" gpt-future-transcribe "), "gpt-future-transcribe");
}

#[test]
fn transcription_provider_has_stable_settings_values() {
    let subscription: Settings =
        serde_json::from_str(r#"{"transcription_provider":"chatgpt_subscription"}"#).unwrap();
    assert_eq!(
        subscription.transcription_provider,
        TranscriptionProvider::ChatGptSubscription
    );

    let wire = serde_json::to_value(Settings::default()).unwrap();
    assert_eq!(wire["transcription_provider"], "openai_api");
    assert_eq!(
        "chatgpt_subscription"
            .parse::<TranscriptionProvider>()
            .unwrap(),
        TranscriptionProvider::ChatGptSubscription
    );
    assert!("unknown".parse::<TranscriptionProvider>().is_err());
    assert_eq!(
        TranscriptionProvider::OpenAiApi.marginal_price_per_audio_minute(0.0045),
        0.0045
    );
    assert_eq!(
        TranscriptionProvider::ChatGptSubscription.marginal_price_per_audio_minute(0.0045),
        0.0
    );
}

#[test]
fn settings_sent_to_the_ui_never_include_the_api_key() {
    let settings = Settings {
        openai_api_key: "sk-private-value".into(),
        transcription_provider: TranscriptionProvider::ChatGptSubscription,
        ..Settings::default()
    };

    let snapshot = SettingsSnapshot::from(&settings);
    let wire = serde_json::to_string(&snapshot).unwrap();

    assert!(snapshot.has_api_key);
    assert!(snapshot.values.openai_api_key.is_empty());
    assert_eq!(
        snapshot.values.transcription_provider,
        TranscriptionProvider::ChatGptSubscription
    );
    assert!(!wire.contains("sk-private-value"));
}

#[test]
fn legacy_zero_price_maps_are_repaired_to_current_defaults() {
    let mut settings = Settings::default();
    for price in settings.transcription_prices.values_mut() {
        price.price_per_audio_minute = 0.0;
    }
    for price in settings.cleanup_prices.values_mut() {
        price.input_price_per_1m_tokens = 0.0;
        price.output_price_per_1m_tokens = 0.0;
    }

    assert!(settings.repair_pricing_defaults());
    assert_eq!(
        settings.transcription_prices["gpt-transcribe"].price_per_audio_minute,
        0.0045
    );
    assert_eq!(
        settings.cleanup_prices["gpt-5.4-nano"].output_price_per_1m_tokens,
        0.40
    );
}
