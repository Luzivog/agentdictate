use agentdictate_core::{
    AppSnapshot, DesktopReadiness, ExposedInput, HotkeyReadiness, MissingTool, Readiness,
};

use crate::{HistoryViewModel, Route, WorkspaceViewModel, needs_setup};

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
    /// Setup fixes it, and the card opens Setup.
    pub opens_setup: bool,
    /// A terminal command the fix starts with, which the card shows with a
    /// Copy button.
    pub command: Option<String>,
}

impl ReadinessFix {
    fn new(title: &'static str, detail: impl Into<String>) -> Self {
        Self {
            title,
            detail: detail.into(),
            opens_setup: false,
            command: None,
        }
    }

    fn in_setup(title: &'static str, detail: impl Into<String>) -> Self {
        Self {
            opens_setup: true,
            ..Self::new(title, detail)
        }
    }
}

/// Why other apps can read the keyboard, and what to do about it. Granting
/// access cannot override another app's rule, because udev applies that
/// rule's mode last, so that rule must go first.
pub(crate) fn exposed_input_detail(exposed: &ExposedInput) -> String {
    match &exposed.rule {
        Some(rule) => format!(
            "{} lets every app read your keyboard. Delete it, or change its MODE to \"0660\", \
             then run agentdictate setup-access.",
            rule.display()
        ),
        None => "Every app on this computer can read your keyboard. Granting access should make \
                 it private again; if not, delete the udev rule that opens it."
            .to_owned(),
    }
}

/// The command that deletes another app's world-access rule.
fn delete_rule_command(exposed: &ExposedInput) -> Option<String> {
    let rule = exposed.rule.as_ref()?;
    Some(format!("sudo rm {}", shell_quoted(&rule.to_string_lossy())))
}

/// `text` as one shell word: unchanged when that is safe, else in single
/// quotes.
fn shell_quoted(text: &str) -> String {
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "/._-+@=:,".contains(character));
    if plain {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

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
            ReadinessFix::in_setup(
                "The shortcut isn't working",
                "AgentDictate can't read your keyboard yet. Setup can give it access.",
            )
        } else if !transcription_key {
            ReadinessFix::in_setup(
                "Add your OpenAI API key",
                "AgentDictate turns your speech into text with OpenAI.",
            )
        } else if missing(MissingTool::PwRecord) {
            ReadinessFix::new(
                "AgentDictate can't record",
                "pw-record isn't installed. Install PipeWire's tools (pipewire-bin), then restart AgentDictate.",
            )
        } else if !paste_access {
            ReadinessFix::in_setup(
                "AgentDictate can't paste",
                "Your text is only copied until it can. Setup can give it access.",
            )
        } else if let Some(exposed) = exposed_input {
            ReadinessFix {
                opens_setup: exposed.rule.is_none(),
                command: delete_rule_command(exposed),
                ..ReadinessFix::new(
                    "Other apps can read your keyboard",
                    exposed_input_detail(exposed),
                )
            }
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
    pub navigation: [NavigationItemViewModel; Route::NAVIGATION.len()],
    pub workspace: WorkspaceViewModel,
}

impl ShellViewModel {
    /// A shell before the daemon's first snapshot.
    pub fn new(active_route: Route) -> Self {
        Self {
            active_route,
            navigation: Route::NAVIGATION.map(|route| NavigationItemViewModel {
                route,
                label: route.title(),
                is_active: route == active_route,
            }),
            workspace: WorkspaceViewModel::default(),
        }
    }

    /// The window as it opens on the daemon's `snapshot`: on Setup while
    /// dictation can't work yet, otherwise on Home.
    pub fn from_app_snapshot(snapshot: AppSnapshot) -> Self {
        let route = if needs_setup(&snapshot.readiness) {
            Route::Setup
        } else {
            Route::Home
        };
        let mut model = Self::new(route);
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
