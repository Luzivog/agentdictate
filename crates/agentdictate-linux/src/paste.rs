#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardProtocol {
    Wayland,
    X11,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FocusTarget {
    protocol: ClipboardProtocol,
    /// The X11 window id; native Wayland targets have none.
    identity: Option<u32>,
    window_class: String,
}

/// The X server's active window, as `focus::observe_x11_focus` reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct X11FocusObservation {
    pub window_id: u32,
    pub window_class: String,
    /// Whether the window carries `_NET_WM_STATE_FOCUSED`. Under XWayland the
    /// active X11 window can be stale while a native Wayland window has focus.
    pub focused: bool,
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
    pub fn x11(window: u32, window_class: impl Into<String>) -> Self {
        Self {
            protocol: ClipboardProtocol::X11,
            identity: Some(window),
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

impl From<agentdictate_core::PasteShortcut> for ShortcutMode {
    fn from(setting: agentdictate_core::PasteShortcut) -> Self {
        match setting {
            agentdictate_core::PasteShortcut::Automatic => Self::Auto,
            agentdictate_core::PasteShortcut::Standard => Self::Standard,
            agentdictate_core::PasteShortcut::Terminal => Self::Terminal,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteShortcut {
    Universal,
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
    /// The target requested the published text after the paste key press:
    /// the only acknowledgement that the paste reached an application.
    pub consumed: bool,
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
    /// The paste chord was sent; `consumed` records whether the target then
    /// requested the text.
    PasteSent {
        consumed: bool,
    },
    /// Sending the chord failed partway, so whether it reached the target is
    /// unknown.
    InjectionFailed,
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
                        consumed: false,
                        failure: Some(DeliveryFailure::ClipboardUnavailable),
                    });
                }
            }
            DeliveryObservation::PasteSent { consumed } => {
                if matches!(self.action, DeliveryAction::InjectPaste { .. }) {
                    self.action = DeliveryAction::Finished(DeliveryResult {
                        copied: self.clipboard_ready,
                        paste_triggered: true,
                        consumed,
                        failure: None,
                    });
                }
            }
            DeliveryObservation::InjectionFailed => {
                if matches!(self.action, DeliveryAction::InjectPaste { .. }) {
                    self.action = DeliveryAction::Finished(DeliveryResult {
                        copied: self.clipboard_ready,
                        paste_triggered: false,
                        consumed: false,
                        failure: Some(DeliveryFailure::InjectionAmbiguous),
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
                    consumed: false,
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
        ShortcutMode::Auto if target.protocol() == ClipboardProtocol::Wayland => {
            PasteShortcut::Universal
        }
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
