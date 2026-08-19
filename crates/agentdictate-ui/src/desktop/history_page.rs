use agentdictate_core::HISTORY_CONTINUATION_PAGE_SIZE;
use gpui::{Context, Entity, SharedString, prelude::*, px};
use gpui_component::{
    Sizable, h_flex,
    input::{Input, InputState},
    v_flex,
};

use crate::action::action_button;

use crate::{
    HistoryViewModel, RecoveryItemViewModel, ThemeTokens, TranscriptViewModel, WorkspaceAction,
};

use super::{SettingsShell, gpui_color, single_line::single_line_clip};

const RECOVERY_ROW_HEIGHT: f32 = 58.0;
const TRANSCRIPT_ROW_HEIGHT: f32 = 50.0;

/// Renders recovery and transcript history as one dense, flat document.
/// Scrolling belongs to the shell's route-content container; this page never
/// introduces a competing scroll region.
pub(super) fn surface(
    history: HistoryViewModel,
    search_input: Option<Entity<InputState>>,
    pending_destructive_action: Option<WorkspaceAction>,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
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
        .when_some(search_input, |page, input| {
            page.child(
                h_flex()
                    .debug_selector(|| "history-search-row".to_owned())
                    .w_full()
                    .min_w_0()
                    .child(
                        gpui::div()
                            .debug_selector(|| "history-search-input".to_owned())
                            .w_full()
                            .min_w_0()
                            .child(Input::new(&input).small().w_full()),
                    ),
            )
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
                .children(
                    transcripts
                        .into_iter()
                        .map(|transcript| transcript_row(transcript, theme, cx)),
                )
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
    let delete_selector = delete_action.selector();
    let delete_pending = pending_destructive_action == Some(&delete_action);
    let delete_button_selector = if delete_pending {
        format!("confirm-{delete_selector}")
    } else {
        delete_selector
    };
    let action_label = item.primary_action_label();
    let metadata = format!("{} · {}", item.captured_at, item.duration);
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
                .child(
                    action_button(SharedString::from(delete_button_selector.clone()))
                        .debug_selector(move || delete_button_selector)
                        .small()
                        .label(if delete_pending { "Confirm" } else { "Delete" })
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            shell.request_destructive_action(delete_action.clone(), cx);
                            cx.notify();
                        })),
                ),
        )
}

fn transcript_row(
    transcript: TranscriptViewModel,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let row_selector = format!("history-transcript-item-{}", transcript.id);
    let action = WorkspaceAction::CopyTranscript { id: transcript.id };
    let action_selector = action.selector();
    let metadata = format!(
        "{} · {} words · {}",
        transcript.created_at, transcript.word_count, transcript.duration
    );
    let title_selector = format!("history-transcript-title-{}", transcript.id);
    let metadata_selector = format!("history-transcript-metadata-{}", transcript.id);

    h_flex()
        .debug_selector(move || row_selector)
        .w_full()
        .h(px(TRANSCRIPT_ROW_HEIGHT))
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
                .child(single_line_clip(title_selector, transcript.preview()).text_sm())
                .child(
                    single_line_clip(metadata_selector, metadata)
                        .text_xs()
                        .text_color(gpui_color(theme.text_muted)),
                ),
        )
        .child(
            action_button(SharedString::from(action_selector.clone()))
                .debug_selector(move || action_selector)
                .small()
                .label("Copy")
                .on_click(cx.listener(move |shell, _, _, cx| {
                    shell.emit_workspace_action(action.clone(), cx);
                    cx.notify();
                })),
        )
}
