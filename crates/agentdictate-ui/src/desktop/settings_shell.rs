use gpui::{AppContext, Context, Entity, ScrollHandle, Window};
use gpui_component::input::{InputEvent, InputState};

use crate::{
    ModelCatalogViewModel, ReplacementDraft, Route, ShellViewModel, ThemeTokens, WorkspaceAction,
    WorkspaceActionSink,
};

use super::{
    CommandSink, SettingsShell, history_action_lane::HistoryActionLane,
    settings_form::SettingsFormState, workspace_actions::workspace_with_currency,
};
use crate::sidebar_motion::SidebarMotion;

pub(super) struct SettingsEditState {
    pub(super) current: agentdictate_core::Settings,
    pub(super) baseline: agentdictate_core::Settings,
    pub(super) form: Option<SettingsFormState>,
    pub(super) applied_model_catalog: ModelCatalogViewModel,
    pub(super) dirty: bool,
    pub(super) shortcut_capture_active: bool,
    pub(super) shortcut_capture_error: Option<String>,
}

impl SettingsEditState {
    fn disconnected(model_catalog: ModelCatalogViewModel) -> Self {
        let settings = agentdictate_core::Settings::default();
        Self {
            current: settings.clone(),
            baseline: settings,
            form: None,
            applied_model_catalog: model_catalog,
            dirty: false,
            shortcut_capture_active: false,
            shortcut_capture_error: None,
        }
    }
}

pub(super) struct SettingsCommandState {
    pub(super) has_api_key: bool,
    pub(super) api_key_input: Option<Entity<InputState>>,
    pub(super) api_key_feedback: Option<String>,
    pub(super) command_sink: Option<CommandSink>,
    pub(super) next_request_id: u64,
}

impl Default for SettingsCommandState {
    fn default() -> Self {
        Self {
            has_api_key: false,
            api_key_input: None,
            api_key_feedback: None,
            command_sink: None,
            next_request_id: 1,
        }
    }
}

#[derive(Default)]
pub(super) struct WorkspaceActionState {
    pub(super) sink: Option<WorkspaceActionSink>,
    pub(super) in_flight: bool,
    pub(super) history_lane: HistoryActionLane,
}

#[derive(Clone)]
pub(super) struct ReplacementEditorState {
    pub(super) id: Option<i64>,
    pub(super) source: Entity<InputState>,
    pub(super) replacement: Entity<InputState>,
    pub(super) enabled: bool,
    pub(super) case_sensitive: bool,
    pub(super) whole_word_only: bool,
}

impl ReplacementEditorState {
    pub(super) fn draft(&self, cx: &Context<SettingsShell>) -> ReplacementDraft {
        ReplacementDraft {
            source: self.source.read(cx).value().trim().to_owned(),
            replacement: self.replacement.read(cx).value().trim().to_owned(),
            enabled: self.enabled,
            case_sensitive: self.case_sensitive,
            whole_word_only: self.whole_word_only,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct RouteUiEntry {
    pub(super) feedback: Option<String>,
    pub(super) scroll: ScrollHandle,
}

pub(super) struct RouteUiState {
    pub(super) entries: [RouteUiEntry; Route::ALL.len()],
    pub(super) history_search_input: Option<Entity<InputState>>,
    pub(super) replacement_editor: Option<ReplacementEditorState>,
    pub(super) pending_destructive_action: Option<WorkspaceAction>,
    pub(super) overview_recent_expanded: bool,
}

impl Default for RouteUiState {
    fn default() -> Self {
        Self {
            entries: std::array::from_fn(|_| RouteUiEntry::default()),
            history_search_input: None,
            replacement_editor: None,
            pending_destructive_action: None,
            overview_recent_expanded: false,
        }
    }
}

impl RouteUiState {
    pub(super) fn entry(&self, route: Route) -> &RouteUiEntry {
        &self.entries[route_index(route)]
    }

    pub(super) fn entry_mut(&mut self, route: Route) -> &mut RouteUiEntry {
        &mut self.entries[route_index(route)]
    }
}

pub(super) struct ShellLayoutState {
    pub(super) sidebar_open: bool,
    pub(super) compact_layout: Option<bool>,
    pub(super) sidebar_motion: SidebarMotion,
}

impl Default for ShellLayoutState {
    fn default() -> Self {
        Self {
            sidebar_open: true,
            compact_layout: None,
            sidebar_motion: SidebarMotion::new(),
        }
    }
}

pub(super) const fn route_index(route: Route) -> usize {
    match route {
        Route::Overview => 0,
        Route::History => 1,
        Route::Replacements => 2,
        Route::Settings => 3,
    }
}

impl SettingsShell {
    pub fn new(model: ShellViewModel) -> Self {
        let model_catalog = model.workspace.model_catalog.clone();
        Self {
            model,
            theme: ThemeTokens::default(),
            settings: SettingsEditState::disconnected(model_catalog),
            settings_commands: SettingsCommandState::default(),
            workspace_actions: WorkspaceActionState::default(),
            routes: RouteUiState::default(),
            layout: ShellLayoutState::default(),
            _subscriptions: Vec::new(),
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

    pub(super) fn connected_internal(
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
        let form = SettingsFormState::new(&settings, &applied_model_catalog, window, cx);
        let mut subscriptions = form.subscriptions(window, cx);
        subscriptions.push(
            cx.subscribe(&api_key_input, |shell, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    shell.settings_commands.api_key_feedback = None;
                    cx.notify();
                }
            }),
        );
        subscriptions.push(cx.subscribe(
            &history_search_input,
            |shell, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let query = input.read(cx).value().to_string();
                    shell.submit_history_search(query, cx);
                }
            },
        ));

        let mut next_request_id = 1;
        let mut routes = RouteUiState {
            history_search_input: Some(history_search_input),
            ..RouteUiState::default()
        };
        if has_api_key && model.active_route == Route::Settings {
            if let Err(error) = command_sink(
                agentdictate_core::ClientCommand::refresh_model_catalog(next_request_id),
            ) {
                routes.entry_mut(Route::Settings).feedback =
                    Some(format!("Could not refresh models: {error}"));
            }
            next_request_id += 1;
        }

        Self {
            model,
            theme: ThemeTokens::default(),
            settings: SettingsEditState {
                current: settings.clone(),
                baseline: settings,
                form: Some(form),
                applied_model_catalog,
                dirty: false,
                shortcut_capture_active: false,
                shortcut_capture_error: None,
            },
            settings_commands: SettingsCommandState {
                has_api_key,
                api_key_input: Some(api_key_input),
                api_key_feedback: None,
                command_sink: Some(command_sink),
                next_request_id,
            },
            workspace_actions: WorkspaceActionState {
                sink: workspace_action_sink,
                ..WorkspaceActionState::default()
            },
            routes,
            layout: ShellLayoutState::default(),
            _subscriptions: subscriptions,
        }
    }

    pub fn with_workspace_actions(
        model: ShellViewModel,
        workspace_action_sink: WorkspaceActionSink,
    ) -> Self {
        let mut shell = Self::new(model);
        shell.workspace_actions.sink = Some(workspace_action_sink);
        shell
    }

    pub const fn active_route(&self) -> Route {
        self.model.active_route
    }

    pub const fn view_model(&self) -> &ShellViewModel {
        &self.model
    }

    pub const fn sidebar_is_open(&self) -> bool {
        self.layout.sidebar_open
    }

    pub(super) fn select_route(
        &mut self,
        route: Route,
        close_overlay_sidebar: bool,
        cx: &mut Context<Self>,
    ) {
        let previous_route = self.model.active_route;
        self.model.select_route(route);
        if route == Route::Settings && previous_route != Route::Settings {
            self.request_model_catalog_refresh();
        }
        self.routes.pending_destructive_action = None;
        self.clear_route_feedback_for(previous_route);
        self.settings_commands.api_key_feedback = None;
        if close_overlay_sidebar {
            self.layout.sidebar_open = false;
        }
        cx.notify();
    }
}
