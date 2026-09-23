use gpui::{Context, IntoElement, Render, ScrollHandle, Window, prelude::*, px};
use gpui_component::{scroll::Scrollbar, v_flex};

use crate::{
    HistoryViewModel, NavigationItemViewModel, Route, ThemeTokens, TranscriptViewModel,
    UsageViewModel, word_rows,
};

use super::{
    ROUTE_SCROLLBAR_WIDTH, SettingsShell, gpui_color,
    history_page::{self, HistoryPageModel},
    overview,
    settings_page::{self, SettingsPageModel},
    settings_shell::{Confirmed, route_index},
    shell_chrome::{shell_title_bar, sidebar_view},
    words_page::{self, WordsPageModel},
};

#[derive(Clone, Copy)]
struct ShellChromeModel {
    navigation: [NavigationItemViewModel; Route::ALL.len()],
    theme: ThemeTokens,
}

struct RouteViewportModel {
    page: RoutePageModel,
    feedback: Option<String>,
    overlay_unavailable: bool,
    history_set_aside: Option<String>,
    window_outdated: bool,
    scroll: ScrollHandle,
}

enum RoutePageModel {
    Home {
        usage: UsageViewModel,
        history: HistoryViewModel,
        recent_transcripts: Vec<TranscriptViewModel>,
        recent_expanded: bool,
        copied_transcript: Option<i64>,
    },
    History(HistoryPageModel),
    Words(WordsPageModel),
    Settings(Box<SettingsPageModel>),
}

impl RoutePageModel {
    fn from_shell(shell: &SettingsShell, cx: &Context<SettingsShell>) -> Self {
        let workspace = &shell.model.workspace;
        match shell.model.active_route {
            Route::Home => Self::Home {
                usage: workspace.usage.clone(),
                history: workspace.history.clone(),
                recent_transcripts: workspace.recent_transcripts.clone(),
                recent_expanded: shell.routes.overview_recent_expanded,
                copied_transcript: shell.copied_transcript(),
            },
            Route::History => Self::History(HistoryPageModel {
                history: workspace.history.clone(),
                search_input: shell.routes.history_search_input.clone(),
                feedback: shell.routes.entry(Route::History).feedback.clone(),
                pending_destructive_action: shell.routes.pending_destructive_action.clone(),
                expanded_transcripts: shell.routes.expanded_transcripts.clone(),
                copied_transcript: shell.copied_transcript(),
                fix_word: shell
                    .routes
                    .fix_word
                    .as_ref()
                    .map(|editor| editor.form.clone()),
                added_to_words: match shell.confirmed() {
                    Some(Confirmed::AddedToWords(id)) => Some(id),
                    _ => None,
                },
            }),
            Route::Words => {
                let words = &shell.routes.words;
                let shown = shell.settings.shown();
                let vocabulary = &shown.vocabulary;
                Self::Words(WordsPageModel {
                    rows: word_rows(vocabulary, &words.filter.read(cx).value()),
                    has_words: !vocabulary.is_empty(),
                    filter: words.filter.clone(),
                    new_spelling: words.new_spelling.clone(),
                    new_sounds_like: words.new_sounds_like.clone(),
                    editor: words.editor.as_ref().map(|editor| editor.form.clone()),
                    error: words.error.clone(),
                    saved: shell.confirmed() == Some(Confirmed::WordsSaved),
                })
            }
            Route::Settings => {
                let form = &shell.settings;
                Self::Settings(Box::new(SettingsPageModel {
                    settings: form.shown().into_owned(),
                    has_api_key: form.saved.has_api_key,
                    replacing_api_key: form.replacing_api_key,
                    controls: form.controls.clone(),
                    advanced_open: form.advanced_open,
                    shortcut_capture_active: form.shortcut_capture.is_listening(),
                    shortcut_capture_error: form.shortcut_capture.failure(),
                    shorter_retention: form.shorter_retention,
                    error: form.error.clone(),
                    saved_row: match shell.confirmed() {
                        Some(Confirmed::SettingSaved(row)) => Some(row),
                        _ => None,
                    },
                    feedback: shell.routes.entry(Route::Settings).feedback.clone(),
                    pending_destructive_action: shell.routes.pending_destructive_action.clone(),
                }))
            }
        }
    }

    const fn route(&self) -> Route {
        match self {
            Self::Home { .. } => Route::Home,
            Self::History(_) => Route::History,
            Self::Words(_) => Route::Words,
            Self::Settings(_) => Route::Settings,
        }
    }

    fn embeds_feedback(&self) -> bool {
        match self {
            Self::Settings(_) | Self::History(_) | Self::Words(_) => true,
            Self::Home { .. } => false,
        }
    }

    fn surface(self, theme: ThemeTokens, cx: &mut Context<SettingsShell>) -> gpui::Div {
        match self {
            Self::Home {
                usage,
                history,
                recent_transcripts,
                recent_expanded,
                copied_transcript,
            } => overview::surface(
                usage,
                history,
                recent_transcripts,
                recent_expanded,
                copied_transcript,
                theme,
                cx,
            ),
            Self::History(history) => history_page::surface(history, theme, cx),
            Self::Words(words) => words_page::surface(words, theme, cx),
            Self::Settings(settings) => settings_page::surface(*settings, theme, cx),
        }
    }
}

impl Render for SettingsShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let route = self.model.active_route;
        let chrome = ShellChromeModel {
            navigation: self.model.navigation,
            theme: self.theme,
        };
        let viewport = RouteViewportModel {
            page: RoutePageModel::from_shell(self, cx),
            feedback: self.routes.entry(route).feedback.clone(),
            overlay_unavailable: self.model.workspace.overlay_unavailable,
            history_set_aside: self.model.workspace.history_set_aside.clone(),
            window_outdated: self.model.workspace.window_outdated,
            scroll: self.routes.entry(route).scroll.clone(),
        };

        shell_root(chrome.theme)
            .child(sidebar_view(chrome.navigation, chrome.theme, cx))
            .child(main_panel(viewport, chrome, window, cx))
    }
}

fn shell_root(theme: ThemeTokens) -> gpui::Div {
    gpui::div()
        .flex()
        .flex_row()
        .relative()
        .size_full()
        .min_w(px(720.))
        .min_h(px(480.))
        .bg(gpui_color(theme.canvas))
        .text_color(gpui_color(theme.text))
}

fn main_panel(
    viewport: RouteViewportModel,
    chrome: ShellChromeModel,
    window: &Window,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let route = viewport.page.route();
    v_flex()
        .h_full()
        .min_w_0()
        .flex_1()
        .child(shell_title_bar(route, window, chrome.theme))
        .child(route_viewport(viewport, chrome.theme, cx))
}

fn route_viewport(
    viewport: RouteViewportModel,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    let RouteViewportModel {
        page,
        feedback,
        overlay_unavailable,
        history_set_aside,
        window_outdated,
        scroll,
    } = viewport;
    let route = page.route();
    let route_scroll_id = route_index(route);
    let route_scroll_selector = format!("route-scroll-{}", route.slug());
    let embeds_feedback = page.embeds_feedback();
    let surface = page.surface(theme, cx);
    gpui::div()
        .debug_selector(|| "route-content".to_owned())
        .relative()
        .overflow_hidden()
        .min_h_0()
        .flex_1()
        .child(
            v_flex()
                .id(("route-scroll", route_scroll_id))
                .debug_selector(move || route_scroll_selector)
                .size_full()
                .p_6()
                .gap_5()
                .when(window_outdated, |content| {
                    content.child(window_outdated_notice(theme))
                })
                .when(overlay_unavailable, |content| {
                    content.child(gpui::div()
                        .debug_selector(|| "overlay-unavailable-notice".to_owned())
                        .rounded_lg().border_1().border_color(gpui_color(theme.border))
                        .p_3().text_sm()
                        .child("Recording overlay unavailable. Dictation and saved audio remain available. Try another recording to reconnect the overlay."))
                })
                .when_some(history_set_aside, |content, set_aside| {
                    content.child(gpui::div()
                        .debug_selector(|| "history-set-aside-notice".to_owned())
                        .rounded_lg().border_1().border_color(gpui_color(theme.border))
                        .p_3().text_sm()
                        .child(format!("AgentDictate couldn't read your history, so it started a new one. The old file was kept at {set_aside}.")))
                })
                .child(surface)
                .when(!embeds_feedback, |content| {
                    content.when_some(feedback, |content, feedback| {
                        content.child(workspace_feedback(feedback, theme))
                    })
                })
                .track_scroll(&scroll)
                .overflow_y_scroll(),
        )
        .child(
            gpui::div()
                .debug_selector(move || format!("route-scrollbar-{}", route.slug()))
                .absolute()
                .top_0()
                .right_0()
                .bottom_0()
                .w(px(ROUTE_SCROLLBAR_WIDTH))
                .child(Scrollbar::vertical(&scroll).id(("route-scrollbar", route_scroll_id))),
        )
}

/// Shown when the daemon or its database is from a newer AgentDictate than
/// this window, which then stops refreshing.
fn window_outdated_notice(theme: ThemeTokens) -> gpui::Div {
    gpui::div()
        .debug_selector(|| "window-outdated-notice".to_owned())
        .rounded_lg()
        .border_1()
        .border_color(gpui_color(theme.accent))
        .p_3()
        .text_sm()
        .font_weight(gpui::FontWeight::MEDIUM)
        .child("AgentDictate was updated — reopen this window")
}

/// The notice that reports a workspace action's outcome on its route.
pub(super) fn workspace_feedback(feedback: String, theme: ThemeTokens) -> gpui::Div {
    gpui::div()
        .debug_selector(|| "workspace-feedback".to_owned())
        .rounded_lg()
        .border_1()
        .border_color(gpui_color(theme.border))
        .p_3()
        .text_xs()
        .text_color(gpui_color(theme.text_muted))
        .child(feedback)
}
