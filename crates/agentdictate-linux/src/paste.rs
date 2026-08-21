#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardProtocol {
    Wayland,
    X11,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardReadinessEvidence {
    ReadbackMatches,
    OwnerTransfer {
        previous_owner_exited: bool,
        new_owner_alive: bool,
    },
    Unobserved,
}

impl ClipboardReadinessEvidence {
    pub const fn confirms_ready(self) -> bool {
        match self {
            Self::ReadbackMatches => true,
            Self::OwnerTransfer {
                previous_owner_exited,
                new_owner_alive,
            } => previous_owner_exited && new_owner_alive,
            Self::Unobserved => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FocusTarget {
    protocol: ClipboardProtocol,
    identity: Option<String>,
    window_class: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct X11FocusObservation {
    pub window_id: String,
    pub window_class: String,
    pub focused: bool,
}

pub fn parse_x11_focus(window_id: impl Into<String>, properties: &str) -> X11FocusObservation {
    let window_class = properties
        .lines()
        .find(|line| line.starts_with("WM_CLASS"))
        .map(quoted_values)
        .unwrap_or_default()
        .join(" ");
    let focused = properties
        .lines()
        .any(|line| line.starts_with("_NET_WM_STATE") && line.contains("_NET_WM_STATE_FOCUSED"));
    X11FocusObservation {
        window_id: window_id.into(),
        window_class,
        focused,
    }
}

fn quoted_values(line: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for character in line.chars() {
        if character == '"' {
            if quoted {
                values.push(std::mem::take(&mut current));
            }
            quoted = !quoted;
        } else if quoted {
            current.push(character);
        }
    }
    values
}

pub fn resolve_focus_target(
    wayland_session: bool,
    x11: Option<X11FocusObservation>,
) -> FocusTarget {
    match x11 {
        Some(window) if !wayland_session || window.focused => {
            FocusTarget::x11(window.window_id, window.window_class)
        }
        _ => FocusTarget::wayland(),
    }
}

impl FocusTarget {
    pub fn x11(identity: impl Into<String>, window_class: impl Into<String>) -> Self {
        Self {
            protocol: ClipboardProtocol::X11,
            identity: Some(identity.into()),
            window_class: window_class.into(),
        }
    }

    pub fn wayland() -> Self {
        Self {
            protocol: ClipboardProtocol::Wayland,
            identity: None,
            window_class: String::new(),
        }
    }

    pub const fn protocol(&self) -> ClipboardProtocol {
        self.protocol
    }

    pub fn window_id(&self) -> Option<&str> {
        self.identity.as_deref()
    }

    pub fn window_class(&self) -> &str {
        &self.window_class
    }

    fn same_focus(&self, other: &Self) -> bool {
        if self.protocol != other.protocol {
            return false;
        }
        match self.protocol {
            ClipboardProtocol::X11 => self.identity == other.identity,
            ClipboardProtocol::Wayland => true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutMode {
    Auto,
    Standard,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteShortcut {
    Standard,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryFailure {
    ClipboardUnavailable,
    FocusUnstable,
    InjectionAmbiguous,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryResult {
    pub copied: bool,
    pub paste_triggered: bool,
    pub failure: Option<DeliveryFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeliveryAction {
    ObserveFocus,
    PublishClipboard(ClipboardProtocol),
    InjectPaste {
        target: FocusTarget,
        shortcut: PasteShortcut,
    },
    Finished(DeliveryResult),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeliveryObservation {
    Focus(FocusTarget),
    ClipboardReady(ClipboardProtocol),
    ClipboardUnavailable,
    InjectionFinished(bool),
    DeadlineReached,
}

pub struct PasteDelivery {
    shortcut_mode: ShortcutMode,
    target: Option<FocusTarget>,
    clipboard_protocol: Option<ClipboardProtocol>,
    clipboard_ready: bool,
    action: DeliveryAction,
}

impl PasteDelivery {
    pub fn new(shortcut_mode: ShortcutMode) -> Self {
        Self {
            shortcut_mode,
            target: None,
            clipboard_protocol: None,
            clipboard_ready: false,
            action: DeliveryAction::ObserveFocus,
        }
    }

    pub fn action(&self) -> DeliveryAction {
        self.action.clone()
    }

    pub fn advance(&mut self, observation: DeliveryObservation) -> DeliveryAction {
        if matches!(self.action, DeliveryAction::Finished(_)) {
            return self.action();
        }
        match observation {
            DeliveryObservation::Focus(target) => self.focus_observed(target),
            DeliveryObservation::ClipboardReady(protocol) => {
                if self.action == DeliveryAction::PublishClipboard(protocol) {
                    self.clipboard_protocol = Some(protocol);
                    self.clipboard_ready = true;
                    self.action = DeliveryAction::ObserveFocus;
                }
            }
            DeliveryObservation::ClipboardUnavailable => {
                if matches!(self.action, DeliveryAction::PublishClipboard(_)) {
                    self.action = DeliveryAction::Finished(DeliveryResult {
                        copied: false,
                        paste_triggered: false,
                        failure: Some(DeliveryFailure::ClipboardUnavailable),
                    });
                }
            }
            DeliveryObservation::InjectionFinished(sent) => {
                if matches!(self.action, DeliveryAction::InjectPaste { .. }) {
                    self.action = DeliveryAction::Finished(DeliveryResult {
                        copied: self.clipboard_ready,
                        paste_triggered: sent,
                        failure: (!sent).then_some(DeliveryFailure::InjectionAmbiguous),
                    });
                }
            }
            DeliveryObservation::DeadlineReached => {
                let failure = if matches!(self.action, DeliveryAction::InjectPaste { .. }) {
                    DeliveryFailure::InjectionAmbiguous
                } else if self.clipboard_ready {
                    DeliveryFailure::FocusUnstable
                } else {
                    DeliveryFailure::ClipboardUnavailable
                };
                self.action = DeliveryAction::Finished(DeliveryResult {
                    copied: self.clipboard_ready,
                    paste_triggered: false,
                    failure: Some(failure),
                });
            }
        }
        self.action()
    }

    fn focus_observed(&mut self, target: FocusTarget) {
        if self.clipboard_ready
            && self.clipboard_protocol == Some(target.protocol)
            && self
                .target
                .as_ref()
                .is_some_and(|known| known.same_focus(&target))
        {
            self.action = DeliveryAction::InjectPaste {
                shortcut: shortcut_for(self.shortcut_mode, &target),
                target,
            };
            return;
        }

        self.clipboard_ready =
            self.clipboard_protocol == Some(target.protocol) && self.clipboard_ready;
        self.target = Some(target.clone());
        self.action = if self.clipboard_ready {
            DeliveryAction::ObserveFocus
        } else {
            DeliveryAction::PublishClipboard(target.protocol)
        };
    }
}

fn shortcut_for(mode: ShortcutMode, target: &FocusTarget) -> PasteShortcut {
    match mode {
        ShortcutMode::Auto if is_terminal_class(target.window_class()) => PasteShortcut::Terminal,
        ShortcutMode::Auto | ShortcutMode::Standard => PasteShortcut::Standard,
        ShortcutMode::Terminal => PasteShortcut::Terminal,
    }
}

fn is_terminal_class(window_class: &str) -> bool {
    window_class
        .to_ascii_lowercase()
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|word| {
            matches!(
                word,
                "kitty"
                    | "terminal"
                    | "alacritty"
                    | "wezterm"
                    | "konsole"
                    | "xterm"
                    | "tilix"
                    | "terminator"
                    | "foot"
                    | "ghostty"
                    | "rio"
                    | "st"
            )
        })
}
