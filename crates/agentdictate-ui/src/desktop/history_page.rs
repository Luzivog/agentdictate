use std::collections::HashSet;

use agentdictate_core::HISTORY_CONTINUATION_PAGE_SIZE;
use gpui::{Context, Entity, SharedString, prelude::*, px};
use gpui_component::{
    Sizable,
    button::Button,
    h_flex,
    input::{Input, InputState},
    v_flex,
};

use crate::action::action_button;

use crate::{
    HistoryViewModel, RecoveryItemViewModel, ThemeTokens, TranscriptViewModel, WorkspaceAction,
};

use super::{
    SettingsShell, gpui_color,
    row_actions::{copy_button, delete_button},
    shell_render::workspace_feedback,
    single_line::single_line_clip,
    words_actions::FixWordForm,
};

const RECOVERY_ROW_HEIGHT: f32 = 58.0;
const TRANSCRIPT_ROW_HEIGHT: f32 = 50.0;

pub(super) struct HistoryPageModel {
    pub(super) history: HistoryViewModel,
    pub(super) search_input: Entity<InputState>,
    pub(super) feedback: Option<String>,
    pub(super) pending_destructive_action: Option<WorkspaceAction>,
    pub(super) expanded_transcripts: HashSet<i64>,
    pub(super) copied_transcript: Option<i64>,
    pub(super) fix_word: Option<FixWordForm>,
    /// The transcript whose "Fix a word" just added to Words.
    pub(super) added_to_words: Option<i64>,
}

/// Renders recovery and transcript history as one dense, flat document.
/// Scrolling belongs to the shell's route-content container; this page never
/// introduces a competing scroll region. Action feedback sits at the top, next
/// to Recovery, rather than below a long transcript list.
pub(super) fn surface(
    model: HistoryPageModel,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let HistoryPageModel {
        history,
        search_input,
        feedback,
        pending_destructive_action,
        expanded_transcripts,
        copied_transcript,
        fix_word,
        added_to_words,
    } = model;
    let has_recoveries = history.recovery.has_items();
    let recovery_detail = history.recovery.detail.clone();
    let recovery_items = history.recovery.items;
    let transcripts = history.transcripts;
    let has_more = history.has_more;
    let search_active = !history.search.trim().is_empty();
    let transcript_detail = if search_active {
        match history.transcript_count {
            1 => "1 matching dictation".to_owned(),
            count => format!("{count} matching dictations"),
        }
    } else {
        format!("{} saved dictations", history.transcript_count)
    };

    v_flex()
        .debug_selector(|| "history-page".to_owned())
        .w_full()
        .min_w_0()
        .gap_5()
        .child(
            h_flex()
                .debug_selector(|| "history-search-row".to_owned())
                .w_full()
                .min_w_0()
                .child(
                    gpui::div()
                        .debug_selector(|| "history-search-input".to_owned())
                        .w_full()
                        .min_w_0()
                        .child(Input::new(&search_input).small().w_full()),
                ),
        )
        .when_some(feedback, |page, feedback| {
            page.child(workspace_feedback(feedback, theme))
        })
        .when(has_recoveries, |page| {
            page.child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_0()
                    .child(
                        section_header(
                            "history-recovery-section",
                            "Recovery",
                            recovery_detail,
                            theme.danger,
                            theme,
                        )
                        .child(
                            gpui::div()
                                .flex_none()
                                .text_xs()
                                .text_color(gpui_color(theme.text_muted))
                                .child("Audio stays available until resolved"),
                        ),
                    )
                    .children(recovery_items.into_iter().map(|item| {
                        recovery_row(item, pending_destructive_action.as_ref(), theme, cx)
                    })),
            )
        })
        .child(
            v_flex()
                .w_full()
                .min_w_0()
                .gap_0()
                .child(section_header(
                    "history-transcript-section",
                    "Transcripts",
                    transcript_detail,
                    theme.info,
                    theme,
                ))
                .when(transcripts.is_empty(), |section| {
                    section.child(
                        h_flex()
                            .debug_selector(|| "history-transcripts-empty".to_owned())
                            .h(px(64.))
                            .items_center()
                            .border_b_1()
                            .border_color(gpui_color(theme.border))
                            .px_1()
                            .text_sm()
                            .text_color(gpui_color(theme.text_muted))
                            .child(if search_active {
                                "No transcripts match this search."
                            } else {
                                "Completed dictations will appear here."
                            }),
                    )
                })
                .children(transcripts.into_iter().map(|transcript| {
                    let id = transcript.id;
                    let state = TranscriptRowState {
                        expanded: expanded_transcripts.contains(&id),
                        copied: copied_transcript == Some(id),
                        added_to_words: added_to_words == Some(id),
                        fix_word: fix_word.clone().filter(|form| form.transcript_id == id),
                    };
                    transcript_row(
                        transcript,
                        state,
                        pending_destructive_action.as_ref(),
                        theme,
                        cx,
                    )
                }))
                .when(has_more, |section| {
                    section.child(
                        h_flex().w_full().justify_center().pt_3().child(
                            action_button("history-load-more")
                                .debug_selector(|| "history-load-more".to_owned())
                                .small()
                                .label(format!("Show {HISTORY_CONTINUATION_PAGE_SIZE} more"))
                                .on_click(cx.listener(|shell, _, _, cx| {
                                    shell.emit_workspace_action(
                                        WorkspaceAction::LoadMoreHistory,
                                        cx,
                                    );
                                })),
                        ),
                    )
                }),
        )
}

fn section_header(
    selector: &'static str,
    title: &'static str,
    detail: String,
    accent: crate::Color,
    theme: ThemeTokens,
) -> gpui::Div {
    h_flex()
        .debug_selector(move || selector.to_owned())
        .w_full()
        .min_w_0()
        .h(px(48.))
        .justify_between()
        .border_b_1()
        .border_color(gpui_color(theme.border))
        .gap_4()
        .child(
            h_flex()
                .min_w_0()
                .gap_2()
                .child(
                    gpui::div()
                        .size_2()
                        .flex_none()
                        .rounded_full()
                        .bg(gpui_color(accent)),
                )
                .child(
                    gpui::div()
                        .flex_none()
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(title),
                )
                .child(
                    single_line_clip(format!("{selector}-detail"), detail)
                        .text_xs()
                        .text_color(gpui_color(theme.text_muted)),
                ),
        )
}

fn recovery_row(
    item: RecoveryItemViewModel,
    pending_destructive_action: Option<&WorkspaceAction>,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let row_selector = format!("history-recovery-item-{}", item.id);
    let retry_action = WorkspaceAction::RetryRecovery {
        id: item.id.clone(),
        stage: item.stage,
    };
    let retry_selector = retry_action.selector();
    let delete_action = WorkspaceAction::DeleteRecovery {
        id: item.id.clone(),
    };
    let action_label = item.primary_action_label();
    let mut metadata = format!("{} · {}", item.captured_at, item.duration);
    if let Some(expires) = &item.expires {
        metadata.push_str(" · ");
        metadata.push_str(expires);
    }
    let metadata_selector = format!("history-recovery-metadata-{}", item.id);
    let error_selector = format!("history-recovery-error-{}", item.id);
    let preview_selector = format!("history-recovery-preview-{}", item.id);

    h_flex()
        .debug_selector(move || row_selector)
        .w_full()
        .h(px(RECOVERY_ROW_HEIGHT))
        .min_w_0()
        .justify_between()
        .border_b_1()
        .border_color(gpui_color(theme.border))
        .px_1()
        .gap_4()
        .child(
            v_flex()
                .min_w_0()
                .flex_auto()
                .gap_1()
                .child(
                    h_flex()
                        .min_w_0()
                        .gap_2()
                        .child(
                            gpui::div()
                                .flex_none()
                                .text_sm()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(item.stage.label()),
                        )
                        .child(
                            single_line_clip(metadata_selector, metadata)
                                .text_xs()
                                .text_color(gpui_color(theme.text_muted)),
                        ),
                )
                .child(
                    h_flex()
                        .min_w_0()
                        .gap_2()
                        .child(
                            single_line_clip(error_selector, item.error)
                                .flex_auto()
                                .text_xs()
                                .text_color(gpui_color(theme.danger)),
                        )
                        .when_some(item.transcript_preview, |line, preview| {
                            line.child(
                                single_line_clip(preview_selector, preview)
                                    .flex_auto()
                                    .text_xs()
                                    .text_color(gpui_color(theme.text_muted)),
                            )
                        }),
                ),
        )
        .child(
            h_flex()
                .flex_none()
                .gap_1()
                .child(
                    action_button(SharedString::from(retry_selector.clone()))
                        .debug_selector(move || retry_selector)
                        .small()
                        .label(action_label)
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            shell.emit_workspace_action(retry_action.clone(), cx);
                            cx.notify();
                        })),
                )
                .child(delete_button(
                    delete_action,
                    "Delete",
                    pending_destructive_action,
                    cx,
                )),
        )
}

/// What a transcript row shows besides the transcript itself.
struct TranscriptRowState {
    expanded: bool,
    copied: bool,
    added_to_words: bool,
    /// The row's open "Fix a word" editor, shown while it is expanded.
    fix_word: Option<FixWordForm>,
}

/// One transcript. Clicking its text expands the row to the whole, wrapped
/// transcript and offers "Fix a word"; clicking again collapses it to one line.
fn transcript_row(
    transcript: TranscriptViewModel,
    state: TranscriptRowState,
    pending_destructive_action: Option<&WorkspaceAction>,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let TranscriptRowState {
        expanded,
        copied,
        added_to_words,
        fix_word,
    } = state;
    let fix_word = fix_word.filter(|_| expanded);
    let id = transcript.id;
    let row_selector = format!("history-transcript-item-{id}");
    let toggle_selector = format!("history-transcript-toggle-{id}");
    let metadata = format!(
        "{} · {} words · {}",
        transcript.created_at, transcript.word_count, transcript.duration
    );
    let metadata_selector = format!("history-transcript-metadata-{id}");
    let text = if expanded {
        gpui::div()
            .debug_selector(move || format!("history-transcript-text-{id}"))
            .w_full()
            .text_sm()
            .child(transcript.text)
    } else {
        single_line_clip(format!("history-transcript-title-{id}"), transcript.preview).text_sm()
    };
    let offers_fix_word = expanded && fix_word.is_none();

    v_flex()
        .debug_selector(move || row_selector)
        .w_full()
        .min_w_0()
        .border_b_1()
        .border_color(gpui_color(theme.border))
        .px_1()
        .child(
            h_flex()
                .w_full()
                .map(|row| {
                    if expanded {
                        row.min_h(px(TRANSCRIPT_ROW_HEIGHT)).items_start().py_2()
                    } else {
                        row.h(px(TRANSCRIPT_ROW_HEIGHT))
                    }
                })
                .min_w_0()
                .justify_between()
                .gap_4()
                .child(
                    v_flex()
                        .id(SharedString::from(toggle_selector.clone()))
                        .debug_selector(move || toggle_selector)
                        .min_w_0()
                        .flex_auto()
                        .gap_1()
                        .cursor_pointer()
                        .on_click(
                            cx.listener(move |shell, _, _, cx| shell.toggle_transcript(id, cx)),
                        )
                        .child(text)
                        .child(
                            single_line_clip(metadata_selector, metadata)
                                .text_xs()
                                .text_color(gpui_color(theme.text_muted)),
                        ),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .gap_1()
                        .when(offers_fix_word, |actions| {
                            actions.child(fix_word_button(id, added_to_words, cx))
                        })
                        .child(copy_button(id, copied, cx))
                        .child(delete_button(
                            WorkspaceAction::DeleteTranscript { id },
                            "Delete",
                            pending_destructive_action,
                            cx,
                        )),
                ),
        )
        .when_some(fix_word, |row, form| {
            row.child(fix_word_editor(form, theme, cx))
        })
}

/// Opens "Fix a word". It reads "Added to Words ✓" while `added`, and its
/// selector gains an `added-` prefix so rendered tests can see that state.
fn fix_word_button(id: i64, added: bool, cx: &mut Context<SettingsShell>) -> Button {
    let selector = format!("history-fix-word-{id}");
    let rendered_selector = if added {
        format!("added-{selector}")
    } else {
        selector.clone()
    };
    action_button(SharedString::from(selector))
        .debug_selector(move || rendered_selector)
        .small()
        .label(if added {
            "Added to Words ✓"
        } else {
            "Fix a word"
        })
        .on_click(cx.listener(move |shell, _, window, cx| {
            shell.open_fix_word(id, window, cx);
        }))
}

/// Teaches Words a correction: what AgentDictate heard, and how it should be
/// spelled. Add to Words (or Enter) saves it; Cancel closes the editor.
fn fix_word_editor(
    form: FixWordForm,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let id = form.transcript_id;
    v_flex()
        .debug_selector(move || format!("history-fix-word-editor-{id}"))
        .w_full()
        .min_w_0()
        .pb_3()
        .gap_1()
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .items_end()
                .gap_2()
                .child(labeled_input(
                    "Heard",
                    "history-fix-word-heard",
                    &form.heard,
                    theme,
                ))
                .child(labeled_input(
                    "Should be",
                    "history-fix-word-spelling",
                    &form.spelling,
                    theme,
                ))
                .child(
                    action_button("history-fix-word-save")
                        .debug_selector(|| "history-fix-word-save".to_owned())
                        .flex_none()
                        .small()
                        .label("Add to Words")
                        .on_click(cx.listener(|shell, _, window, cx| {
                            shell.save_fix_word(window, cx);
                        })),
                )
                .child(
                    action_button("history-fix-word-cancel")
                        .debug_selector(|| "history-fix-word-cancel".to_owned())
                        .flex_none()
                        .small()
                        .label("Cancel")
                        .on_click(cx.listener(|shell, _, _, cx| shell.close_fix_word(cx))),
                ),
        )
        .when_some(form.error, |editor, error| {
            editor.child(
                gpui::div()
                    .debug_selector(|| "history-fix-word-error".to_owned())
                    .text_xs()
                    .text_color(gpui_color(theme.danger))
                    .child(error),
            )
        })
}

fn labeled_input(
    label: &'static str,
    selector: &'static str,
    input: &Entity<InputState>,
    theme: ThemeTokens,
) -> gpui::Div {
    v_flex()
        .min_w_0()
        .flex_1()
        .gap_1()
        .child(
            gpui::div()
                .text_xs()
                .text_color(gpui_color(theme.text_muted))
                .child(label),
        )
        .child(
            gpui::div()
                .debug_selector(move || selector.to_owned())
                .child(Input::new(input).small()),
        )
}
