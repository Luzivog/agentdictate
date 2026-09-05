use std::time::Instant;

use gpui::{Context, Entity, IntoElement, Render, ScrollHandle, Window, prelude::*, px};
use gpui_component::{input::InputState, scroll::Scrollbar, v_flex};

use crate::sidebar_motion::SidebarFrame;
use crate::{
    HistoryViewModel, NavigationItemViewModel, ReplacementsViewModel, Route,
    SIDEBAR_OVERLAY_BREAKPOINT, SettingsDraft, ThemeTokens, TranscriptViewModel, UsageViewModel,
    WorkspaceAction, sidebar_open_for_layout,
};

use super::{
    ROUTE_SCROLLBAR_WIDTH, SIDEBAR_WIDTH, SettingsShell, gpui_color, history_page, overview,
    replacements_page,
    settings_page::{self, SettingsPageModel},
    settings_shell::{ReplacementEditorState, route_index},
    shell_chrome::{shell_title_bar, sidebar_view},
};

struct PreparedLayout {
    compact: bool,
    frame: SidebarFrame,
}

#[derive(Clone, Copy)]
struct ShellChromeModel {
    navigation: [NavigationItemViewModel; 4],
    sidebar_open: bool,
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
        search_input: Option<Entity<InputState>>,
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
                pending_destructive_action: shell.routes.pending_destructive_action.clone(),
            },
            Route::Replacements => Self::Replacements {
                replacements: workspace.replacements.clone(),
                editor: shell.routes.replacement_editor.clone(),
                feedback: shell.routes.entry(Route::Replacements).feedback.clone(),
                pending_destructive_action: shell.routes.pending_destructive_action.clone(),
            },
            Route::Settings => Self::Settings(Box::new(SettingsPageModel {
                draft: shell.settings.form.as_ref().map_or_else(
                    || SettingsDraft::from(&shell.settings.current),
                    |form| form.snapshot(cx),
                ),
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
            Self::Settings(_) => true,
            Self::Replacements { editor, .. } => editor.is_some(),
            Self::Overview { .. } | Self::History { .. } => false,
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
                pending_destructive_action,
            } => {
                history_page::surface(history, search_input, pending_destructive_action, theme, cx)
            }
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

impl SettingsShell {
    fn prepare_layout(&mut self, viewport_width: f32, now: Instant) -> PreparedLayout {
        let compact = viewport_width < SIDEBAR_OVERLAY_BREAKPOINT as f32;
        let first_layout = self.layout.compact_layout.is_none();
        if self.layout.compact_layout != Some(compact) {
            self.layout.sidebar_open = sidebar_open_for_layout(
                self.layout.sidebar_open,
                self.layout.compact_layout,
                compact,
            );
            self.layout.compact_layout = Some(compact);
        }
        let frame =
            self.layout
                .sidebar_motion
                .update(self.layout.sidebar_open, compact, first_layout, now);
        PreparedLayout { compact, frame }
    }
}

impl Render for SettingsShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_model_catalog_editor(window, cx);
        let layout = self.prepare_layout(f32::from(window.viewport_size().width), Instant::now());
        if layout.frame.active {
            window.request_animation_frame();
        }
        let route = self.model.active_route;
        let chrome = ShellChromeModel {
            navigation: self.model.navigation,
            sidebar_open: self.layout.sidebar_open,
            theme: self.theme,
        };
        let viewport = RouteViewportModel {
            page: RoutePageModel::from_shell(self, cx),
            feedback: self.routes.entry(route).feedback.clone(),
            overlay_unavailable: self.model.workspace.overlay_unavailable,
            scroll: self.routes.entry(route).scroll.clone(),
        };

        shell_root(chrome.theme)
            .when(!layout.compact, |root| {
                root.child(wide_sidebar_rail(chrome, layout.frame, cx))
            })
            .child(main_panel(viewport, chrome, window, cx))
            .when(compact_sidebar_is_visible(chrome, &layout), |root| {
                root.children(compact_sidebar_layers(chrome, layout.frame, cx))
            })
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

fn wide_sidebar_rail(
    chrome: ShellChromeModel,
    frame: SidebarFrame,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    gpui::div()
        .debug_selector(|| "sidebar-rail".to_owned())
        .h_full()
        .w(px(SIDEBAR_WIDTH * frame.panel))
        .flex_shrink_0()
        .overflow_hidden()
        .when(frame.panel > 0.0, |rail| {
            rail.child(
                gpui::div()
                    .relative()
                    .left(px(-SIDEBAR_WIDTH * (1.0 - frame.panel)))
                    .w(px(SIDEBAR_WIDTH))
                    .h_full()
                    .child(sidebar_view(chrome.navigation, false, chrome.theme, cx)),
            )
        })
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
        .child(shell_title_bar(
            route,
            chrome.sidebar_open,
            window,
            chrome.theme,
            cx,
        ))
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
                        content.child(
                            gpui::div()
                                .debug_selector(|| "workspace-feedback".to_owned())
                                .rounded_lg()
                                .border_1()
                                .border_color(gpui_color(theme.border))
                                .p_3()
                                .text_xs()
                                .text_color(gpui_color(theme.text_muted))
                                .child(feedback),
                        )
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

fn compact_sidebar_is_visible(chrome: ShellChromeModel, layout: &PreparedLayout) -> bool {
    layout.compact && (chrome.sidebar_open || layout.frame.panel > 0.0 || layout.frame.scrim > 0.0)
}

fn compact_sidebar_layers(
    chrome: ShellChromeModel,
    frame: SidebarFrame,
    cx: &mut Context<SettingsShell>,
) -> [gpui::AnyElement; 2] {
    let dismiss = gpui::div()
        .id("sidebar-dismiss")
        .debug_selector(|| "sidebar-dismiss".to_owned())
        .absolute()
        .top_0()
        .right_0()
        .bottom_0()
        .left_0()
        .cursor_pointer()
        .occlude()
        .bg(gpui::hsla(0.0, 0.0, 0.0, 0.45 * frame.scrim))
        .when(chrome.sidebar_open, |dismiss| {
            dismiss.on_click(cx.listener(|shell, _, _, cx| {
                shell.layout.sidebar_open = false;
                cx.notify();
            }))
        });
    let panel = gpui::div()
        .debug_selector(|| "sidebar-overlay-panel".to_owned())
        .absolute()
        .top_0()
        .bottom_0()
        .left(px(-SIDEBAR_WIDTH * (1.0 - frame.panel)))
        .w(px(SIDEBAR_WIDTH))
        .occlude()
        .shadow_xl()
        .child(sidebar_view(chrome.navigation, true, chrome.theme, cx));
    [dismiss.into_any_element(), panel.into_any_element()]
}
