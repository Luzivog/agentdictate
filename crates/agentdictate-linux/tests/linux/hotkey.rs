use agentdictate_linux::hotkey::{
    HotkeyParseError, HotkeySignal, HotkeySpec, HotkeyTracker, KEY_ESC, KEY_F8, KEY_F9,
    KEY_LEFT_ALT, KEY_LEFT_CTRL, KEY_LEFT_META, KEY_LEFT_SHIFT, KEY_RIGHT_CTRL, KEY_SPACE,
    KeyInput, KeyState,
};

#[test]
fn ctrl_space_accepts_either_control_key() {
    let hotkey: HotkeySpec = "Ctrl+Space".parse().expect("supported hotkey");

    assert!(hotkey.matches([KEY_LEFT_CTRL, KEY_SPACE]));
    assert!(hotkey.matches([KEY_RIGHT_CTRL, KEY_SPACE]));
    assert!(!hotkey.matches([KEY_SPACE]));
}

#[test]
fn one_physical_chord_emits_one_press_and_one_release() {
    let spec: HotkeySpec = "Ctrl+Space".parse().expect("supported hotkey");
    let mut tracker = HotkeyTracker::new(spec);

    assert_eq!(
        tracker.input(10, KeyInput::new(KEY_LEFT_CTRL, KeyState::Pressed)),
        None
    );
    assert_eq!(
        tracker.input(10, KeyInput::new(KEY_SPACE, KeyState::Pressed)),
        Some(HotkeySignal::Pressed)
    );
    assert_eq!(
        tracker.input(10, KeyInput::new(KEY_SPACE, KeyState::Repeated)),
        None
    );
    assert_eq!(
        tracker.input(10, KeyInput::new(KEY_SPACE, KeyState::Released)),
        Some(HotkeySignal::Released)
    );
}

#[test]
fn escape_cancels_without_turning_release_into_a_second_action() {
    let spec: HotkeySpec = "Ctrl+Space".parse().expect("supported hotkey");
    let mut tracker = HotkeyTracker::new(spec);

    tracker.input(10, KeyInput::new(KEY_LEFT_CTRL, KeyState::Pressed));
    assert_eq!(
        tracker.input(10, KeyInput::new(KEY_SPACE, KeyState::Pressed)),
        Some(HotkeySignal::Pressed)
    );
    assert_eq!(
        tracker.input(10, KeyInput::new(KEY_ESC, KeyState::Pressed)),
        Some(HotkeySignal::Cancelled)
    );
    assert_eq!(
        tracker.input(10, KeyInput::new(KEY_SPACE, KeyState::Released)),
        None
    );
    assert_eq!(
        tracker.input(10, KeyInput::new(KEY_LEFT_CTRL, KeyState::Released)),
        None
    );

    tracker.input(10, KeyInput::new(KEY_LEFT_CTRL, KeyState::Pressed));
    assert_eq!(
        tracker.input(10, KeyInput::new(KEY_SPACE, KeyState::Pressed)),
        Some(HotkeySignal::Pressed)
    );
}

#[test]
fn removing_a_keyboard_clears_stale_pressed_keys() {
    let spec: HotkeySpec = "Ctrl+Space".parse().expect("supported hotkey");
    let mut tracker = HotkeyTracker::new(spec);

    tracker.input(10, KeyInput::new(KEY_LEFT_CTRL, KeyState::Pressed));
    assert_eq!(
        tracker.input(10, KeyInput::new(KEY_SPACE, KeyState::Pressed)),
        Some(HotkeySignal::Pressed)
    );
    assert_eq!(tracker.remove_device(10), Some(HotkeySignal::Released));

    assert_eq!(
        tracker.input(20, KeyInput::new(KEY_SPACE, KeyState::Pressed)),
        None
    );
}

#[test]
fn chord_keys_from_different_devices_never_manufacture_a_press() {
    let spec: HotkeySpec = "Ctrl+Space".parse().expect("supported hotkey");
    let mut tracker = HotkeyTracker::new(spec);

    assert_eq!(
        tracker.input(10, KeyInput::new(KEY_LEFT_CTRL, KeyState::Pressed)),
        None
    );
    assert_eq!(
        tracker.input(20, KeyInput::new(KEY_SPACE, KeyState::Pressed)),
        None
    );
}

#[test]
fn supported_hotkey_vocabulary_maps_to_linux_key_codes() {
    let modified: HotkeySpec = "Shift Alt Meta F8".parse().expect("supported hotkey");
    let function_key: HotkeySpec = "F9".parse().expect("supported hotkey");

    assert!(modified.matches([KEY_LEFT_SHIFT, KEY_LEFT_ALT, KEY_LEFT_META, KEY_F8]));
    assert!(function_key.matches([KEY_F9]));
}

#[test]
fn shortcut_capture_vocabulary_accepts_letters_numbers_navigation_and_function_keys() {
    let letter: HotkeySpec = "Ctrl+Alt+D".parse().expect("captured letter chord");
    let number: HotkeySpec = "Super+7".parse().expect("captured number chord");
    let tab: HotkeySpec = "Ctrl+Tab".parse().expect("captured Tab chord");
    let function: HotkeySpec = "F12".parse().expect("captured function key");

    assert!(letter.matches([KEY_LEFT_CTRL, KEY_LEFT_ALT, 32]));
    assert!(number.matches([KEY_LEFT_META, 8]));
    assert!(tab.matches([KEY_LEFT_CTRL, 15]));
    assert!(function.matches([88]));
}

#[test]
fn invalid_hotkeys_fail_with_actionable_parse_errors() {
    assert_eq!("".parse::<HotkeySpec>(), Err(HotkeyParseError::Empty));
    assert_eq!(
        "Ctrl+Hyper".parse::<HotkeySpec>(),
        Err(HotkeyParseError::UnsupportedPart("hyper".into()))
    );
}
