use super::events::{
    ListenerCommand, NativeHotkeyControl, NativeHotkeyControlError, NativeHotkeyEvent,
    NativeHotkeySignalTrigger, ReconfigurationFailure,
};
use super::listener::{DiscoverDevices, NativeHotkeyListener};
use crate::hotkey::{AGENTDICTATE_TEST_DEVICE_NAME, HotkeyListenerStatus, HotkeySignal};
use evdev::{AttributeSet, EventType, InputEvent, KeyCode, uinput::VirtualDevice};
use std::{
    io::{self, Write},
    os::unix::net::UnixDatagram,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

#[test]
fn native_listener_opens_polls_reads_and_reconnects_evdev_keyboards() {
    let Some((mut keyboard, path)) =
        test_keyboard_or_skip("native_listener_opens_polls_reads_and_reconnects_evdev_keyboards")
    else {
        return;
    };
    let expected_path = path.clone();
    let discovered = Arc::new(Mutex::new(vec![path]));
    let discovery_state = Arc::clone(&discovered);
    let discover: Arc<DiscoverDevices> = Arc::new(move |_| {
        Ok(discovery_state
            .lock()
            .expect("discovery paths lock")
            .clone())
    });
    let listener =
        NativeHotkeyListener::start_with_discovery("F24".parse().expect("valid hotkey"), discover)
            .expect("native listener starts");

    if !listener.readiness().is_ready() {
        receive_until(&listener, |event| {
            matches!(
                event,
                NativeHotkeyEvent::Status(HotkeyListenerStatus::Ready { active_devices: 1 })
            )
        });
    }
    // Held down: the disconnect below must release it.
    press_f24(&mut keyboard);
    let pressed = receive_until(
        &listener,
        |event| matches!(event, NativeHotkeyEvent::Signal(signal) if signal.signal == HotkeySignal::Pressed),
    );
    let NativeHotkeyEvent::Signal(pressed) = pressed else {
        unreachable!("the predicate accepts only a hotkey press")
    };
    assert_eq!(pressed.device.path, expected_path);
    assert_eq!(pressed.device.name, AGENTDICTATE_TEST_DEVICE_NAME);
    assert!(matches!(
        pressed.trigger,
        NativeHotkeySignalTrigger::Input(input)
            if input.code == KeyCode::KEY_F24.code()
                && input.state == crate::hotkey::KeyState::Pressed
    ));

    discovered.lock().expect("discovery paths lock").clear();
    drop(keyboard);
    let mut released = false;
    let mut unavailable = false;
    while !released || !unavailable {
        match receive_until(&listener, |event| {
            matches!(
                event,
                NativeHotkeyEvent::Signal(signal) if signal.signal == HotkeySignal::Released
            ) || matches!(
                event,
                NativeHotkeyEvent::Status(HotkeyListenerStatus::Unavailable { active_devices: 0 })
            )
        }) {
            NativeHotkeyEvent::Signal(signal) if signal.signal == HotkeySignal::Released => {
                released = true
            }
            NativeHotkeyEvent::Status(HotkeyListenerStatus::Unavailable { active_devices: 0 }) => {
                unavailable = true
            }
            _ => unreachable!("predicate only accepts disconnect events"),
        }
    }

    let (mut replacement, replacement_path) =
        virtual_keyboard().expect("replacement virtual keyboard");
    *discovered.lock().expect("discovery paths lock") = vec![replacement_path];
    receive_until(&listener, |event| {
        matches!(
            event,
            NativeHotkeyEvent::Status(HotkeyListenerStatus::Ready { active_devices: 1 })
        )
    });
    tap_f24(&mut replacement);
    assert!(matches!(
        receive_until(&listener, |event| {
            matches!(event, NativeHotkeyEvent::Signal(signal) if signal.signal == HotkeySignal::Pressed)
        }),
        NativeHotkeyEvent::Signal(signal) if signal.signal == HotkeySignal::Pressed
    ));
}

#[test]
fn cloneable_control_reconfigures_the_live_listener_without_a_polling_delay() {
    let Some((mut keyboard, path)) = test_keyboard_or_skip(
        "cloneable_control_reconfigures_the_live_listener_without_a_polling_delay",
    ) else {
        return;
    };
    let discovered = vec![path];
    let discover: Arc<DiscoverDevices> = Arc::new(move |_| Ok(discovered.clone()));
    let listener = NativeHotkeyListener::start_with_discovery(
        "Ctrl+F24".parse().expect("valid initial hotkey"),
        discover,
    )
    .expect("native listener starts");
    wait_until_ready(&listener);

    let control = listener.control_handle().clone();
    assert!(matches!(
        control.reconfigure_text("Ctrl+Hyper"),
        Err(NativeHotkeyControlError::Parse(_))
    ));
    control
        .reconfigure("F24".parse().expect("valid replacement hotkey"))
        .expect("reconfiguration is queued and wakes poll");
    receive_until(&listener, |event| {
        matches!(
            event,
            NativeHotkeyEvent::Status(HotkeyListenerStatus::Ready { active_devices: 1 })
        )
    });

    // F24 alone never matched the initial Ctrl+F24, so a press proves the swap.
    tap_f24(&mut keyboard);
    assert!(matches!(
        receive_until(&listener, |event| {
            matches!(event, NativeHotkeyEvent::Signal(signal) if signal.signal == HotkeySignal::Pressed)
        }),
        NativeHotkeyEvent::Signal(signal) if signal.signal == HotkeySignal::Pressed
    ));
}

#[test]
fn failed_reconfiguration_keeps_the_ready_hotkey_active() {
    let Some((mut keyboard, path)) =
        test_keyboard_or_skip("failed_reconfiguration_keeps_the_ready_hotkey_active")
    else {
        return;
    };
    let discover: Arc<DiscoverDevices> = Arc::new(move |spec| {
        Ok((spec.display() == "F24")
            .then(|| path.clone())
            .into_iter()
            .collect())
    });
    let listener = NativeHotkeyListener::start_with_discovery(
        "F24".parse().expect("valid initial hotkey"),
        discover,
    )
    .expect("native listener starts");
    wait_until_ready(&listener);

    let error = listener
        .control_handle()
        .reconfigure_text("Ctrl+F24")
        .expect_err("the caller learns that the live listener rejected Ctrl+F24");
    assert!(error.to_string().contains("no keyboard supports"));
    receive_until(&listener, |event| {
        matches!(
            event,
            NativeHotkeyEvent::ReconfigurationRejected { hotkey, .. } if hotkey == "Ctrl+F24"
        )
    });
    assert_eq!(
        receive_until(&listener, |event| {
            matches!(
                event,
                NativeHotkeyEvent::Status(HotkeyListenerStatus::Ready { active_devices: 1 })
            )
        }),
        NativeHotkeyEvent::Status(HotkeyListenerStatus::Ready { active_devices: 1 })
    );

    // Only the kept F24 hotkey matches F24 alone; Ctrl+F24 would not.
    tap_f24(&mut keyboard);
    assert!(matches!(
        receive_until(&listener, |event| {
            matches!(event, NativeHotkeyEvent::Signal(signal) if signal.signal == HotkeySignal::Pressed)
        }),
        NativeHotkeyEvent::Signal(signal) if signal.signal == HotkeySignal::Pressed
    ));
}

#[test]
fn control_only_returns_success_after_the_worker_accepts_reconfiguration() {
    let (wake, control_reader) = UnixDatagram::pair().unwrap();
    wake.set_nonblocking(true).unwrap();
    control_reader.set_nonblocking(true).unwrap();
    let (command_sender, commands) = mpsc::channel();
    let control = NativeHotkeyControl {
        commands: command_sender,
        wake: Arc::new(wake),
    };
    let worker = thread::spawn(move || {
        let command = commands.recv().unwrap();
        let ListenerCommand::Reconfigure { response, .. } = command else {
            panic!("expected reconfiguration")
        };
        response
            .send(Err(ReconfigurationFailure {
                hotkey: "F9".into(),
                reason: "no keyboard supports the requested hotkey".into(),
            }))
            .unwrap();
        drop(control_reader);
    });

    let error = control
        .reconfigure_text("F9")
        .expect_err("rejection must be synchronous");

    assert!(matches!(
        error,
        NativeHotkeyControlError::ReconfigurationRejected { .. }
    ));
    worker.join().unwrap();
}

/// Creates the test keyboard, or prints a visible skip when this environment
/// has no usable /dev/uinput. Written straight to stderr because libtest hides
/// `eprintln!` output of passing tests.
fn test_keyboard_or_skip(test: &str) -> Option<(VirtualDevice, PathBuf)> {
    virtual_keyboard()
        .inspect_err(|error| {
            let _ = writeln!(
                io::stderr(),
                "SKIPPED {test}: cannot create a uinput keyboard ({error})"
            );
        })
        .ok()
}

/// The listener under test grabs this keyboard when it opens it (see
/// `open_keyboard`), so its presses never reach the desktop. Running daemons
/// also ignore its name (`AGENTDICTATE_TEST_DEVICE_NAME`), and its only key,
/// F24, is bound by nothing (xkb maps F20–F23 to mic and touchpad toggles).
fn virtual_keyboard() -> io::Result<(VirtualDevice, PathBuf)> {
    let mut keys = AttributeSet::<KeyCode>::new();
    keys.insert(KeyCode::KEY_F24);
    let mut keyboard = VirtualDevice::builder()?
        .name(AGENTDICTATE_TEST_DEVICE_NAME)
        .with_keys(&keys)?
        .build()?;
    let path = keyboard
        .enumerate_dev_nodes_blocking()?
        .next()
        .transpose()?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "virtual event node"))?;
    Ok((keyboard, path))
}

fn press_f24(keyboard: &mut VirtualDevice) {
    emit_f24(keyboard, 1);
}

fn tap_f24(keyboard: &mut VirtualDevice) {
    emit_f24(keyboard, 1);
    emit_f24(keyboard, 0);
}

fn emit_f24(keyboard: &mut VirtualDevice, value: i32) {
    keyboard
        .emit(&[InputEvent::new(
            EventType::KEY.0,
            KeyCode::KEY_F24.code(),
            value,
        )])
        .expect("virtual F24 event is emitted");
}

fn wait_until_ready(listener: &NativeHotkeyListener) {
    if !listener.readiness().is_ready() {
        receive_until(listener, |event| {
            matches!(
                event,
                NativeHotkeyEvent::Status(HotkeyListenerStatus::Ready { active_devices: 1 })
            )
        });
    }
}

fn receive_until(
    listener: &NativeHotkeyListener,
    predicate: impl Fn(&NativeHotkeyEvent) -> bool,
) -> NativeHotkeyEvent {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let event = listener
            .recv_timeout(remaining)
            .expect("native listener event before deadline");
        if predicate(&event) {
            return event;
        }
    }
}
