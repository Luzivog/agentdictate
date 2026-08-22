use gpui::{App, Context, SharedString, prelude::*, px, relative};
use gpui_component::{
    ActiveTheme, Selectable, Sizable, StyledExt, chart::AreaChart, h_flex, tooltip::Tooltip, v_flex,
};

use crate::action::action_button;
use crate::usage::format_currency_amount;
use crate::{
    HistoryViewModel, Route, ThemeTokens, TranscriptViewModel, UsageDayViewModel, UsagePeriod,
    UsageViewModel, WorkspaceAction,
};

use super::{SettingsShell, gpui_color, single_line::single_line_clip};

const CHART_HEIGHT: f32 = 280.0;
const AXIS_GAP: f32 = 18.0;
const CHART_TOP_GAP: f32 = 10.0;
const RECENT_HISTORY_COLLAPSED_LIMIT: usize = 10;
const RECENT_HISTORY_EXPANDED_LIMIT: usize = 30;
const RECENT_HISTORY_EXPANSION_SIZE: usize = 20;

pub(super) fn surface(
    usage: UsageViewModel,
    history: HistoryViewModel,
    recent_transcripts: Vec<TranscriptViewModel>,
    recent_history_expanded: bool,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let activity = usage.activity.clone();
    let activity_empty = activity.is_empty();
    let peak_audio_seconds = usage.peak_audio_seconds();
    let tick_margin = match usage.period {
        UsagePeriod::Last7Days => 1,
        UsagePeriod::Last30Days => 7,
        UsagePeriod::AllTime => activity.len().saturating_sub(1).div_ceil(5).max(1),
    };

    v_flex()
        .gap_5()
        .child(
            v_flex()
                .debug_selector(|| "overview-activity-card".to_owned())
                .rounded_xl()
                .border_1()
                .border_color(gpui_color(theme.border))
                .bg(gpui_color(theme.surface))
                .p_4()
                .gap_4()
                .child(activity_header(&usage, theme, cx))
                .child(
                    h_flex()
                        .flex_wrap()
                        .items_start()
                        .gap_6()
                        .child(activity_summary(&usage, theme))
                        .child(
                            gpui::div()
                                .debug_selector(|| "overview-activity-plot".to_owned())
                                .flex_1()
                                .min_w(px(340.))
                                .when(activity_empty, |plot| {
                                    plot.child(
                                        h_flex()
                                            .h(px(CHART_HEIGHT))
                                            .items_center()
                                            .justify_center()
                                            .text_sm()
                                            .text_color(gpui_color(theme.text_muted))
                                            .child(
                                                "Your activity graph will appear after your first dictation.",
                                            ),
                                    )
                                })
                                .when(!activity_empty, |plot| {
                                    plot.child(activity_plot(
                                        activity,
                                        peak_audio_seconds,
                                        usage.currency.clone(),
                                        tick_margin,
                                        theme,
                                        cx,
                                    ))
                                }),
                        ),
                ),
        )
        .when(history.recovery.has_items(), |page| {
            page.child(recovery_notice(&history, theme, cx))
        })
        .child(recent_history(
            recent_transcripts,
            recent_history_expanded,
            theme,
            cx,
        ))
}

fn activity_header(
    usage: &UsageViewModel,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    h_flex()
        .justify_between()
        .items_center()
        .gap_4()
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    gpui::div()
                        .size_2()
                        .rounded_full()
                        .bg(gpui_color(theme.accent)),
                )
                .child(
                    gpui::div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child("Dictation activity"),
                )
                .child(
                    gpui::div()
                        .text_xs()
                        .text_color(gpui_color(theme.text_muted))
                        .child(activity_scope_label(usage.period)),
                ),
        )
        .child(
            h_flex()
                .gap_1()
                .children(UsagePeriod::ALL.into_iter().map(|period| {
                    let action = WorkspaceAction::SelectUsagePeriod(period);
                    let selector = action.selector();
                    action_button(SharedString::from(selector.clone()))
                        .debug_selector(move || selector)
                        .small()
                        .selected(period == usage.period)
                        .label(period.label())
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            shell.emit_workspace_action(action.clone(), cx);
                            cx.notify();
                        }))
                })),
        )
}

fn activity_summary(usage: &UsageViewModel, theme: ThemeTokens) -> gpui::Div {
    v_flex()
        .debug_selector(|| "overview-activity-summary".to_owned())
        .w(px(300.))
        .h(px(CHART_HEIGHT))
        .flex_shrink_0()
        .child(summary_metric(
            "overview-summary-audio",
            "DICTATION TIME",
            usage.audio_value(),
            false,
            theme,
        ))
        .child(summary_metric(
            "overview-summary-dictations",
            "DICTATIONS",
            usage.dictations_value(),
            false,
            theme,
        ))
        .child(summary_metric(
            "overview-summary-words",
            "WORDS",
            usage.words_value(),
            false,
            theme,
        ))
        .child(summary_metric(
            "overview-summary-wpm",
            "AVG. WPM",
            usage.average_wpm_value(),
            false,
            theme,
        ))
        .child(summary_metric(
            "overview-summary-cost",
            "EST. API COST",
            usage.cost_value(),
            true,
            theme,
        ))
}

fn summary_metric(
    selector: &'static str,
    label: &'static str,
    value: String,
    last: bool,
    theme: ThemeTokens,
) -> gpui::Div {
    h_flex()
        .debug_selector(move || selector.to_owned())
        .w_full()
        .h(px(CHART_HEIGHT / 5.0))
        .flex_shrink_0()
        .items_center()
        .justify_between()
        .gap_4()
        .when(!last, |row| {
            row.border_b_1().border_color(gpui_color(theme.border))
        })
        .child(
            gpui::div()
                .text_xs()
                .whitespace_nowrap()
                .text_color(gpui_color(theme.text_muted))
                .child(label),
        )
        .child(
            gpui::div()
                .min_w_0()
                .flex_1()
                .text_right()
                .whitespace_nowrap()
                .text_lg()
                .font_semibold()
                .child(value),
        )
}

fn activity_plot(
    data: Vec<UsageDayViewModel>,
    peak_audio_seconds: u64,
    currency: String,
    tick_margin: usize,
    theme: ThemeTokens,
    cx: &App,
) -> gpui::Div {
    let maximum = peak_audio_seconds as f64;
    let point_count = data.len();
    let hover_targets = data
        .iter()
        .cloned()
        .enumerate()
        .filter(|(_, point)| point.audio_seconds > 0)
        .map(|(index, point)| {
            let (left, width, marker_x) = hover_geometry(index, point_count);
            let marker_y = marker_top(point.audio_seconds as f64, maximum);
            let group: SharedString = format!("activity-point-{index}").into();
            let currency = currency.clone();
            gpui::div()
                .group(group.clone())
                .id(("overview-activity-point", index))
                .absolute()
                .top_0()
                .left(relative(left))
                .w(relative(width))
                .h_full()
                .tooltip(move |window, cx| {
                    let point = point.clone();
                    let currency = currency.clone();
                    Tooltip::element(move |_, cx| activity_tooltip(&point, &currency, cx))
                        .p_0()
                        .build(window, cx)
                })
                .child(
                    gpui::div()
                        .absolute()
                        .debug_selector(move || format!("overview-activity-marker-{index}"))
                        .left(relative(marker_x))
                        .top(relative(marker_y))
                        .ml(px(-5.))
                        .mt(px(-5.))
                        .size(px(10.))
                        .rounded_full()
                        .border_2()
                        .border_color(cx.theme().background)
                        .bg(gpui_color(theme.accent))
                        .invisible()
                        .group_hover(group, |marker| marker.visible()),
                )
        })
        .collect::<Vec<_>>();

    gpui::div()
        .relative()
        .w_full()
        .h(px(CHART_HEIGHT))
        .min_w_0()
        .child(
            AreaChart::new(data)
                .x(|point: &UsageDayViewModel| SharedString::from(point.label.clone()))
                .y(|point: &UsageDayViewModel| point.audio_seconds as f64)
                .stroke(gpui_color(theme.accent))
                .fill(gpui_color(theme.accent).opacity(0.22))
                .linear()
                .tick_margin(tick_margin),
        )
        .child(
            gpui::div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .children(hover_targets),
        )
}

fn activity_tooltip(point: &UsageDayViewModel, currency: &str, cx: &App) -> gpui::Div {
    v_flex()
        .w(px(230.))
        .gap_2()
        .p_3()
        .child(
            gpui::div()
                .text_sm()
                .font_semibold()
                .child(point.label.clone()),
        )
        .child(tooltip_row(
            "Dictation time",
            format_duration(point.audio_seconds),
            cx,
        ))
        .child(tooltip_row("Dictations", point.dictations.to_string(), cx))
        .child(tooltip_row("Words", point.words.to_string(), cx))
        .child(tooltip_row(
            "Est. cost",
            format_currency_amount(currency, point.estimated_cost_usd),
            cx,
        ))
}

fn tooltip_row(label: &'static str, value: String, cx: &App) -> gpui::Div {
    h_flex()
        .justify_between()
        .text_sm()
        .child(
            gpui::div()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(gpui::div().font_semibold().child(value))
}

fn recovery_notice(
    history: &HistoryViewModel,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let count = history.recovery.item_count;
    h_flex()
        .debug_selector(|| "overview-recovery-notice".to_owned())
        .justify_between()
        .gap_4()
        .border_l_2()
        .border_color(gpui_color(theme.danger))
        .bg(gpui_color(theme.surface))
        .px_4()
        .py_3()
        .child(
            v_flex()
                .gap_1()
                .child(gpui::div().text_sm().font_semibold().child(format!(
                    "{count} recording{} need attention",
                    if count == 1 { "" } else { "s" }
                )))
                .child(
                    gpui::div()
                        .text_xs()
                        .text_color(gpui_color(theme.text_muted))
                        .child("The audio is saved and can be recovered from History."),
                ),
        )
        .child(
            action_button("overview-recovery-open")
                .small()
                .label("Open History")
                .on_click(cx.listener(|shell, _, _, cx| {
                    shell.model.select_route(Route::History);
                    cx.notify();
                })),
        )
}

fn recent_history(
    transcripts: Vec<TranscriptViewModel>,
    expanded: bool,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let can_expand = !expanded && transcripts.len() > RECENT_HISTORY_COLLAPSED_LIMIT;
    let visible_limit = if expanded {
        RECENT_HISTORY_EXPANDED_LIMIT
    } else {
        RECENT_HISTORY_COLLAPSED_LIMIT
    };
    let recent = transcripts
        .into_iter()
        .take(visible_limit)
        .collect::<Vec<_>>();
    v_flex()
        .debug_selector(|| "overview-recent-history".to_owned())
        .gap_2()
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .pb_2()
                .border_b_1()
                .border_color(gpui_color(theme.border))
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            gpui::div()
                                .text_sm()
                                .font_semibold()
                                .child("Recent dictations"),
                        )
                        .child(
                            gpui::div()
                                .text_xs()
                                .text_color(gpui_color(theme.text_muted))
                                .child("Your latest completed transcripts"),
                        ),
                )
                .child(
                    action_button("overview-history-view-all")
                        .debug_selector(|| "overview-history-view-all".to_owned())
                        .small()
                        .label("View all")
                        .on_click(cx.listener(|shell, _, _, cx| {
                            shell.model.select_route(Route::History);
                            cx.notify();
                        })),
                ),
        )
        .when(recent.is_empty(), |section| {
            section.child(
                gpui::div()
                    .py_5()
                    .text_sm()
                    .text_color(gpui_color(theme.text_muted))
                    .child("Completed dictations will appear here."),
            )
        })
        .children(recent.into_iter().map(|transcript| {
            let selector = format!("overview-recent-transcript-{}", transcript.id);
            let title_selector = format!("overview-recent-transcript-title-{}", transcript.id);
            let metadata_selector =
                format!("overview-recent-transcript-metadata-{}", transcript.id);
            let metadata = format!(
                "{} · {} words · {}",
                transcript.created_at, transcript.word_count, transcript.duration
            );
            let action = WorkspaceAction::CopyTranscript { id: transcript.id };
            h_flex()
                .debug_selector(move || selector)
                .h(px(48.))
                .justify_between()
                .gap_4()
                .border_b_1()
                .border_color(gpui_color(theme.border))
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
                    action_button(SharedString::from(action.selector()))
                        .debug_selector({
                            let selector = action.selector();
                            move || selector.clone()
                        })
                        .small()
                        .label("Copy")
                        .on_click(cx.listener(move |shell, _, _, cx| {
                            shell.emit_workspace_action(action.clone(), cx);
                            cx.notify();
                        })),
                )
        }))
        .when(can_expand, |section| {
            section.child(
                h_flex().justify_center().pt_3().child(
                    action_button("overview-recent-show-more")
                        .debug_selector(|| "overview-recent-show-more".to_owned())
                        .small()
                        .label(format!("Show {RECENT_HISTORY_EXPANSION_SIZE} more"))
                        .on_click(cx.listener(|shell, _, _, cx| {
                            shell.overview_recent_expanded = true;
                            cx.notify();
                        })),
                ),
            )
        })
}

fn format_duration(seconds: u64) -> String {
    format!("{}m {:02}s", seconds / 60, seconds % 60)
}

fn activity_scope_label(period: UsagePeriod) -> &'static str {
    match period {
        UsagePeriod::Last7Days => "Last 7 days",
        UsagePeriod::Last30Days => "Last 30 days",
        UsagePeriod::AllTime => "All time · grouped by week",
    }
}

fn marker_top(value: f64, maximum: f64) -> f32 {
    let plot_bottom = CHART_HEIGHT - AXIS_GAP;
    let y = if maximum > 0.0 {
        plot_bottom + (value / maximum) as f32 * (CHART_TOP_GAP - plot_bottom)
    } else {
        plot_bottom
    };
    (y / CHART_HEIGHT).clamp(0.0, 1.0)
}

fn hover_geometry(index: usize, count: usize) -> (f32, f32, f32) {
    if count <= 1 {
        return (0.0, 1.0, 0.5);
    }
    let step = 1.0 / (count - 1) as f32;
    let point = index as f32 * step;
    let left = if index == 0 { 0.0 } else { point - step / 2.0 };
    let right = if index + 1 == count {
        1.0
    } else {
        point + step / 2.0
    };
    let point_in_region = (point - left) / (right - left);
    (left, right - left, point_in_region)
}
