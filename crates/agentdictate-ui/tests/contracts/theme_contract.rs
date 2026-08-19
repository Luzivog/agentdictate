//! Theme token contracts.

use agentdictate_ui::{Color, ThemeTokens};

#[test]
fn tokscope_dark_theme_exposes_the_semantic_palette() {
    let theme = ThemeTokens::tokscope_dark();

    assert_eq!(theme.canvas, Color::rgb(10, 10, 10));
    assert_eq!(theme.sidebar, Color::rgb(13, 13, 13));
    assert_eq!(theme.sidebar_border, Color::rgb(30, 30, 30));
    assert_eq!(theme.surface, Color::rgb(18, 18, 18));
    assert_eq!(theme.surface_hovered, Color::rgb(26, 26, 26));
    assert_eq!(theme.border, Color::rgb(33, 33, 33));
    assert_eq!(theme.text, Color::rgb(237, 237, 237));
    assert_eq!(theme.text_muted, Color::rgb(133, 133, 133));
    assert_eq!(theme.accent, Color::rgb(217, 119, 87));
    assert_eq!(theme.info, Color::rgb(113, 197, 234));
    assert_eq!(theme.success, Color::rgb(121, 201, 142));
    assert_eq!(theme.danger, Color::rgb(227, 104, 104));
    assert_eq!(theme.spacing.compact, 4);
    assert_eq!(theme.spacing.control, 8);
    assert_eq!(theme.spacing.section, 16);
    assert_eq!(theme.spacing.page, 20);
    assert_eq!(theme.radii.control, 8);
    assert_eq!(theme.radii.surface, 12);
    assert_eq!(theme.radii.pill, 999);
    assert_eq!(theme.typography.caption, 11);
    assert_eq!(theme.typography.body, 13);
    assert_eq!(theme.typography.title, 16);
}
