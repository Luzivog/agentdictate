use agentdictate_core::{AppSnapshot, DesktopReadiness, HotkeyReadiness, MissingTool, Readiness};

use crate::{HistoryViewModel, Route, WorkspaceViewModel};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NavigationItemViewModel {
    pub route: Route,
    pub label: &'static str,
    pub is_active: bool,
}

/// Home's first line: that dictation is ready, or the one thing to fix
/// first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HomeStatus {
    /// "Ready — press Ctrl+Space anywhere to dictate".
    Ready {
        shortcut: String,
    },
    /// The shortcut listener is still starting.
    Starting,
    Fix(ReadinessFix),
}

/// Something that stops or weakens dictation, and how to fix it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadinessFix {
    pub title: &'static str,
    pub detail: String,
    /// The fix is made in Settings, which the card opens.
    pub opens_settings: bool,
}

impl ReadinessFix {
    fn new(title: &'static str, detail: impl Into<String>) -> Self {
        Self {
            title,
            detail: detail.into(),
            opens_settings: false,
        }
    }
}

const SETUP_ACCESS: &str = "Run agentdictate setup-access in a terminal, then log out and back in.";

impl HomeStatus {
    /// What Home says about `readiness`, with `shortcut` the configured
    /// shortcut's label. What stops dictation comes first, then what puts
    /// your keyboard at risk, then what only makes it worse.
    #[must_use]
    pub fn new(readiness: &Readiness, shortcut: &str) -> Self {
        let Readiness {
            shortcut: listener,
            transcription_key,
            desktop:
                DesktopReadiness {
                    paste_access,
                    exposed_input,
                    missing_tools,
                },
        } = readiness;
        let missing = |tool| missing_tools.contains(&tool);
        let fix = if let HotkeyReadiness::Unavailable { .. } = listener {
            ReadinessFix::new(
                "The shortcut isn't working",
                format!("AgentDictate can't read your keyboard. {SETUP_ACCESS}"),
            )
        } else if !transcription_key {
            ReadinessFix {
                opens_settings: true,
                ..ReadinessFix::new(
                    "Add your OpenAI API key",
                    "AgentDictate turns your speech into text with OpenAI. Add your key in Settings.",
                )
            }
        } else if missing(MissingTool::PwRecord) {
            ReadinessFix::new(
                "AgentDictate can't record",
                "pw-record isn't installed. Install PipeWire's tools (pipewire-bin), then restart AgentDictate.",
            )
        } else if !paste_access {
            ReadinessFix::new(
                "AgentDictate can't paste",
                format!("Your text will only be copied. {SETUP_ACCESS}"),
            )
        } else if let Some(exposed) = exposed_input {
            ReadinessFix::new(
                "Other apps can read your keyboard",
                match &exposed.rule {
                    Some(rule) => format!(
                        "Run agentdictate setup-access or remove the rule from {}.",
                        rule.display()
                    ),
                    None => "Run agentdictate setup-access to make your keyboard private again."
                        .to_owned(),
                },
            )
        } else if missing(MissingTool::Ffmpeg) {
            ReadinessFix::new(
                "Install ffmpeg for faster results",
                "Without ffmpeg, recordings upload uncompressed, which takes longer.",
            )
        } else if missing(MissingTool::Pactl) {
            ReadinessFix::new(
                "Other sounds can't be lowered",
                "pactl isn't installed. Install pulseaudio-utils, or turn off Lower other sounds.",
            )
        } else if *listener == HotkeyReadiness::Starting {
            return Self::Starting;
        } else {
            return Self::Ready {
                shortcut: shortcut.to_owned(),
            };
        };
        Self::Fix(fix)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShellViewModel {
    pub active_route: Route,
    pub navigation: [NavigationItemViewModel; Route::ALL.len()],
    pub workspace: WorkspaceViewModel,
}

impl ShellViewModel {
    /// A shell before the daemon's first snapshot.
    pub fn new(active_route: Route) -> Self {
        Self {
            active_route,
            navigation: Route::ALL.map(|route| NavigationItemViewModel {
                route,
                label: route.title(),
                is_active: route == active_route,
            }),
            workspace: WorkspaceViewModel::default(),
        }
    }

    pub fn from_app_snapshot(active_route: Route, snapshot: AppSnapshot) -> Self {
        let mut model = Self::new(active_route);
        model.workspace = WorkspaceViewModel::default().with_status(&snapshot);
        model.workspace.history = HistoryViewModel::new(
            0,
            u64::try_from(snapshot.recoverable_count).unwrap_or(u64::MAX),
        );
        model
    }

    pub fn with_history(mut self, history: HistoryViewModel) -> Self {
        self.workspace.history = history;
        self
    }

    pub fn with_workspace(mut self, workspace: WorkspaceViewModel) -> Self {
        self.workspace = workspace;
        self
    }

    pub fn select_route(&mut self, route: Route) {
        self.active_route = route;
        for item in &mut self.navigation {
            item.is_active = item.route == route;
        }
    }
}
