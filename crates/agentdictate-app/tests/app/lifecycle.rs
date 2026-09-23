use agentdictate_app::{LifecycleAction, Trigger, lifecycle_action};
use agentdictate_core::{
    DictationMode, FailureKind, JobId, JobStage, ProcessingStage, RecordingMode, WorkflowPhase,
};
use agentdictate_linux::hotkey::HotkeySignal;

#[test]
fn lifecycle_action_covers_every_trigger_and_phase() {
    use LifecycleAction::{CancelProcessing, Discard, Start, Stop};
    use RecordingMode::{Hold, Toggle};
    let job_id = JobId::new();
    let ready = WorkflowPhase::Ready;
    // A dictation waiting in Recovery behaves like Ready.
    let attention = WorkflowPhase::NeedsAttention {
        job_id,
        at: JobStage::Failed,
        failure: FailureKind::Offline,
    };
    let recording = WorkflowPhase::Recording { job_id };
    let stopping = WorkflowPhase::Stopping { job_id };
    let transcribing = WorkflowPhase::Processing {
        job_id,
        stage: ProcessingStage::Transcribing,
    };
    let pressed = Trigger::Hotkey(HotkeySignal::Pressed);
    let released = Trigger::Hotkey(HotkeySignal::Released);
    let escape = Trigger::Hotkey(HotkeySignal::Cancelled);
    let literal = Start(Some(DictationMode::Literal));
    let cases = [
        // (trigger, mode, [ready, attention, recording, stopping, transcribing])
        (
            pressed,
            Toggle,
            [Some(Start(None)), Some(Start(None)), Some(Stop), None, None],
        ),
        (
            pressed,
            Hold,
            [Some(Start(None)), Some(Start(None)), None, None, None],
        ),
        (released, Hold, [None, None, Some(Stop), None, None]),
        (released, Toggle, [None, None, None, None, None]),
        (escape, Toggle, [None, None, Some(Discard), None, None]),
        (
            Trigger::TrayToggle,
            Hold,
            [Some(Start(None)), Some(Start(None)), Some(Stop), None, None],
        ),
        (
            Trigger::TrayStartLiteral,
            Toggle,
            [Some(literal), Some(literal), None, None, None],
        ),
        (
            Trigger::TrayCancel,
            Toggle,
            [None, None, Some(Discard), None, Some(CancelProcessing)],
        ),
    ];

    for (trigger, mode, expected) in cases {
        for (phase, expected) in [ready, attention, recording, stopping, transcribing]
            .into_iter()
            .zip(expected)
        {
            assert_eq!(
                lifecycle_action(trigger, mode, phase),
                expected,
                "{trigger:?} in {mode:?} while {phase:?}"
            );
        }
    }
}
