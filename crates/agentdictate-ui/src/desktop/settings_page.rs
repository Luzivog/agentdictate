use agentdictate_core::TranscriptionProvider;
use gpui::{Context, Entity, prelude::*, px};
use gpui_component::{
    Disableable, Selectable, Sizable,
    button::{ButtonCustomVariant, ButtonVariants},
    h_flex,
    input::{Input, InputState, NumberInput},
    select::Select,
    v_flex,
};

use crate::action::action_button;
use crate::{ModelCatalogViewModel, SettingsDraft, ThemeTokens};

use super::{
    SettingsShell, gpui_color,
    settings_form::{
        SettingSelectState, SettingsFormState, selected_setting, selected_transcription_provider,
    },
};

pub(super) struct SettingsPageModel {
    pub(super) draft: SettingsDraft,
    pub(super) model_catalog: ModelCatalogViewModel,
    pub(super) settings_dirty: bool,
    pub(super) has_api_key: bool,
    pub(super) api_key_input: Entity<InputState>,
    pub(super) api_key_feedback: Option<String>,
    pub(super) feedback: Option<String>,
    pub(super) settings_form: SettingsFormState,
    pub(super) shortcut_capture_active: bool,
    pub(super) shortcut_capture_error: Option<String>,
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
        model_catalog,
        has_api_key,
        api_key_input,
        api_key_feedback,
        settings_form,
        shortcut_capture_active,
        shortcut_capture_error,
        ..
    } = model;
    let transcription_provider = selected_transcription_provider(
        &settings_form.transcription_provider,
        settings.transcription_provider,
        cx,
    );
    let uses_chatgpt_subscription =
        transcription_provider == TranscriptionProvider::ChatGptSubscription;
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
                uses_chatgpt_subscription,
                theme,
                cx,
            ))
            .child(dictation_section(
                &settings,
                &model_catalog,
                &settings_form,
                theme,
                cx,
            ))
            .child(output_section(&settings, &settings_form, theme, cx))
            .child(recording_audio_section(
                &settings,
                &settings_form,
                shortcut_capture_active,
                shortcut_capture_error,
                theme,
                cx,
            ))
            .child(delivery_storage_section(
                &settings,
                &settings_form,
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
    uses_chatgpt_subscription: bool,

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
                if uses_chatgpt_subscription {
                    "Not needed for transcription"
                } else {
                    "Used for API transcription"
                },
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
                            } else if uses_chatgpt_subscription {
                                theme.text_muted
                            } else {
                                theme.danger
                            }))
                            .child(if has_api_key {
                                "Configured"
                            } else if uses_chatgpt_subscription {
                                "Not needed"
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

fn dictation_section(
    settings: &SettingsDraft,
    model_catalog: &ModelCatalogViewModel,
    editor: &SettingsFormState,
    theme: ThemeTokens,
    cx: &Context<SettingsShell>,
) -> gpui::Div {
    let transcription_provider = selected_transcription_provider(
        &editor.transcription_provider,
        settings.transcription_provider,
        cx,
    );
    let uses_chatgpt_subscription =
        transcription_provider == TranscriptionProvider::ChatGptSubscription;
    settings_section(
        "settings-group-dictation",
        "Dictation",
        "Choose how recorded speech is recognized.",
        true,
        theme,
    )
    .child(select_row(
        "Transcription source",
        if uses_chatgpt_subscription {
            "Uses your Codex sign-in"
        } else {
            "How speech is transcribed"
        },
        "settings-input-transcription-provider",
        editor.transcription_provider.clone(),
        false,
        theme,
    ))
    .when(uses_chatgpt_subscription, |section| {
        section.child(value_row(
            "Speech model",
            "Selected automatically",
            "Managed by ChatGPT",
            "settings-transcription-managed-by-chatgpt",
            theme,
        ))
    })
    .when(!uses_chatgpt_subscription, |section| {
        section
            .child(model_catalog_status(model_catalog, theme))
            .child(select_row(
                "Speech model",
                "OpenAI transcription model",
                "settings-input-transcription-model",
                editor.transcription_model.clone(),
                false,
                theme,
            ))
            .when(
                selected_setting(
                    &editor.transcription_model,
                    &settings.transcription_model,
                    cx,
                ) == "Custom",
                |section| {
                    section.child(input_row(
                        "Custom speech model",
                        "Exact OpenAI model identifier",
                        "settings-input-custom-transcription-model",
                        editor.custom_transcription_model.clone(),
                        false,
                        theme,
                    ))
                },
            )
    })
    .child(select_row(
        "Language",
        "One language or automatic detection. English & French requires gpt-transcribe.",
        "settings-input-language",
        editor.language.clone(),
        false,
        theme,
    ))
    .when(!uses_chatgpt_subscription, |section| {
        section.child(prompt_row(
            "Context prompt",
            "Describe what you are talking about; spellings belong in Vocabulary",
            "settings-input-transcription-prompt",
            editor.transcription_prompt.clone(),
            false,
            theme,
        ))
    })
}

fn model_catalog_status(model_catalog: &ModelCatalogViewModel, theme: ThemeTokens) -> gpui::Div {
    let status = &model_catalog.status;
    let selector = status.selector();
    h_flex()
        .debug_selector(move || selector.to_owned())
        .min_h(px(34.))
        .gap_2()
        .border_b_1()
        .border_color(gpui_color(theme.border))
        .pb_2()
        .text_xs()
        .child(
            gpui::div()
                .size_1p5()
                .rounded_full()
                .bg(gpui_color(if status.is_error {
                    theme.danger
                } else {
                    theme.success
                })),
        )
        .child(
            h_flex()
                .min_w_0()
                .flex_wrap()
                .gap_x_2()
                .child(status.label)
                .child(
                    gpui::div()
                        .text_color(gpui_color(theme.text_muted))
                        .child(status.detail.clone()),
                ),
        )
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
        "Vocabulary and literal text.",
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
        "Vocabulary",
        "One spelling per line. Add = spoken alias only for automatic corrections.",
        "settings-input-vocabulary",
        editor.vocabulary.clone(),
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

fn delivery_storage_section(
    settings: &SettingsDraft,
    editor: &SettingsFormState,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    settings_section(
        "settings-group-delivery-storage",
        "Delivery & storage",
        "Choose how finished dictation is pasted, retained, and started.",
        true,
        theme,
    )
    .child(select_row(
        "Paste shortcut",
        "Automatic detects X11/XWayland apps; native Wayland uses Shift+Insert",
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
    .child(toggle_row(
        "Save history",
        "Keep delivered transcripts in your local database",
        settings.save_history,
        "toggle-save-history",
        theme,
        cx,
        |draft| draft.save_history = !draft.save_history,
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

fn value_row(
    label: &'static str,
    detail: &'static str,
    value: &'static str,
    selector: &'static str,
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
            control_slot(selector, SettingsControlKind::Choice).child(
                gpui::div()
                    .text_sm()
                    .text_color(gpui_color(theme.text_muted))
                    .child(value),
            ),
        )
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
    hotkey: &str,
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
                                    .child(hotkey.to_owned()),
                            )
                            .child(
                                action_button("settings-hotkey-change")
                                    .debug_selector(|| "settings-hotkey-change".to_owned())
                                    .small()
                                    .label("Change")
                                    .on_click(cx.listener(|shell, _, _, cx| {
                                        shell.begin_shortcut_capture(cx);
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
                                        .child("Press a shortcut…"),
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
                        .when_some(capture_error, |control, error| {
                            control.child(
                                gpui::div()
                                    .text_xs()
                                    .text_color(gpui_color(theme.danger))
                                    .child(error),
                            )
                        })
                }),
        )
}

#[allow(clippy::too_many_arguments)]
fn input_row(
    label: &'static str,
    detail: &'static str,
    selector: &'static str,
    input: Entity<InputState>,
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
        .child(stacked_setting_label(label, detail, theme))
        .child(
            control_slot(selector, SettingsControlKind::Choice)
                .child(Input::new(&input).small().w_full().disabled(disabled)),
        )
        .when(disabled, |row| row.opacity(0.48))
}

#[allow(clippy::too_many_arguments)]
fn prompt_row(
    label: &'static str,
    detail: &'static str,
    selector: &'static str,
    input: Entity<InputState>,
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
                .child(Input::new(&input).small().w_full().disabled(disabled)),
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

fn stacked_setting_label(
    label: &'static str,
    detail: &'static str,
    theme: ThemeTokens,
) -> gpui::Div {
    v_flex()
        .w_full()
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
