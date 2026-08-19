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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpacingTokens {
    pub compact: u16,
    pub control: u16,
    pub section: u16,
    pub page: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RadiusTokens {
    pub control: u16,
    pub surface: u16,
    pub pill: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypographyTokens {
    pub caption: u16,
    pub body: u16,
    pub title: u16,
}

/// Semantic colors for AgentDictate's compact dark interface.
///
/// Components consume these roles rather than depending on palette literals,
/// which keeps interaction and status meaning consistent across every window.
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
    pub spacing: SpacingTokens,
    pub radii: RadiusTokens,
    pub typography: TypographyTokens,
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
            spacing: SpacingTokens {
                compact: 4,
                control: 8,
                section: 16,
                page: 20,
            },
            radii: RadiusTokens {
                control: 8,
                surface: 12,
                pill: 999,
            },
            typography: TypographyTokens {
                caption: 11,
                body: 13,
                title: 16,
            },
        }
    }
}

impl Default for ThemeTokens {
    fn default() -> Self {
        Self::tokscope_dark()
    }
}

#[cfg(feature = "desktop")]
pub(crate) fn initialize_gpui_theme(cx: &mut gpui::App) {
    use gpui::rgb;
    use gpui_component::{Theme, ThemeMode};

    gpui_component::init(cx);
    Theme::change(ThemeMode::Dark, None, cx);
    let theme = Theme::global_mut(cx);
    theme.background = rgb(0x0a0a0a).into();
    theme.foreground = rgb(0xededed).into();
    theme.muted_foreground = rgb(0x858585).into();
    theme.sidebar = rgb(0x0d0d0d).into();
    theme.sidebar_foreground = rgb(0xededed).into();
    theme.sidebar_border = rgb(0x1e1e1e).into();
    theme.sidebar_accent = rgb(0x1a1a1a).into();
    theme.sidebar_accent_foreground = rgb(0xededed).into();
    theme.skeleton = rgb(0x2a2a2a).into();
    theme.secondary = rgb(0x121212).into();
    theme.secondary_foreground = rgb(0xededed).into();
    theme.secondary_hover = rgb(0x1a1a1a).into();
    theme.secondary_active = rgb(0x212121).into();
    theme.popover = rgb(0x121212).into();
    theme.popover_foreground = rgb(0xededed).into();
    theme.border = rgb(0x212121).into();
    theme.window_border = rgb(0x212121).into();
    theme.title_bar = rgb(0x0a0a0a).into();
    theme.title_bar_border = rgb(0x0a0a0a).into();
}
