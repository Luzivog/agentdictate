use std::sync::Arc;

use gpui::{AppContext, Context};

use crate::{Route, WorkspaceAction, WorkspaceViewModel};

use super::{SettingsShell, row_actions::CONFIRM_DELETE_LABEL};

impl SettingsShell {
    /// Atomically replaces the workspace projection received from the daemon.
    pub fn apply_workspace_update(
        &mut self,
        workspace: WorkspaceViewModel,
        cx: &mut Context<Self>,
    ) {
        self.model.workspace = workspace;
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
        let feedback_route = self.model.active_route;
        let success_feedback = action.success_feedback();
        let copied_transcript = match action {
            WorkspaceAction::CopyTranscript { id } => Some(id),
            _ => None,
        };
        let sink = Arc::clone(&self.workspace_actions.sink);
        self.routes.pending_destructive_action = None;
        self.workspace_actions.in_flight = true;
        self.clear_route_feedback_for(feedback_route);
        let task = cx.background_spawn(async move { sink(action) });
        cx.spawn(async move |shell, cx| {
            let result = task.await;
            if let Some(shell) = shell.upgrade() {
                shell.update(cx, |shell, cx| {
                    shell.workspace_actions.in_flight = false;
                    match result {
                        Ok(workspace) => {
                            shell.model.workspace = workspace;
                            match success_feedback {
                                Some(message) => {
                                    shell.set_route_feedback_for(feedback_route, message);
                                }
                                None => shell.clear_route_feedback_for(feedback_route),
                            }
                            if let Some(id) = copied_transcript {
                                shell.show_copied(id, cx);
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
                });
            }
        })
        .detach();
    }

    /// History reads have their own latest-wins lane. A slow search must not
    /// block Copy or other workspace mutations, and an
    /// obsolete response must never replace a newer query.
    fn emit_history_action(&mut self, action: WorkspaceAction, cx: &mut Context<Self>) {
        if !self.workspace_actions.history_lane.schedule(&action) {
            return;
        }
        let sink = Arc::clone(&self.workspace_actions.sink);
        self.clear_route_feedback_for(Route::History);
        let task = cx.background_spawn(async move { sink(action) });
        cx.spawn(async move |shell, cx| {
            let result = task.await;
            if let Some(shell) = shell.upgrade() {
                shell.update(cx, |shell, cx| {
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
                });
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
            self.set_route_feedback(format!(
                "Click {CONFIRM_DELETE_LABEL} to delete it permanently, or continue elsewhere to cancel."
            ));
        }
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

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn route_feedback_for_test(&self, route: Route) -> Option<&str> {
        self.routes.entry(route).feedback.as_deref()
    }
}
