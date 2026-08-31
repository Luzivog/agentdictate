use std::{
    fmt, io, thread,
    time::{Duration, Instant},
};

use evdev::{AttributeSet, EventType, InputEvent, KeyCode, uinput::VirtualDevice};

use crate::{hotkey::AGENTDICTATE_INJECTION_DEVICE_NAME, paste::PasteShortcut};

/// Paced like a physical chord: busy application event loops (Electron in
/// particular) intermittently drop zero-gap synthetic press/release bursts.
const KEY_EVENT_GAP: Duration = Duration::from_millis(25);

/// A freshly created uinput keyboard is invisible to the compositor until udev
/// and libinput pick it up; events emitted earlier are silently dropped.
const DEVICE_SETTLE: Duration = Duration::from_millis(500);

/// Every key any paste chord can press. Declared up front because uinput
/// devices advertise their capabilities at creation time.
const CHORD_KEYS: [KeyCode; 4] = [
    KeyCode::KEY_LEFTCTRL,
    KeyCode::KEY_LEFTSHIFT,
    KeyCode::KEY_INSERT,
    KeyCode::KEY_V,
];

#[derive(Debug)]
pub enum InjectionError {
    /// /dev/uinput is missing or not writable, or device creation failed.
    UinputUnavailable { source: io::Error },
    /// A key event could not be written; the virtual device has been destroyed
    /// so the kernel released anything still held.
    Emit { source: io::Error },
    /// The chord could not start and finish before the delivery deadline.
    /// Nothing was pressed.
    DeadlineBeforeInjection,
}

impl fmt::Display for InjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UinputUnavailable { source } => {
                write!(formatter, "uinput paste keyboard is unavailable: {source}")
            }
            Self::Emit { source } => {
                write!(formatter, "paste chord emission failed: {source}")
            }
            Self::DeadlineBeforeInjection => {
                write!(formatter, "delivery deadline expired before the paste chord started")
            }
        }
    }
}

impl std::error::Error for InjectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::UinputUnavailable { source } | Self::Emit { source } => Some(source),
            Self::DeadlineBeforeInjection => None,
        }
    }
}

/// Injects paste chords from an in-process uinput virtual keyboard. Keeping
/// the injection in-process guarantees every press is paired with a release,
/// and the kernel releases any held key if the daemon dies, so a chord can
/// never leave a key stuck (the failure mode of the previous ydotool path).
#[derive(Debug)]
pub struct PasteInjector {
    device: Option<ReadyDevice>,
}

#[derive(Debug)]
struct ReadyDevice {
    device: VirtualDevice,
    /// Earliest instant at which the compositor is assumed to see our events.
    ready_at: Instant,
}

impl Default for PasteInjector {
    fn default() -> Self {
        Self::new()
    }
}

impl PasteInjector {
    /// Creates the virtual keyboard eagerly so the settle window elapses long
    /// before the first paste. Failure is deferred to `inject`, which retries.
    #[must_use]
    pub fn new() -> Self {
        let mut injector = Self { device: None };
        if let Err(error) = injector.ensure_device() {
            tracing::warn!(%error, "paste keyboard creation deferred to first paste");
        }
        injector
    }

    /// Sends exactly one paste chord. The deadline is checked before the first
    /// press; a started chord always runs to completion so press/release stay
    /// paired. Failures are intentionally returned to the delivery state
    /// machine because retrying an ambiguous injection can duplicate text in
    /// the focused application.
    pub fn inject(
        &mut self,
        shortcut: PasteShortcut,
        deadline: Instant,
    ) -> Result<(), InjectionError> {
        let (modifiers, key) = chord(shortcut);
        let ready = self.ensure_device()?;
        let start = ready.ready_at.max(Instant::now());
        let events = 2 * (modifiers.len() as u32 + 1);
        if start + KEY_EVENT_GAP * events > deadline {
            return Err(InjectionError::DeadlineBeforeInjection);
        }
        thread::sleep(start.saturating_duration_since(Instant::now()));
        let result = emit_chord(&mut ready.device, modifiers, key);
        if result.is_err() {
            // Destroying the device makes the kernel release every held key.
            self.device = None;
        }
        result
    }

    /// Path of this injector's own event node. Lets tests (and diagnostics)
    /// observe exactly this device instead of matching by name, which is
    /// ambiguous when another AgentDictate process is running.
    pub fn device_node(&mut self) -> Option<std::path::PathBuf> {
        self.device
            .as_mut()?
            .device
            .enumerate_dev_nodes_blocking()
            .ok()?
            .next()?
            .ok()
    }

    fn ensure_device(&mut self) -> Result<&mut ReadyDevice, InjectionError> {
        if self.device.is_none() {
            let mut keys = AttributeSet::<KeyCode>::new();
            for key in CHORD_KEYS {
                keys.insert(key);
            }
            let device = VirtualDevice::builder()
                .and_then(|builder| {
                    builder
                        .name(AGENTDICTATE_INJECTION_DEVICE_NAME)
                        .with_keys(&keys)?
                        .build()
                })
                .map_err(|source| InjectionError::UinputUnavailable { source })?;
            self.device = Some(ReadyDevice {
                device,
                ready_at: Instant::now() + DEVICE_SETTLE,
            });
        }
        Ok(self.device.as_mut().expect("device was just ensured"))
    }
}

fn chord(shortcut: PasteShortcut) -> (&'static [KeyCode], KeyCode) {
    match shortcut {
        PasteShortcut::Universal => (&[KeyCode::KEY_LEFTSHIFT], KeyCode::KEY_INSERT),
        PasteShortcut::Standard => (&[KeyCode::KEY_LEFTCTRL], KeyCode::KEY_V),
        PasteShortcut::Terminal => (
            &[KeyCode::KEY_LEFTCTRL, KeyCode::KEY_LEFTSHIFT],
            KeyCode::KEY_V,
        ),
    }
}

/// Tracks pressed keys so every press is released even on early return or
/// panic. The happy path releases explicitly through `release_all` so errors
/// surface; `Drop` is only the safety net.
struct PressedKeys<'a> {
    device: &'a mut VirtualDevice,
    pressed: Vec<KeyCode>,
}

impl PressedKeys<'_> {
    fn press(&mut self, key: KeyCode) -> Result<(), InjectionError> {
        emit_key(self.device, key, 1)?;
        self.pressed.push(key);
        thread::sleep(KEY_EVENT_GAP);
        Ok(())
    }

    /// Releases in reverse order and empties `pressed` so Drop is a no-op.
    fn release_all(&mut self) -> Result<(), InjectionError> {
        while let Some(key) = self.pressed.pop() {
            emit_key(self.device, key, 0)?;
            thread::sleep(KEY_EVENT_GAP);
        }
        Ok(())
    }
}

impl Drop for PressedKeys<'_> {
    fn drop(&mut self) {
        for key in std::mem::take(&mut self.pressed).into_iter().rev() {
            let _ = emit_key(self.device, key, 0);
            thread::sleep(KEY_EVENT_GAP);
        }
    }
}

fn emit_chord(
    device: &mut VirtualDevice,
    modifiers: &[KeyCode],
    key: KeyCode,
) -> Result<(), InjectionError> {
    let mut chord = PressedKeys {
        device,
        pressed: Vec::with_capacity(modifiers.len() + 1),
    };
    for modifier in modifiers {
        chord.press(*modifier)?;
    }
    chord.press(key)?;
    chord.release_all()
}

fn emit_key(device: &mut VirtualDevice, key: KeyCode, value: i32) -> Result<(), InjectionError> {
    device
        .emit(&[InputEvent::new(EventType::KEY.0, key.code(), value)])
        .map_err(|source| InjectionError::Emit { source })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use evdev::Device;

    use super::*;

    /// Opens the injector's own event node and grabs it (EVIOCGRAB) so the
    /// chord is consumed exclusively by the test and never reaches the live
    /// compositor or the developer's focused window.
    fn grabbed_reader(injector: &mut PasteInjector) -> Option<Device> {
        let ready = injector.device.as_mut()?;
        let node = ready
            .device
            .enumerate_dev_nodes_blocking()
            .ok()?
            .next()?
            .ok()?;
        // udev applies the session ACL to a fresh uinput node asynchronously;
        // retry the open briefly instead of failing on the race.
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut reader = loop {
            match Device::open(&node) {
                Ok(reader) => break reader,
                Err(error)
                    if error.kind() == io::ErrorKind::PermissionDenied
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(_) => return None,
            }
        };
        reader.grab().ok()?;
        reader.set_nonblocking(true).ok()?;
        // The reader consumes the events directly; no compositor settle needed.
        ready.ready_at = Instant::now();
        Some(reader)
    }

    fn key_events(reader: &mut Device, expected: usize) -> Vec<(KeyCode, i32)> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut events = Vec::new();
        while events.len() < expected && Instant::now() < deadline {
            match reader.fetch_events() {
                Ok(batch) => {
                    events.extend(batch.filter(|event| event.event_type() == EventType::KEY).map(
                        |event| (KeyCode::new(event.code()), event.value()),
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("reading injected events failed: {error}"),
            }
        }
        events
    }

    fn assert_nothing_pressed(reader: &Device) {
        let pressed = reader.get_key_state().expect("key state is readable");
        assert_eq!(pressed.iter().count(), 0, "keys left pressed: {pressed:?}");
    }

    fn test_injector() -> Option<PasteInjector> {
        if !Path::new("/dev/uinput").exists() {
            return None;
        }
        let injector = PasteInjector::new();
        injector.device.as_ref()?;
        Some(injector)
    }

    #[test]
    fn universal_chord_pairs_every_press_with_a_release_in_reverse_order() {
        let Some(mut injector) = test_injector() else { return };
        let Some(mut reader) = grabbed_reader(&mut injector) else { return };

        injector
            .inject(PasteShortcut::Universal, Instant::now() + Duration::from_secs(5))
            .expect("universal chord is injected");

        assert_eq!(
            key_events(&mut reader, 4),
            vec![
                (KeyCode::KEY_LEFTSHIFT, 1),
                (KeyCode::KEY_INSERT, 1),
                (KeyCode::KEY_INSERT, 0),
                (KeyCode::KEY_LEFTSHIFT, 0),
            ],
        );
        assert_nothing_pressed(&reader);
    }

    #[test]
    fn terminal_chord_releases_modifiers_in_reverse_order() {
        let Some(mut injector) = test_injector() else { return };
        let Some(mut reader) = grabbed_reader(&mut injector) else { return };

        injector
            .inject(PasteShortcut::Terminal, Instant::now() + Duration::from_secs(5))
            .expect("terminal chord is injected");

        assert_eq!(
            key_events(&mut reader, 6),
            vec![
                (KeyCode::KEY_LEFTCTRL, 1),
                (KeyCode::KEY_LEFTSHIFT, 1),
                (KeyCode::KEY_V, 1),
                (KeyCode::KEY_V, 0),
                (KeyCode::KEY_LEFTSHIFT, 0),
                (KeyCode::KEY_LEFTCTRL, 0),
            ],
        );
        assert_nothing_pressed(&reader);
    }

    #[test]
    fn expired_deadline_fails_before_any_key_is_pressed() {
        let Some(mut injector) = test_injector() else { return };
        let Some(mut reader) = grabbed_reader(&mut injector) else { return };

        let result = injector.inject(
            PasteShortcut::Standard,
            Instant::now() - Duration::from_secs(1),
        );

        assert!(matches!(result, Err(InjectionError::DeadlineBeforeInjection)));
        assert_eq!(key_events(&mut reader, 1), vec![]);
        assert_nothing_pressed(&reader);
    }

    #[test]
    fn injection_device_uses_the_hotkey_excluded_name() {
        let Some(mut injector) = test_injector() else { return };
        let Some(reader) = grabbed_reader(&mut injector) else { return };

        assert_eq!(reader.name(), Some(AGENTDICTATE_INJECTION_DEVICE_NAME));
    }
}
