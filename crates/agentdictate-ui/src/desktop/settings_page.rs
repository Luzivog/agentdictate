use gpui::{Context, Entity, prelude::*, px};
use gpui_component::{
    Disableable, Selectable, Sizable,
    button::{ButtonCustomVariant, ButtonVariants},
    h_flex,
    input::{Input, InputState, NumberInput, Textarea, TextareaState},
    select::Select,
    v_flex,
};

use crate::action::action_button;
use crate::{SettingsDraft, ThemeTokens, WorkspaceAction};

use super::{
    SettingsShell, gpui_color,
    row_actions::delete_button,
    settings_form::{SettingSelectState, SettingsFormState},
};

pub(super) struct SettingsPageModel {
    pub(super) draft: SettingsDraft,
    pub(super) settings_dirty: bool,
    pub(super) has_api_key: bool,
    pub(super) api_key_input: Entity<InputState>,
    pub(super) api_key_feedback: Option<String>,
    pub(super) feedback: Option<String>,
    pub(super) settings_form: SettingsFormState,
    pub(super) shortcut_capture_active: bool,
    pub(super) shortcut_capture_error: Option<String>,
    pub(super) pending_destructive_action: Option<WorkspaceAction>,
}

/// Keeps short controls compact while allowing long-form prompts to use the
/// page width. This is the single sizing policy for Settings controls.
#[derive(Clone, Copy)]
enum SettingsControlKind {
    Choice,
    Number,
    Shortcut,
    Credential,
}

impl SettingsControlKind {
    const fn width(self) -> f32 {
        match self {
            Self::Choice | Self::Shortcut => 300.,
            Self::Number => 180.,
            Self::Credential => 360.,
        }
    }
}

fn control_slot(selector: &'static str, kind: SettingsControlKind) -> gpui::Div {
    gpui::div()
        .debug_selector(move || format!("{selector}-control"))
        .w(px(kind.width()))
        .max_w(gpui::relative(1.))
        .flex_none()
}

fn prompt_control_slot(selector: &'static str) -> gpui::Div {
    h_flex()
        .debug_selector(move || format!("{selector}-control"))
        .w_full()
        .min_w_0()
}

pub(super) fn surface(
    model: SettingsPageModel,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let SettingsPageModel {
        draft: settings,
        has_api_key,
        api_key_input,
        api_key_feedback,
        settings_form,
        shortcut_capture_active,
        shortcut_capture_error,
        pending_destructive_action,
        ..
    } = model;
    h_flex().w_full().justify_center().child(
        v_flex()
            .debug_selector(|| "settings-page".to_owned())
            .w_full()
            .max_w(px(980.))
            .gap_6()
            .child(
                h_flex().justify_between().gap_6().child(
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
                                .child("Changes apply to the running dictation service."),
                        ),
                ),
            )
            .child(account_section(
                has_api_key,
                api_key_input,
                api_key_feedback,
                theme,
                cx,
            ))
            .child(dictation_section(&settings_form, theme))
            .child(output_section(&settings, &settings_form, theme, cx))
            .child(recording_audio_section(
                &settings,
                &settings_form,
                shortcut_capture_active,
                shortcut_capture_error,
                theme,
                cx,
            ))
            .child(delivery_section(&settings, &settings_form, theme, cx))
            .child(privacy_section(
                &settings,
                &settings_form,
                pending_destructive_action.as_ref(),
                theme,
                cx,
            )),
    )
}

/// Rendered below the route viewport so actions and feedback stay visible.
pub(super) fn footer(
    model: &SettingsPageModel,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> Option<gpui::Div> {
    if !model.settings_dirty && model.feedback.is_none() {
        return None;
    }
    Some(
        h_flex()
            .debug_selector(|| "settings-footer".to_owned())
            .flex_none()
            .w_full()
            .justify_center()
            .border_t_1()
            .border_color(gpui_color(theme.border))
            .bg(gpui_color(theme.canvas))
            .px_6()
            .py_3()
            .child(
                h_flex()
                    .w_full()
                    .max_w(px(980.))
                    .justify_between()
                    .gap_4()
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap_1()
                            .text_xs()
                            .text_color(gpui_color(theme.text_muted))
                            .when(model.settings_dirty, |status| {
                                status.child("Unsaved changes")
                            })
                            .when_some(model.feedback.clone(), |status, feedback| {
                                status.child(
                                    gpui::div()
                                        .debug_selector(|| "settings-feedback".to_owned())
                                        .child(feedback),
                                )
                            }),
                    )
                    .when(model.settings_dirty, |bar| bar.child(save_bar(theme, cx))),
            ),
    )
}

fn save_bar(theme: ThemeTokens, cx: &mut Context<SettingsShell>) -> gpui::Div {
    h_flex()
        .debug_selector(|| "settings-save-bar".to_owned())
        .flex_none()
        .gap_2()
        .child(
            action_button("discard-settings")
                .debug_selector(|| "discard-settings".to_owned())
                .ghost()
                .small()
                .label("Discard")
                .on_click(cx.listener(|shell, _, window, cx| {
                    shell.discard_settings_editor(window, cx);
                })),
        )
        .child(
            action_button("save-settings")
                .debug_selector(|| "save-settings".to_owned())
                .custom(
                    ButtonCustomVariant::new(cx)
                        .color(gpui_color(theme.accent))
                        .foreground(gpui_color(theme.canvas))
                        .hover(gpui_color(theme.accent).opacity(0.88))
                        .active(gpui_color(theme.accent).opacity(0.76)),
                )
                .small()
                .label("Save changes")
                .on_click(cx.listener(|shell, _, _, cx| {
                    shell.save_settings_editor(cx);
                    cx.notify();
                })),
        )
}

fn account_section(
    has_api_key: bool,
    api_key_input: Entity<InputState>,
    api_key_feedback: Option<String>,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    settings_section(
        "settings-group-account",
        "Account",
        "API access is stored locally.",
        false,
        theme,
    )
    .child(
        h_flex()
            .debug_selector(|| "settings-api-key".to_owned())
            .min_h(px(54.))
            .items_start()
            .flex_wrap()
            .justify_between()
            .gap_6()
            .border_b_1()
            .border_color(gpui_color(theme.border))
            .py_2()
            .child(setting_label(
                "OpenAI API key",
                "Used for API transcription",
                theme,
            ))
            .child(
                control_slot("settings-api-key", SettingsControlKind::Credential)
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .child(
                        gpui::div()
                            .debug_selector(|| "settings-api-key-input".to_owned())
                            .min_w_0()
                            .flex_1()
                            .child(Input::new(&api_key_input).small()),
                    )
                    .child(
                        action_button("save-api-key")
                            .debug_selector(|| "save-api-key".to_owned())
                            .small()
                            .label("Save key")
                            .on_click(cx.listener(|shell, _, window, cx| {
                                shell.save_api_key(window, cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        gpui::div()
                            .flex_none()
                            .text_xs()
                            .text_color(gpui_color(if has_api_key {
                                theme.success
                            } else {
                                theme.danger
                            }))
                            .child(if has_api_key {
                                "Configured"
                            } else {
                                "Required"
                            }),
                    ),
            ),
    )
    .when_some(api_key_feedback, |section, feedback| {
        section.child(
            gpui::div()
                .debug_selector(|| "api-key-feedback".to_owned())
                .border_b_1()
                .border_color(gpui_color(theme.border))
                .pb_2()
                .text_xs()
                .text_color(gpui_color(theme.text_muted))
                .child(feedback),
        )
    })
}

fn dictation_section(editor: &SettingsFormState, theme: ThemeTokens) -> gpui::Div {
    settings_section(
        "settings-group-dictation",
        "Dictation",
        "Choose how recorded speech is recognized.",
        true,
        theme,
    )
    .child(select_row(
        "Language",
        "One language or automatic detection. English & French requires gpt-transcribe.",
        "settings-input-language",
        editor.language.clone(),
        false,
        theme,
    ))
    .child(prompt_row(
        "Context prompt",
        "Describe what you are talking about; spellings belong in Words",
        "settings-input-transcription-prompt",
        editor.transcription_prompt.clone(),
        false,
        theme,
    ))
}

fn output_section(
    settings: &SettingsDraft,
    editor: &SettingsFormState,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    settings_section(
        "settings-group-cleanup",
        "Dictation output",
        "Literal text, work context and streaming.",
        true,
        theme,
    )
    .child(select_row(
        "Output mode",
        "Literal skips context hints and automatic spelling corrections",
        "settings-input-dictation-mode",
        editor.dictation_mode.clone(),
        false,
        theme,
    ))
    .child(prompt_row(
        "Current work context",
        "Optional context you supply; clear it when changing projects",
        "settings-input-project-context",
        editor.project_context.clone(),
        false,
        theme,
    ))
    .child(toggle_row(
        "Stream speech",
        "Experimental OpenAI API streaming; falls back to saved audio",
        settings.streaming_enabled,
        "toggle-streaming",
        theme,
        cx,
        |draft| draft.streaming_enabled = !draft.streaming_enabled,
    ))
}

fn recording_audio_section(
    settings: &SettingsDraft,
    editor: &SettingsFormState,
    shortcut_capture_active: bool,
    shortcut_capture_error: Option<String>,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    settings_section(
        "settings-group-recording-audio",
        "Recording & audio",
        "Control the global shortcut, recording lifecycle, and playback ducking.",
        true,
        theme,
    )
    .child(shortcut_row(
        &settings.hotkey,
        shortcut_capture_active,
        shortcut_capture_error,
        theme,
        cx,
    ))
    .child(select_row(
        "Recording mode",
        "Use toggle or hold",
        "settings-input-recording-mode",
        editor.recording_mode.clone(),
        false,
        theme,
    ))
    .child(number_row(
        "Maximum recording",
        "Use 0 to disable the automatic stop",
        "settings-input-max-recording",
        editor.max_recording_seconds.clone(),
        "seconds",
        false,
        theme,
    ))
    .child(toggle_row(
        "Audio ducking",
        "Lower the default output while you dictate",
        settings.audio_ducking_enabled,
        "toggle-audio-ducking",
        theme,
        cx,
        |draft| draft.audio_ducking_enabled = !draft.audio_ducking_enabled,
    ))
    .child(number_row(
        "Ducked volume",
        "Percentage of the original playback volume",
        "settings-input-ducked-volume",
        editor.audio_ducking_volume_percent.clone(),
        "%",
        !settings.audio_ducking_enabled,
        theme,
    ))
    .child(number_row(
        "Fade out (ms)",
        "Time to lower playback volume",
        "settings-input-ducking-fade-out",
        editor.audio_ducking_fade_out_ms.clone(),
        "ms",
        !settings.audio_ducking_enabled,
        theme,
    ))
    .child(number_row(
        "Fade in (ms)",
        "Time to restore playback volume",
        "settings-input-ducking-fade-in",
        editor.audio_ducking_fade_in_ms.clone(),
        "ms",
        !settings.audio_ducking_enabled,
        theme,
    ))
}

fn delivery_section(
    settings: &SettingsDraft,
    editor: &SettingsFormState,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    settings_section(
        "settings-group-delivery",
        "Delivery",
        "Choose how finished dictation is pasted and started.",
        true,
        theme,
    )
    .child(select_row(
        "Paste shortcut",
        "Automatic uses Shift+Insert, which terminals and other apps both paste with",
        "settings-input-paste-shortcut",
        editor.paste_shortcut.clone(),
        false,
        theme,
    ))
    .child(toggle_row(
        "Start on login",
        "Make the shortcut ready with your desktop session",
        settings.start_on_login,
        "toggle-start-on-login",
        theme,
        cx,
        |draft| draft.start_on_login = !draft.start_on_login,
    ))
}

/// What this computer keeps, and the one control that deletes all of it.
fn privacy_section(
    settings: &SettingsDraft,
    editor: &SettingsFormState,
    pending_destructive_action: Option<&WorkspaceAction>,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    settings_section(
        "settings-group-privacy",
        "Privacy",
        "Choose what AgentDictate keeps on this computer.",
        true,
        theme,
    )
    .child(select_row(
        "Keep transcripts",
        "How long History keeps transcripts, including saved ones. Usage numbers stay",
        "settings-input-keep-transcripts",
        editor.keep_transcripts.clone(),
        false,
        theme,
    ))
    .child(toggle_row(
        "Preserve temporary audio",
        "Keep recordings after successful transcription",
        settings.preserve_temp_audio,
        "toggle-preserve-audio",
        theme,
        cx,
        |draft| draft.preserve_temp_audio = !draft.preserve_temp_audio,
    ))
    .child(
        h_flex()
            .min_h(px(52.))
            .justify_between()
            .gap_6()
            .border_b_1()
            .border_color(gpui_color(theme.border))
            .py_2()
            .child(setting_label(
                "Delete all history",
                "Permanently removes every saved transcript and its usage stats",
                theme,
            ))
            .child(delete_button(
                WorkspaceAction::ClearHistory,
                "Delete all history…",
                pending_destructive_action,
                cx,
            )),
    )
}

fn settings_section(
    selector: &'static str,
    title: &'static str,
    detail: &'static str,
    _divided: bool,
    theme: ThemeTokens,
) -> gpui::Div {
    v_flex()
        .w_full()
        .debug_selector(move || selector.to_owned())
        .w_full()
        .gap_0()
        .child(
            v_flex()
                .gap_1()
                .pb_3()
                .child(
                    gpui::div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(title),
                )
                .child(
                    gpui::div()
                        .text_xs()
                        .text_color(gpui_color(theme.text_muted))
                        .child(detail),
                ),
        )
}

#[allow(clippy::too_many_arguments)]
fn select_row(
    label: &'static str,
    detail: &'static str,
    selector: &'static str,
    select: Entity<SettingSelectState>,
    disabled: bool,
    theme: ThemeTokens,
) -> gpui::Div {
    h_flex()
        .debug_selector(move || selector.to_owned())
        .min_h(px(54.))
        .items_start()
        .flex_wrap()
        .justify_between()
        .gap_6()
        .border_b_1()
        .border_color(gpui_color(theme.border))
        .py_2()
        .child(setting_label(label, detail, theme))
        .child(
            control_slot(selector, SettingsControlKind::Choice)
                .cursor_pointer()
                .child(Select::new(&select).small().w_full().disabled(disabled)),
        )
        .when(disabled, |row| row.opacity(0.48))
}

#[allow(clippy::too_many_arguments)]
fn number_row(
    label: &'static str,
    detail: &'static str,
    selector: &'static str,
    input: Entity<InputState>,
    suffix: &'static str,
    disabled: bool,
    theme: ThemeTokens,
) -> gpui::Div {
    h_flex()
        .debug_selector(move || selector.to_owned())
        .min_h(px(54.))
        .items_start()
        .flex_wrap()
        .justify_between()
        .gap_6()
        .border_b_1()
        .border_color(gpui_color(theme.border))
        .py_2()
        .child(setting_label(label, detail, theme))
        .child(
            control_slot(selector, SettingsControlKind::Number)
                .cursor_pointer()
                .child(
                    NumberInput::new(&input)
                        .small()
                        .w_full()
                        .suffix(
                            gpui::div()
                                .pr_2()
                                .text_xs()
                                .text_color(gpui_color(theme.text_muted))
                                .child(suffix),
                        )
                        .disabled(disabled),
                ),
        )
        .when(disabled, |row| row.opacity(0.48))
}

fn shortcut_row(
    hotkey: &agentdictate_core::Hotkey,
    capture_active: bool,
    capture_error: Option<String>,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    h_flex()
        .debug_selector(|| "settings-hotkey-row".to_owned())
        .min_h(px(54.))
        .items_start()
        .flex_wrap()
        .justify_between()
        .gap_6()
        .border_b_1()
        .border_color(gpui_color(theme.border))
        .py_2()
        .child(setting_label(
            "Global shortcut",
            "Applied live without restarting the service",
            theme,
        ))
        .child(
            control_slot("settings-hotkey-row", SettingsControlKind::Shortcut)
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
                                        .on_click(cx.listener(|shell, _, _, cx| {
                                            shell.cancel_shortcut_capture(cx);
                                        })),
                                ),
                        )
                        .child(
                            gpui::div()
                                .text_xs()
                                .text_color(gpui_color(theme.text_muted))
                                .child(
                                    "Hold Ctrl, Alt, Shift or Super and press a key. Esc cancels.",
                                ),
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
                }),
        )
}

#[allow(clippy::too_many_arguments)]
fn prompt_row(
    label: &'static str,
    detail: &'static str,
    selector: &'static str,
    input: Entity<TextareaState>,
    disabled: bool,
    theme: ThemeTokens,
) -> gpui::Div {
    v_flex()
        .w_full()
        .debug_selector(move || selector.to_owned())
        .gap_2()
        .border_b_1()
        .border_color(gpui_color(theme.border))
        .py_3()
        .child(setting_label(label, detail, theme))
        .child(
            prompt_control_slot(selector)
                .child(Textarea::new(&input).w_full().text_sm().disabled(disabled)),
        )
        .when(disabled, |row| row.opacity(0.48))
}

#[allow(clippy::too_many_arguments)]
fn toggle_row(
    label: &'static str,
    detail: &'static str,
    enabled: bool,
    selector: &'static str,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
    update: fn(&mut SettingsDraft),
) -> gpui::Div {
    h_flex()
        .min_h(px(52.))
        .justify_between()
        .gap_6()
        .border_b_1()
        .border_color(gpui_color(theme.border))
        .py_2()
        .child(setting_label(label, detail, theme))
        .child(
            action_button(selector)
                .debug_selector(move || selector.to_owned())
                .small()
                .selected(enabled)
                .label(enabled_label(enabled))
                .on_click(cx.listener(move |shell, _, _, cx| {
                    shell.update_settings_draft(cx, update);
                })),
        )
}

fn setting_label(label: &'static str, detail: &'static str, theme: ThemeTokens) -> gpui::Div {
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
        )
}

const fn enabled_label(enabled: bool) -> &'static str {
    if enabled { "On" } else { "Off" }
}
