use std::{
    collections::{BTreeSet, HashMap, HashSet},
    error::Error,
    fmt,
    str::FromStr,
};

#[cfg(feature = "native-hotkey")]
use std::path::{Path, PathBuf};

pub type KeyCode = u16;

pub const KEY_LEFT_CTRL: KeyCode = 29;
pub const KEY_RIGHT_CTRL: KeyCode = 97;
pub const KEY_LEFT_ALT: KeyCode = 56;
pub const KEY_RIGHT_ALT: KeyCode = 100;
pub const KEY_LEFT_META: KeyCode = 125;
pub const KEY_RIGHT_META: KeyCode = 126;
pub const KEY_LEFT_SHIFT: KeyCode = 42;
pub const KEY_RIGHT_SHIFT: KeyCode = 54;
pub const KEY_ESC: KeyCode = 1;
pub const KEY_TAB: KeyCode = 15;
pub const KEY_ENTER: KeyCode = 28;
pub const KEY_SPACE: KeyCode = 57;
pub const KEY_F8: KeyCode = 66;
pub const KEY_F9: KeyCode = 67;
pub const AGENTDICTATE_INJECTION_DEVICE_NAME: &str = "AgentDictate virtual keyboard";
pub const YDOTOOL_INJECTION_DEVICE_NAME: &str = "ydotoold virtual device";

pub type DeviceId = u64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceFacts {
    pub supports_hotkey: bool,
    /// Virtual origin is diagnostic data only. Accessibility and remoting
    /// keyboards remain eligible unless their exact identity is self-injection.
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
        if is_self_injection_keyboard(&name) {
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

fn is_self_injection_keyboard(name: &str) -> bool {
    name.eq_ignore_ascii_case(YDOTOOL_INJECTION_DEVICE_NAME)
        || name.eq_ignore_ascii_case(AGENTDICTATE_INJECTION_DEVICE_NAME)
        || name.eq_ignore_ascii_case("AgentDictate paste device")
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
#[cfg(feature = "native-hotkey")]
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

#[cfg(feature = "native-hotkey")]
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HotkeyParseError {
    Empty,
    UnsupportedPart(String),
}

impl fmt::Display for HotkeyParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("hotkey is empty"),
            Self::UnsupportedPart(part) => write!(formatter, "unsupported hotkey part: {part}"),
        }
    }
}

impl Error for HotkeyParseError {}

impl FromStr for HotkeySpec {
    type Err = HotkeyParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut groups = Vec::new();
        for part in value
            .split(|character: char| character == '+' || character.is_whitespace())
            .filter(|part| !part.is_empty())
        {
            let normalized = part.to_ascii_lowercase();
            let group = match normalized.as_str() {
                "ctrl" | "control" => vec![KEY_LEFT_CTRL, KEY_RIGHT_CTRL],
                "alt" => vec![KEY_LEFT_ALT, KEY_RIGHT_ALT],
                "super" | "meta" => vec![KEY_LEFT_META, KEY_RIGHT_META],
                "shift" => vec![KEY_LEFT_SHIFT, KEY_RIGHT_SHIFT],
                other => captured_key_code(other)
                    .map(|code| vec![code])
                    .ok_or_else(|| HotkeyParseError::UnsupportedPart(other.to_owned()))?,
            };
            groups.push(group);
        }
        if groups.is_empty() {
            return Err(HotkeyParseError::Empty);
        }
        Ok(Self {
            display: value.to_owned(),
            groups,
        })
    }
}

fn captured_key_code(key: &str) -> Option<KeyCode> {
    Some(match key {
        "space" => KEY_SPACE,
        "tab" => KEY_TAB,
        "enter" | "return" => KEY_ENTER,
        "1" => 2,
        "2" => 3,
        "3" => 4,
        "4" => 5,
        "5" => 6,
        "6" => 7,
        "7" => 8,
        "8" => 9,
        "9" => 10,
        "0" => 11,
        "q" => 16,
        "w" => 17,
        "e" => 18,
        "r" => 19,
        "t" => 20,
        "y" => 21,
        "u" => 22,
        "i" => 23,
        "o" => 24,
        "p" => 25,
        "a" => 30,
        "s" => 31,
        "d" => 32,
        "f" => 33,
        "g" => 34,
        "h" => 35,
        "j" => 36,
        "k" => 37,
        "l" => 38,
        "z" => 44,
        "x" => 45,
        "c" => 46,
        "v" => 47,
        "b" => 48,
        "n" => 49,
        "m" => 50,
        "f1" => 59,
        "f2" => 60,
        "f3" => 61,
        "f4" => 62,
        "f5" => 63,
        "f6" => 64,
        "f7" => 65,
        "f8" => KEY_F8,
        "f9" => KEY_F9,
        "f10" => 68,
        "f11" => 87,
        "f12" => 88,
        _ => return None,
    })
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
            self.matched = false;
            self.cancelled_until_release = true;
            return Some(HotkeySignal::Cancelled);
        }

        let matches = self
            .pressed_by_device
            .values()
            .any(|pressed| self.spec.matches_set(pressed));
        self.transition(matches)
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
