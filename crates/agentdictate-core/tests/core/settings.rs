use agentdictate_core::{
    KeepTranscripts, PasteShortcut, RecordingMode, SettingChange, Settings, SettingsError,
    SettingsSnapshot, VocabularyEntry,
};

#[test]
fn stored_settings_keep_their_values_and_ignore_retired_or_unknown_fields() {
    let settings: Settings = serde_json::from_str(
        r#"{
            "hotkey": "Alt+Space",
            "max_recording_seconds": 45,
            "transcription_model": "Custom",
            "custom_transcription_model": "my-transcriber",
            "transcription_provider": "chatgpt_subscription",
            "cleanup_enabled": true,
            "streaming_enabled": true,
            "transcription_prices": {},
            "cleanup_prices": {},
            "field_from_a_newer_version": "ignored"
        }"#,
    )
    .unwrap();

    assert_eq!(
        settings,
        Settings {
            hotkey: "Alt+Space".parse().unwrap(),
            max_recording_seconds: 45,
            ..Settings::default()
        }
    );
}

#[test]
fn a_stored_model_override_is_ignored_and_not_saved_again() {
    let settings: Settings =
        serde_json::from_value(serde_json::json!({ "transcription_model": "gpt-future" })).unwrap();

    assert_eq!(settings, Settings::default());
    assert!(
        serde_json::to_value(&settings)
            .unwrap()
            .get("transcription_model")
            .is_none()
    );
}

#[test]
fn keep_transcripts_loads_from_the_save_history_switch_it_replaced() {
    let keep = |stored: serde_json::Value| {
        serde_json::from_value::<Settings>(stored)
            .unwrap()
            .keep_transcripts
    };

    assert_eq!(keep(serde_json::json!({})), KeepTranscripts::Forever);
    assert_eq!(
        keep(serde_json::json!({ "save_history": true })),
        KeepTranscripts::Forever
    );
    assert_eq!(
        keep(serde_json::json!({ "save_history": false })),
        KeepTranscripts::Never
    );
    let saved = serde_json::to_value(Settings {
        keep_transcripts: KeepTranscripts::Days30,
        ..Settings::default()
    })
    .unwrap();
    assert_eq!(saved["keep_transcripts"], "30_days");
    assert!(saved.get("save_history").is_none());
    assert_eq!(keep(saved), KeepTranscripts::Days30);
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
fn a_setting_change_edits_only_its_setting_and_refuses_invalid_values() {
    let word = VocabularyEntry {
        spelling: "Siobhan".into(),
        aliases: vec!["shiv on".into()],
    };
    let mut settings = Settings {
        vocabulary: vec![word.clone()],
        ..Settings::default()
    };

    SettingChange::Language(" en,fr ".into())
        .apply(&mut settings)
        .unwrap();
    SettingChange::TranscriptionPrompt(" Rust and GPUI\n".into())
        .apply(&mut settings)
        .unwrap();
    assert_eq!(
        settings,
        Settings {
            language: "en,fr".into(),
            transcription_prompt: "Rust and GPUI".into(),
            vocabulary: vec![word.clone()],
            ..Settings::default()
        }
    );

    let before = settings.clone();
    assert_eq!(
        SettingChange::Language("tlh".into()).apply(&mut settings),
        Err(SettingsError::UnknownLanguage)
    );
    assert_eq!(
        SettingChange::AudioDuckingVolumePercent(101).apply(&mut settings),
        Err(SettingsError::DuckedVolumeOutOfRange)
    );
    assert!(matches!(
        SettingChange::Vocabulary(vec![word.clone(), word]).apply(&mut settings),
        Err(SettingsError::Vocabulary(_))
    ));
    assert_eq!(settings, before);
}
