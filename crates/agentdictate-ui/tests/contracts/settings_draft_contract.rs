//! Settings draft contracts.

use agentdictate_core::{Settings, TranscriptionProvider};
use agentdictate_ui::{SettingsDraft, SettingsDraftError};

#[test]
fn settings_draft_validates_and_updates_every_editable_runtime_value() {
    let original = Settings {
        openai_api_key: "secret-kept-outside-the-form".to_owned(),
        save_history: false,
        ..Settings::default()
    };
    let mut draft = SettingsDraft::from(&original);
    draft.transcription_provider = TranscriptionProvider::ChatGptSubscription;
    draft.language = "en".to_owned();
    draft.transcription_prompt = "Leadlord, AgentDictate".to_owned();
    draft.hotkey = "Alt+Space".to_owned();
    draft.recording_mode = "hold".to_owned();
    draft.max_recording_seconds = "420".to_owned();
    draft.audio_ducking_enabled = false;
    draft.audio_ducking_volume_percent = "25".to_owned();
    draft.audio_ducking_fade_out_ms = "450".to_owned();
    draft.audio_ducking_fade_in_ms = "725".to_owned();
    draft.paste_shortcut = "Ctrl+Shift+V".to_owned();
    draft.start_on_login = false;
    draft.save_history = true;
    draft.preserve_temp_audio = true;

    let updated = draft.apply_to(&original).unwrap();

    assert_eq!(
        updated.transcription_provider,
        TranscriptionProvider::ChatGptSubscription
    );
    assert_eq!(updated.language, "en");
    assert_eq!(updated.transcription_prompt, "Leadlord, AgentDictate");
    assert_eq!(updated.hotkey, "Alt+Space");
    assert_eq!(updated.recording_mode, "hold");
    assert_eq!(updated.max_recording_seconds, 420);
    assert!(!updated.audio_ducking_enabled);
    assert_eq!(updated.audio_ducking_volume_percent, 25);
    assert_eq!(updated.audio_ducking_fade_out_ms, 450);
    assert_eq!(updated.audio_ducking_fade_in_ms, 725);
    assert_eq!(updated.paste_shortcut, "Ctrl+Shift+V");
    assert!(!updated.start_on_login);
    assert!(updated.save_history);
    assert!(updated.preserve_temp_audio);
    assert_eq!(updated.openai_api_key, original.openai_api_key);
}

#[test]
fn settings_draft_accepts_zero_and_rejects_invalid_ducking_fades() {
    let original = Settings::default();
    let mut draft = SettingsDraft::from(&original);
    draft.audio_ducking_fade_out_ms = "0".to_owned();
    draft.audio_ducking_fade_in_ms = "0".to_owned();

    let updated = draft.apply_to(&original).unwrap();
    assert_eq!(updated.audio_ducking_fade_out_ms, 0);
    assert_eq!(updated.audio_ducking_fade_in_ms, 0);

    for invalid in ["-1", "1.5"] {
        let mut draft = SettingsDraft::from(&original);
        draft.audio_ducking_fade_out_ms = invalid.to_owned();
        assert_eq!(
            draft.apply_to(&original),
            Err(SettingsDraftError::InvalidNumber { field: "Fade out" })
        );

        let mut draft = SettingsDraft::from(&original);
        draft.audio_ducking_fade_in_ms = invalid.to_owned();
        assert_eq!(
            draft.apply_to(&original),
            Err(SettingsDraftError::InvalidNumber { field: "Fade in" })
        );
    }
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
    let mut streaming = SettingsDraft::from(&persisted);
    streaming.streaming_enabled = !streaming.streaming_enabled;
    toggle_edits.push(streaming);
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
        start_on_login: false,
        ..Settings::default()
    };
    let mut draft = SettingsDraft::from(&persisted);
    draft.language = "fr".to_owned();
    draft.start_on_login = true;

    draft.discard_changes(&persisted);

    assert_eq!(draft, SettingsDraft::from(&persisted));
    assert!(!draft.is_dirty_against(&persisted));
}

#[test]
fn applying_a_draft_preserves_settings_that_the_form_does_not_expose() {
    let original = Settings {
        openai_api_key: "secret".to_owned(),
        transcription_model: "private-transcriber".to_owned(),
        show_tray_icon: false,
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
