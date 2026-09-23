/// A settings-window destination with a stable navigation order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Route {
    #[default]
    Overview,
    History,
    Words,
    Settings,
}

impl Route {
    pub const ALL: [Self; 4] = [Self::Overview, Self::History, Self::Words, Self::Settings];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::History => "History",
            Self::Words => "Words",
            Self::Settings => "Settings",
        }
    }

    pub const fn slug(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::History => "history",
            Self::Words => "words",
            Self::Settings => "settings",
        }
    }

    pub const fn navigation_id(self) -> &'static str {
        match self {
            Self::Overview => "nav-overview",
            Self::History => "nav-history",
            Self::Words => "nav-words",
            Self::Settings => "nav-settings",
        }
    }

    pub const fn accessibility_label(self) -> &'static str {
        match self {
            Self::Overview => "Open Overview",
            Self::History => "Open History",
            Self::Words => "Open Words",
            Self::Settings => "Open Settings",
        }
    }
}
