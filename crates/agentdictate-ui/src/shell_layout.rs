pub const SIDEBAR_OVERLAY_BREAKPOINT: u32 = 1_100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarMode {
    Docked,
    Overlay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShellLayout {
    pub sidebar_mode: SidebarMode,
}

impl ShellLayout {
    pub const fn from_width(width: u32) -> Self {
        let sidebar_mode = if width < SIDEBAR_OVERLAY_BREAKPOINT {
            SidebarMode::Overlay
        } else {
            SidebarMode::Docked
        };
        Self { sidebar_mode }
    }
}

pub fn sidebar_open_for_layout(
    currently_open: bool,
    previous_compact_layout: Option<bool>,
    compact_layout: bool,
) -> bool {
    if previous_compact_layout == Some(compact_layout) {
        currently_open
    } else {
        !compact_layout
    }
}
