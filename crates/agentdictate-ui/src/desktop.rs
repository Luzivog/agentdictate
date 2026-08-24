use futures::{StreamExt, channel::mpsc};
use gpui::{
    App, Application, Bounds, Context, Entity, Hsla, IntoElement, Render, ScrollHandle,
    Subscription, Window, WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowKind,
    WindowOptions, point, prelude::*, px, rgb, size,
};
use gpui_component::{
    Root, TitleBar,
    input::{InputEvent, InputState, NumberInputEvent, StepAction},
    scroll::Scrollbar,
    select::{SearchableVec, SelectEvent},
    v_flex,
};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, mpsc::Receiver},
    time::Instant,
};

use crate::{
    Color, ModelCatalogViewModel, OverlayPresentation, ReplacementDraft, Route, ShellViewModel,
    ThemeTokens, WorkspaceAction, WorkspaceActionSink, WorkspaceViewModel,
};
use crate::{sidebar_motion::SidebarMotion, sidebar_open_for_layout};

mod history_action_lane;
mod history_page;
mod overlay_view;
mod overview;
mod replacements_page;
mod settings_controller;
mod settings_page;
mod shell_chrome;
pub(crate) mod single_line;

use settings_controller::{SettingOption, SettingsEditorState};

use shell_chrome::{shell_title_bar, sidebar_view};

pub use overlay_view::RecordingOverlay;

const SIDEBAR_WIDTH: f32 = 250.0;
const SIDEBAR_OVERLAY_BREAKPOINT: f32 = 1_100.0;
const ROUTE_SCROLLBAR_WIDTH: f32 = 16.0;
pub const APPLICATION_ID: &str = "local.agentdictate.AgentDictate";

/// Starts the native GPUI settings window from a daemon snapshot.
pub type CommandSink =
    Arc<dyn Fn(agentdictate_core::ClientCommand) -> Result<(), String> + Send + Sync>;

pub fn run_settings_shell(
    model: ShellViewModel,
    settings: agentdictate_core::Settings,
    has_api_key: bool,
    command_sink: CommandSink,
) {
    run_settings_shell_internal(model, settings, has_api_key, command_sink, None, None);
}

/// Starts the settings window with connected workspace actions.
pub fn run_settings_shell_with_workspace_actions(
    model: ShellViewModel,
    settings: agentdictate_core::Settings,
    has_api_key: bool,
    command_sink: CommandSink,
    action_sink: WorkspaceActionSink,
) {
    run_settings_shell_internal(
        model,
        settings,
        has_api_key,
        command_sink,
        Some(action_sink),
        None,
    );
}

/// Starts the settings window with actions and event-driven daemon workspace
/// updates. Existing entrypoints remain available for callers that provide a
/// fixed startup snapshot.
pub fn run_settings_shell_with_workspace_actions_and_updates(
    model: ShellViewModel,
    settings: agentdictate_core::Settings,
    has_api_key: bool,
    command_sink: CommandSink,
    action_sink: WorkspaceActionSink,
    updates: Receiver<WorkspaceViewModel>,
) {
    run_settings_shell_internal(
        model,
        settings,
        has_api_key,
        command_sink,
        Some(action_sink),
        Some(updates),
    );
}

fn run_settings_shell_internal(
    model: ShellViewModel,
    settings: agentdictate_core::Settings,
    has_api_key: bool,
    command_sink: CommandSink,
    action_sink: Option<WorkspaceActionSink>,
    workspace_updates: Option<Receiver<WorkspaceViewModel>>,
) {
    Application::new()
        .with_assets(crate::AgentDictateAssets)
        .run(move |cx: &mut App| {
            crate::theme::initialize_gpui_theme(cx);
            let bounds = Bounds::centered(None, size(px(1180.), px(760.)), cx);
            let shell_slot = Rc::new(RefCell::new(None));
            let window_shell_slot = Rc::clone(&shell_slot);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitleBar::title_bar_options()),
                    window_background: WindowBackgroundAppearance::Opaque,
                    window_decorations: Some(WindowDecorations::Client),
                    app_id: Some(APPLICATION_ID.to_owned()),
                    window_min_size: Some(size(px(720.), px(480.))),
                    ..Default::default()
                },
                move |window, cx| {
                    let view = cx.new(|cx| {
                        SettingsShell::connected_internal(
                            model,
                            settings,
                            has_api_key,
                            command_sink,
                            action_sink,
                            window,
                            cx,
                        )
                    });
                    *window_shell_slot.borrow_mut() = Some(view.clone());
                    let frame = cx.new(|_| crate::AgentDictateWindowFrame::new(view));
                    cx.new(|cx| Root::new(frame, window, cx))
                },
            )
            .expect("AgentDictate settings window should open");
            if let Some(workspace_updates) = workspace_updates {
                let shell = shell_slot
                    .borrow_mut()
                    .take()
                    .expect("settings shell should exist after its window opens")
                    .downgrade();
                let (sender, mut receiver) = mpsc::unbounded();
                std::thread::Builder::new()
                    .name("agentdictate-workspace-updates".into())
                    .spawn(move || {
                        while let Ok(workspace) = workspace_updates.recv() {
                            if sender.unbounded_send(workspace).is_err() {
                                return;
                            }
                        }
                    })
                    .expect("workspace update bridge should start");
                cx.spawn(async move |cx| {
                    while let Some(workspace) = receiver.next().await {
                        if shell
                            .update(cx, |shell, cx| {
                                shell.apply_workspace_update(workspace, cx);
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                })
                .detach();
            }
            cx.activate(true);
        });
}

/// Runs one transient status-overlay session. The headless daemon launches this
/// in a helper process only after a visible workflow state exists.
pub fn run_recording_overlay(
    initial: OverlayPresentation,
    snapshots: Receiver<OverlayPresentation>,
    work_area: Option<crate::LogicalRect>,
) {
    run_recording_overlay_with_ready(initial, snapshots, work_area, || {});
}

/// Runs an overlay session and reports when GPUI has created its platform
/// window. The daemon helper uses this to distinguish spawn from readiness.
#[doc(hidden)]
pub fn run_recording_overlay_with_ready(
    initial: OverlayPresentation,
    snapshots: Receiver<OverlayPresentation>,
    work_area: Option<crate::LogicalRect>,
    on_ready: impl FnOnce() + 'static,
) {
    Application::new()
        .with_assets(crate::AgentDictateAssets)
        .run(move |cx: &mut App| {
            crate::theme::initialize_gpui_theme(cx);
            let display = cx.primary_display();
            let display_id = display.as_ref().map(|display| display.id());
            let primary_bounds = display.as_ref().map(|display| {
                let bounds = display.bounds();
                crate::LogicalRect::new(
                    f32::from(bounds.origin.x).round() as i32,
                    f32::from(bounds.origin.y).round() as i32,
                    f32::from(bounds.size.width).max(0.0).round() as u32,
                    f32::from(bounds.size.height).max(0.0).round() as u32,
                )
            });
            let available = match (work_area, primary_bounds) {
                (Some(work_area), Some(primary_bounds)) => {
                    crate::intersect_logical_rects(work_area, primary_bounds)
                        .unwrap_or(primary_bounds)
                }
                (Some(work_area), None) => work_area,
                (None, Some(primary_bounds)) => primary_bounds,
                (None, None) => crate::LogicalRect::new(0, 0, 0, 0),
            };
            let (sender, mut receiver) = mpsc::unbounded();
            std::thread::Builder::new()
                .name("agentdictate-overlay-events".into())
                .spawn(move || {
                    while let Ok(snapshot) = snapshots.recv() {
                        if sender.unbounded_send(snapshot).is_err() {
                            return;
                        }
                    }
                })
                .expect("overlay event bridge should start");

            let initial_state = initial.state();
            if !initial_state.is_visible() {
                cx.quit();
                return;
            }
            let placement = crate::OverlayPlacement::bottom_centered(
                available,
                crate::LogicalSize::new(crate::OVERLAY_WIDTH, crate::OVERLAY_HEIGHT),
                crate::OVERLAY_BOTTOM_GAP,
            );
            let frame = placement.frame;
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(frame.x as f32), px(frame.y as f32)),
                    size(px(frame.width as f32), px(frame.height as f32)),
                ))),
                titlebar: None,
                focus: false,
                show: true,
                kind: WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                display_id,
                window_background: WindowBackgroundAppearance::Transparent,
                // PopUp maps to _NET_WM_WINDOW_TYPE_NOTIFICATION on X11. Sharing
                // the main application id also prevents a second app identity.
                app_id: Some(APPLICATION_ID.to_owned()),
                window_min_size: None,
                window_decorations: None,
                tabbing_identifier: None,
            };
            let overlay_window = cx
                .open_window(options, move |_, cx| {
                    cx.new(|_| RecordingOverlay::from_presentation(initial))
                })
                .expect("recording overlay should open");
            on_ready();

            cx.spawn(async move |cx| {
                while let Some(presentation) = receiver.next().await {
                    let state = presentation.state();
                    if !state.is_visible() {
                        let _ = overlay_window.update(cx, |_, window, _| window.remove_window());
                        return cx.update(|cx| cx.quit());
                    }
                    let _ = overlay_window.update(cx, |overlay, _, cx| {
                        overlay.set_presentation(presentation);
                        cx.notify();
                    });
                }
                let _ = overlay_window.update(cx, |_, window, _| window.remove_window());
                cx.update(|cx| cx.quit())
            })
            .detach();
        });
}

/// GPUI settings shell built from the toolkit-independent presentation model.
///
/// The shell owns navigation interactions while individual routes remain free
/// to supply their own content components as the migration proceeds.
pub struct SettingsShell {
    model: ShellViewModel,
    theme: ThemeTokens,
    settings: agentdictate_core::Settings,
    settings_baseline: agentdictate_core::Settings,
    has_api_key: bool,
    api_key_input: Option<Entity<InputState>>,
    history_search_input: Option<Entity<InputState>>,
    api_key_feedback: Option<String>,
    command_sink: Option<CommandSink>,
    workspace_action_sink: Option<WorkspaceActionSink>,
    next_request_id: u64,
    route_feedbacks: RouteFeedbacks,
    settings_editor: Option<SettingsEditorState>,
    applied_model_catalog: ModelCatalogViewModel,
    settings_dirty: bool,
    shortcut_capture_active: bool,
    shortcut_capture_error: Option<String>,
    _settings_input_subscriptions: Vec<Subscription>,
    replacement_editor: Option<ReplacementEditorState>,
    pending_destructive_action: Option<WorkspaceAction>,
    workspace_action_in_flight: bool,
    history_action_lane: history_action_lane::HistoryActionLane,
    overview_recent_expanded: bool,
    sidebar_open: bool,
    compact_layout: Option<bool>,
    sidebar_motion: SidebarMotion,
    route_scroll_handles: RouteScrollHandles,
}

#[derive(Clone, Debug, Default)]
struct RouteScrollHandles {
    overview: ScrollHandle,
    history: ScrollHandle,
    replacements: ScrollHandle,
    settings: ScrollHandle,
}

#[derive(Clone, Debug, Default)]
struct RouteFeedbacks {
    overview: Option<String>,
    history: Option<String>,
    replacements: Option<String>,
    settings: Option<String>,
}

impl RouteFeedbacks {
    fn for_route(&self, route: Route) -> &Option<String> {
        match route {
            Route::Overview => &self.overview,
            Route::History => &self.history,
            Route::Replacements => &self.replacements,
            Route::Settings => &self.settings,
        }
    }

    fn for_route_mut(&mut self, route: Route) -> &mut Option<String> {
        match route {
            Route::Overview => &mut self.overview,
            Route::History => &mut self.history,
            Route::Replacements => &mut self.replacements,
            Route::Settings => &mut self.settings,
        }
    }
}

impl RouteScrollHandles {
    fn for_route(&self, route: Route) -> ScrollHandle {
        match route {
            Route::Overview => self.overview.clone(),
            Route::History => self.history.clone(),
            Route::Replacements => self.replacements.clone(),
            Route::Settings => self.settings.clone(),
        }
    }
}

#[derive(Clone)]
struct ReplacementEditorState {
    id: Option<i64>,
    source: Entity<InputState>,
    replacement: Entity<InputState>,
    enabled: bool,
    case_sensitive: bool,
    whole_word_only: bool,
}

impl SettingsShell {
    pub fn new(model: ShellViewModel) -> Self {
        let applied_model_catalog = model.workspace.model_catalog.clone();
        Self {
            model,
            theme: ThemeTokens::default(),
            settings: agentdictate_core::Settings::default(),
            settings_baseline: agentdictate_core::Settings::default(),
            has_api_key: false,
            api_key_input: None,
            history_search_input: None,
            api_key_feedback: None,
            command_sink: None,
            workspace_action_sink: None,
            next_request_id: 1,
            route_feedbacks: RouteFeedbacks::default(),
            settings_editor: None,
            applied_model_catalog,
            settings_dirty: false,
            shortcut_capture_active: false,
            shortcut_capture_error: None,
            _settings_input_subscriptions: Vec::new(),
            replacement_editor: None,
            pending_destructive_action: None,
            workspace_action_in_flight: false,
            history_action_lane: Default::default(),
            overview_recent_expanded: false,
            sidebar_open: true,
            compact_layout: None,
            sidebar_motion: SidebarMotion::new(),
            route_scroll_handles: RouteScrollHandles::default(),
        }
    }

    pub fn connected(
        model: ShellViewModel,
        settings: agentdictate_core::Settings,
        has_api_key: bool,
        command_sink: CommandSink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::connected_internal(model, settings, has_api_key, command_sink, None, window, cx)
    }

    pub fn connected_with_workspace_actions(
        model: ShellViewModel,
        settings: agentdictate_core::Settings,
        has_api_key: bool,
        command_sink: CommandSink,
        workspace_action_sink: WorkspaceActionSink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::connected_internal(
            model,
            settings,
            has_api_key,
            command_sink,
            Some(workspace_action_sink),
            window,
            cx,
        )
    }

    fn connected_internal(
        mut model: ShellViewModel,
        settings: agentdictate_core::Settings,
        has_api_key: bool,
        command_sink: CommandSink,
        workspace_action_sink: Option<WorkspaceActionSink>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        model.workspace = workspace_with_currency(model.workspace, &settings.currency);
        let api_key_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("sk-…").masked(true));
        let initial_history_search = model.workspace.history.search.clone();
        let history_search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search every transcript")
                .default_value(initial_history_search)
        });
        let applied_model_catalog = model.workspace.model_catalog.clone();
        let settings_editor =
            SettingsEditorState::new(&settings, &applied_model_catalog, window, cx);
        let mut settings_input_subscriptions: Vec<Subscription> = settings_editor
            .inputs()
            .into_iter()
            .map(|input| {
                cx.subscribe(&input, |shell, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        shell.recompute_settings_dirty(cx);
                        cx.notify();
                    }
                })
            })
            .collect();
        settings_input_subscriptions.extend(settings_editor.selects().into_iter().map(|select| {
            cx.subscribe(
                &select,
                |shell, _, event: &SelectEvent<SearchableVec<SettingOption>>, cx| {
                    if matches!(event, SelectEvent::Confirm(Some(_))) {
                        shell.recompute_settings_dirty(cx);
                        shell.clear_route_feedback();
                        cx.notify();
                    }
                },
            )
        }));
        settings_input_subscriptions.push(cx.subscribe_in(
            &settings_editor.cleanup_model,
            window,
            |shell, _, event: &SelectEvent<SearchableVec<SettingOption>>, window, cx| {
                if matches!(event, SelectEvent::Confirm(Some(_))) {
                    shell.cleanup_model_selection_changed(window, cx);
                }
            },
        ));
        for (input, maximum) in [
            (
                settings_editor.max_recording_seconds.clone(),
                u64::from(u32::MAX),
            ),
            (settings_editor.audio_ducking_volume_percent.clone(), 100),
        ] {
            settings_input_subscriptions.push(cx.subscribe_in(
                &input,
                window,
                move |_, input, event: &NumberInputEvent, window, cx| {
                    let NumberInputEvent::Step(step) = event;
                    input.update(cx, |input, cx| {
                        let current = input.value().parse::<u64>().unwrap_or_default();
                        let next = match step {
                            StepAction::Increment => current.saturating_add(1).min(maximum),
                            StepAction::Decrement => current.saturating_sub(1),
                        };
                        input.set_value(next.to_string(), window, cx);
                    });
                },
            ));
        }
        settings_input_subscriptions.push(cx.subscribe(
            &api_key_input,
            |shell, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    shell.api_key_feedback = None;
                    cx.notify();
                }
            },
        ));
        settings_input_subscriptions.push(cx.subscribe(
            &history_search_input,
            |shell, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let query = input.read(cx).value().to_string();
                    shell.submit_history_search(query, cx);
                }
            },
        ));
        settings_input_subscriptions.push(cx.observe_keystrokes(|shell, event, _window, cx| {
            if shell.shortcut_capture_active {
                shell.capture_shortcut(&event.keystroke, cx);
                cx.stop_propagation();
            }
        }));
        let mut next_request_id = 1;
        let mut route_feedbacks = RouteFeedbacks::default();
        if has_api_key && model.active_route == Route::Settings {
            if let Err(error) = command_sink(
                agentdictate_core::ClientCommand::refresh_model_catalog(next_request_id),
            ) {
                route_feedbacks.settings = Some(format!("Could not refresh models: {error}"));
            }
            next_request_id += 1;
        }
        Self {
            model,
            theme: ThemeTokens::default(),
            settings: settings.clone(),
            settings_baseline: settings,
            has_api_key,
            api_key_input: Some(api_key_input),
            history_search_input: Some(history_search_input),
            api_key_feedback: None,
            command_sink: Some(command_sink),
            workspace_action_sink,
            next_request_id,
            route_feedbacks,
            settings_editor: Some(settings_editor),
            applied_model_catalog,
            settings_dirty: false,
            shortcut_capture_active: false,
            shortcut_capture_error: None,
            _settings_input_subscriptions: settings_input_subscriptions,
            replacement_editor: None,
            pending_destructive_action: None,
            workspace_action_in_flight: false,
            history_action_lane: Default::default(),
            overview_recent_expanded: false,
            sidebar_open: true,
            compact_layout: None,
            sidebar_motion: SidebarMotion::new(),
            route_scroll_handles: RouteScrollHandles::default(),
        }
    }

    pub fn with_theme(model: ShellViewModel, theme: ThemeTokens) -> Self {
        Self {
            theme,
            ..Self::new(model)
        }
    }

    pub fn with_workspace_actions(
        model: ShellViewModel,
        workspace_action_sink: WorkspaceActionSink,
    ) -> Self {
        Self {
            workspace_action_sink: Some(workspace_action_sink),
            ..Self::new(model)
        }
    }

    pub const fn active_route(&self) -> Route {
        self.model.active_route
    }

    pub const fn view_model(&self) -> &ShellViewModel {
        &self.model
    }

    pub const fn sidebar_is_open(&self) -> bool {
        self.sidebar_open
    }

    fn open_replacement_editor(
        &mut self,
        id: Option<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let draft = id
            .and_then(|id| {
                self.model
                    .workspace
                    .replacements
                    .rules
                    .iter()
                    .find(|rule| rule.id == id)
            })
            .map(crate::ReplacementRuleViewModel::draft)
            .unwrap_or_else(|| ReplacementDraft::new("", ""));
        let source_value = draft.source.clone();
        let replacement_value = draft.replacement.clone();
        let source = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Spoken phrase")
                .default_value(source_value)
        });
        let replacement = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Replacement text")
                .default_value(replacement_value)
        });
        self.replacement_editor = Some(ReplacementEditorState {
            id,
            source,
            replacement,
            enabled: draft.enabled,
            case_sensitive: draft.case_sensitive,
            whole_word_only: draft.whole_word_only,
        });
        self.clear_route_feedback();
    }

    fn save_replacement(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &self.replacement_editor else {
            return;
        };
        let draft = ReplacementDraft {
            source: editor.source.read(cx).value().trim().to_owned(),
            replacement: editor.replacement.read(cx).value().trim().to_owned(),
            enabled: editor.enabled,
            case_sensitive: editor.case_sensitive,
            whole_word_only: editor.whole_word_only,
        };
        if !draft.is_valid() {
            self.set_route_feedback("Both phrases are required");
            return;
        }
        let action = match editor.id {
            Some(id) => WorkspaceAction::UpdateReplacement { id, draft },
            None => WorkspaceAction::CreateReplacement { draft },
        };
        self.emit_workspace_action(action, cx);
    }

    fn set_route_feedback(&mut self, message: impl Into<String>) {
        self.set_route_feedback_for(self.model.active_route, message);
    }

    fn set_route_feedback_for(&mut self, route: Route, message: impl Into<String>) {
        *self.route_feedbacks.for_route_mut(route) = Some(message.into());
    }

    fn clear_route_feedback(&mut self) {
        self.clear_route_feedback_for(self.model.active_route);
    }

    fn clear_route_feedback_for(&mut self, route: Route) {
        *self.route_feedbacks.for_route_mut(route) = None;
    }
}

fn workspace_with_currency(
    mut workspace: WorkspaceViewModel,
    currency: &str,
) -> WorkspaceViewModel {
    workspace.usage = workspace.usage.with_currency(currency);
    workspace
}

impl Render for SettingsShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_model_catalog_editor(window, cx);
        let theme = self.theme;
        let active_route = self.model.active_route;
        let navigation = self.model.navigation;
        let workspace = self.model.workspace.clone();
        let settings = self.settings.clone();
        let settings_dirty = self.settings_is_dirty();
        let has_api_key = self.has_api_key;
        let api_key_input = self.api_key_input.clone();
        let history_search_input = self.history_search_input.clone();
        let api_key_feedback = self.api_key_feedback.clone();
        let route_feedback = self.route_feedbacks.for_route(active_route).clone();
        let settings_editor = self.settings_editor.clone();
        let shortcut_capture_active = self.shortcut_capture_active;
        let shortcut_capture_error = self.shortcut_capture_error.clone();
        let replacement_editor = self.replacement_editor.clone();
        let replacement_editor_open = replacement_editor.is_some();
        let pending_destructive_action = self.pending_destructive_action.clone();
        let overview_recent_expanded = self.overview_recent_expanded;
        let route_scroll_handle = self.route_scroll_handles.for_route(active_route);
        let route_scroll_selector = format!("route-scroll-{}", active_route.slug());
        let route_scroll_id = match active_route {
            Route::Overview => 0_usize,
            Route::History => 1,
            Route::Replacements => 2,
            Route::Settings => 3,
        };
        let compact = f32::from(window.viewport_size().width) < SIDEBAR_OVERLAY_BREAKPOINT;
        let first_layout = self.compact_layout.is_none();
        if self.compact_layout != Some(compact) {
            self.sidebar_open =
                sidebar_open_for_layout(self.sidebar_open, self.compact_layout, compact);
            self.compact_layout = Some(compact);
        }
        let frame =
            self.sidebar_motion
                .update(self.sidebar_open, compact, first_layout, Instant::now());
        if frame.active {
            window.request_animation_frame();
        }

        gpui::div()
            .flex()
            .flex_row()
            .relative()
            .size_full()
            .min_w(px(720.))
            .min_h(px(480.))
            .bg(gpui_color(theme.canvas))
            .text_color(gpui_color(theme.text))
            .when(!compact, |root| {
                root.child(
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
                                    .child(sidebar_view(navigation, false, theme, cx)),
                            )
                        }),
                )
            })
            .child(
                v_flex()
                    .h_full()
                    .min_w_0()
                    .flex_1()
                    .child(shell_title_bar(
                        active_route,
                        self.sidebar_open,
                        window,
                        theme,
                        cx,
                    ))
                    .child(
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
                                    .child(route_surface(
                                        RouteSurfaceModel {
                                            active_route,
                                            workspace,
                                            settings,
                                            settings_dirty,
                                            has_api_key,
                                            api_key_input,
                                            history_search_input,
                                            api_key_feedback,
                                            feedback: route_feedback.clone(),
                                            settings_editor,
                                            shortcut_capture_active,
                                            shortcut_capture_error,
                                            replacement_editor,
                                            pending_destructive_action,
                                            overview_recent_expanded,
                                        },
                                        theme,
                                        cx,
                                    ))
                                    .when(
                                        active_route != Route::Settings
                                            && !(active_route == Route::Replacements
                                                && replacement_editor_open),
                                        |content| {
                                            content.when_some(
                                                route_feedback,
                                                |content, feedback| {
                                                    content.child(
                                                        gpui::div()
                                                            .debug_selector(|| {
                                                                "workspace-feedback".to_owned()
                                                            })
                                                            .rounded_lg()
                                                            .border_1()
                                                            .border_color(gpui_color(theme.border))
                                                            .p_3()
                                                            .text_xs()
                                                            .text_color(gpui_color(
                                                                theme.text_muted,
                                                            ))
                                                            .child(feedback),
                                                    )
                                                },
                                            )
                                        },
                                    )
                                    .track_scroll(&route_scroll_handle)
                                    .overflow_y_scroll(),
                            )
                            .child(
                                gpui::div()
                                    .debug_selector(move || {
                                        format!("route-scrollbar-{}", active_route.slug())
                                    })
                                    .absolute()
                                    .top_0()
                                    .right_0()
                                    .bottom_0()
                                    .w(px(ROUTE_SCROLLBAR_WIDTH))
                                    .child(
                                        Scrollbar::vertical(&route_scroll_handle)
                                            .id(("route-scrollbar", route_scroll_id)),
                                    ),
                            ),
                    ),
            )
            .when(
                compact && (self.sidebar_open || frame.panel > 0.0 || frame.scrim > 0.0),
                |root| {
                    root.child(
                        gpui::div()
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
                            .when(self.sidebar_open, |dismiss| {
                                dismiss.on_click(cx.listener(|shell, _, _, cx| {
                                    shell.sidebar_open = false;
                                    cx.notify();
                                }))
                            }),
                    )
                    .child(
                        gpui::div()
                            .debug_selector(|| "sidebar-overlay-panel".to_owned())
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(px(-SIDEBAR_WIDTH * (1.0 - frame.panel)))
                            .w(px(SIDEBAR_WIDTH))
                            .occlude()
                            .shadow_xl()
                            .child(sidebar_view(navigation, true, theme, cx)),
                    )
                },
            )
    }
}

struct RouteSurfaceModel {
    active_route: Route,
    workspace: WorkspaceViewModel,
    settings: agentdictate_core::Settings,
    settings_dirty: bool,
    has_api_key: bool,
    api_key_input: Option<Entity<InputState>>,
    history_search_input: Option<Entity<InputState>>,
    api_key_feedback: Option<String>,
    feedback: Option<String>,
    settings_editor: Option<SettingsEditorState>,
    shortcut_capture_active: bool,
    shortcut_capture_error: Option<String>,
    replacement_editor: Option<ReplacementEditorState>,
    pending_destructive_action: Option<WorkspaceAction>,
    overview_recent_expanded: bool,
}

struct SettingsSurfaceModel {
    settings: agentdictate_core::Settings,
    model_catalog: ModelCatalogViewModel,
    settings_dirty: bool,
    has_api_key: bool,
    api_key_input: Option<Entity<InputState>>,
    api_key_feedback: Option<String>,
    feedback: Option<String>,
    settings_editor: Option<SettingsEditorState>,
    shortcut_capture_active: bool,
    shortcut_capture_error: Option<String>,
}

fn route_surface(
    model: RouteSurfaceModel,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    match model.active_route {
        Route::Overview => overview::surface(
            model.workspace.usage,
            model.workspace.history,
            model.workspace.recent_transcripts,
            model.overview_recent_expanded,
            theme,
            cx,
        ),
        Route::History => history_page::surface(
            model.workspace.history,
            model.history_search_input,
            model.pending_destructive_action,
            theme,
            cx,
        ),
        Route::Settings => settings_page::surface(
            SettingsSurfaceModel {
                settings: model.settings,
                model_catalog: model.workspace.model_catalog,
                settings_dirty: model.settings_dirty,
                has_api_key: model.has_api_key,
                api_key_input: model.api_key_input,
                api_key_feedback: model.api_key_feedback,
                feedback: model.feedback,
                settings_editor: model.settings_editor,
                shortcut_capture_active: model.shortcut_capture_active,
                shortcut_capture_error: model.shortcut_capture_error,
            },
            theme,
            cx,
        ),
        Route::Replacements => replacements_page::surface(
            model.workspace.replacements,
            model.replacement_editor,
            model.feedback,
            model.pending_destructive_action,
            theme,
            cx,
        ),
    }
}

const fn enabled_label(enabled: bool) -> &'static str {
    if enabled { "On" } else { "Off" }
}

fn gpui_color(color: Color) -> Hsla {
    rgb((u32::from(color.red) << 16) | (u32::from(color.green) << 8) | u32::from(color.blue)).into()
}
