use crate::WorkspaceAction;

/// Serializes history reads while retaining only the newest query. History
/// uses its own lane so unrelated workspace mutations remain interactive.
#[derive(Default)]
pub(super) struct HistoryActionLane {
    in_flight: bool,
    pending_search: Option<String>,
}

pub(super) struct HistoryActionCompletion {
    pub apply_result: bool,
    pub next_search: Option<String>,
}

impl HistoryActionLane {
    /// Returns true when the caller should start the action immediately.
    pub fn schedule(&mut self, action: &WorkspaceAction) -> bool {
        if self.in_flight {
            if let WorkspaceAction::SearchHistory { query } = action {
                self.pending_search = Some(query.clone());
            }
            return false;
        }
        self.in_flight = true;
        true
    }

    /// Completes the active read. A queued newer search makes the completed
    /// result obsolete and is returned for immediate dispatch.
    pub fn complete(&mut self) -> HistoryActionCompletion {
        self.in_flight = false;
        let next_search = self.pending_search.take();
        HistoryActionCompletion {
            apply_result: next_search.is_none(),
            next_search,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newer_query_replaces_every_queued_query_and_discards_the_old_result() {
        let mut lane = HistoryActionLane::default();
        assert!(lane.schedule(&WorkspaceAction::SearchHistory { query: "n".into() }));
        assert!(!lane.schedule(&WorkspaceAction::SearchHistory { query: "ne".into() }));
        assert!(!lane.schedule(&WorkspaceAction::SearchHistory {
            query: "needle".into(),
        }));

        let completion = lane.complete();

        assert!(!completion.apply_result);
        assert_eq!(completion.next_search.as_deref(), Some("needle"));
        assert!(lane.schedule(&WorkspaceAction::SearchHistory {
            query: "needle".into(),
        }));
    }

    #[test]
    fn an_uncontested_result_is_applied_and_load_more_is_not_duplicated() {
        let mut lane = HistoryActionLane::default();
        assert!(lane.schedule(&WorkspaceAction::LoadMoreHistory));
        assert!(!lane.schedule(&WorkspaceAction::LoadMoreHistory));

        let completion = lane.complete();

        assert!(completion.apply_result);
        assert!(completion.next_search.is_none());
    }
}
