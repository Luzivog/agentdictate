//! Desktop notifications for dictations that end without a paste, and the
//! actions their buttons ask for. Notifications never take the keyboard
//! focus, so they are safe to show while the user types elsewhere.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};

use agentdictate_core::{DictationNotice, FailureKind, JobId};
use agentdictate_ui::notice_wording;

use crate::DaemonHandle;
use crate::tray::open_settings_window;

const APPLICATION_NAME: &str = "AgentDictate";
const APPLICATION_ICON: &str = "agentdictate";
const DESKTOP_ENTRY: &str = "local.agentdictate.AgentDictate";

/// What a notification's button, or a click on the notification itself,
/// asks the daemon to do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationAction {
    /// Transcribe a failed dictation again; its text is then copied.
    TryAgain(JobId),
    /// Paste the last dictation into the focused app.
    PasteLast,
    /// Open the settings window, where Recovery lists the dictation.
    OpenWindow,
}

impl NotificationAction {
    /// The action key sent to the notification service. "default" is the
    /// click on the notification itself.
    const fn key(self) -> &'static str {
        match self {
            Self::TryAgain(_) => "try-again",
            Self::PasteLast => "paste-again",
            Self::OpenWindow => "default",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::TryAgain(_) => "Try again",
            Self::PasteLast => "Paste again",
            Self::OpenWindow => "Open AgentDictate",
        }
    }
}

/// One desktop notification about how a dictation ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Notification {
    pub summary: &'static str,
    pub body: &'static str,
    pub actions: Vec<NotificationAction>,
    /// A transient notification is not kept in the notification list once
    /// it has been shown.
    pub transient: bool,
}

impl Notification {
    /// The notification for `notice` about the dictation `job_id`.
    #[must_use]
    pub fn for_notice(notice: DictationNotice, job_id: JobId) -> Self {
        let wording = notice_wording(notice);
        let (actions, transient) = match notice {
            DictationNotice::Copied => (vec![NotificationAction::PasteLast], true),
            DictationNotice::NothingHeard => (Vec::new(), true),
            DictationNotice::Failed { failure } => {
                let fix = match failure {
                    FailureKind::Offline
                    | FailureKind::RateLimited
                    | FailureKind::ProviderError
                    | FailureKind::NoSpeech
                    | FailureKind::MicrophoneStalled
                    | FailureKind::Unexpected => Some(NotificationAction::TryAgain(job_id)),
                    // The text is transcribed; only its paste failed.
                    FailureKind::PasteNotConfirmed => Some(NotificationAction::PasteLast),
                    // The key must be fixed first, or nothing was recorded.
                    FailureKind::CredentialMissing
                    | FailureKind::CredentialRejected
                    | FailureKind::MicrophoneUnavailable => None,
                };
                (
                    fix.into_iter()
                        .chain([NotificationAction::OpenWindow])
                        .collect(),
                    false,
                )
            }
        };
        Self {
            summary: wording.title,
            body: wording.body,
            actions,
            transient,
        }
    }
}

/// The session's notification service; tests substitute a fake.
pub trait NotificationBus: Send + 'static {
    /// Shows `notification` in place of the one with id `replaces` (0 for
    /// none) and returns its id.
    fn show(&mut self, notification: &Notification, replaces: u32) -> Result<u32, String>;
}

/// The notification on screen and the actions it offers. Each new one
/// replaces the last, so AgentDictate never stacks notifications.
#[derive(Default)]
struct Shown {
    id: u32,
    actions: Vec<NotificationAction>,
}

impl Shown {
    /// What a click on `key` of notification `id` asks for. A click on an
    /// older notification, or on a key it does not offer, asks for nothing.
    fn action(&self, id: u32, key: &str) -> Option<NotificationAction> {
        if id != self.id {
            return None;
        }
        self.actions
            .iter()
            .copied()
            .find(|action| action.key() == key)
    }
}

/// Shows notices as desktop notifications on its own thread, away from the
/// daemon lock, and turns a clicked action into a `NotificationAction`.
#[derive(Clone)]
pub struct Notifier {
    notices: Sender<Notification>,
    shown: Arc<Mutex<Shown>>,
    actions: Sender<NotificationAction>,
}

impl Notifier {
    /// Starts the thread that shows notifications on `bus`. Clicked actions
    /// arrive on `actions`.
    pub fn start(
        mut bus: impl NotificationBus,
        actions: Sender<NotificationAction>,
    ) -> std::io::Result<Self> {
        let (notices, pending) = channel::<Notification>();
        let shown = Arc::new(Mutex::new(Shown::default()));
        let worker_shown = Arc::clone(&shown);
        std::thread::Builder::new()
            .name("agentdictate-notifications".into())
            .spawn(move || {
                for notification in pending {
                    let replaces = lock(&worker_shown).id;
                    match bus.show(&notification, replaces) {
                        Ok(id) => {
                            *lock(&worker_shown) = Shown {
                                id,
                                actions: notification.actions,
                            };
                        }
                        Err(error) => {
                            tracing::warn!(%error, "could not show a desktop notification");
                        }
                    }
                }
            })?;
        Ok(Self {
            notices,
            shown,
            actions,
        })
    }

    /// Shows `notice` about the dictation `job_id`. Never blocks.
    pub fn notify(&self, notice: DictationNotice, job_id: JobId) {
        let _ = self.notices.send(Notification::for_notice(notice, job_id));
    }

    /// Routes a click on `key` of notification `id`; see `Shown::action`.
    pub fn action_invoked(&self, id: u32, key: &str) {
        let action = lock(&self.shown).action(id, key);
        if let Some(action) = action {
            let _ = self.actions.send(action);
        }
    }
}

fn lock(shown: &Mutex<Shown>) -> std::sync::MutexGuard<'_, Shown> {
    shown.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `org.freedesktop.Notifications` on the session bus.
struct SessionNotifications {
    proxy: zbus::blocking::Proxy<'static>,
}

impl NotificationBus for SessionNotifications {
    fn show(&mut self, notification: &Notification, replaces: u32) -> Result<u32, String> {
        let actions = notification
            .actions
            .iter()
            .flat_map(|action| [action.key(), action.label()])
            .collect::<Vec<_>>();
        let mut hints = HashMap::<&str, zbus::zvariant::Value<'_>>::new();
        hints.insert("desktop-entry", DESKTOP_ENTRY.into());
        if notification.transient {
            hints.insert("transient", true.into());
        }
        self.proxy
            .call(
                "Notify",
                &(
                    APPLICATION_NAME,
                    replaces,
                    APPLICATION_ICON,
                    notification.summary,
                    notification.body,
                    actions,
                    hints,
                    -1_i32,
                ),
            )
            .map_err(|error| error.to_string())
    }
}

/// Starts desktop notifications on the session bus, with a thread that
/// routes clicked actions. Returns `None` when the session has no
/// notification service; dictation works the same without it.
pub fn start_session_notifier(actions: Sender<NotificationAction>) -> Option<Notifier> {
    let proxy = zbus::blocking::Connection::session()
        .and_then(|connection| {
            zbus::blocking::Proxy::new_owned(
                connection,
                "org.freedesktop.Notifications",
                "/org/freedesktop/Notifications",
                "org.freedesktop.Notifications",
            )
        })
        .inspect_err(|error| tracing::warn!(%error, "desktop notifications are unavailable"))
        .ok()?;
    let signals = proxy
        .receive_signal("ActionInvoked")
        .inspect_err(|error| {
            tracing::warn!(%error, "notification buttons are unavailable");
        })
        .ok();
    let notifier = Notifier::start(
        SessionNotifications {
            proxy: proxy.clone(),
        },
        actions,
    )
    .inspect_err(|error| tracing::warn!(%error, "desktop notifications are unavailable"))
    .ok()?;
    if let Some(signals) = signals {
        let listener = notifier.clone();
        let spawned = std::thread::Builder::new()
            .name("agentdictate-notification-actions".into())
            .spawn(move || {
                for message in signals {
                    match message.body().deserialize::<(u32, String)>() {
                        Ok((id, key)) => listener.action_invoked(id, &key),
                        Err(error) => {
                            tracing::warn!(%error, "unreadable notification action");
                        }
                    }
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "notification buttons are unavailable");
        }
    }
    Some(notifier)
}

/// Carries out clicked notification actions, one at a time, on their own
/// thread. `settings_executable` opens the window.
pub fn follow_notification_actions(
    handle: DaemonHandle,
    actions: Receiver<NotificationAction>,
    settings_executable: std::path::PathBuf,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("agentdictate-notification-follow".into())
        .spawn(move || {
            for action in actions {
                let result = match action {
                    NotificationAction::TryAgain(job_id) => {
                        handle.try_again(job_id).map_err(|error| error.to_string())
                    }
                    NotificationAction::PasteLast => {
                        handle.paste_last().map_err(|error| error.to_string())
                    }
                    NotificationAction::OpenWindow => open_settings_window(&settings_executable)
                        .map_err(|error| error.to_string()),
                };
                match result {
                    Ok(()) => tracing::info!(?action, "notification action completed"),
                    Err(error) => tracing::warn!(?action, %error, "notification action failed"),
                }
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_offers_try_again_only_when_transcribing_again_can_help() {
        let job_id = JobId::new();
        let offline = Notification::for_notice(
            DictationNotice::Failed {
                failure: FailureKind::Offline,
            },
            job_id,
        );
        assert_eq!(offline.summary, "Couldn't reach OpenAI");
        assert_eq!(
            offline.actions,
            [
                NotificationAction::TryAgain(job_id),
                NotificationAction::OpenWindow
            ]
        );
        assert!(!offline.transient);

        let no_key = Notification::for_notice(
            DictationNotice::Failed {
                failure: FailureKind::CredentialMissing,
            },
            job_id,
        );
        assert_eq!(no_key.actions, [NotificationAction::OpenWindow]);

        let copied = Notification::for_notice(DictationNotice::Copied, job_id);
        assert_eq!(copied.actions, [NotificationAction::PasteLast]);
        assert!(copied.transient);
    }

    #[test]
    fn only_a_button_of_the_notification_on_screen_is_routed() {
        let job_id = JobId::new();
        let shown = Shown {
            id: 7,
            actions: vec![
                NotificationAction::TryAgain(job_id),
                NotificationAction::OpenWindow,
            ],
        };

        assert_eq!(
            shown.action(7, "try-again"),
            Some(NotificationAction::TryAgain(job_id))
        );
        assert_eq!(
            shown.action(7, "default"),
            Some(NotificationAction::OpenWindow)
        );
        // An older notification, replaced by this one, and a key it lacks.
        assert_eq!(shown.action(6, "try-again"), None);
        assert_eq!(shown.action(7, "paste-again"), None);
    }
}
