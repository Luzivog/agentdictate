use std::sync::Arc;

use gpui::{AppContext, Context, Window};
use gpui_component::input::InputState;

use crate::{ReplacementDraft, Route, WorkspaceAction, WorkspaceViewModel};

use super::{SettingsShell, settings_shell::ReplacementEditorState};

impl SettingsShell {
    /// Atomically replaces the workspace projection received from the daemon.
    pub fn apply_workspace_update(
        &mut self,
        workspace: WorkspaceViewModel,
        cx: &mut Context<Self>,
    ) {
        self.model.workspace = workspace_with_currency(workspace, &self.settings.current.currency);
        cx.notify();
    }

    pub(super) fn emit_workspace_action(
        &mut self,
        action: WorkspaceAction,
        cx: &mut Context<Self>,
    ) {
        if matches!(
            action,
            WorkspaceAction::SearchHistory { .. } | WorkspaceAction::LoadMoreHistory
        ) {
            self.emit_history_action(action, cx);
            return;
        }
        if self.workspace_actions.in_flight {
            self.set_route_feedback("Another action is still running");
            return;
        }
        let Some(sink) = &self.workspace_actions.sink else {
            self.set_route_feedback("This action is not connected yet");
            return;
        };
        let feedback_route = self.model.active_route;
        let sink = Arc::clone(sink);
        let closes_editor = matches!(
            action,
            WorkspaceAction::CreateReplacement { .. } | WorkspaceAction::UpdateReplacement { .. }
        );
        self.routes.pending_destructive_action = None;
        self.workspace_actions.in_flight = true;
        self.clear_route_feedback_for(feedback_route);
        let task = cx.background_spawn(async move { sink(action) });
        cx.spawn(async move |shell, cx| {
            let result = task.await;
            if let Some(shell) = shell.upgrade() {
                shell
                    .update(cx, |shell, cx| {
                        shell.workspace_actions.in_flight = false;
                        match result {
                            Ok(workspace) => {
                                shell.model.workspace = workspace_with_currency(
                                    workspace,
                                    &shell.settings.current.currency,
                                );
                                shell.clear_route_feedback_for(feedback_route);
                                if closes_editor {
                                    shell.routes.replacement_editor = None;
                                }
                            }
                            Err(error) => {
                                shell.set_route_feedback_for(
                                    feedback_route,
                                    format!("Could not complete action: {error}"),
                                );
                            }
                        }
                        cx.notify();
                    })
                    .ok();
            }
        })
        .detach();
    }

    /// History reads have their own latest-wins lane. A slow search must not
    /// block Copy, replacement edits, or other workspace mutations, and an
    /// obsolete response must never replace a newer query.
    fn emit_history_action(&mut self, action: WorkspaceAction, cx: &mut Context<Self>) {
        if !self.workspace_actions.history_lane.schedule(&action) {
            return;
        }
        let Some(sink) = &self.workspace_actions.sink else {
            self.set_route_feedback_for(Route::History, "History search is not connected yet");
            return;
        };
        let sink = Arc::clone(sink);
        self.clear_route_feedback_for(Route::History);
        let task = cx.background_spawn(async move { sink(action) });
        cx.spawn(async move |shell, cx| {
            let result = task.await;
            if let Some(shell) = shell.upgrade() {
                shell
                    .update(cx, |shell, cx| {
                        let completion = shell.workspace_actions.history_lane.complete();
                        if completion.apply_result {
                            match result {
                                Ok(workspace) => {
                                    shell.model.workspace.history = workspace.history;
                                    shell.clear_route_feedback_for(Route::History);
                                }
                                Err(error) => shell.set_route_feedback_for(
                                    Route::History,
                                    format!("Could not search history: {error}"),
                                ),
                            }
                        }
                        if let Some(query) = completion.next_search {
                            shell.emit_history_action(WorkspaceAction::SearchHistory { query }, cx);
                        }
                        cx.notify();
                    })
                    .ok();
            }
        })
        .detach();
    }

    pub(super) fn submit_history_search(&mut self, query: String, cx: &mut Context<Self>) {
        self.emit_history_action(WorkspaceAction::SearchHistory { query }, cx);
    }

    pub(super) fn request_destructive_action(
        &mut self,
        action: WorkspaceAction,
        cx: &mut Context<Self>,
    ) {
        if self.routes.pending_destructive_action.as_ref() == Some(&action) {
            self.routes.pending_destructive_action = None;
            self.emit_workspace_action(action, cx);
        } else {
            self.routes.pending_destructive_action = Some(action);
            self.set_route_feedback(
                "Click Confirm delete to permanently remove this item, or continue elsewhere to cancel."
                    .to_owned(),
            );
        }
    }

    pub(super) fn open_replacement_editor(
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
        self.routes.replacement_editor = Some(ReplacementEditorState {
            id,
            source,
            replacement,
            enabled: draft.enabled,
            case_sensitive: draft.case_sensitive,
            whole_word_only: draft.whole_word_only,
        });
        self.clear_route_feedback();
    }

    pub(super) fn save_replacement(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &self.routes.replacement_editor else {
            return;
        };
        let draft = editor.draft(cx);
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

    pub(super) fn set_route_feedback(&mut self, message: impl Into<String>) {
        self.set_route_feedback_for(self.model.active_route, message);
    }

    pub(super) fn set_route_feedback_for(&mut self, route: Route, message: impl Into<String>) {
        self.routes.entry_mut(route).feedback = Some(message.into());
    }

    pub(super) fn clear_route_feedback(&mut self) {
        self.clear_route_feedback_for(self.model.active_route);
    }

    pub(super) fn clear_route_feedback_for(&mut self, route: Route) {
        self.routes.entry_mut(route).feedback = None;
    }
}

pub(super) fn workspace_with_currency(
    mut workspace: WorkspaceViewModel,
    currency: &str,
) -> WorkspaceViewModel {
    workspace.usage = workspace.usage.with_currency(currency);
    workspace
}
