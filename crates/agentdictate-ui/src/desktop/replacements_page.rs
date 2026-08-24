use gpui::{IntoElement, SharedString, prelude::*, px};
use gpui_component::{Selectable, Sizable, h_flex, input::Input, v_flex};

use crate::action::action_button;

use crate::{ReplacementRuleViewModel, ReplacementsViewModel, ThemeTokens, WorkspaceAction};

use super::{
    SettingsShell, enabled_label, gpui_color, settings_shell::ReplacementEditorState,
    single_line::single_line_clip,
};

const REPLACEMENT_ROW_HEIGHT: f32 = 50.0;

/// Renders the replacements workspace as a dense, flat list. The page keeps
/// editing and destructive confirmation state in `SettingsShell`; this module
/// owns only the route's layout and interactions.
pub(super) fn surface(
    replacements: ReplacementsViewModel,
    editor: Option<ReplacementEditorState>,
    feedback: Option<String>,
    pending_destructive_action: Option<WorkspaceAction>,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let is_empty = replacements.rules.is_empty();
    let rule_count = replacements.rule_count();
    let enabled_count = replacements.enabled_count();

    v_flex()
        .debug_selector(|| "replacements-page".to_owned())
        .w_full()
        .min_w_0()
        .gap_0()
        .child(
            h_flex()
                .debug_selector(|| "replacements-header".to_owned())
                .w_full()
                .min_w_0()
                .justify_between()
                .border_b_1()
                .border_color(gpui_color(theme.border))
                .pb_4()
                .gap_4()
                .child(
                    v_flex()
                        .min_w_0()
                        .flex_auto()
                        .gap_1()
                        .child(
                            h_flex()
                                .debug_selector(|| "replacements-title".to_owned())
                                .flex_none()
                                .text_sm()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(
                                    gpui::div()
                                        .debug_selector(|| {
                                            "replacements-title-glyphs".to_owned()
                                        })
                                        .flex_none()
                                        .child("Spoken replacements"),
                                ),
                        )
                        .child(
                            single_line_clip(
                                "replacements-header-detail",
                                format!(
                                    "Turn recurring phrases into the exact text you want. · {rule_count} rules · {enabled_count} enabled"
                                ),
                            )
                                .text_xs()
                                .text_color(gpui_color(theme.text_muted)),
                        ),
                )
                .child(
                    action_button("replacement-add")
                        .flex_none()
                        .debug_selector(|| "replacement-add".to_owned())
                        .small()
                        .label("Add replacement")
                        .on_click(cx.listener(move |shell, _, window, cx| {
                            shell.open_replacement_editor(None, window, cx);
                            cx.notify();
                        })),
                ),
        )
        .when_some(editor, |page, editor| {
            page.child(editor_surface(editor, feedback, theme, cx))
        })
        .when(is_empty, |page| {
            page.child(
                h_flex()
                    .debug_selector(|| "replacements-empty".to_owned())
                    .w_full()
                    .h(px(72.))
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(gpui_color(theme.border))
                    .text_sm()
                    .text_color(gpui_color(theme.text_muted))
                    .child("No replacements yet")
                    .child("·")
                    .child("Try “agent dictate” → “AgentDictate”"),
            )
        })
        .children(
            replacements
                .rules
                .into_iter()
                .map(|rule| rule_row(rule, pending_destructive_action.as_ref(), theme, cx)),
        )
}

fn rule_row(
    rule: ReplacementRuleViewModel,
    pending_destructive_action: Option<&WorkspaceAction>,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let row_selector = format!("replacement-item-{}", rule.id);
    let toggle_action = WorkspaceAction::SetReplacementEnabled {
        id: rule.id,
        enabled: !rule.enabled,
    };
    let toggle_selector = toggle_action.selector();
    let edit_selector = format!("replacement-edit-{}", rule.id);
    let rule_id = rule.id;
    let delete_action = WorkspaceAction::DeleteReplacement { id: rule.id };
    let delete_selector = delete_action.selector();
    let delete_confirmation_pending = pending_destructive_action == Some(&delete_action);
    let delete_button_selector = if delete_confirmation_pending {
        format!("confirm-{delete_selector}")
    } else {
        delete_selector
    };
    let match_policy_label = rule.match_policy_label();
    let source_selector = format!("replacement-source-{}", rule.id);
    let value_selector = format!("replacement-value-{}", rule.id);
    let policy_selector = format!("replacement-policy-{}", rule.id);

    h_flex()
        .debug_selector(move || row_selector)
        .w_full()
        .h(px(REPLACEMENT_ROW_HEIGHT))
        .min_w_0()
        .justify_between()
        .border_b_1()
        .border_color(gpui_color(theme.border))
        .px_1()
        .gap_4()
        .when(!rule.enabled, |row| row.opacity(0.58))
        .child(
            h_flex()
                .min_w_0()
                .flex_auto()
                .gap_3()
                .child(
                    single_line_clip(source_selector, rule.source)
                        .flex_auto()
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM),
                )
                .child(
                    gpui::div()
                        .flex_none()
                        .text_color(gpui_color(theme.text_muted))
                        .child("→"),
                )
                .child(
                    single_line_clip(value_selector, rule.replacement)
                        .flex_auto()
                        .text_sm(),
                ),
        )
        .child(
            single_line_clip(policy_selector, match_policy_label)
                .w(px(150.))
                .flex_none()
                .text_xs()
                .text_color(gpui_color(theme.text_muted)),
        )
        .child(
            h_flex()
                .flex_none()
                .gap_1()
                .child(
                    action_button(SharedString::from(toggle_selector.clone()))
                        .debug_selector(move || toggle_selector)
                        .small()
                        .selected(rule.enabled)
                        .label(enabled_label(rule.enabled))
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            shell.emit_workspace_action(toggle_action.clone(), cx);
                            cx.notify();
                        })),
                )
                .child(
                    action_button(SharedString::from(edit_selector.clone()))
                        .debug_selector(move || edit_selector)
                        .small()
                        .label("Edit")
                        .on_click(cx.listener(move |shell, _, window, cx| {
                            shell.open_replacement_editor(Some(rule_id), window, cx);
                            cx.notify();
                        })),
                )
                .child(
                    action_button(SharedString::from(delete_button_selector.clone()))
                        .debug_selector(move || delete_button_selector)
                        .small()
                        .label(if delete_confirmation_pending {
                            "Confirm"
                        } else {
                            "Delete"
                        })
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            shell.request_destructive_action(delete_action.clone(), cx);
                            cx.notify();
                        })),
                ),
        )
}

fn editor_surface(
    editor: ReplacementEditorState,
    feedback: Option<String>,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let save_selector = editor.id.map_or_else(
        || "replacement-save-new".to_owned(),
        |id| format!("replacement-save-{id}"),
    );

    v_flex()
        .debug_selector(|| "replacement-editor".to_owned())
        .w_full()
        .border_b_1()
        .border_color(gpui_color(theme.accent))
        .py_3()
        .gap_3()
        .child(
            h_flex()
                .justify_between()
                .gap_4()
                .child(
                    gpui::div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(if editor.id.is_some() {
                            "Edit replacement"
                        } else {
                            "New replacement"
                        }),
                )
                .child(
                    gpui::div()
                        .text_xs()
                        .text_color(gpui_color(theme.text_muted))
                        .child("Applied after cleanup, before paste"),
                ),
        )
        .child(
            h_flex()
                .min_w_0()
                .gap_3()
                .child(
                    v_flex()
                        .min_w_0()
                        .flex_1()
                        .gap_1()
                        .child(
                            gpui::div()
                                .text_xs()
                                .text_color(gpui_color(theme.text_muted))
                                .child("When I say"),
                        )
                        .child(
                            gpui::div()
                                .debug_selector(|| "replacement-editor-source".to_owned())
                                .child(Input::new(&editor.source).small()),
                        ),
                )
                .child(
                    v_flex()
                        .min_w_0()
                        .flex_1()
                        .gap_1()
                        .child(
                            gpui::div()
                                .text_xs()
                                .text_color(gpui_color(theme.text_muted))
                                .child("Write"),
                        )
                        .child(
                            gpui::div()
                                .debug_selector(|| "replacement-editor-output".to_owned())
                                .child(Input::new(&editor.replacement).small()),
                        ),
                ),
        )
        .child(
            h_flex()
                .justify_between()
                .gap_4()
                .child(
                    h_flex()
                        .gap_1()
                        .child(editor_toggle(
                            "Enabled",
                            editor.enabled,
                            "replacement-editor-enabled",
                            |editor| editor.enabled = !editor.enabled,
                            cx,
                        ))
                        .child(editor_toggle(
                            "Match case",
                            editor.case_sensitive,
                            "replacement-editor-case-sensitive",
                            |editor| editor.case_sensitive = !editor.case_sensitive,
                            cx,
                        ))
                        .child(editor_toggle(
                            "Whole words",
                            editor.whole_word_only,
                            "replacement-editor-whole-words",
                            |editor| editor.whole_word_only = !editor.whole_word_only,
                            cx,
                        )),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .child(
                            action_button("replacement-editor-cancel")
                                .debug_selector(|| "replacement-editor-cancel".to_owned())
                                .small()
                                .label("Cancel")
                                .on_click(cx.listener(|shell, _, _, cx| {
                                    shell.routes.replacement_editor = None;
                                    shell.clear_route_feedback();
                                    cx.notify();
                                })),
                        )
                        .child(
                            action_button(SharedString::from(save_selector.clone()))
                                .debug_selector(move || save_selector)
                                .small()
                                .label("Save replacement")
                                .on_click(cx.listener(|shell, _, _, cx| {
                                    shell.save_replacement(cx);
                                    cx.notify();
                                })),
                        ),
                ),
        )
        .when_some(feedback, |editor, feedback| {
            editor.child(
                gpui::div()
                    .text_xs()
                    .text_color(gpui_color(theme.danger))
                    .child(feedback),
            )
        })
}

fn editor_toggle(
    label: &'static str,
    selected: bool,
    id: &'static str,
    toggle: fn(&mut ReplacementEditorState),
    cx: &mut Context<SettingsShell>,
) -> impl IntoElement {
    action_button(id)
        .debug_selector(move || id.to_owned())
        .small()
        .selected(selected)
        .label(label)
        .on_click(cx.listener(move |shell, _, _, cx| {
            if let Some(editor) = &mut shell.routes.replacement_editor {
                toggle(editor);
            }
            cx.notify();
        }))
}
