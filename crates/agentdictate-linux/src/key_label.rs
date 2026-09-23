//! Names keys the way the active keyboard layout prints them.

use agentdictate_core::{KeyCode, physical_key_name};
use x11rb::protocol::xproto::ConnectionExt as _;

/// X11 keycodes are evdev keycodes plus 8.
const X11_KEYCODE_OFFSET: KeyCode = 8;

/// Names `key` as the current layout prints it, e.g. "A" for the key right
/// of Tab on AZERTY. Keys that type no visible character (Space, arrows,
/// F-keys) and hosts without an X server use the physical name.
pub fn key_label(key: KeyCode) -> String {
    layout_keysym(key)
        .and_then(keysym_label)
        .unwrap_or_else(|| physical_key_name(key))
}

/// The keysym the layout's first group types for `key` without modifiers.
fn layout_keysym(key: KeyCode) -> Option<u32> {
    let keycode = u8::try_from(key.checked_add(X11_KEYCODE_OFFSET)?).ok()?;
    let (connection, _) = x11rb::connect(None).ok()?;
    let mapping = connection
        .get_keyboard_mapping(keycode, 1)
        .ok()?
        .reply()
        .ok()?;
    mapping.keysyms.first().copied()
}

/// The visible character a keysym types, uppercased like a keycap label.
/// Common dead keys show the accent they add, like AZERTY's "^" key.
fn keysym_label(keysym: u32) -> Option<String> {
    let code_point = match keysym {
        // Latin-1 keysyms equal their code points.
        0x21..=0x7e | 0xa1..=0xff => keysym,
        0x0100_0000..=0x0110_ffff => keysym - 0x0100_0000,
        0xfe50 => u32::from('`'),
        0xfe51 => u32::from('´'),
        0xfe52 => u32::from('^'),
        0xfe53 => u32::from('~'),
        0xfe57 => u32::from('¨'),
        _ => return None,
    };
    let character = char::from_u32(code_point)
        .filter(|character| !character.is_whitespace() && !character.is_control())?;
    Some(character.to_uppercase().collect())
}

#[cfg(test)]
mod tests {
    use super::keysym_label;

    #[test]
    fn printable_keysyms_become_keycap_labels_and_the_rest_fall_back() {
        assert_eq!(keysym_label(0x61).as_deref(), Some("A"));
        assert_eq!(keysym_label(0x26).as_deref(), Some("&"));
        assert_eq!(keysym_label(0xe9).as_deref(), Some("É"));
        assert_eq!(keysym_label(0x0100_20ac).as_deref(), Some("€"));
        assert_eq!(keysym_label(0xfe52).as_deref(), Some("^"));
        // Space and Left type nothing visible.
        assert_eq!(keysym_label(0x20), None);
        assert_eq!(keysym_label(0xff51), None);
    }
}
