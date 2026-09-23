//! The Setup screen's state and what its buttons do. Every step is
//! optional, and each runs its slow part off the UI thread.

use std::sync::Arc;

use agentdictate_core::{ApiKeyCheck, MicrophoneCheck, Readiness};
use futures::{StreamExt, channel::mpsc};
use gpui::{AppContext, Context, Entity, Subscription, Window};
use gpui_component::input::{InputEvent, InputState};

use crate::{SettingsRequest, SetupSink, UiActionError};

use super::SettingsShell;

/// How far the user has taken each Setup step.
pub(super) struct SetupState {
    pub(super) sink: SetupSink,
    pub(super) api_key: Entity<InputState>,
    /// The saved key's Replace was clicked, so the key field shows.
    pub(super) replacing_key: bool,
    pub(super) key: KeyCheck,
    /// Counts key checks, and Replace and Cancel, so that only the answer to
    /// the latest check shows: an earlier key's answer must not be saved.
    key_checks: u64,
    pub(super) access: AccessGrant,
    pub(super) microphone: MicrophoneTest,
    /// The level meter, from 0 to 100: how loud the microphone is while the
    /// test listens, then the loudest it was.
    pub(super) meter: u8,
    /// The Try it box that a dictation pastes into.
    pub(super) try_it: Entity<InputState>,
    /// Text arrived in the Try it box.
    pub(super) tried: bool,
}

/// Step 1: whether OpenAI accepts the key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum KeyCheck {
    Idle,
    Checking,
    Works,
    /// Why the key can't be used, or couldn't be checked.
    Problem(String),
}

/// Step 2: giving AgentDictate keyboard and paste access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum AccessGrant {
    Idle,
    /// Grant access was clicked; the password prompt comes once confirmed.
    Confirming,
    Granting,
    Granted,
    Failed(String),
}

/// Step 3: the microphone test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MicrophoneTest {
    Idle,
    Listening,
    Done(MicrophoneCheck),
    Failed(String),
}

impl SetupState {
    /// Creates the screen's text boxes: Enter in the key box checks the
    /// key, and the first text in the Try it box completes that step.
    pub(super) fn new(
        sink: SetupSink,
        window: &mut Window,
        cx: &mut Context<SettingsShell>,
    ) -> (Self, Vec<Subscription>) {
        let api_key = cx.new(|cx| InputState::new(window, cx).placeholder("sk-…").masked(true));
        let try_it = cx.new(|cx| InputState::new(window, cx).placeholder("Your words appear here"));
        let subscriptions = vec![
            cx.subscribe_in(
                &api_key,
                window,
                |shell, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        shell.check_setup_key(window, cx);
                    }
                },
            ),
            cx.subscribe(&try_it, |shell, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) && !input.read(cx).value().trim().is_empty()
                {
                    shell.setup.tried = true;
                    cx.notify();
                }
            }),
        ];
        let state = Self {
            sink,
            api_key,
            replacing_key: false,
            key: KeyCheck::Idle,
            key_checks: 0,
            access: AccessGrant::Idle,
            microphone: MicrophoneTest::Idle,
            meter: 0,
            try_it,
            tried: false,
        };
        (state, subscriptions)
    }
}

impl SettingsShell {
    /// Asks OpenAI whether the pasted key works, and saves it if it does;
    /// with a key saved and not being replaced, checks the saved key.
    pub(super) fn check_setup_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.setup.key == KeyCheck::Checking {
            return;
        }
        let pasted = if self.settings.saved.has_api_key && !self.setup.replacing_key {
            None
        } else {
            let key = self.setup.api_key.read(cx).value().trim().to_owned();
            if key.is_empty() {
                self.setup.key = KeyCheck::Problem("Paste your API key first.".to_owned());
                cx.notify();
                return;
            }
            Some(key)
        };
        self.setup.key_checks += 1;
        let check = self.setup.key_checks;
        let sink = Arc::clone(&self.setup.sink);
        let checked = pasted.clone();
        let answer = cx.background_spawn(async move { sink.check_api_key(checked) });
        cx.spawn_in(window, async move |shell, cx| {
            let answer = answer.await;
            let _ = shell.update_in(cx, |shell, window, cx| {
                if shell.setup.key_checks == check {
                    shell.finish_key_check(pasted, answer, window, cx);
                }
            });
        })
        .detach();
        self.setup.key = KeyCheck::Checking;
        cx.notify();
    }

    fn finish_key_check(
        &mut self,
        pasted: Option<String>,
        answer: Result<ApiKeyCheck, UiActionError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.setup.key = match answer {
            Ok(ApiKeyCheck::Works) => {
                if let Some(api_key) = pasted {
                    self.save_setup_key(api_key, window, cx);
                }
                KeyCheck::Works
            }
            Ok(ApiKeyCheck::Rejected) if pasted.is_some() => KeyCheck::Problem(
                "OpenAI didn't accept this key. Check that you copied all of it.".to_owned(),
            ),
            Ok(ApiKeyCheck::Rejected) => KeyCheck::Problem(
                "OpenAI didn't accept your saved key. Replace it with a new one.".to_owned(),
            ),
            Ok(ApiKeyCheck::Unreachable) => KeyCheck::Problem(
                "Couldn't check the key with OpenAI. Check your internet connection and try again."
                    .to_owned(),
            ),
            Err(error) => KeyCheck::Problem(format!("Couldn't check the key: {error}")),
        };
        cx.notify();
    }

    /// Saves a key OpenAI accepted, then empties the key box.
    fn save_setup_key(&mut self, api_key: String, window: &mut Window, cx: &mut Context<Self>) {
        self.send_settings_request(
            SettingsRequest::SetApiKey(api_key),
            window,
            cx,
            |shell, result, window, cx| match result {
                Ok(()) => {
                    shell.setup.api_key.update(cx, |input, cx| {
                        input.set_value(String::new(), window, cx);
                    });
                    shell.setup.replacing_key = false;
                }
                Err(error) => {
                    shell.setup.key =
                        KeyCheck::Problem(format!("The key works but wasn't saved: {error}"));
                }
            },
        );
    }

    /// Shows the key box in place of the saved key, or hides it again.
    pub(super) fn set_replacing_setup_key(
        &mut self,
        replacing: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.setup.replacing_key = replacing;
        self.setup.key = KeyCheck::Idle;
        // A check still on its way answers for the key that was shown.
        self.setup.key_checks += 1;
        self.setup.api_key.update(cx, |input, cx| {
            input.set_value(String::new(), window, cx);
            if replacing {
                input.focus(window, cx);
            }
        });
        cx.notify();
    }

    /// Asks before Grant access, which prompts for the user's password;
    /// `false` takes the question back.
    pub(super) fn confirm_grant(&mut self, asking: bool, cx: &mut Context<Self>) {
        if self.setup.access == AccessGrant::Granting {
            return;
        }
        self.setup.access = if asking {
            AccessGrant::Confirming
        } else {
            AccessGrant::Idle
        };
        cx.notify();
    }

    /// Grants keyboard and paste access, then shows the readiness it left.
    /// A grant already waiting for the password is not started again.
    pub(super) fn grant_access(&mut self, cx: &mut Context<Self>) {
        if self.setup.access == AccessGrant::Granting {
            return;
        }
        let sink = Arc::clone(&self.setup.sink);
        let answer = cx.background_spawn(async move { sink.grant_access() });
        cx.spawn(async move |shell, cx| {
            let answer = answer.await;
            let _ = shell.update(cx, |shell, cx| shell.finish_grant(answer, cx));
        })
        .detach();
        self.setup.access = AccessGrant::Granting;
        cx.notify();
    }

    fn finish_grant(&mut self, answer: Result<Readiness, UiActionError>, cx: &mut Context<Self>) {
        self.setup.access = match answer {
            Ok(readiness) => {
                self.model.workspace.readiness = readiness;
                AccessGrant::Granted
            }
            Err(error) => AccessGrant::Failed(format!("Access wasn't granted: {error}")),
        };
        cx.notify();
    }

    /// Listens to the microphone for a few seconds, showing its level as it
    /// goes, then says whether it heard anything.
    pub(super) fn test_microphone(&mut self, cx: &mut Context<Self>) {
        if self.setup.microphone == MicrophoneTest::Listening {
            return;
        }
        let (levels, mut heard) = mpsc::unbounded();
        let sink = Arc::clone(&self.setup.sink);
        let answer = cx.background_spawn(async move {
            sink.test_microphone(&mut |level| {
                let _ = levels.unbounded_send(level);
            })
        });
        cx.spawn(async move |shell, cx| {
            let mut loudest = 0;
            // The levels end when the test does.
            while let Some(level) = heard.next().await {
                loudest = loudest.max(level);
                let shown = shell.update(cx, |shell, cx| {
                    shell.setup.meter = level;
                    cx.notify();
                });
                if shown.is_err() {
                    return;
                }
            }
            let answer = answer.await;
            let _ = shell.update(cx, |shell, cx| {
                shell.setup.meter = loudest;
                shell.setup.microphone = match answer {
                    Ok(heard) => MicrophoneTest::Done(heard),
                    Err(error) => MicrophoneTest::Failed(format!(
                        "Couldn't listen to the microphone: {error}"
                    )),
                };
                cx.notify();
            });
        })
        .detach();
        self.setup.microphone = MicrophoneTest::Listening;
        self.setup.meter = 0;
        cx.notify();
    }
}
