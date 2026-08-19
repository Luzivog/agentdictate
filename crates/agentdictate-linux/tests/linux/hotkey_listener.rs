use agentdictate_linux::hotkey::{
    HotkeyListenerStatus, HotkeySession, HotkeySignal, HotkeySpec, KEY_LEFT_CTRL, KEY_SPACE,
    KeyInput, KeyState,
};

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

#[cfg(feature = "native-hotkey")]
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
