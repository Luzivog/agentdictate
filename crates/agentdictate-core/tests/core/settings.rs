use agentdictate_core::{
    PasteShortcut, RecordingMode, Settings, SettingsError, SettingsSnapshot, TRANSCRIPTION_MODEL,
};

#[test]
fn existing_python_settings_load_with_new_defaults_and_ignore_unknown_fields() {
    let settings: Settings = serde_json::from_str(
        r#"{
            "transcription_model": "Custom",
            "custom_transcription_model": "my-transcriber",
            "cleanup_enabled": true,
            "transcription_provider": "chatgpt_subscription",
            "hotkey": "Ctrl+Space",
            "future_python_field": "ignored"
        }"#,
    )
    .unwrap();

    assert_eq!(settings.transcription_model, TRANSCRIPTION_MODEL);
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
fn every_stored_recording_mode_and_paste_label_loads() {
    let load = |mode: &str, paste: &str| {
        let settings: Settings = serde_json::from_value(serde_json::json!({
            "recording_mode": mode,
            "paste_shortcut": paste,
        }))
        .unwrap();
        (settings.recording_mode, settings.paste_shortcut)
    };

    assert_eq!(
        load("toggle", "Automatic"),
        (RecordingMode::Toggle, PasteShortcut::Automatic)
    );
    assert_eq!(
        load("Hold", "Standard (Ctrl+V)"),
        (RecordingMode::Hold, PasteShortcut::Standard)
    );
    assert_eq!(
        load("hold", "Terminal (Ctrl+Shift+V)"),
        (RecordingMode::Hold, PasteShortcut::Terminal)
    );
    let saved = serde_json::to_value(Settings {
        recording_mode: RecordingMode::Hold,
        paste_shortcut: PasteShortcut::Terminal,
        ..Settings::default()
    })
    .unwrap();
    assert_eq!(
        load(
            saved["recording_mode"].as_str().unwrap(),
            saved["paste_shortcut"].as_str().unwrap()
        ),
        (RecordingMode::Hold, PasteShortcut::Terminal)
    );
}

#[test]
fn settings_that_cannot_work_together_are_rejected() {
    assert_eq!(Settings::default().validate(), Ok(()));
    let loud = Settings {
        audio_ducking_volume_percent: 101,
        ..Settings::default()
    };
    assert_eq!(loud.validate(), Err(SettingsError::DuckedVolumeOutOfRange));
}
