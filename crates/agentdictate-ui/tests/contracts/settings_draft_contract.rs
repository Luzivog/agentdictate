//! Settings draft contracts.

use agentdictate_core::Settings;
use agentdictate_ui::{SettingsDraft, SettingsDraftError};

#[test]
fn settings_draft_validates_and_updates_every_editable_runtime_value() {
    let original = Settings {
        openai_api_key: "secret-kept-outside-the-form".to_owned(),
        save_history: false,
        ..Settings::default()
    };
    let mut draft = SettingsDraft::from(&original);
    draft.transcription_model = "gpt-4o-transcribe".to_owned();
    draft.language = "en".to_owned();
    draft.transcription_prompt = "Leadlord, AgentDictate".to_owned();
    draft.cleanup_enabled = false;
    draft.cleanup_model = "gpt-5.4-mini".to_owned();
    draft.cleanup_reasoning_effort = "low".to_owned();
    draft.cleanup_style = "Technical".to_owned();
    draft.cleanup_prompt = "Preserve exact identifiers.".to_owned();
    draft.hotkey = "Alt+Space".to_owned();
    draft.recording_mode = "hold".to_owned();
    draft.max_recording_seconds = "420".to_owned();
    draft.audio_ducking_enabled = false;
    draft.audio_ducking_volume_percent = "25".to_owned();
    draft.paste_shortcut = "Ctrl+Shift+V".to_owned();
    draft.start_on_login = false;
    draft.save_history = true;
    draft.preserve_temp_audio = true;

    let updated = draft.apply_to(&original).unwrap();

    assert_eq!(updated.transcription_model, "gpt-4o-transcribe");
    assert_eq!(updated.language, "en");
    assert_eq!(updated.transcription_prompt, "Leadlord, AgentDictate");
    assert!(!updated.cleanup_enabled);
    assert_eq!(updated.cleanup_model, "gpt-5.4-mini");
    assert_eq!(updated.cleanup_reasoning_effort, "low");
    assert_eq!(updated.cleanup_style, "Technical");
    assert_eq!(updated.cleanup_prompt, "Preserve exact identifiers.");
    assert_eq!(updated.hotkey, "Alt+Space");
    assert_eq!(updated.recording_mode, "hold");
    assert_eq!(updated.max_recording_seconds, 420);
    assert!(!updated.audio_ducking_enabled);
    assert_eq!(updated.audio_ducking_volume_percent, 25);
    assert_eq!(updated.paste_shortcut, "Ctrl+Shift+V");
    assert!(!updated.start_on_login);
    assert!(updated.save_history);
    assert!(updated.preserve_temp_audio);
    assert_eq!(updated.openai_api_key, original.openai_api_key);
}

#[test]
fn settings_draft_rejects_invalid_modes_and_out_of_range_volume() {
    let original = Settings::default();
    let mut draft = SettingsDraft::from(&original);
    draft.recording_mode = "sometimes".to_owned();
    assert_eq!(
        draft.apply_to(&original),
        Err(SettingsDraftError::InvalidRecordingMode)
    );

    draft.recording_mode = "toggle".to_owned();
    draft.audio_ducking_volume_percent = "101".to_owned();
    assert_eq!(
        draft.apply_to(&original),
        Err(SettingsDraftError::DuckedVolumeOutOfRange)
    );
}

#[test]
fn settings_draft_reports_unsaved_text_and_toggle_changes() {
    let persisted = Settings::default();
    let mut draft = SettingsDraft::from(&persisted);

    assert!(!draft.is_dirty_against(&persisted));

    draft.language = "fr".to_owned();
    assert!(draft.is_dirty_against(&persisted));

    let mut toggle_edits = Vec::new();
    let mut cleanup = SettingsDraft::from(&persisted);
    cleanup.cleanup_enabled = !cleanup.cleanup_enabled;
    toggle_edits.push(cleanup);
    let mut ducking = SettingsDraft::from(&persisted);
    ducking.audio_ducking_enabled = !ducking.audio_ducking_enabled;
    toggle_edits.push(ducking);
    let mut startup = SettingsDraft::from(&persisted);
    startup.start_on_login = !startup.start_on_login;
    toggle_edits.push(startup);
    let mut history = SettingsDraft::from(&persisted);
    history.save_history = !history.save_history;
    toggle_edits.push(history);
    let mut audio = SettingsDraft::from(&persisted);
    audio.preserve_temp_audio = !audio.preserve_temp_audio;
    toggle_edits.push(audio);

    assert!(
        toggle_edits
            .iter()
            .all(|draft| draft.is_dirty_against(&persisted))
    );
}

#[test]
fn discarding_changes_restores_the_entire_persisted_form() {
    let persisted = Settings {
        language: "en".to_owned(),
        cleanup_enabled: true,
        start_on_login: false,
        ..Settings::default()
    };
    let mut draft = SettingsDraft::from(&persisted);
    draft.language = "fr".to_owned();
    draft.cleanup_enabled = false;
    draft.start_on_login = true;

    draft.discard_changes(&persisted);

    assert_eq!(draft, SettingsDraft::from(&persisted));
    assert!(!draft.is_dirty_against(&persisted));
}

#[test]
fn applying_a_draft_preserves_settings_that_the_form_does_not_expose() {
    let original = Settings {
        openai_api_key: "secret".to_owned(),
        custom_transcription_model: "private-transcriber".to_owned(),
        custom_cleanup_model: "private-cleaner".to_owned(),
        sound_feedback: true,
        start_sound: true,
        stop_sound: true,
        audio_ducking_fade_ms: 73,
        show_tray_icon: false,
        minimize_to_tray_on_close: false,
        launch_window_on_startup: true,
        restore_clipboard_after_paste: true,
        debug_mode: true,
        currency: "EUR".to_owned(),
        transcription_prices: Default::default(),
        cleanup_prices: Default::default(),
        ..Settings::default()
    };
    let mut draft = SettingsDraft::from(&original);
    draft.language = "fr".to_owned();

    let updated = draft.apply_to(&original).unwrap();
    let mut expected = original;
    expected.language = "fr".to_owned();

    assert_eq!(updated, expected);
}

#[test]
fn api_key_changes_do_not_participate_in_the_ordinary_form_dirty_state() {
    let persisted = Settings {
        openai_api_key: "first-secret".to_owned(),
        ..Settings::default()
    };
    let draft = SettingsDraft::from(&persisted);
    let credential_rotated = Settings {
        openai_api_key: "second-secret".to_owned(),
        ..persisted
    };

    assert!(!draft.is_dirty_against(&credential_rotated));
}

#[test]
fn custom_model_choices_round_trip_without_collapsing_the_custom_sentinel() {
    let persisted = Settings {
        transcription_model: "Custom".to_owned(),
        custom_transcription_model: "whisper-enterprise".to_owned(),
        cleanup_model: "Custom".to_owned(),
        custom_cleanup_model: "cleanup-enterprise".to_owned(),
        ..Settings::default()
    };

    let draft = SettingsDraft::from(&persisted);

    assert_eq!(draft.transcription_model, "Custom");
    assert_eq!(draft.custom_transcription_model, "whisper-enterprise");
    assert_eq!(draft.cleanup_model, "Custom");
    assert_eq!(draft.custom_cleanup_model, "cleanup-enterprise");
    assert_eq!(draft.apply_to(&persisted).unwrap(), persisted);
}
