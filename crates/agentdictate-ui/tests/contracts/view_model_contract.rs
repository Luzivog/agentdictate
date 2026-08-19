//! Shell view-model contracts.

use agentdictate_core::{AppSnapshot, HotkeyReadiness, JobId, WorkflowPhase, WorkflowSnapshot};
use agentdictate_ui::{Route, ShellViewModel, StatusTone};

#[test]
fn shell_view_model_derives_navigation_and_recording_status_from_domain_state() {
    let model = ShellViewModel::from_snapshot(
        Route::History,
        WorkflowSnapshot {
            phase: WorkflowPhase::Recording {
                job_id: JobId::new(),
            },
        },
    );

    assert_eq!(model.active_route, Route::History);
    assert_eq!(model.navigation.len(), 4);
    assert_eq!(
        model
            .navigation
            .iter()
            .filter(|item| item.is_active)
            .map(|item| item.route)
            .collect::<Vec<_>>(),
        vec![Route::History]
    );
    assert_eq!(model.status.label, "Recording");
    assert_eq!(model.status.detail, "Listening to your microphone");
    assert_eq!(model.status.tone, StatusTone::Recording);
    assert!(model.status.is_busy);
}

#[test]
fn app_snapshot_projects_hotkey_failure_and_recovery_into_the_shell() {
    let model = ShellViewModel::from_app_snapshot(
        Route::Overview,
        AppSnapshot {
            sequence: 42,
            workflow: WorkflowSnapshot {
                phase: WorkflowPhase::Ready,
            },
            hotkey: HotkeyReadiness::Unavailable {
                message: "Permission denied".to_owned(),
            },
            recoverable_count: 3,
            last_transcript: Some("Safe transcript".to_owned()),
        },
    );

    assert_eq!(model.snapshot_sequence, Some(42));
    assert_eq!(model.hotkey.label, "Shortcut unavailable");
    assert_eq!(model.hotkey.detail, "Permission denied");
    assert_eq!(model.hotkey.tone, StatusTone::Danger);
    assert!(!model.hotkey.is_ready);
    assert_eq!(model.workspace.history.recovery.item_count, 3);
    assert_eq!(model.last_transcript.as_deref(), Some("Safe transcript"));
}

#[test]
fn selecting_a_route_updates_the_route_and_navigation_atomically() {
    let mut model = ShellViewModel::from_snapshot(
        Route::Overview,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    );

    model.select_route(Route::Settings);

    assert_eq!(model.active_route, Route::Settings);
    assert!(
        model
            .navigation
            .iter()
            .find(|item| item.route == Route::Settings)
            .expect("settings navigation item")
            .is_active
    );
    assert_eq!(
        model
            .navigation
            .iter()
            .filter(|item| item.is_active)
            .count(),
        1
    );
}
