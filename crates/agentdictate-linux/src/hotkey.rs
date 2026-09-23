use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    str::FromStr,
};

pub use agentdictate_core::{Hotkey, HotkeyModifier, HotkeyParseError, KeyCode};

pub const KEY_LEFT_CTRL: KeyCode = 29;
pub const KEY_RIGHT_CTRL: KeyCode = 97;
pub const KEY_LEFT_ALT: KeyCode = 56;
pub const KEY_RIGHT_ALT: KeyCode = 100;
pub const KEY_LEFT_META: KeyCode = 125;
pub const KEY_RIGHT_META: KeyCode = 126;
pub const KEY_LEFT_SHIFT: KeyCode = 42;
pub const KEY_RIGHT_SHIFT: KeyCode = 54;
pub const KEY_ESC: KeyCode = 1;
pub const KEY_SPACE: KeyCode = 57;
pub const KEY_F8: KeyCode = 66;
pub const KEY_F9: KeyCode = 67;
pub const AGENTDICTATE_INJECTION_DEVICE_NAME: &str = "AgentDictate virtual keyboard";
pub const YDOTOOL_INJECTION_DEVICE_NAME: &str = "ydotoold virtual device";
/// Name of the uinput keyboard the native listener tests type on. It is
/// ignored by discovery so a test run can never reach a running daemon.
pub const AGENTDICTATE_TEST_DEVICE_NAME: &str = "AgentDictate hotkey test keyboard";

/// Virtual keyboards the hotkey listener never reads: AgentDictate's own paste
/// injector (current and legacy names), ydotool's injector, and the listener
/// tests' keyboard. Injected keys must never trigger the hotkey.
const IGNORED_KEYBOARD_NAMES: [&str; 4] = [
    AGENTDICTATE_INJECTION_DEVICE_NAME,
    "AgentDictate paste device",
    YDOTOOL_INJECTION_DEVICE_NAME,
    AGENTDICTATE_TEST_DEVICE_NAME,
];

pub type DeviceId = u64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceFacts {
    pub supports_hotkey: bool,
    /// Virtual origin is diagnostic data only. Accessibility and remoting
    /// keyboards remain eligible unless their exact name is ignored
    /// (`IGNORED_KEYBOARD_NAMES`).
    pub is_virtual: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyCapabilities {
    least_significant_words_first: Vec<usize>,
}

impl KeyCapabilities {
    pub fn parse(value: &str) -> Result<Self, std::num::ParseIntError> {
        let mut words = value
            .split_whitespace()
            .map(|word| usize::from_str_radix(word, 16))
            .collect::<Result<Vec<_>, _>>()?;
        words.reverse();
        Ok(Self {
            least_significant_words_first: words,
        })
    }

    pub fn supports(&self, hotkey: &HotkeySpec) -> bool {
        hotkey
            .groups
            .iter()
            .all(|group| group.iter().any(|code| self.contains(*code)))
    }

    fn contains(&self, code: KeyCode) -> bool {
        let code = usize::from(code);
        let word_index = code / usize::BITS as usize;
        let bit_index = code % usize::BITS as usize;
        self.least_significant_words_first
            .get(word_index)
            .is_some_and(|word| word & (1_usize << bit_index) != 0)
    }
}

pub fn discover_keyboard_devices(
    proc_devices: &str,
    mut facts_for: impl FnMut(&str) -> DeviceFacts,
) -> Vec<String> {
    let mut devices = BTreeSet::new();
    for block in proc_devices.split("\n\n") {
        let name = block
            .lines()
            .find_map(|line| line.strip_prefix("N: Name=\""))
            .and_then(|name| name.strip_suffix('"'))
            .unwrap_or_default()
            .to_ascii_lowercase();
        if is_ignored_keyboard(&name) {
            continue;
        }
        let Some(handlers) = block
            .lines()
            .find_map(|line| line.strip_prefix("H: Handlers="))
        else {
            continue;
        };
        if !handlers.split_whitespace().any(|handler| handler == "kbd") {
            continue;
        }
        for handler in handlers
            .split_whitespace()
            .filter(|handler| is_event_handler(handler))
        {
            let facts = facts_for(handler);
            if facts.supports_hotkey {
                devices.insert(handler.to_owned());
            }
        }
    }
    devices.into_iter().collect()
}

fn is_ignored_keyboard(name: &str) -> bool {
    IGNORED_KEYBOARD_NAMES
        .iter()
        .any(|ignored| name.eq_ignore_ascii_case(ignored))
}

fn is_event_handler(handler: &str) -> bool {
    handler.strip_prefix("event").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// Discovers readable keyboard event paths using Linux's procfs/sysfs metadata.
///
/// Opening and polling the returned evdev nodes remains the runtime's concern;
/// this adapter only performs repeatable eligibility and virtual-device checks.
pub fn keyboard_event_paths(hotkey: &HotkeySpec) -> std::io::Result<Vec<PathBuf>> {
    let proc_devices = std::fs::read_to_string("/proc/bus/input/devices")?;
    Ok(discover_keyboard_devices(&proc_devices, |handler| {
        native_device_facts(handler, hotkey)
    })
    .into_iter()
    .map(|handler| Path::new("/dev/input").join(handler))
    .filter(|path| path.exists())
    .collect())
}

fn native_device_facts(handler: &str, hotkey: &HotkeySpec) -> DeviceFacts {
    let sysfs_device = Path::new("/sys/class/input").join(handler).join("device");
    let canonical = std::fs::canonicalize(&sysfs_device).ok();
    let is_virtual = canonical
        .as_deref()
        .and_then(Path::to_str)
        .is_some_and(|path| path.contains("/devices/virtual/"));
    let supports_hotkey = std::fs::read_to_string(sysfs_device.join("capabilities/key"))
        .ok()
        .and_then(|value| KeyCapabilities::parse(&value).ok())
        .is_some_and(|capabilities| capabilities.supports(hotkey));
    DeviceFacts {
        supports_hotkey,
        is_virtual,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyState {
    Released,
    Pressed,
    Repeated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyInput {
    pub code: KeyCode,
    pub state: KeyState,
}

impl KeyInput {
    pub const fn new(code: KeyCode, state: KeyState) -> Self {
        Self { code, state }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeySignal {
    Pressed,
    Released,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyListenerStatus {
    Starting,
    Ready { active_devices: usize },
    Unavailable { active_devices: usize },
}

/// Owns cross-device chord state while devices are added and removed by the
/// native listener. Readiness is explicit so the daemon never reports itself
/// ready before the initial set of readable keyboards has been opened.
pub struct HotkeySession {
    tracker: HotkeyTracker,
    connected_devices: HashSet<DeviceId>,
    initial_scan_finished: bool,
}

impl HotkeySession {
    pub fn new(spec: HotkeySpec) -> Self {
        Self {
            tracker: HotkeyTracker::new(spec),
            connected_devices: HashSet::new(),
            initial_scan_finished: false,
        }
    }

    pub fn connect_device(&mut self, device: DeviceId) {
        self.connected_devices.insert(device);
    }

    pub fn disconnect_device(&mut self, device: DeviceId) -> Option<HotkeySignal> {
        self.connected_devices.remove(&device);
        self.tracker.remove_device(device)
    }

    pub fn input(&mut self, device: DeviceId, input: KeyInput) -> Option<HotkeySignal> {
        self.connected_devices
            .contains(&device)
            .then(|| self.tracker.input(device, input))
            .flatten()
    }

    /// Feeds one key event while a shortcut capture is armed. Pressed keys
    /// stay tracked, but hotkey signals are withheld: pressing the current
    /// shortcut to capture it never starts dictation, and its release after
    /// the capture is swallowed like one after Esc.
    pub fn capture_input(&mut self, device: DeviceId, input: KeyInput) -> Option<CaptureStep> {
        if !self.connected_devices.contains(&device) {
            return None;
        }
        if self.tracker.input(device, input) == Some(HotkeySignal::Pressed) {
            self.tracker.swallow_until_release();
        }
        capture_step(self.tracker.pressed(device), input)
    }

    pub fn finish_initial_scan(&mut self) -> HotkeyListenerStatus {
        self.initial_scan_finished = true;
        self.status()
    }

    pub fn status(&self) -> HotkeyListenerStatus {
        if !self.initial_scan_finished {
            HotkeyListenerStatus::Starting
        } else if self.connected_devices.is_empty() {
            HotkeyListenerStatus::Unavailable { active_devices: 0 }
        } else {
            HotkeyListenerStatus::Ready {
                active_devices: self.connected_devices.len(),
            }
        }
    }
}

/// The key groups one shortcut needs held: one group per modifier (either
/// side's key) and one for the key itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotkeySpec {
    display: String,
    groups: Vec<Vec<KeyCode>>,
}

impl HotkeySpec {
    pub fn display(&self) -> &str {
        &self.display
    }

    pub fn matches(&self, pressed: impl IntoIterator<Item = KeyCode>) -> bool {
        let pressed: HashSet<_> = pressed.into_iter().collect();
        self.matches_set(&pressed)
    }

    fn matches_set(&self, pressed: &HashSet<KeyCode>) -> bool {
        self.groups
            .iter()
            .all(|group| group.iter().any(|code| pressed.contains(code)))
    }
}

impl From<&Hotkey> for HotkeySpec {
    fn from(hotkey: &Hotkey) -> Self {
        let mut groups = hotkey
            .modifiers()
            .iter()
            .map(|modifier| modifier_key_codes(*modifier).to_vec())
            .collect::<Vec<_>>();
        groups.push(vec![hotkey.key()]);
        Self {
            display: hotkey.label().to_owned(),
            groups,
        }
    }
}

impl FromStr for HotkeySpec {
    type Err = HotkeyParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse::<Hotkey>().map(|hotkey| Self::from(&hotkey))
    }
}

const fn modifier_key_codes(modifier: HotkeyModifier) -> [KeyCode; 2] {
    match modifier {
        HotkeyModifier::Ctrl => [KEY_LEFT_CTRL, KEY_RIGHT_CTRL],
        HotkeyModifier::Alt => [KEY_LEFT_ALT, KEY_RIGHT_ALT],
        HotkeyModifier::Shift => [KEY_LEFT_SHIFT, KEY_RIGHT_SHIFT],
        HotkeyModifier::Super => [KEY_LEFT_META, KEY_RIGHT_META],
    }
}

const fn key_modifier(code: KeyCode) -> Option<HotkeyModifier> {
    match code {
        KEY_LEFT_CTRL | KEY_RIGHT_CTRL => Some(HotkeyModifier::Ctrl),
        KEY_LEFT_ALT | KEY_RIGHT_ALT => Some(HotkeyModifier::Alt),
        KEY_LEFT_SHIFT | KEY_RIGHT_SHIFT => Some(HotkeyModifier::Shift),
        KEY_LEFT_META | KEY_RIGHT_META => Some(HotkeyModifier::Super),
        _ => None,
    }
}

/// A key press that ends a shortcut capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureStep {
    /// `key` went down while `modifiers` were held on the same keyboard.
    Chord {
        modifiers: BTreeSet<HotkeyModifier>,
        key: KeyCode,
    },
    /// Esc: stop capturing without a shortcut.
    Cancelled,
}

/// Decides whether a key event ends a shortcut capture. `pressed` holds the
/// keys down on the event's keyboard, this one included. Only a fresh press
/// counts; lone modifiers wait for their key, and a bare key needs a
/// modifier unless it is a function key, so typing never becomes a shortcut.
pub fn capture_step(pressed: Option<&HashSet<KeyCode>>, input: KeyInput) -> Option<CaptureStep> {
    if input.state != KeyState::Pressed || is_pointer_button(input.code) {
        return None;
    }
    if input.code == KEY_ESC {
        return Some(CaptureStep::Cancelled);
    }
    if key_modifier(input.code).is_some() {
        return None;
    }
    let modifiers = pressed
        .into_iter()
        .flatten()
        .filter_map(|code| key_modifier(*code))
        .collect::<BTreeSet<_>>();
    (!modifiers.is_empty() || is_function_key(input.code)).then_some(CaptureStep::Chord {
        modifiers,
        key: input.code,
    })
}

/// Mouse and joystick buttons (evdev `BTN_*`), which some keyboards report.
const fn is_pointer_button(code: KeyCode) -> bool {
    matches!(code, 0x100..=0x15f)
}

/// F1–F24.
const fn is_function_key(code: KeyCode) -> bool {
    matches!(code, 59..=68 | 87 | 88 | 183..=194)
}

pub struct HotkeyTracker {
    spec: HotkeySpec,
    pressed_by_device: HashMap<DeviceId, HashSet<KeyCode>>,
    matched: bool,
    cancelled_until_release: bool,
}

impl HotkeyTracker {
    pub fn new(spec: HotkeySpec) -> Self {
        Self {
            spec,
            pressed_by_device: HashMap::new(),
            matched: false,
            cancelled_until_release: false,
        }
    }

    pub fn input(&mut self, device: DeviceId, input: KeyInput) -> Option<HotkeySignal> {
        let pressed = self.pressed_by_device.entry(device).or_default();
        match input.state {
            KeyState::Pressed | KeyState::Repeated => {
                pressed.insert(input.code);
            }
            KeyState::Released => {
                pressed.remove(&input.code);
            }
        }

        if input.code == KEY_ESC && input.state == KeyState::Pressed {
            self.swallow_until_release();
            return Some(HotkeySignal::Cancelled);
        }

        let matches = self
            .pressed_by_device
            .values()
            .any(|pressed| self.spec.matches_set(pressed));
        self.transition(matches)
    }

    /// Ends the current match without a release signal and ignores the
    /// hotkey until its keys are let go.
    fn swallow_until_release(&mut self) {
        self.matched = false;
        self.cancelled_until_release = true;
    }

    fn pressed(&self, device: DeviceId) -> Option<&HashSet<KeyCode>> {
        self.pressed_by_device.get(&device)
    }

    pub fn remove_device(&mut self, device: DeviceId) -> Option<HotkeySignal> {
        self.pressed_by_device.remove(&device);
        let matches = self
            .pressed_by_device
            .values()
            .any(|pressed| self.spec.matches_set(pressed));
        self.transition(matches)
    }

    fn transition(&mut self, matches: bool) -> Option<HotkeySignal> {
        if self.cancelled_until_release {
            if !matches {
                self.cancelled_until_release = false;
            }
            return None;
        }
        match (self.matched, matches) {
            (false, true) => {
                self.matched = true;
                Some(HotkeySignal::Pressed)
            }
            (true, false) => {
                self.matched = false;
                Some(HotkeySignal::Released)
            }
            _ => None,
        }
    }
}
