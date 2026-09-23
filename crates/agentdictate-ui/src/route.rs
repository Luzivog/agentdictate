/// A settings-window destination with a stable navigation order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Route {
    #[default]
    Overview,
    History,
    Settings,
}

impl Route {
    pub const ALL: [Self; 3] = [Self::Overview, Self::History, Self::Settings];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::History => "History",
            Self::Settings => "Settings",
        }
    }

    pub const fn slug(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::History => "history",
            Self::Settings => "settings",
        }
    }

    pub const fn navigation_id(self) -> &'static str {
        match self {
            Self::Overview => "nav-overview",
            Self::History => "nav-history",
            Self::Settings => "nav-settings",
        }
    }

    pub const fn accessibility_label(self) -> &'static str {
        match self {
            Self::Overview => "Open Overview",
            Self::History => "Open History",
            Self::Settings => "Open Settings",
        }
    }
}
