use gpui::{Context, Entity, SharedString, prelude::*, px, relative};
use gpui_component::{
    Sizable, h_flex,
    input::{Input, InputState},
    v_flex,
};

use crate::{ThemeTokens, WordRowViewModel, action::action_button};

use super::{SettingsShell, gpui_color, single_line::single_line_clip, words_actions::WordForm};

const WORD_ROW_HEIGHT: f32 = 44.0;
/// The Spelling column's share of the list width, in the header and rows.
const SPELLING_COLUMN: f32 = 0.35;

pub(super) struct WordsPageModel {
    /// The words the filter keeps.
    pub(super) rows: Vec<WordRowViewModel>,
    pub(super) has_words: bool,
    pub(super) filter: Entity<InputState>,
    pub(super) new_spelling: Entity<InputState>,
    pub(super) new_sounds_like: Entity<InputState>,
    pub(super) editor: Option<WordForm>,
    pub(super) error: Option<String>,
    pub(super) saved: bool,
}

/// Renders the Words screen: an explanation, the add row, a filter and the
/// list of words. Every change saves at once; "Saved ✓" confirms it.
pub(super) fn surface(
    model: WordsPageModel,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let WordsPageModel {
        rows,
        has_words,
        filter,
        new_spelling,
        new_sounds_like,
        editor,
        error,
        saved,
    } = model;
    let filtered_out = has_words && rows.is_empty();

    v_flex()
        .debug_selector(|| "words-page".to_owned())
        .w_full()
        .min_w_0()
        .gap_5()
        .child(
            h_flex()
                .w_full()
                .items_start()
                .justify_between()
                .gap_4()
                .child(
                    v_flex()
                        .min_w_0()
                        .gap_1()
                        .child(
                            gpui::div()
                                .text_sm()
                                .child("AgentDictate always spells these words your way."),
                        )
                        .child(
                            gpui::div()
                                .text_xs()
                                .text_color(gpui_color(theme.text_muted))
                                .child(
                                    "Sounds like: what you say that should become this spelling. \
                                     Leave empty to only hint the spelling.",
                                ),
                        ),
                )
                .when(saved, |header| {
                    header.child(
                        gpui::div()
                            .debug_selector(|| "words-saved".to_owned())
                            .flex_none()
                            .text_xs()
                            .text_color(gpui_color(theme.success))
                            .child("Saved ✓"),
                    )
                }),
        )
        .child(
            v_flex()
                .w_full()
                .gap_1()
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .child(
                            input_slot("words-new-spelling", &new_spelling)
                                .w(relative(SPELLING_COLUMN)),
                        )
                        .child(input_slot("words-new-sounds-like", &new_sounds_like).flex_1())
                        .child(
                            action_button("words-add")
                                .debug_selector(|| "words-add".to_owned())
                                .flex_none()
                                .small()
                                .label("Add word")
                                .on_click(cx.listener(|shell, _, window, cx| {
                                    shell.add_word(window, cx);
                                })),
                        ),
                )
                .when_some(error, |add, error| {
                    add.child(error_line("words-error", error, theme))
                }),
        )
        .when(has_words, |page| {
            page.child(input_slot("words-filter", &filter).w_full())
        })
        .child(
            v_flex()
                .w_full()
                .min_w_0()
                .child(
                    h_flex()
                        .w_full()
                        .h(px(32.))
                        .px_1()
                        .gap_4()
                        .border_b_1()
                        .border_color(gpui_color(theme.border))
                        .text_xs()
                        .text_color(gpui_color(theme.text_muted))
                        .child(
                            gpui::div()
                                .w(relative(SPELLING_COLUMN))
                                .flex_none()
                                .child("Spelling"),
                        )
                        .child(gpui::div().flex_1().child("Sounds like")),
                )
                .when(!has_words, |list| {
                    list.child(empty_line(
                        "words-empty",
                        "Add names and terms AgentDictate should always spell your way — \
                         for example Siobhan or Kubernetes.",
                        theme,
                    ))
                })
                .when(filtered_out, |list| {
                    list.child(empty_line(
                        "words-no-match",
                        "No words match this filter.",
                        theme,
                    ))
                })
                .children(rows.into_iter().map(|row| {
                    match editor.as_ref().filter(|form| form.index == row.index) {
                        Some(form) => editor_row(form.clone(), theme, cx),
                        None => word_row(row, theme, cx),
                    }
                })),
        )
}

fn input_slot(selector: &'static str, input: &Entity<InputState>) -> gpui::Div {
    gpui::div()
        .debug_selector(move || selector.to_owned())
        .min_w_0()
        .child(Input::new(input).small())
}

fn error_line(selector: &'static str, error: String, theme: ThemeTokens) -> gpui::Div {
    gpui::div()
        .debug_selector(move || selector.to_owned())
        .text_xs()
        .text_color(gpui_color(theme.danger))
        .child(error)
}

fn empty_line(selector: &'static str, text: &'static str, theme: ThemeTokens) -> gpui::Div {
    h_flex()
        .debug_selector(move || selector.to_owned())
        .w_full()
        .min_h(px(64.))
        .items_center()
        .border_b_1()
        .border_color(gpui_color(theme.border))
        .px_1()
        .text_sm()
        .text_color(gpui_color(theme.text_muted))
        .child(text)
}

fn word_row(
    row: WordRowViewModel,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let index = row.index;
    let edit_selector = format!("word-edit-{index}");
    let delete_selector = format!("word-delete-{index}");
    let sounds_like = if row.sounds_like.is_empty() {
        "—".to_owned()
    } else {
        row.sounds_like
    };

    h_flex()
        .debug_selector(move || format!("word-row-{index}"))
        .w_full()
        .h(px(WORD_ROW_HEIGHT))
        .min_w_0()
        .px_1()
        .gap_4()
        .border_b_1()
        .border_color(gpui_color(theme.border))
        .child(
            single_line_clip(format!("word-spelling-{index}"), row.spelling)
                .w(relative(SPELLING_COLUMN))
                .flex_none()
                .text_sm()
                .font_weight(gpui::FontWeight::MEDIUM),
        )
        .child(
            single_line_clip(format!("word-sounds-like-{index}"), sounds_like)
                .flex_1()
                .text_sm()
                .text_color(gpui_color(theme.text_muted)),
        )
        .child(
            h_flex()
                .flex_none()
                .gap_1()
                .child(
                    action_button(SharedString::from(edit_selector.clone()))
                        .debug_selector(move || edit_selector)
                        .small()
                        .label("Edit")
                        .on_click(cx.listener(move |shell, _, window, cx| {
                            shell.open_word_editor(index, window, cx);
                        })),
                )
                .child(
                    action_button(SharedString::from(delete_selector.clone()))
                        .debug_selector(move || delete_selector)
                        .small()
                        .label("Delete")
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            shell.delete_word(index, cx);
                        })),
                ),
        )
}

/// A row open for editing. Done (or Enter) saves it; Cancel restores it.
fn editor_row(form: WordForm, theme: ThemeTokens, cx: &mut Context<SettingsShell>) -> gpui::Div {
    let index = form.index;
    v_flex()
        .debug_selector(move || format!("word-row-{index}"))
        .w_full()
        .min_w_0()
        .min_h(px(WORD_ROW_HEIGHT))
        .justify_center()
        .px_1()
        .py_1()
        .gap_1()
        .border_b_1()
        .border_color(gpui_color(theme.accent))
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .child(
                    input_slot("word-editor-spelling", &form.spelling).w(relative(SPELLING_COLUMN)),
                )
                .child(input_slot("word-editor-sounds-like", &form.sounds_like).flex_1())
                .child(
                    action_button("word-editor-done")
                        .debug_selector(|| "word-editor-done".to_owned())
                        .flex_none()
                        .small()
                        .label("Done")
                        .on_click(cx.listener(|shell, _, window, cx| {
                            shell.commit_word_editor(window, cx);
                        })),
                )
                .child(
                    action_button("word-editor-cancel")
                        .debug_selector(|| "word-editor-cancel".to_owned())
                        .flex_none()
                        .small()
                        .label("Cancel")
                        .on_click(cx.listener(|shell, _, _, cx| {
                            shell.close_word_editor(cx);
                        })),
                ),
        )
        .when_some(form.error, |row, error| {
            row.child(error_line("word-editor-error", error, theme))
        })
}
