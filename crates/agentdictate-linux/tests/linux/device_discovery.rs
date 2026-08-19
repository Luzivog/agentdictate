use agentdictate_linux::hotkey::{
    DeviceFacts, HotkeySpec, KeyCapabilities, discover_keyboard_devices,
};

#[test]
fn virtual_injection_keyboard_is_excluded_from_hotkey_sources() {
    let proc_devices = concat!(
        "N: Name=\"AT Translated Set 2 keyboard\"\n",
        "H: Handlers=sysrq kbd event4 leds\n\n",
        "N: Name=\"ydotoold virtual device\"\n",
        "H: Handlers=sysrq kbd event17\n",
    );

    let devices = discover_keyboard_devices(proc_devices, |_| DeviceFacts {
        supports_hotkey: true,
        is_virtual: false,
    });

    assert_eq!(devices, ["event4"]);
}

#[test]
fn sysfs_key_capabilities_must_include_every_hotkey_group() {
    let hotkey: HotkeySpec = "Ctrl+Space".parse().expect("supported hotkey");
    let complete = KeyCapabilities::parse("0200000020000000").expect("valid capability mask");
    let missing_control =
        KeyCapabilities::parse("0200000000000000").expect("valid capability mask");

    assert!(complete.supports(&hotkey));
    assert!(!missing_control.supports(&hotkey));
}

#[test]
fn unrelated_virtual_keyboard_is_allowed_when_it_supports_the_hotkey() {
    let proc_devices = concat!("N: Name=\"USB Keyboard\"\n", "H: Handlers=kbd event8\n",);

    let devices = discover_keyboard_devices(proc_devices, |_| DeviceFacts {
        supports_hotkey: true,
        is_virtual: true,
    });

    assert_eq!(devices, ["event8"]);
}

#[test]
fn agentdictate_self_injection_keyboard_is_excluded_by_identity() {
    let proc_devices = concat!(
        "N: Name=\"AgentDictate virtual keyboard\"\n",
        "H: Handlers=kbd event18\n",
    );

    let devices = discover_keyboard_devices(proc_devices, |_| DeviceFacts {
        supports_hotkey: true,
        is_virtual: true,
    });

    assert!(devices.is_empty());
}

#[test]
fn malformed_sysfs_key_capabilities_are_rejected() {
    assert!(KeyCapabilities::parse("not-hex").is_err());
}
