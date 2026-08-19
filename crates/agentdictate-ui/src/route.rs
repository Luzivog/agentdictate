use std::{error::Error, fmt, str::FromStr};

/// A settings-window destination with stable ordering and deep-link identity.
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteParseError {
    slug: String,
}

impl fmt::Display for RouteParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown AgentDictate route: {}", self.slug)
    }
}

impl Error for RouteParseError {}

impl FromStr for Route {
    type Err = RouteParseError;

    fn from_str(slug: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|route| route.slug() == slug)
            .ok_or_else(|| RouteParseError {
                slug: slug.to_owned(),
            })
    }
}
