//! Responsive shell layout contracts.

use agentdictate_ui::{ShellLayout, SidebarMode, sidebar_open_for_layout};

#[test]
fn sidebar_becomes_an_overlay_below_the_tokscope_breakpoint() {
    assert_eq!(
        ShellLayout::from_width(1_099).sidebar_mode,
        SidebarMode::Overlay
    );
    assert_eq!(
        ShellLayout::from_width(1_100).sidebar_mode,
        SidebarMode::Docked
    );
}

#[test]
fn sidebar_layout_transitions_preserve_explicit_user_choice_until_mode_changes() {
    assert!(!sidebar_open_for_layout(true, None, true));
    assert!(sidebar_open_for_layout(true, Some(true), true));
    assert!(sidebar_open_for_layout(false, Some(true), false));
    assert!(sidebar_open_for_layout(true, Some(false), false));
}
