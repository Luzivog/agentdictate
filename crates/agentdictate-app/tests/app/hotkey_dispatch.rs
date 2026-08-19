use agentdictate_app::command_for_hotkey;
use agentdictate_core::{ClientCommandKind, JobId, WorkflowPhase};
use agentdictate_linux::hotkey::HotkeySignal;

#[test]
fn toggle_hotkey_starts_and_stops_only_on_press_edges() {
    let start = command_for_hotkey("toggle", HotkeySignal::Pressed, WorkflowPhase::Ready, 1)
        .expect("ready press should start");
    let job_id = JobId::new();
    let stop = command_for_hotkey(
        "toggle",
        HotkeySignal::Pressed,
        WorkflowPhase::Recording { job_id },
        2,
    )
    .expect("recording press should stop");

    assert!(matches!(
        start.kind,
        ClientCommandKind::StartRecording { .. }
    ));
    assert!(matches!(stop.kind, ClientCommandKind::StopRecording { .. }));
    assert!(
        command_for_hotkey(
            "toggle",
            HotkeySignal::Released,
            WorkflowPhase::Recording { job_id },
            3,
        )
        .is_none()
    );
}

#[test]
fn hold_and_escape_edges_never_create_duplicate_commands() {
    let job_id = JobId::new();
    assert!(matches!(
        command_for_hotkey("hold", HotkeySignal::Pressed, WorkflowPhase::Ready, 1)
            .unwrap()
            .kind,
        ClientCommandKind::StartRecording { .. }
    ));
    assert!(matches!(
        command_for_hotkey(
            "hold",
            HotkeySignal::Released,
            WorkflowPhase::Recording { job_id },
            2,
        )
        .unwrap()
        .kind,
        ClientCommandKind::StopRecording { .. }
    ));
    assert!(matches!(
        command_for_hotkey(
            "toggle",
            HotkeySignal::Cancelled,
            WorkflowPhase::Recording { job_id },
            3,
        )
        .unwrap()
        .kind,
        ClientCommandKind::Cancel { .. }
    ));
    assert!(
        command_for_hotkey(
            "hold",
            HotkeySignal::Pressed,
            WorkflowPhase::Recording { job_id },
            4,
        )
        .is_none()
    );
}
