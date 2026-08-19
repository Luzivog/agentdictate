//! History presentation contracts.

use agentdictate_core::{WorkflowPhase, WorkflowSnapshot};
use agentdictate_ui::{
    HistoryViewModel, RecoveryViewModel, Route, ShellViewModel, TranscriptViewModel,
};

#[test]
fn history_keeps_recoverable_recordings_as_a_first_class_section() {
    let history = HistoryViewModel::new(18, 2);

    assert_eq!(history.transcript_count, 18);
    assert_eq!(
        history.recovery,
        RecoveryViewModel {
            title: "Recovery",
            detail: "2 recordings need your attention".to_owned(),
            item_count: 2,
            items: Vec::new(),
        }
    );
    assert!(history.recovery.has_items());

    let shell = ShellViewModel::from_snapshot(
        Route::History,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    )
    .with_history(history);
    assert_eq!(shell.workspace.history.recovery.item_count, 2);
}

#[test]
fn history_page_keeps_archive_count_search_and_load_more_state_separate_from_visible_rows() {
    let visible = vec![TranscriptViewModel::new(
        9,
        "Today, 14:18",
        "Matching transcript",
        2,
        "3s",
    )];

    let history =
        HistoryViewModel::from_page(Vec::new(), visible, 2_553, "matching".to_owned(), true);

    assert_eq!(history.transcript_count, 2_553);
    assert_eq!(history.search, "matching");
    assert_eq!(history.transcripts.len(), 1);
    assert!(history.has_more);
}
