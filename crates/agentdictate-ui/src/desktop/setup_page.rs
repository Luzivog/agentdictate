//! The Setup screen: four steps from a fresh install to a first dictation.
//! Each step is one sentence and one button, and any can be skipped.

use agentdictate_core::{HotkeyReadiness, MicrophoneCheck, Readiness, RecordingMode};
use gpui::{Context, Entity, prelude::*, px, relative};
use gpui_component::{
    Disableable, Sizable,
    button::ButtonVariants,
    h_flex,
    input::{Input, InputState},
    v_flex,
};

use crate::{
    Color, Route, ThemeTokens, action::action_button, has_input_access,
    view_model::exposed_input_detail,
};

use super::{
    SettingsShell, gpui_color,
    setup_actions::{AccessGrant, KeyCheck, MicrophoneTest},
};

pub(super) struct SetupPageModel {
    pub(super) readiness: Readiness,
    pub(super) has_api_key: bool,
    pub(super) replacing_key: bool,
    pub(super) api_key: Entity<InputState>,
    pub(super) key: KeyCheck,
    pub(super) access: AccessGrant,
    pub(super) microphone: MicrophoneTest,
    /// The level meter, from 0 to 100.
    pub(super) meter: u8,
    /// The dictation shortcut's label.
    pub(super) shortcut: String,
    pub(super) recording_mode: RecordingMode,
    pub(super) try_it: Entity<InputState>,
    pub(super) tried: bool,
}

pub(super) fn surface(
    model: SetupPageModel,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    // A saved key is done unless OpenAI just refused it.
    let key_done = model.has_api_key && !matches!(model.key, KeyCheck::Problem(_));
    let shortcut_works = model.readiness.shortcut == HotkeyReadiness::Ready;
    let access_done = shortcut_works && model.readiness.desktop.paste_access;
    h_flex().w_full().justify_center().child(
        v_flex()
            .debug_selector(|| "setup-page".to_owned())
            .w_full()
            .max_w(px(760.))
            .gap_4()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        gpui::div()
                            .text_base()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child("Four steps to your first dictation"),
                    )
                    .child(muted("Skip any step you don't need.", theme)),
            )
            .child(step(
                "setup-step-key",
                1,
                "OpenAI API key",
                key_done,
                key_step(&model, theme, cx),
                theme,
            ))
            .child(step(
                "setup-step-access",
                2,
                "Keyboard shortcut & pasting",
                access_done,
                access_step(&model, theme, cx),
                theme,
            ))
            .child(step(
                "setup-step-microphone",
                3,
                "Microphone",
                model.microphone == MicrophoneTest::Done(MicrophoneCheck::Heard),
                microphone_step(&model.microphone, model.meter, theme, cx),
                theme,
            ))
            .child(step(
                "setup-step-try-it",
                4,
                "Try it",
                model.tried,
                try_it_step(&model, theme),
                theme,
            ))
            .child(
                h_flex().justify_end().child(
                    action_button("setup-done")
                        .debug_selector(|| "setup-done".to_owned())
                        .primary()
                        .label("Done")
                        .on_click(cx.listener(|shell, _, _, cx| {
                            shell.select_route(Route::Home, cx);
                        })),
                ),
            ),
    )
}

/// One numbered step, with a check mark in place of its number once done.
fn step(
    selector: &'static str,
    number: u8,
    title: &'static str,
    done: bool,
    body: gpui::Div,
    theme: ThemeTokens,
) -> gpui::Div {
    let marker = gpui::div()
        .size(px(24.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD);
    let marker = if done {
        marker
            .debug_selector(move || format!("{selector}-done"))
            .bg(gpui_color(theme.success))
            .text_color(gpui_color(theme.canvas))
            .child("✓")
    } else {
        marker
            .border_1()
            .border_color(gpui_color(theme.border))
            .text_color(gpui_color(theme.text_muted))
            .child(number.to_string())
    };
    v_flex()
        .debug_selector(move || selector.to_owned())
        .w_full()
        .gap_3()
        .rounded_xl()
        .border_1()
        .border_color(gpui_color(theme.border))
        .bg(gpui_color(theme.surface))
        .p_4()
        .child(
            h_flex().gap_3().items_center().child(marker).child(
                gpui::div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title),
            ),
        )
        .child(body.pl(px(36.)))
}

fn muted(text: impl Into<gpui::SharedString>, theme: ThemeTokens) -> gpui::Div {
    gpui::div()
        .text_xs()
        .text_color(gpui_color(theme.text_muted))
        .child(text.into())
}

/// A short outcome under a step, in `color`.
fn outcome(selector: &'static str, color: Color, text: impl Into<gpui::SharedString>) -> gpui::Div {
    gpui::div()
        .debug_selector(move || selector.to_owned())
        .text_sm()
        .text_color(gpui_color(color))
        .child(text.into())
}

/// Step 1: paste a key and check it, or check the saved one.
fn key_step(
    model: &SetupPageModel,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let checking = model.key == KeyCheck::Checking;
    let check = action_button("setup-check-key")
        .debug_selector(|| "setup-check-key".to_owned())
        .small()
        .label(if checking { "Checking…" } else { "Check key" })
        .disabled(checking)
        .on_click(cx.listener(|shell, _, window, cx| {
            shell.check_setup_key(window, cx);
        }));
    let control = if model.has_api_key && !model.replacing_key {
        h_flex()
            .gap_3()
            .items_center()
            .child(
                gpui::div()
                    .debug_selector(|| "setup-key-saved".to_owned())
                    .text_sm()
                    .child("Your key is saved."),
            )
            .child(check)
            .child(
                action_button("setup-key-replace")
                    .debug_selector(|| "setup-key-replace".to_owned())
                    .ghost()
                    .small()
                    .label("Replace")
                    .on_click(cx.listener(|shell, _, window, cx| {
                        shell.set_replacing_setup_key(true, window, cx);
                    })),
            )
    } else {
        h_flex()
            .w_full()
            .max_w(px(480.))
            .gap_2()
            .child(
                gpui::div()
                    .debug_selector(|| "setup-key-input".to_owned())
                    .min_w_0()
                    .flex_1()
                    .child(Input::new(&model.api_key).small()),
            )
            .child(check)
            .when(model.has_api_key, |control| {
                control.child(
                    action_button("setup-key-cancel")
                        .ghost()
                        .small()
                        .label("Cancel")
                        .on_click(cx.listener(|shell, _, window, cx| {
                            shell.set_replacing_setup_key(false, window, cx);
                        })),
                )
            })
    };
    v_flex()
        .gap_2()
        .child(muted(
            "AgentDictate turns your speech into text with OpenAI. Paste an API key from \
             platform.openai.com; it stays on this computer.",
            theme,
        ))
        .child(control)
        .when_some(
            match &model.key {
                KeyCheck::Works => Some(outcome("setup-key-works", theme.success, "Key works ✓")),
                KeyCheck::Problem(problem) => {
                    Some(outcome("setup-key-problem", theme.danger, problem.clone()))
                }
                KeyCheck::Idle | KeyCheck::Checking => None,
            },
            |step, outcome| step.child(outcome),
        )
}

/// Step 2: what AgentDictate can do with the keyboard, and Grant access
/// when it can't read the shortcut or paste.
fn access_step(
    model: &SetupPageModel,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let readiness = &model.readiness;
    let line = |selector: &'static str, color: Color, text: String| {
        h_flex()
            .debug_selector(move || selector.to_owned())
            .gap_2()
            .items_center()
            .text_sm()
            .child(gpui::div().size_2().rounded_full().bg(gpui_color(color)))
            .child(text)
    };
    let shortcut = match &readiness.shortcut {
        HotkeyReadiness::Ready => line(
            "setup-shortcut-works",
            theme.success,
            format!("AgentDictate hears your shortcut, {}.", model.shortcut),
        ),
        HotkeyReadiness::Starting => line(
            "setup-shortcut-starting",
            theme.text_muted,
            "Starting the shortcut…".to_owned(),
        ),
        HotkeyReadiness::Unavailable { .. } => line(
            "setup-shortcut-missing",
            theme.danger,
            "AgentDictate can't read your keyboard yet.".to_owned(),
        ),
    };
    let paste = if readiness.desktop.paste_access {
        line(
            "setup-paste-works",
            theme.success,
            "AgentDictate can paste into your apps.".to_owned(),
        )
    } else {
        line(
            "setup-paste-missing",
            theme.danger,
            "AgentDictate can't paste yet, so your words are only copied.".to_owned(),
        )
    };
    let exposed = readiness.desktop.exposed_input.as_ref();
    let missing = !has_input_access(readiness);
    // Granting also makes world-readable devices private again, unless
    // another app's rule opens them.
    let offer_grant = missing || exposed.is_some_and(|exposed| exposed.rule.is_none());
    let grant_button = || {
        action_button("setup-grant-access")
            .debug_selector(|| "setup-grant-access".to_owned())
            .small()
            .label("Grant access")
            .on_click(cx.listener(|shell, _, _, cx| shell.confirm_grant(true, cx)))
    };
    let grant = match &model.access {
        AccessGrant::Confirming => Some(
            v_flex()
                .debug_selector(|| "setup-grant-confirm".to_owned())
                .gap_2()
                .child(gpui::div().text_sm().child(
                    "Your computer will ask for your password. AgentDictate can then read \
                     your shortcut and paste, only while you are logged in.",
                ))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            action_button("setup-grant-continue")
                                .debug_selector(|| "setup-grant-continue".to_owned())
                                .primary()
                                .small()
                                .label("Continue")
                                .on_click(cx.listener(|shell, _, _, cx| shell.grant_access(cx))),
                        )
                        .child(
                            action_button("setup-grant-cancel")
                                .debug_selector(|| "setup-grant-cancel".to_owned())
                                .ghost()
                                .small()
                                .label("Cancel")
                                .on_click(
                                    cx.listener(|shell, _, _, cx| shell.confirm_grant(false, cx)),
                                ),
                        ),
                ),
        ),
        AccessGrant::Granting => Some(muted("Waiting for your password…", theme)),
        // The rule is installed, but this login still has the old access.
        AccessGrant::Granted if missing => Some(outcome(
            "setup-log-out",
            theme.text,
            "Log out and back in to finish.",
        )),
        AccessGrant::Granted => None,
        AccessGrant::Failed(error) => Some(
            v_flex()
                .gap_2()
                .items_start()
                .child(outcome("setup-grant-failed", theme.danger, error.clone()))
                .when(offer_grant, |failed| failed.child(grant_button())),
        ),
        AccessGrant::Idle => offer_grant.then(|| h_flex().child(grant_button())),
    };
    v_flex()
        .gap_2()
        .child(shortcut)
        .child(paste)
        .when_some(exposed, |step, exposed| {
            step.child(outcome(
                "setup-exposed-input",
                theme.danger,
                exposed_input_detail(exposed),
            ))
        })
        .when_some(grant, |step, grant| step.child(grant))
}

/// Step 3: a few seconds of listening with a live level meter.
fn microphone_step(
    test: &MicrophoneTest,
    meter: u8,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let listening = matches!(test, MicrophoneTest::Listening);
    v_flex()
        .gap_2()
        .child(muted(
            "Press Say something and talk for a few seconds. Nothing is recorded or sent.",
            theme,
        ))
        .child(
            h_flex()
                .gap_3()
                .items_center()
                .child(
                    action_button("setup-test-microphone")
                        .debug_selector(|| "setup-test-microphone".to_owned())
                        .small()
                        .label(match test {
                            MicrophoneTest::Idle => "Say something",
                            MicrophoneTest::Listening => "Listening…",
                            MicrophoneTest::Done(_) | MicrophoneTest::Failed(_) => "Test again",
                        })
                        .disabled(listening)
                        .on_click(cx.listener(|shell, _, _, cx| shell.test_microphone(cx))),
                )
                .when(*test != MicrophoneTest::Idle, |row| {
                    row.child(level_meter(meter, theme))
                }),
        )
        .when_some(
            match test {
                MicrophoneTest::Done(MicrophoneCheck::Heard) => Some(outcome(
                    "setup-microphone-heard",
                    theme.success,
                    "We can hear you ✓",
                )),
                MicrophoneTest::Done(MicrophoneCheck::Silent) => Some(outcome(
                    "setup-microphone-silent",
                    theme.danger,
                    "We can't hear anything — check your input device in Sound settings.",
                )),
                MicrophoneTest::Failed(error) => Some(outcome(
                    "setup-microphone-failed",
                    theme.danger,
                    error.clone(),
                )),
                MicrophoneTest::Idle | MicrophoneTest::Listening => None,
            },
            |step, outcome| step.child(outcome),
        )
}

/// How loud the microphone is while it listens, then the loudest it was.
fn level_meter(level: u8, theme: ThemeTokens) -> gpui::Div {
    gpui::div()
        .debug_selector(|| "setup-microphone-meter".to_owned())
        .w(px(220.))
        .h(px(8.))
        .rounded_full()
        .overflow_hidden()
        .bg(gpui_color(theme.border))
        .child(
            gpui::div()
                .debug_selector(|| "setup-microphone-level".to_owned())
                .h_full()
                .w(relative(f32::from(level) / 100.))
                .rounded_full()
                .bg(gpui_color(theme.success)),
        )
}

/// Step 4: a box to dictate into, with how to use the shortcut.
fn try_it_step(model: &SetupPageModel, theme: ThemeTokens) -> gpui::Div {
    let shortcut = &model.shortcut;
    let instructions = match model.recording_mode {
        RecordingMode::Toggle => format!(
            "Click the box, press {shortcut}, say “hello world”, then press {shortcut} again."
        ),
        RecordingMode::Hold => {
            format!("Click the box, hold {shortcut} while you say “hello world”, then let go.")
        }
    };
    v_flex()
        .gap_2()
        .child(gpui::div().text_sm().child(instructions))
        .child(
            gpui::div()
                .debug_selector(|| "setup-try-it-input".to_owned())
                .w_full()
                .max_w(px(480.))
                .child(Input::new(&model.try_it)),
        )
        .when(model.tried, |step| {
            step.child(outcome(
                "setup-try-it-works",
                theme.success,
                "It works ✓ You can dictate like this in any app.",
            ))
        })
}
