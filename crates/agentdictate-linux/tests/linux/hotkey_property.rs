use agentdictate_linux::hotkey::HotkeySpec;
use proptest::prelude::*;

proptest! {
    #[test]
    fn parsing_arbitrary_text_never_panics(value in any::<String>()) {
        let _ = value.parse::<HotkeySpec>();
    }

    #[test]
    fn reparsing_a_spec_display_is_stable(
        modifiers in proptest::collection::vec("ctrl|control|alt|super|meta|shift", 0..=2),
        key in "space|tab|return|enter|[0-9a-z]|f[1-9]",
    ) {
        let mut parts = modifiers.clone();
        parts.push(key);
        let value = parts.join("+");

        let spec = value.parse::<HotkeySpec>().expect("generated combos are valid hotkeys");
        prop_assert_eq!(spec.display(), value);
        prop_assert_eq!(spec.display().parse::<HotkeySpec>().unwrap(), spec);
    }
}
