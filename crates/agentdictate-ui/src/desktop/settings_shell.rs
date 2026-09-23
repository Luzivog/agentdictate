use gpui::{AppContext, Context, Entity, ScrollHandle, Window};
use gpui_component::input::{InputEvent, InputState};

use crate::{
    ReplacementDraft, Route, ShellViewModel, ThemeTokens, WorkspaceAction, WorkspaceActionSink,
};

use super::{
    CommandSink, SettingsShell, history_action_lane::HistoryActionLane,
    settings_form::SettingsFormState,
};

pub(super) struct SettingsEditState {
    pub(super) current: agentdictate_core::Settings,
    pub(super) baseline: agentdictate_core::Settings,
    pub(super) form: SettingsFormState,
    pub(super) dirty: bool,
    pub(super) shortcut_capture_active: bool,
    pub(super) shortcut_capture_error: Option<String>,
}

pub(super) struct SettingsCommandState {
    pub(super) has_api_key: bool,
    pub(super) api_key_input: Entity<InputState>,
    pub(super) api_key_feedback: Option<String>,
    pub(super) command_sink: CommandSink,
    pub(super) next_request_id: u64,
}

pub(super) struct WorkspaceActionState {
    pub(super) sink: WorkspaceActionSink,
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
    pub(super) history_search_input: Entity<InputState>,
    pub(super) replacement_editor: Option<ReplacementEditorState>,
    pub(super) pending_destructive_action: Option<WorkspaceAction>,
    pub(super) overview_recent_expanded: bool,
}

impl RouteUiState {
    pub(super) fn entry(&self, route: Route) -> &RouteUiEntry {
        &self.entries[route_index(route)]
    }

    pub(super) fn entry_mut(&mut self, route: Route) -> &mut RouteUiEntry {
        &mut self.entries[route_index(route)]
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
    /// Builds the settings window's shell around the daemon's settings and the
    /// sinks that send commands and workspace actions back to it.
    pub fn new(
        model: ShellViewModel,
        settings: agentdictate_core::Settings,
        has_api_key: bool,
        command_sink: CommandSink,
        workspace_action_sink: WorkspaceActionSink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let api_key_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("sk-…").masked(true));
        let initial_history_search = model.workspace.history.search.clone();
        let history_search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search every transcript")
                .default_value(initial_history_search)
        });
        let form = SettingsFormState::new(&settings, window, cx);
        let mut subscriptions = form.subscriptions(cx);
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

        let routes = RouteUiState {
            entries: std::array::from_fn(|_| RouteUiEntry::default()),
            history_search_input,
            replacement_editor: None,
            pending_destructive_action: None,
            overview_recent_expanded: false,
        };

        Self {
            model,
            theme: ThemeTokens::default(),
            settings: SettingsEditState {
                current: settings.clone(),
                baseline: settings,
                form,
                dirty: false,
                shortcut_capture_active: false,
                shortcut_capture_error: None,
            },
            settings_commands: SettingsCommandState {
                has_api_key,
                api_key_input,
                api_key_feedback: None,
                command_sink,
                next_request_id: 1,
            },
            workspace_actions: WorkspaceActionState {
                sink: workspace_action_sink,
                in_flight: false,
                history_lane: HistoryActionLane::default(),
            },
            routes,
            _subscriptions: subscriptions,
        }
    }

    pub const fn active_route(&self) -> Route {
        self.model.active_route
    }

    pub const fn view_model(&self) -> &ShellViewModel {
        &self.model
    }

    pub(super) fn select_route(&mut self, route: Route, cx: &mut Context<Self>) {
        let previous_route = self.model.active_route;
        self.model.select_route(route);
        self.routes.pending_destructive_action = None;
        self.clear_route_feedback_for(previous_route);
        self.settings_commands.api_key_feedback = None;
        cx.notify();
    }
}
