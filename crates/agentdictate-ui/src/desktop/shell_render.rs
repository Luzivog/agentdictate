use gpui::{Context, Entity, IntoElement, Render, ScrollHandle, Window, prelude::*, px};
use gpui_component::{input::InputState, scroll::Scrollbar, v_flex};

use crate::{
    HistoryViewModel, NavigationItemViewModel, ReplacementsViewModel, Route, ThemeTokens,
    TranscriptViewModel, UsageViewModel, WorkspaceAction,
};

use super::{
    ROUTE_SCROLLBAR_WIDTH, SettingsShell, gpui_color, history_page, overview, replacements_page,
    settings_page::{self, SettingsPageModel},
    settings_shell::{ReplacementEditorState, route_index},
    shell_chrome::{shell_title_bar, sidebar_view},
};

#[derive(Clone, Copy)]
struct ShellChromeModel {
    navigation: [NavigationItemViewModel; 4],
    theme: ThemeTokens,
}

struct RouteViewportModel {
    page: RoutePageModel,
    feedback: Option<String>,
    overlay_unavailable: bool,
    scroll: ScrollHandle,
}

enum RoutePageModel {
    Overview {
        usage: UsageViewModel,
        history: HistoryViewModel,
        recent_transcripts: Vec<TranscriptViewModel>,
        recent_expanded: bool,
    },
    History {
        history: HistoryViewModel,
        search_input: Entity<InputState>,
        feedback: Option<String>,
        pending_destructive_action: Option<WorkspaceAction>,
    },
    Replacements {
        replacements: ReplacementsViewModel,
        editor: Option<ReplacementEditorState>,
        feedback: Option<String>,
        pending_destructive_action: Option<WorkspaceAction>,
    },
    Settings(Box<SettingsPageModel>),
}

impl RoutePageModel {
    fn from_shell(shell: &SettingsShell, cx: &Context<SettingsShell>) -> Self {
        let workspace = &shell.model.workspace;
        match shell.model.active_route {
            Route::Overview => Self::Overview {
                usage: workspace.usage.clone(),
                history: workspace.history.clone(),
                recent_transcripts: workspace.recent_transcripts.clone(),
                recent_expanded: shell.routes.overview_recent_expanded,
            },
            Route::History => Self::History {
                history: workspace.history.clone(),
                search_input: shell.routes.history_search_input.clone(),
                feedback: shell.routes.entry(Route::History).feedback.clone(),
                pending_destructive_action: shell.routes.pending_destructive_action.clone(),
            },
            Route::Replacements => Self::Replacements {
                replacements: workspace.replacements.clone(),
                editor: shell.routes.replacement_editor.clone(),
                feedback: shell.routes.entry(Route::Replacements).feedback.clone(),
                pending_destructive_action: shell.routes.pending_destructive_action.clone(),
            },
            Route::Settings => Self::Settings(Box::new(SettingsPageModel {
                draft: shell.settings.form.snapshot(cx),
                model_catalog: workspace.model_catalog.clone(),
                settings_dirty: shell.settings.dirty,
                has_api_key: shell.settings_commands.has_api_key,
                api_key_input: shell.settings_commands.api_key_input.clone(),
                api_key_feedback: shell.settings_commands.api_key_feedback.clone(),
                feedback: shell.routes.entry(Route::Settings).feedback.clone(),
                settings_form: shell.settings.form.clone(),
                shortcut_capture_active: shell.settings.shortcut_capture_active,
                shortcut_capture_error: shell.settings.shortcut_capture_error.clone(),
            })),
        }
    }

    const fn route(&self) -> Route {
        match self {
            Self::Overview { .. } => Route::Overview,
            Self::History { .. } => Route::History,
            Self::Replacements { .. } => Route::Replacements,
            Self::Settings(_) => Route::Settings,
        }
    }

    fn embeds_feedback(&self) -> bool {
        match self {
            Self::Settings(_) | Self::History { .. } => true,
            Self::Replacements { editor, .. } => editor.is_some(),
            Self::Overview { .. } => false,
        }
    }

    fn surface(self, theme: ThemeTokens, cx: &mut Context<SettingsShell>) -> gpui::Div {
        match self {
            Self::Overview {
                usage,
                history,
                recent_transcripts,
                recent_expanded,
            } => overview::surface(
                usage,
                history,
                recent_transcripts,
                recent_expanded,
                theme,
                cx,
            ),
            Self::History {
                history,
                search_input,
                feedback,
                pending_destructive_action,
            } => history_page::surface(
                history,
                search_input,
                feedback,
                pending_destructive_action,
                theme,
                cx,
            ),
            Self::Replacements {
                replacements,
                editor,
                feedback,
                pending_destructive_action,
            } => replacements_page::surface(
                replacements,
                editor,
                feedback,
                pending_destructive_action,
                theme,
                cx,
            ),
            Self::Settings(settings) => settings_page::surface(*settings, theme, cx),
        }
    }
}

impl Render for SettingsShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_model_catalog_editor(window, cx);
        let route = self.model.active_route;
        let chrome = ShellChromeModel {
            navigation: self.model.navigation,
            theme: self.theme,
        };
        let viewport = RouteViewportModel {
            page: RoutePageModel::from_shell(self, cx),
            feedback: self.routes.entry(route).feedback.clone(),
            overlay_unavailable: self.model.workspace.overlay_unavailable,
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
    let footer = match &viewport.page {
        RoutePageModel::Settings(settings) => settings_page::footer(settings, chrome.theme, cx),
        _ => None,
    };
    v_flex()
        .h_full()
        .min_w_0()
        .flex_1()
        .child(shell_title_bar(route, window, chrome.theme))
        .child(route_viewport(viewport, chrome.theme, cx))
        .when_some(footer, |panel, footer| panel.child(footer))
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
                .when(overlay_unavailable, |content| {
                    content.child(gpui::div()
                        .debug_selector(|| "overlay-unavailable-notice".to_owned())
                        .rounded_lg().border_1().border_color(gpui_color(theme.border))
                        .p_3().text_sm()
                        .child("Recording overlay unavailable. Dictation and saved audio remain available. Try another recording to reconnect the overlay."))
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
