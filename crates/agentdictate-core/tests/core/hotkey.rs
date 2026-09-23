use std::collections::BTreeSet;

use agentdictate_core::{Hotkey, HotkeyModifier, HotkeyParseError, Settings, physical_key_name};

fn stored_hotkey(stored: serde_json::Value) -> Hotkey {
    serde_json::from_value::<Settings>(serde_json::json!({ "hotkey": stored }))
        .unwrap()
        .hotkey
}

#[test]
fn named_shortcuts_from_older_configs_keep_their_us_qwerty_keys_and_text() {
    let ctrl_space = stored_hotkey("Ctrl+Space".into());
    assert_eq!(ctrl_space, Hotkey::default());
    assert_eq!(ctrl_space.label(), "Ctrl+Space");

    // The key right of Caps Lock: "A" on QWERTY, "Q" on AZERTY.
    let letter = stored_hotkey("Ctrl+Alt+A".into());
    assert_eq!(
        letter.modifiers(),
        &BTreeSet::from([HotkeyModifier::Ctrl, HotkeyModifier::Alt])
    );
    assert_eq!(letter.key(), 30);
    assert_eq!(letter.label(), "Ctrl+Alt+A");
}

#[test]
fn captured_shortcuts_round_trip_through_config_with_their_keycode_and_label() {
    // Thomas's AZERTY "A" is the physical Q key.
    let captured = Hotkey::captured(BTreeSet::from([HotkeyModifier::Ctrl]), 16, "A");
    let settings = Settings {
        hotkey: captured.clone(),
        ..Settings::default()
    };

    let json = serde_json::to_value(&settings).unwrap();
    assert_eq!(
        json["hotkey"],
        serde_json::json!({ "modifiers": ["ctrl"], "key": 16, "label": "Ctrl+A" })
    );
    assert_eq!(
        serde_json::from_value::<Settings>(json).unwrap().hotkey,
        captured
    );
}

#[test]
fn an_unreadable_stored_shortcut_falls_back_to_the_default_instead_of_failing_the_load() {
    assert_eq!(stored_hotkey("Ctrl+Hyper".into()), Hotkey::default());
    assert_eq!(stored_hotkey("Ctrl+Alt".into()), Hotkey::default());
}

#[test]
fn named_shortcuts_need_exactly_one_known_key() {
    assert_eq!("".parse::<Hotkey>(), Err(HotkeyParseError::Empty));
    assert_eq!(
        "Ctrl+Hyper".parse::<Hotkey>(),
        Err(HotkeyParseError::UnsupportedPart("hyper".into()))
    );
    assert_eq!(
        "Ctrl+Alt".parse::<Hotkey>(),
        Err(HotkeyParseError::NotOneKey)
    );
    assert_eq!("A+B".parse::<Hotkey>(), Err(HotkeyParseError::NotOneKey));
    assert_eq!("Shift Meta return".parse::<Hotkey>().unwrap().key(), 28);
}

#[test]
fn physical_names_label_keys_by_us_position_and_unknown_codes_by_number() {
    assert_eq!(physical_key_name(16), "Q");
    assert_eq!(physical_key_name(57), "Space");
    assert_eq!(physical_key_name(105), "Left");
    assert_eq!(physical_key_name(194), "F24");
    assert_eq!(physical_key_name(250), "Key 250");
}
