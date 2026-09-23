/// A settings-window destination with a stable navigation order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Route {
    #[default]
    Overview,
    History,
    Replacements,
    Settings,
}

impl Route {
    pub const ALL: [Self; 4] = [
        Self::Overview,
        Self::History,
        Self::Replacements,
        Self::Settings,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::History => "History",
            Self::Replacements => "Replacements",
            Self::Settings => "Settings",
        }
    }

    pub const fn slug(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::History => "history",
            Self::Replacements => "replacements",
            Self::Settings => "settings",
        }
    }

    pub const fn navigation_id(self) -> &'static str {
        match self {
            Self::Overview => "nav-overview",
            Self::History => "nav-history",
            Self::Replacements => "nav-replacements",
            Self::Settings => "nav-settings",
        }
    }

    pub const fn accessibility_label(self) -> &'static str {
        match self {
            Self::Overview => "Open Overview",
            Self::History => "Open History",
            Self::Replacements => "Open Replacements",
            Self::Settings => "Open Settings",
        }
    }
}
