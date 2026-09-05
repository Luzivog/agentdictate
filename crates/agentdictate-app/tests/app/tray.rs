use agentdictate_app::{TrayAction, tray_command_for_phase};
use agentdictate_core::{ClientCommandKind, JobId, ProcessingStage, WorkflowPhase};

#[test]
fn tray_toggle_maps_only_actionable_workflow_phases() {
    let ready = tray_command_for_phase(TrayAction::ToggleDictation, WorkflowPhase::Ready, 7)
        .expect("ready starts recording");
    assert!(matches!(
        ready.kind,
        ClientCommandKind::StartRecording {
            request_id: 7,
            mode: None
        }
    ));

    let recording = tray_command_for_phase(
        TrayAction::ToggleDictation,
        WorkflowPhase::Recording {
            job_id: JobId::new(),
        },
        8,
    )
    .expect("recording stops");
    assert!(matches!(
        recording.kind,
        ClientCommandKind::StopRecording { request_id: 8 }
    ));

    assert!(
        tray_command_for_phase(
            TrayAction::ToggleDictation,
            WorkflowPhase::Processing {
                job_id: JobId::new(),
                stage: ProcessingStage::Transcribing,
            },
            9,
        )
        .is_none(),
        "the tray must not queue a new recording while transcription is busy"
    );
}

#[test]
fn non_toggle_tray_actions_never_fabricate_workflow_commands() {
    assert!(tray_command_for_phase(TrayAction::OpenSettings, WorkflowPhase::Ready, 1).is_none());
    assert!(tray_command_for_phase(TrayAction::Quit, WorkflowPhase::Ready, 2).is_none());
}

#[test]
fn mode_override_is_one_start_and_never_interrupts_an_active_recording() {
    let command =
        tray_command_for_phase(TrayAction::StartLiteral, WorkflowPhase::Ready, 3).unwrap();
    assert!(matches!(
        command.kind,
        ClientCommandKind::StartRecording {
            mode: Some(agentdictate_core::DictationMode::Literal),
            ..
        }
    ));
    assert!(
        tray_command_for_phase(
            TrayAction::StartOrganize,
            WorkflowPhase::Recording {
                job_id: JobId::new()
            },
            4
        )
        .is_none()
    );
}
