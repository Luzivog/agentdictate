use super::events::{
    ListenerCommand, NativeHotkeyControl, NativeHotkeyControlError, NativeHotkeyEvent,
    NativeHotkeySignalTrigger, ReconfigurationFailure,
};
use super::listener::{DiscoverDevices, NativeHotkeyListener};
use crate::hotkey::{HotkeyListenerStatus, HotkeySignal};
use evdev::{AttributeSet, EventType, InputEvent, KeyCode, uinput::VirtualDevice};
use std::{
    io,
    os::unix::net::UnixDatagram,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

#[test]
fn native_listener_opens_polls_reads_and_reconnects_evdev_keyboards() {
    if !Path::new("/dev/uinput").exists() {
        return;
    }
    let Ok((mut keyboard, path)) = virtual_keyboard() else {
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
    let listener = NativeHotkeyListener::start_with_discovery(
        "Ctrl+Space".parse().expect("valid hotkey"),
        discover,
    )
    .expect("native listener starts");

    if !listener.readiness().is_ready() {
        receive_until(&listener, |event| {
            matches!(
                event,
                NativeHotkeyEvent::Status(HotkeyListenerStatus::Ready { active_devices: 1 })
            )
        });
    }
    emit_chord(&mut keyboard);
    let pressed = receive_until(
        &listener,
        |event| matches!(event, NativeHotkeyEvent::Signal(signal) if signal.signal == HotkeySignal::Pressed),
    );
    let NativeHotkeyEvent::Signal(pressed) = pressed else {
        unreachable!("the predicate accepts only a hotkey press")
    };
    assert_eq!(pressed.device.path, expected_path);
    assert_eq!(pressed.device.name, "AgentDictate listener test");
    assert!(matches!(
        pressed.trigger,
        NativeHotkeySignalTrigger::Input(input)
            if input.code == crate::hotkey::KEY_SPACE
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
    emit_chord(&mut replacement);
    assert!(matches!(
        receive_until(&listener, |event| {
            matches!(event, NativeHotkeyEvent::Signal(signal) if signal.signal == HotkeySignal::Pressed)
        }),
        NativeHotkeyEvent::Signal(signal) if signal.signal == HotkeySignal::Pressed
    ));
}

#[test]
fn cloneable_control_reconfigures_the_live_listener_without_a_polling_delay() {
    if !Path::new("/dev/uinput").exists() {
        return;
    }
    let Ok((mut keyboard, path)) = virtual_keyboard() else {
        return;
    };
    let discovered = vec![path];
    let discover: Arc<DiscoverDevices> = Arc::new(move |_| Ok(discovered.clone()));
    let listener = NativeHotkeyListener::start_with_discovery(
        "Ctrl+Space".parse().expect("valid initial hotkey"),
        discover,
    )
    .expect("native listener starts");
    wait_until_ready(&listener);

    keyboard
        .emit(&[InputEvent::new(
            EventType::KEY.0,
            KeyCode::KEY_LEFTCTRL.code(),
            1,
        )])
        .expect("partial old chord is emitted");
    let control = listener.control_handle().clone();
    assert!(matches!(
        control.reconfigure_text("Ctrl+Hyper"),
        Err(NativeHotkeyControlError::Parse(_))
    ));
    control
        .reconfigure("F9".parse().expect("valid replacement hotkey"))
        .expect("reconfiguration is queued and wakes poll");
    receive_until(&listener, |event| {
        matches!(
            event,
            NativeHotkeyEvent::Status(HotkeyListenerStatus::Ready { active_devices: 1 })
        )
    });

    keyboard
        .emit(&[InputEvent::new(EventType::KEY.0, KeyCode::KEY_F9.code(), 1)])
        .expect("new hotkey is emitted");
    assert!(matches!(
        receive_until(&listener, |event| {
            matches!(event, NativeHotkeyEvent::Signal(signal) if signal.signal == HotkeySignal::Pressed)
        }),
        NativeHotkeyEvent::Signal(signal) if signal.signal == HotkeySignal::Pressed
    ));
}

#[test]
fn failed_reconfiguration_keeps_the_ready_hotkey_active() {
    if !Path::new("/dev/uinput").exists() {
        return;
    }
    let Ok((mut keyboard, path)) = virtual_keyboard() else {
        return;
    };
    let discover: Arc<DiscoverDevices> = Arc::new(move |spec| {
        Ok((spec.display() == "Ctrl+Space")
            .then(|| path.clone())
            .into_iter()
            .collect())
    });
    let listener = NativeHotkeyListener::start_with_discovery(
        "Ctrl+Space".parse().expect("valid initial hotkey"),
        discover,
    )
    .expect("native listener starts");
    wait_until_ready(&listener);

    let error = listener
        .control_handle()
        .reconfigure_text("F9")
        .expect_err("the caller learns that the live listener rejected F9");
    assert!(error.to_string().contains("no keyboard supports"));
    receive_until(&listener, |event| {
        matches!(
            event,
            NativeHotkeyEvent::ReconfigurationRejected { hotkey, .. } if hotkey == "F9"
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

    emit_chord(&mut keyboard);
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

fn virtual_keyboard() -> io::Result<(VirtualDevice, PathBuf)> {
    let mut keys = AttributeSet::<KeyCode>::new();
    keys.insert(KeyCode::KEY_LEFTCTRL);
    keys.insert(KeyCode::KEY_SPACE);
    keys.insert(KeyCode::KEY_F9);
    let mut keyboard = VirtualDevice::builder()?
        .name("AgentDictate listener test")
        .with_keys(&keys)?
        .build()?;
    let path = keyboard
        .enumerate_dev_nodes_blocking()?
        .next()
        .transpose()?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "virtual event node"))?;
    Ok((keyboard, path))
}

fn emit_chord(keyboard: &mut VirtualDevice) {
    keyboard
        .emit(&[
            InputEvent::new(EventType::KEY.0, KeyCode::KEY_LEFTCTRL.code(), 1),
            InputEvent::new(EventType::KEY.0, KeyCode::KEY_SPACE.code(), 1),
        ])
        .expect("virtual chord is emitted");
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
