use std::time::{Duration, Instant};

use agentdictate_app::{HotkeyActionOutcome, HotkeyDispatchGate, HotkeyIgnoreReason};
use agentdictate_linux::{
    hotkey::{HotkeySignal, KEY_ESC, KEY_SPACE, KeyInput, KeyState},
    native_hotkey::{NativeHotkeyDevice, NativeHotkeySignal, NativeHotkeySignalTrigger},
};

fn hotkey_event(signal: HotkeySignal, observed_at: Instant) -> NativeHotkeySignal {
    let input = match signal {
        HotkeySignal::Pressed => KeyInput::new(KEY_SPACE, KeyState::Pressed),
        HotkeySignal::Released => KeyInput::new(KEY_SPACE, KeyState::Released),
        HotkeySignal::Cancelled => KeyInput::new(KEY_ESC, KeyState::Pressed),
    };
    NativeHotkeySignal {
        signal,
        device: NativeHotkeyDevice {
            id: 20,
            path: "/dev/input/event20".into(),
            name: "Test keyboard".into(),
        },
        trigger: NativeHotkeySignalTrigger::Input(input),
        observed_at,
    }
}

#[test]
fn toggle_start_rearms_at_the_dispatch_boundary() {
    let started_at = Instant::now();
    let completed_at = started_at + Duration::from_millis(100);
    let mut gate = HotkeyDispatchGate::default();

    assert!(
        gate.accept("toggle", &hotkey_event(HotkeySignal::Pressed, started_at))
            .is_ok()
    );
    assert!(
        gate.complete(HotkeyActionOutcome::ToggleRecordingStarted, completed_at)
            .is_none()
    );
    assert_eq!(
        gate.accept(
            "toggle",
            &hotkey_event(
                HotkeySignal::Released,
                completed_at + Duration::from_millis(10),
            ),
        ),
        Err(HotkeyIgnoreReason::ToggleRelease)
    );
    assert!(matches!(
        gate.accept(
            "toggle",
            &hotkey_event(
                HotkeySignal::Pressed,
                completed_at + Duration::from_millis(149),
            ),
        ),
        Err(HotkeyIgnoreReason::ToggleRearming { .. })
    ));
    assert!(
        gate.accept(
            "toggle",
            &hotkey_event(
                HotkeySignal::Pressed,
                completed_at + Duration::from_millis(150),
            ),
        )
        .is_ok()
    );
}

#[test]
fn cancellation_replaces_one_queued_hold_release() {
    let started_at = Instant::now();
    let mut gate = HotkeyDispatchGate::default();

    assert!(
        gate.accept("hold", &hotkey_event(HotkeySignal::Pressed, started_at))
            .is_ok()
    );
    assert_eq!(
        gate.accept(
            "hold",
            &hotkey_event(
                HotkeySignal::Released,
                started_at + Duration::from_millis(20),
            ),
        ),
        Err(HotkeyIgnoreReason::TerminalQueued)
    );
    assert_eq!(
        gate.accept(
            "hold",
            &hotkey_event(
                HotkeySignal::Cancelled,
                started_at + Duration::from_millis(30),
            ),
        ),
        Err(HotkeyIgnoreReason::TerminalQueued)
    );
    assert_eq!(
        gate.accept(
            "hold",
            &hotkey_event(
                HotkeySignal::Released,
                started_at + Duration::from_millis(40),
            ),
        ),
        Err(HotkeyIgnoreReason::ActionInFlight)
    );

    assert_eq!(
        gate.complete(
            HotkeyActionOutcome::Other,
            started_at + Duration::from_millis(100),
        )
        .map(|event| event.signal),
        Some(HotkeySignal::Cancelled)
    );
    assert!(
        gate.complete(
            HotkeyActionOutcome::Other,
            started_at + Duration::from_millis(120),
        )
        .is_none()
    );
}
