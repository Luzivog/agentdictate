use std::{collections::BTreeSet, fmt, str::FromStr};

use serde::{Deserialize, Serialize};

/// A Linux evdev keycode. It names a physical key position, so a shortcut
/// stays on the same key whatever keyboard layout is active.
pub type KeyCode = u16;

/// A modifier of the global shortcut. Either physical key of the pair counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyModifier {
    Ctrl,
    Alt,
    Shift,
    Super,
}

impl HotkeyModifier {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ctrl => "Ctrl",
            Self::Alt => "Alt",
            Self::Shift => "Shift",
            Self::Super => "Super",
        }
    }
}

/// The global dictation shortcut: modifiers held while one key goes down.
///
/// `key` is an evdev keycode, so the shortcut follows the physical key the
/// user pressed when capturing it; `label` is how the layout named it then.
/// config.json written before keycodes stores a name such as "Ctrl+Space";
/// it still loads, with each name meaning its key on a US QWERTY keyboard.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "StoredHotkey")]
pub struct Hotkey {
    modifiers: BTreeSet<HotkeyModifier>,
    key: KeyCode,
    label: String,
}

impl Hotkey {
    /// A shortcut captured from the keyboard, labelled like "Ctrl+Alt+D"
    /// with `key_label` naming the key.
    #[must_use]
    pub fn captured(modifiers: BTreeSet<HotkeyModifier>, key: KeyCode, key_label: &str) -> Self {
        let label = modifiers
            .iter()
            .map(|modifier| modifier.label())
            .chain([key_label])
            .collect::<Vec<_>>()
            .join("+");
        Self {
            modifiers,
            key,
            label,
        }
    }

    #[must_use]
    pub const fn modifiers(&self) -> &BTreeSet<HotkeyModifier> {
        &self.modifiers
    }

    #[must_use]
    pub const fn key(&self) -> KeyCode {
        self.key
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }
}

impl Default for Hotkey {
    fn default() -> Self {
        Self::captured(BTreeSet::from([HotkeyModifier::Ctrl]), KEY_SPACE, "Space")
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

/// Reads a named shortcut such as "Ctrl+Space" or "Shift Alt F8". Key names
/// mean their US QWERTY position; the text is kept as the label.
impl FromStr for Hotkey {
    type Err = HotkeyParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut modifiers = BTreeSet::new();
        let mut keys = Vec::new();
        for part in value
            .split(|character: char| character == '+' || character.is_whitespace())
            .filter(|part| !part.is_empty())
        {
            let normalized = part.to_ascii_lowercase();
            let modifier = match normalized.as_str() {
                "ctrl" | "control" => HotkeyModifier::Ctrl,
                "alt" => HotkeyModifier::Alt,
                "super" | "meta" => HotkeyModifier::Super,
                "shift" => HotkeyModifier::Shift,
                other => {
                    keys.push(
                        named_key_code(other)
                            .ok_or_else(|| HotkeyParseError::UnsupportedPart(other.to_owned()))?,
                    );
                    continue;
                }
            };
            modifiers.insert(modifier);
        }
        match keys.as_slice() {
            [] if modifiers.is_empty() => Err(HotkeyParseError::Empty),
            [key] => Ok(Self {
                modifiers,
                key: *key,
                label: value.trim().to_owned(),
            }),
            _ => Err(HotkeyParseError::NotOneKey),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HotkeyParseError {
    #[error("hotkey is empty")]
    Empty,
    #[error("unsupported hotkey part: {0}")]
    UnsupportedPart(String),
    #[error("a hotkey needs exactly one key besides its modifiers")]
    NotOneKey,
}

/// The two stored forms of a shortcut: a name from before keycodes were
/// stored, or the keys themselves.
#[derive(Deserialize)]
#[serde(untagged)]
enum StoredHotkey {
    Named(String),
    Keys {
        modifiers: BTreeSet<HotkeyModifier>,
        key: KeyCode,
        label: String,
    },
}

impl TryFrom<StoredHotkey> for Hotkey {
    type Error = HotkeyParseError;

    fn try_from(stored: StoredHotkey) -> Result<Self, Self::Error> {
        match stored {
            StoredHotkey::Named(name) => name.parse(),
            StoredHotkey::Keys {
                modifiers,
                key,
                label,
            } => Ok(Self {
                modifiers,
                key,
                label,
            }),
        }
    }
}

/// Reads config.json's shortcut. A stored name this version cannot read
/// falls back to the default, so a hand edit never stops the daemon.
pub(crate) fn deserialize_hotkey<'de, D>(deserializer: D) -> Result<Hotkey, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let stored = StoredHotkey::deserialize(deserializer)?;
    Ok(Hotkey::try_from(stored).unwrap_or_default())
}

const KEY_SPACE: KeyCode = 57;

/// Physical key names, by US QWERTY position. They label keys that no
/// layout character names (Space, arrows, F-keys) and read named shortcuts.
const KEY_NAMES: &[(KeyCode, &str)] = &[
    (2, "1"),
    (3, "2"),
    (4, "3"),
    (5, "4"),
    (6, "5"),
    (7, "6"),
    (8, "7"),
    (9, "8"),
    (10, "9"),
    (11, "0"),
    (12, "-"),
    (13, "="),
    (14, "Backspace"),
    (15, "Tab"),
    (16, "Q"),
    (17, "W"),
    (18, "E"),
    (19, "R"),
    (20, "T"),
    (21, "Y"),
    (22, "U"),
    (23, "I"),
    (24, "O"),
    (25, "P"),
    (26, "["),
    (27, "]"),
    (28, "Enter"),
    (30, "A"),
    (31, "S"),
    (32, "D"),
    (33, "F"),
    (34, "G"),
    (35, "H"),
    (36, "J"),
    (37, "K"),
    (38, "L"),
    (39, ";"),
    (40, "'"),
    (41, "`"),
    (43, "\\"),
    (44, "Z"),
    (45, "X"),
    (46, "C"),
    (47, "V"),
    (48, "B"),
    (49, "N"),
    (50, "M"),
    (51, ","),
    (52, "."),
    (53, "/"),
    (KEY_SPACE, "Space"),
    (59, "F1"),
    (60, "F2"),
    (61, "F3"),
    (62, "F4"),
    (63, "F5"),
    (64, "F6"),
    (65, "F7"),
    (66, "F8"),
    (67, "F9"),
    (68, "F10"),
    (87, "F11"),
    (88, "F12"),
    (102, "Home"),
    (103, "Up"),
    (104, "PageUp"),
    (105, "Left"),
    (106, "Right"),
    (107, "End"),
    (108, "Down"),
    (109, "PageDown"),
    (110, "Insert"),
    (111, "Delete"),
    (183, "F13"),
    (184, "F14"),
    (185, "F15"),
    (186, "F16"),
    (187, "F17"),
    (188, "F18"),
    (189, "F19"),
    (190, "F20"),
    (191, "F21"),
    (192, "F22"),
    (193, "F23"),
    (194, "F24"),
];

/// Names `key` by its US QWERTY position, e.g. "Q" for the key right of Tab.
#[must_use]
pub fn physical_key_name(key: KeyCode) -> String {
    KEY_NAMES
        .iter()
        .find(|(code, _)| *code == key)
        .map_or_else(|| format!("Key {key}"), |(_, name)| (*name).to_owned())
}

/// Reads a key name in any case; "Return" is Enter.
fn named_key_code(name: &str) -> Option<KeyCode> {
    if name == "return" {
        return Some(28);
    }
    KEY_NAMES
        .iter()
        .find(|(_, known)| known.eq_ignore_ascii_case(name))
        .map(|(code, _)| *code)
}
