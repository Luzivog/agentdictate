//! Desktop notifications for dictations that end without a paste, and the
//! actions their buttons ask for. Notifications never take the keyboard
//! focus, so they are safe to show while the user types elsewhere.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

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
    /// Paste this dictation's text into the focused app.
    PasteAgain(JobId),
    /// Open the settings window, where Recovery lists the dictation.
    OpenWindow,
}

/// The key of a click on the notification itself.
const DEFAULT_KEY: &str = "default";
const TRY_AGAIN_KEY: &str = "try-again";
const PASTE_AGAIN_KEY: &str = "paste-again";

impl NotificationAction {
    /// The action key sent to the notification service. A button about a
    /// dictation carries its job id, so a click acts on that dictation even
    /// after later ones ended, or after the daemon restarted.
    fn key(self) -> String {
        match self {
            Self::TryAgain(job_id) => format!("{TRY_AGAIN_KEY}:{job_id}"),
            Self::PasteAgain(job_id) => format!("{PASTE_AGAIN_KEY}:{job_id}"),
            Self::OpenWindow => DEFAULT_KEY.to_owned(),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::TryAgain(_) => "Try again",
            Self::PasteAgain(_) => "Paste again",
            Self::OpenWindow => "Open AgentDictate",
        }
    }

    /// What a click on `key` of notification `id` asks for, with `shown` the
    /// notification on screen. A button about a dictation names it, so it is
    /// followed from any notification. A click on a notification itself is
    /// only ours when it is the one on screen: every app's notifications
    /// send "default".
    fn clicked(shown: u32, id: u32, key: &str) -> Option<Self> {
        if key == DEFAULT_KEY {
            return (id == shown).then_some(Self::OpenWindow);
        }
        let (name, job_id) = key.split_once(':')?;
        let job_id = job_id.parse().ok()?;
        match name {
            TRY_AGAIN_KEY => Some(Self::TryAgain(job_id)),
            PASTE_AGAIN_KEY => Some(Self::PasteAgain(job_id)),
            _ => None,
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
            DictationNotice::Copied => (vec![NotificationAction::PasteAgain(job_id)], true),
            DictationNotice::NothingHeard | DictationNotice::PasteUnavailable => (Vec::new(), true),
            DictationNotice::Failed { failure } => {
                let fix = match failure {
                    FailureKind::Offline
                    | FailureKind::RateLimited
                    | FailureKind::ProviderError
                    | FailureKind::NoSpeech
                    | FailureKind::MicrophoneStalled
                    | FailureKind::Unexpected => Some(NotificationAction::TryAgain(job_id)),
                    // The text is transcribed; only its paste failed.
                    FailureKind::PasteNotConfirmed => Some(NotificationAction::PasteAgain(job_id)),
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

/// Shows notices as desktop notifications on its own thread, away from the
/// daemon lock, and turns a clicked action into a `NotificationAction`.
#[derive(Clone)]
pub struct Notifier {
    notices: Sender<Notification>,
    /// The id of the notification on screen, 0 before the first. Each new
    /// one replaces it, so AgentDictate never stacks notifications.
    shown: Arc<AtomicU32>,
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
        let shown = Arc::new(AtomicU32::new(0));
        let worker_shown = Arc::clone(&shown);
        std::thread::Builder::new()
            .name("agentdictate-notifications".into())
            .spawn(move || {
                for notification in pending {
                    let replaces = worker_shown.load(Ordering::Acquire);
                    match bus.show(&notification, replaces) {
                        Ok(id) => worker_shown.store(id, Ordering::Release),
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

    /// Routes a click on `key` of notification `id`; see
    /// `NotificationAction::clicked`.
    pub fn action_invoked(&self, id: u32, key: &str) {
        let shown = self.shown.load(Ordering::Acquire);
        if let Some(action) = NotificationAction::clicked(shown, id, key) {
            let _ = self.actions.send(action);
        }
    }
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
            .flat_map(|action| [action.key(), action.label().to_owned()])
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
                    NotificationAction::PasteAgain(job_id) => handle
                        .paste_dictation(job_id)
                        .map_err(|error| error.to_string()),
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
        assert_eq!(copied.actions, [NotificationAction::PasteAgain(job_id)]);
        assert!(copied.transient);
    }

    #[test]
    fn a_button_acts_on_its_own_dictation_and_only_our_notification_opens_the_window() {
        let job_id = JobId::new();
        for action in [
            NotificationAction::TryAgain(job_id),
            NotificationAction::PasteAgain(job_id),
        ] {
            // From the notification on screen, an older one, or one shown
            // before the daemon restarted.
            for (shown, id) in [(7, 7), (7, 3), (0, 3)] {
                assert_eq!(
                    NotificationAction::clicked(shown, id, &action.key()),
                    Some(action)
                );
            }
        }
        assert_eq!(
            NotificationAction::clicked(7, 7, "default"),
            Some(NotificationAction::OpenWindow)
        );
        // Another app's notification, and keys that are not ours.
        assert_eq!(NotificationAction::clicked(7, 8, "default"), None);
        assert_eq!(NotificationAction::clicked(7, 7, "paste-again"), None);
        assert_eq!(NotificationAction::clicked(7, 7, "reply:42"), None);
    }
}
