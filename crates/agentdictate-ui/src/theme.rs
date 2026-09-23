/// An opaque sRGB color used by the toolkit-independent presentation model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl Color {
    pub const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }
}

/// Semantic colors for AgentDictate's compact dark interface.
///
/// This is the single palette: GPUI components, the gpui-component theme and
/// the window frame all read these roles instead of their own literals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeTokens {
    pub canvas: Color,
    pub sidebar: Color,
    pub sidebar_border: Color,
    pub surface: Color,
    pub surface_hovered: Color,
    pub border: Color,
    pub text: Color,
    pub text_muted: Color,
    pub accent: Color,
    pub info: Color,
    pub success: Color,
    pub danger: Color,
    /// Secondary accent that marks the Settings route.
    pub highlight: Color,
}

impl ThemeTokens {
    pub const fn tokscope_dark() -> Self {
        Self {
            canvas: Color::rgb(10, 10, 10),
            sidebar: Color::rgb(13, 13, 13),
            sidebar_border: Color::rgb(30, 30, 30),
            surface: Color::rgb(18, 18, 18),
            surface_hovered: Color::rgb(26, 26, 26),
            border: Color::rgb(33, 33, 33),
            text: Color::rgb(237, 237, 237),
            text_muted: Color::rgb(133, 133, 133),
            accent: Color::rgb(217, 119, 87),
            info: Color::rgb(113, 197, 234),
            success: Color::rgb(121, 201, 142),
            danger: Color::rgb(227, 104, 104),
            highlight: Color::rgb(167, 139, 250),
        }
    }
}

impl Default for ThemeTokens {
    fn default() -> Self {
        Self::tokscope_dark()
    }
}

#[cfg(feature = "desktop")]
pub(crate) fn gpui_color(color: Color) -> gpui::Hsla {
    gpui::rgb((u32::from(color.red) << 16) | (u32::from(color.green) << 8) | u32::from(color.blue))
        .into()
}

/// Installs gpui-component and maps its dark theme onto `ThemeTokens`.
#[cfg(feature = "desktop")]
pub(crate) fn initialize_gpui_theme(cx: &mut gpui::App) {
    use gpui_component::{Theme, ThemeMode};

    gpui_component::init(cx);
    Theme::change(ThemeMode::Dark, None, cx);
    let tokens = ThemeTokens::default();
    let theme = Theme::global_mut(cx);
    theme.background = gpui_color(tokens.canvas);
    theme.foreground = gpui_color(tokens.text);
    theme.muted_foreground = gpui_color(tokens.text_muted);
    theme.sidebar = gpui_color(tokens.sidebar);
    theme.sidebar_foreground = gpui_color(tokens.text);
    theme.sidebar_border = gpui_color(tokens.sidebar_border);
    theme.sidebar_accent = gpui_color(tokens.surface_hovered);
    theme.sidebar_accent_foreground = gpui_color(tokens.text);
    theme.secondary = gpui_color(tokens.surface);
    theme.secondary_foreground = gpui_color(tokens.text);
    theme.secondary_hover = gpui_color(tokens.surface_hovered);
    theme.secondary_active = gpui_color(tokens.border);
    theme.popover = gpui_color(tokens.surface);
    theme.popover_foreground = gpui_color(tokens.text);
    theme.border = gpui_color(tokens.border);
    theme.window_border = gpui_color(tokens.border);
    theme.title_bar = gpui_color(tokens.canvas);
    theme.title_bar_border = gpui_color(tokens.canvas);
    // Root, tooltips and popovers read the resolved token copy, and the Base
    // layer (scrollbars, resize handles) mirrors the theme only on sync.
    theme.tokens = gpui_component::theme::ThemeTokens::from(&theme.colors);
    Theme::sync_base(cx);
}
