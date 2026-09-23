use agentdictate_core::{DictationMode, KeepTranscripts, RecordingMode, SettingChange, Settings};
use gpui::{Context, Entity, IntoElement, prelude::*, px};
use gpui_component::{
    Sizable,
    button::ButtonVariants,
    h_flex,
    input::{Input, Textarea},
    radio::Radio,
    select::Select,
    switch::Switch,
    v_flex,
};

use crate::action::action_button;
use crate::{ThemeTokens, WorkspaceAction};

use super::{
    SettingsShell, gpui_color,
    row_actions::{CONFIRM_DELETE_LABEL, delete_button},
    settings_form::{SettingRow, SettingSelect, SettingsControls},
};

pub(super) struct SettingsPageModel {
    /// The settings as shown: the daemon's, with edits on their way applied.
    pub(super) settings: Settings,
    pub(super) has_api_key: bool,
    pub(super) replacing_api_key: bool,
    pub(super) controls: SettingsControls,
    pub(super) advanced_open: bool,
    pub(super) shortcut_capture_active: bool,
    pub(super) shortcut_capture_error: Option<String>,
    /// A shorter Keep transcripts choice waiting for Confirm delete.
    pub(super) shorter_retention: Option<KeepTranscripts>,
    /// Why a row's last change was refused.
    pub(super) error: Option<(SettingRow, String)>,
    /// The row showing "Saved ✓", if any.
    pub(super) saved_row: Option<SettingRow>,
    /// The outcome of "Delete all history…".
    pub(super) feedback: Option<String>,
    pub(super) pending_destructive_action: Option<WorkspaceAction>,
}

impl SettingsPageModel {
    /// The note under `row`'s control: its refusal, or "Saved ✓".
    fn note(&self, row: SettingRow, theme: ThemeTokens) -> Option<gpui::Div> {
        let error = self
            .error
            .as_ref()
            .filter(|(errored, _)| *errored == row)
            .map(|(_, error)| error.clone());
        match error {
            Some(error) => Some(
                gpui::div()
                    .debug_selector(|| "settings-error".to_owned())
                    .text_xs()
                    .text_color(gpui_color(theme.danger))
                    .child(error),
            ),
            None if self.saved_row == Some(row) => Some(
                gpui::div()
                    .debug_selector(|| "settings-saved".to_owned())
                    .text_xs()
                    .text_color(gpui_color(theme.success))
                    .child("Saved ✓"),
            ),
            None => None,
        }
    }
}

/// The Settings screen: six everyday settings, then the rest behind
/// "Show advanced settings". Every change saves as it is made.
pub(super) fn surface(
    model: SettingsPageModel,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let advanced_open = model.advanced_open;
    h_flex().w_full().justify_center().child(
        v_flex()
            .debug_selector(|| "settings-page".to_owned())
            .w_full()
            .max_w(px(980.))
            .gap_4()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        gpui::div()
                            .text_base()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child("Settings"),
                    )
                    .child(
                        gpui::div()
                            .text_xs()
                            .text_color(gpui_color(theme.text_muted))
                            .child("Changes are saved as you make them."),
                    ),
            )
            .child(primary_settings(&model, theme, cx))
            .child(
                h_flex().child(
                    action_button("settings-show-advanced")
                        .debug_selector(|| "settings-show-advanced".to_owned())
                        .ghost()
                        .small()
                        .label(if advanced_open {
                            "Hide advanced settings"
                        } else {
                            "Show advanced settings"
                        })
                        .on_click(cx.listener(|shell, _, _, cx| {
                            shell.toggle_advanced_settings(cx);
                        })),
                ),
            )
            .when(advanced_open, |page| {
                page.child(advanced_settings(&model, theme, cx))
            }),
    )
}

fn primary_settings(
    model: &SettingsPageModel,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let settings = &model.settings;
    v_flex()
        .debug_selector(|| "settings-primary".to_owned())
        .w_full()
        .child(setting_row(
            "settings-api-key",
            "OpenAI API key",
            "Turns your speech into text. It is stored only on this computer.",
            with_note(
                api_key_control(model, theme, cx),
                model.note(SettingRow::ApiKey, theme),
            ),
            theme,
        ))
        .child(setting_row(
            "settings-language",
            "Language you speak",
            "Pick one, or let AgentDictate detect it.",
            with_note(
                choice(&model.controls.language, false),
                model.note(SettingRow::Language, theme),
            ),
            theme,
        ))
        .child(setting_row(
            "settings-hotkey-row",
            "Dictation shortcut",
            "Press it in any app to dictate.",
            with_note(
                v_flex()
                    .items_end()
                    .gap_2()
                    .child(shortcut_control(
                        &settings.hotkey,
                        model.shortcut_capture_active,
                        model.shortcut_capture_error.clone(),
                        theme,
                        cx,
                    ))
                    .child(recording_mode_choice(settings.recording_mode, cx)),
                model.note(SettingRow::Shortcut, theme),
            ),
            theme,
        ))
        .child(setting_row(
            "settings-lower-sounds",
            "Lower other sounds while dictating",
            "Turns down music and videos while you speak.",
            with_note(
                switch(
                    "toggle-lower-sounds",
                    settings.audio_ducking_enabled,
                    SettingRow::LowerSounds,
                    SettingChange::AudioDuckingEnabled,
                    cx,
                ),
                model.note(SettingRow::LowerSounds, theme),
            ),
            theme,
        ))
        .child(setting_row(
            "settings-keep-transcripts",
            "Keep transcripts",
            "How long History keeps what you dictate. Usage numbers always stay.",
            with_note(
                h_flex()
                    .gap_3()
                    .child(choice(&model.controls.keep_transcripts, false))
                    .child(delete_button(
                        WorkspaceAction::ClearHistory,
                        "Delete all history…",
                        model.pending_destructive_action.as_ref(),
                        cx,
                    )),
                model
                    .shorter_retention
                    .map(|choice| shorter_retention_warning(choice, theme, cx))
                    .or_else(|| model.note(SettingRow::KeepTranscripts, theme))
                    .or_else(|| {
                        model.feedback.clone().map(|feedback| {
                            gpui::div()
                                .debug_selector(|| "settings-feedback".to_owned())
                                .max_w(px(360.))
                                .text_xs()
                                .text_color(gpui_color(theme.text_muted))
                                .child(feedback)
                        })
                    }),
            ),
            theme,
        ))
        .child(setting_row(
            "settings-start-on-login",
            "Start AgentDictate when I log in",
            "Your shortcut works as soon as you log in.",
            with_note(
                switch(
                    "toggle-start-on-login",
                    settings.start_on_login,
                    SettingRow::StartOnLogin,
                    SettingChange::StartOnLogin,
                    cx,
                ),
                model.note(SettingRow::StartOnLogin, theme),
            ),
            theme,
        ))
}

fn advanced_settings(
    model: &SettingsPageModel,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let settings = &model.settings;
    v_flex()
        .debug_selector(|| "settings-advanced".to_owned())
        .w_full()
        .child(setting_row(
            "settings-about-your-work",
            "About your work",
            "Names, topics and jargon you often mention. Helps accuracy; saved when you \
             click away or press Enter.",
            gpui::div()
                .debug_selector(|| "settings-about-your-work-control".to_owned())
                .w_full()
                .child(with_note(
                    Textarea::new(&model.controls.about_your_work)
                        .w_full()
                        .text_sm(),
                    model.note(SettingRow::AboutYourWork, theme),
                )),
            theme,
        ))
        .child(setting_row(
            "settings-exact-mode",
            "Exact mode",
            "Type exactly what I say, with no spelling fixes from Words or About your work.",
            with_note(
                switch(
                    "toggle-exact-mode",
                    settings.dictation_mode == DictationMode::Literal,
                    SettingRow::ExactMode,
                    |exact| {
                        SettingChange::DictationMode(if exact {
                            DictationMode::Literal
                        } else {
                            DictationMode::Dictate
                        })
                    },
                    cx,
                ),
                model.note(SettingRow::ExactMode, theme),
            ),
            theme,
        ))
        .child(setting_row(
            "settings-paste-method",
            "Paste method",
            "The keys AgentDictate presses to paste into the app you are using.",
            with_note(
                choice(&model.controls.paste_method, false),
                model.note(SettingRow::PasteMethod, theme),
            ),
            theme,
        ))
        .child(setting_row(
            "settings-stop-after",
            "Stop recording after",
            "Ends a recording you forgot to stop.",
            with_note(
                choice(&model.controls.stop_after, false),
                model.note(SettingRow::StopAfter, theme),
            ),
            theme,
        ))
        .child(setting_row(
            "settings-how-much-to-lower",
            "How much to lower",
            "How quiet other sounds get while you dictate.",
            with_note(
                choice(
                    &model.controls.how_much_to_lower,
                    !settings.audio_ducking_enabled,
                ),
                model.note(SettingRow::HowMuchToLower, theme),
            ),
            theme,
        ))
        .child(setting_row(
            "settings-keep-audio",
            "Keep audio recordings",
            "Keeps each recording's audio on this computer after it is transcribed.",
            with_note(
                switch(
                    "toggle-keep-audio",
                    settings.preserve_temp_audio,
                    SettingRow::KeepAudio,
                    SettingChange::PreserveTempAudio,
                    cx,
                ),
                model.note(SettingRow::KeepAudio, theme),
            ),
            theme,
        ))
}

/// One Settings row: a label with a plain explanation, and its control. The
/// control moves below the label when the window is too narrow for both.
fn setting_row(
    selector: &'static str,
    label: &'static str,
    detail: &'static str,
    control: impl IntoElement,
    theme: ThemeTokens,
) -> gpui::Div {
    h_flex()
        .debug_selector(move || selector.to_owned())
        .w_full()
        .min_h(px(54.))
        .items_start()
        .flex_wrap()
        .justify_between()
        .gap_x_6()
        .gap_y_2()
        .border_b_1()
        .border_color(gpui_color(theme.border))
        .py_3()
        .child(
            v_flex()
                .min_w(px(220.))
                .flex_1()
                .gap_0p5()
                .child(gpui::div().text_sm().child(label))
                .child(
                    gpui::div()
                        .text_xs()
                        .text_color(gpui_color(theme.text_muted))
                        .child(detail),
                ),
        )
        .child(control)
}

/// Stacks `note` under `control`, both aligned to the row's right edge.
fn with_note(control: impl IntoElement, note: Option<gpui::Div>) -> gpui::Div {
    v_flex()
        .items_end()
        .gap_1()
        .child(control)
        .when_some(note, |column, note| column.child(note))
}

fn choice<T: Clone + PartialEq + 'static>(
    select: &Entity<SettingSelect<T>>,
    disabled: bool,
) -> gpui::Div {
    gpui::div()
        .w(px(260.))
        .cursor_pointer()
        .when(disabled, |slot| slot.opacity(0.48))
        .child(Select::new(select).small().w_full().disabled(disabled))
}

/// An on/off switch that applies `change` from `row` when flipped. Its
/// wrapper carries `selector` for rendered tests.
fn switch(
    selector: &'static str,
    on: bool,
    row: SettingRow,
    change: fn(bool) -> SettingChange,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    gpui::div()
        .debug_selector(move || selector.to_owned())
        .flex_none()
        .child(Switch::new(selector).checked(on).on_click(cx.listener(
            move |shell, on: &bool, window, cx| {
                shell.change_setting(row, change(*on), window, cx);
            },
        )))
}

fn recording_mode_choice(mode: RecordingMode, cx: &mut Context<SettingsShell>) -> gpui::Div {
    let option = |selector: &'static str, label: &'static str, option: RecordingMode| {
        Radio::new(selector)
            .debug_selector(move || selector.to_owned())
            .label(label)
            .checked(mode == option)
            .on_click(cx.listener(move |shell, _: &bool, window, cx| {
                let change = SettingChange::RecordingMode(option);
                shell.change_setting(SettingRow::Shortcut, change, window, cx);
            }))
    };
    v_flex()
        .gap_1()
        .text_sm()
        .child(option(
            "settings-recording-mode-toggle",
            "Press to start, press again to stop",
            RecordingMode::Toggle,
        ))
        .child(option(
            "settings-recording-mode-hold",
            "Hold while speaking",
            RecordingMode::Hold,
        ))
}

/// Asks before a shorter Keep transcripts choice deletes stored text.
fn shorter_retention_warning(
    choice: KeepTranscripts,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let question = match choice {
        KeepTranscripts::Never => "Delete every saved transcript? This can't be undone.",
        KeepTranscripts::Days30 => "Delete transcripts older than 30 days? This can't be undone.",
        // Keeping more never asks.
        KeepTranscripts::Forever => "Keep every transcript from now on?",
    };
    v_flex()
        .debug_selector(|| "settings-keep-transcripts-warning".to_owned())
        .items_end()
        .gap_2()
        .max_w(px(360.))
        .child(
            gpui::div()
                .text_xs()
                .text_color(gpui_color(theme.danger))
                .child(question),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    action_button("settings-keep-transcripts-cancel")
                        .debug_selector(|| "settings-keep-transcripts-cancel".to_owned())
                        .ghost()
                        .small()
                        .label("Cancel")
                        .on_click(cx.listener(|shell, _, window, cx| {
                            shell.settle_shorter_retention(false, window, cx);
                        })),
                )
                .child(
                    action_button("settings-keep-transcripts-confirm")
                        .debug_selector(|| "settings-keep-transcripts-confirm".to_owned())
                        .small()
                        .label(CONFIRM_DELETE_LABEL)
                        .on_click(cx.listener(|shell, _, window, cx| {
                            shell.settle_shorter_retention(true, window, cx);
                        })),
                ),
        )
}

/// "Key saved ✓" with Replace, or the field to paste a key into.
fn api_key_control(
    model: &SettingsPageModel,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    if model.has_api_key && !model.replacing_api_key {
        return h_flex()
            .gap_3()
            .child(
                gpui::div()
                    .debug_selector(|| "settings-api-key-saved".to_owned())
                    .text_sm()
                    .text_color(gpui_color(theme.success))
                    .child("Key saved ✓"),
            )
            .child(
                action_button("settings-api-key-replace")
                    .debug_selector(|| "settings-api-key-replace".to_owned())
                    .small()
                    .label("Replace")
                    .on_click(cx.listener(|shell, _, window, cx| {
                        shell.set_replacing_api_key(true, window, cx);
                    })),
            );
    }
    h_flex()
        .w(px(360.))
        .max_w(gpui::relative(1.))
        .gap_2()
        .child(
            gpui::div()
                .debug_selector(|| "settings-api-key-input".to_owned())
                .min_w_0()
                .flex_1()
                .child(Input::new(&model.controls.api_key).small()),
        )
        .child(
            action_button("save-api-key")
                .debug_selector(|| "save-api-key".to_owned())
                .small()
                .label("Save key")
                .on_click(cx.listener(|shell, _, window, cx| {
                    shell.save_api_key(window, cx);
                })),
        )
        .when(model.has_api_key, |control| {
            control.child(
                action_button("settings-api-key-cancel")
                    .debug_selector(|| "settings-api-key-cancel".to_owned())
                    .ghost()
                    .small()
                    .label("Cancel")
                    .on_click(cx.listener(|shell, _, window, cx| {
                        shell.set_replacing_api_key(false, window, cx);
                    })),
            )
        })
}

fn shortcut_control(
    hotkey: &agentdictate_core::Hotkey,
    capture_active: bool,
    capture_error: Option<String>,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    gpui::div()
        .w(px(300.))
        .max_w(gpui::relative(1.))
        .flex()
        .flex_col()
        .items_end()
        .gap_1()
        .when(!capture_active, |control| {
            control.child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(
                        gpui::div()
                            .rounded_md()
                            .border_1()
                            .border_color(gpui_color(theme.border))
                            .px_3()
                            .py_1()
                            .text_sm()
                            .child(hotkey.label().to_owned()),
                    )
                    .child(
                        action_button("settings-hotkey-change")
                            .debug_selector(|| "settings-hotkey-change".to_owned())
                            .small()
                            .label("Change")
                            .on_click(cx.listener(|shell, _, window, cx| {
                                shell.begin_shortcut_capture(window, cx);
                            })),
                    ),
            )
        })
        .when(capture_active, |control| {
            control
                .child(
                    h_flex()
                        .debug_selector(|| "settings-hotkey-capture".to_owned())
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(
                            gpui::div()
                                .flex_1()
                                .rounded_md()
                                .border_1()
                                .border_color(gpui_color(theme.accent))
                                .px_3()
                                .py_1()
                                .text_sm()
                                .child("Press the new shortcut…"),
                        )
                        .child(
                            action_button("settings-hotkey-cancel")
                                .debug_selector(|| "settings-hotkey-cancel".to_owned())
                                .small()
                                .label("Cancel")
                                .on_click(cx.listener(|shell, _, window, cx| {
                                    shell.cancel_shortcut_capture(window, cx);
                                })),
                        ),
                )
                .child(
                    gpui::div()
                        .text_xs()
                        .text_color(gpui_color(theme.text_muted))
                        .child("Hold Ctrl, Alt, Shift or Super and press a key. Esc cancels."),
                )
        })
        .when_some(capture_error, |control, error| {
            control.child(
                gpui::div()
                    .debug_selector(|| "settings-hotkey-capture-error".to_owned())
                    .text_xs()
                    .text_color(gpui_color(theme.danger))
                    .child(error),
            )
        })
}
