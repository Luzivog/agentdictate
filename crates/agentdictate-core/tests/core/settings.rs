use agentdictate_core::{Settings, SettingsSnapshot};

#[test]
fn reasoning_effort_has_one_exhaustive_settings_and_openai_mapping() {
    use agentdictate_core::ReasoningEffort::{
        Default, High, Low, Max, Medium, Minimal, None, Xhigh,
    };

    for (effort, settings_value, openai_value) in [
        (Default, "default", Option::<&str>::None),
        (None, "none", Some("none")),
        (Minimal, "minimal", Some("minimal")),
        (Low, "low", Some("low")),
        (Medium, "medium", Some("medium")),
        (High, "high", Some("high")),
        (Xhigh, "xhigh", Some("xhigh")),
        (Max, "max", Some("max")),
    ] {
        assert_eq!(effort.settings_value(), settings_value);
        assert_eq!(effort.openai_value(), openai_value);
        assert_eq!(
            agentdictate_core::ReasoningEffort::from_settings_value(settings_value),
            Some(effort)
        );
    }
    assert_eq!(
        agentdictate_core::ReasoningEffort::from_settings_value("unsupported"),
        Option::None
    );
}

#[test]
fn existing_python_settings_load_with_new_defaults_and_ignore_unknown_fields() {
    let settings: Settings = serde_json::from_str(
        r#"{
            "transcription_model": "Custom",
            "custom_transcription_model": "my-transcriber",
            "cleanup_enabled": false,
            "hotkey": "Ctrl+Space",
            "future_python_field": "ignored"
        }"#,
    )
    .unwrap();

    assert_eq!(settings.active_transcription_model(), "my-transcriber");
    assert!(!settings.cleanup_enabled);
    assert_eq!(settings.max_recording_seconds, 300);
    assert_eq!(settings.audio_ducking_volume_percent, 15);
}

#[test]
fn settings_sent_to_the_ui_never_include_the_api_key() {
    let settings = Settings {
        openai_api_key: "sk-private-value".into(),
        ..Settings::default()
    };

    let snapshot = SettingsSnapshot::from(&settings);
    let wire = serde_json::to_string(&snapshot).unwrap();

    assert!(snapshot.has_api_key);
    assert!(snapshot.values.openai_api_key.is_empty());
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
