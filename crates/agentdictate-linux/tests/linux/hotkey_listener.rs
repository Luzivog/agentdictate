use std::collections::BTreeSet;

use agentdictate_linux::hotkey::{
    CaptureStep, HotkeyListenerStatus, HotkeyModifier, HotkeySession, HotkeySignal, HotkeySpec,
    KEY_ESC, KEY_LEFT_CTRL, KEY_LEFT_SHIFT, KEY_RIGHT_ALT, KEY_SPACE, KeyInput, KeyState,
};

const KEY_Q: u16 = 16;
const KEY_F5: u16 = 63;

fn press(code: u16) -> KeyInput {
    KeyInput::new(code, KeyState::Pressed)
}

fn ready_session(hotkey: &str) -> HotkeySession {
    let mut session = HotkeySession::new(hotkey.parse().expect("valid hotkey"));
    session.connect_device(10);
    session.connect_device(20);
    session.finish_initial_scan();
    session
}

#[test]
fn listener_is_ready_only_after_initial_devices_are_connected() {
    let spec: HotkeySpec = "Ctrl+Space".parse().expect("valid hotkey");
    let mut session = HotkeySession::new(spec);

    assert_eq!(session.status(), HotkeyListenerStatus::Starting);
    session.connect_device(10);

    assert_eq!(
        session.finish_initial_scan(),
        HotkeyListenerStatus::Ready { active_devices: 1 }
    );
}

#[test]
fn disconnected_keyboard_releases_its_state_and_reconnected_device_works() {
    let spec: HotkeySpec = "Ctrl+Space".parse().expect("valid hotkey");
    let mut session = HotkeySession::new(spec);
    session.connect_device(10);
    session.finish_initial_scan();

    session.input(10, KeyInput::new(KEY_LEFT_CTRL, KeyState::Pressed));
    assert_eq!(
        session.input(10, KeyInput::new(KEY_SPACE, KeyState::Pressed)),
        Some(HotkeySignal::Pressed)
    );
    assert_eq!(session.disconnect_device(10), Some(HotkeySignal::Released));
    assert_eq!(
        session.status(),
        HotkeyListenerStatus::Unavailable { active_devices: 0 }
    );

    session.connect_device(20);
    assert_eq!(
        session.status(),
        HotkeyListenerStatus::Ready { active_devices: 1 }
    );
    session.input(20, KeyInput::new(KEY_LEFT_CTRL, KeyState::Pressed));
    assert_eq!(
        session.input(20, KeyInput::new(KEY_SPACE, KeyState::Pressed)),
        Some(HotkeySignal::Pressed)
    );
}

#[test]
fn native_evdev_key_events_are_translated_without_losing_repeat_or_release() {
    use agentdictate_linux::native_hotkey::evdev_key_input;
    use evdev::{EventType, InputEvent};

    assert_eq!(
        evdev_key_input(InputEvent::new(EventType::KEY.0, KEY_SPACE, 1)),
        Some(KeyInput::new(KEY_SPACE, KeyState::Pressed))
    );
    assert_eq!(
        evdev_key_input(InputEvent::new(EventType::KEY.0, KEY_SPACE, 2)),
        Some(KeyInput::new(KEY_SPACE, KeyState::Repeated))
    );
    assert_eq!(
        evdev_key_input(InputEvent::new(EventType::KEY.0, KEY_SPACE, 0)),
        Some(KeyInput::new(KEY_SPACE, KeyState::Released))
    );
}

#[test]
fn capture_records_the_physical_key_pressed_with_its_modifiers() {
    let mut session = ready_session("Ctrl+Space");

    assert_eq!(session.capture_input(10, press(KEY_LEFT_CTRL)), None);
    assert_eq!(session.capture_input(10, press(KEY_RIGHT_ALT)), None);
    // Physical Q: "A" on an AZERTY keyboard. Capture keeps the keycode.
    assert_eq!(
        session.capture_input(10, press(KEY_Q)),
        Some(CaptureStep::Chord {
            modifiers: BTreeSet::from([HotkeyModifier::Ctrl, HotkeyModifier::Alt]),
            key: KEY_Q,
        })
    );
}

#[test]
fn capture_waits_past_bare_keys_and_modifiers_and_esc_cancels_it() {
    let mut session = ready_session("Ctrl+Space");

    assert_eq!(session.capture_input(10, press(KEY_Q)), None);
    assert_eq!(session.capture_input(10, press(KEY_LEFT_SHIFT)), None);
    assert_eq!(
        session.capture_input(10, KeyInput::new(KEY_LEFT_SHIFT, KeyState::Repeated)),
        None
    );
    // A modifier held on another keyboard is not part of this chord.
    assert_eq!(session.capture_input(20, press(KEY_Q)), None);
    assert_eq!(
        session.capture_input(20, press(KEY_F5)),
        Some(CaptureStep::Chord {
            modifiers: BTreeSet::new(),
            key: KEY_F5,
        })
    );
    assert_eq!(
        session.capture_input(10, press(KEY_ESC)),
        Some(CaptureStep::Cancelled)
    );
}

#[test]
fn pressing_the_current_shortcut_during_capture_captures_it_without_a_hotkey_signal() {
    let mut session = ready_session("Ctrl+Space");

    session.capture_input(10, press(KEY_LEFT_CTRL));
    assert!(matches!(
        session.capture_input(10, press(KEY_SPACE)),
        Some(CaptureStep::Chord { key: KEY_SPACE, .. })
    ));

    // The withheld press never gets a lone release after the capture, and
    // the next press works as a hotkey again.
    assert_eq!(
        session.input(10, KeyInput::new(KEY_SPACE, KeyState::Released)),
        None
    );
    assert_eq!(
        session.input(10, press(KEY_SPACE)),
        Some(HotkeySignal::Pressed)
    );
}
