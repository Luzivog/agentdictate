use agentdictate_core::{JobId, JobStage, Workflow, WorkflowPhase, WorkflowSignal};

#[test]
fn recording_is_not_announced_until_audio_is_durable() {
    let mut workflow = Workflow::new();
    let job_id = JobId::new();

    let starting = workflow
        .apply(WorkflowSignal::StartRequested { job_id })
        .expect("an idle workflow accepts a recording request");
    assert_eq!(starting.phase, WorkflowPhase::Starting { job_id });

    let recording = workflow
        .apply(WorkflowSignal::FirstAudioFrameWritten { job_id })
        .expect("the matching recorder can announce its first durable frame");
    assert_eq!(recording.phase, WorkflowPhase::Recording { job_id });
}

#[test]
fn completed_dictation_returns_the_workflow_to_ready() {
    let mut workflow = Workflow::new();
    let job_id = JobId::new();

    workflow
        .apply(WorkflowSignal::StartRequested { job_id })
        .unwrap();
    workflow
        .apply(WorkflowSignal::FirstAudioFrameWritten { job_id })
        .unwrap();
    workflow.apply(WorkflowSignal::StopRequested).unwrap();
    workflow
        .apply(WorkflowSignal::CaptureFinalized { job_id })
        .unwrap();
    workflow
        .apply(WorkflowSignal::TranscriptStored { job_id })
        .unwrap();
    workflow
        .apply(WorkflowSignal::DeliveryStarted { job_id })
        .unwrap();
    let completed = workflow
        .apply(WorkflowSignal::DeliverySubmitted { job_id })
        .unwrap();

    assert_eq!(completed.phase, WorkflowPhase::Ready);
}

#[test]
fn an_interrupted_recording_becomes_explicitly_recoverable() {
    let mut workflow = Workflow::new();
    let job_id = JobId::new();
    workflow
        .apply(WorkflowSignal::StartRequested { job_id })
        .unwrap();

    let recoverable = workflow
        .apply(WorkflowSignal::Interrupted {
            job_id,
            at: JobStage::Starting,
        })
        .unwrap();

    assert_eq!(
        recoverable.phase,
        WorkflowPhase::NeedsAttention {
            job_id,
            at: JobStage::Starting,
        }
    );
}

#[test]
fn stale_recorder_events_cannot_change_the_active_job() {
    let mut workflow = Workflow::new();
    let active_job = JobId::new();
    let error_job = JobId::new();
    workflow
        .apply(WorkflowSignal::StartRequested { job_id: active_job })
        .unwrap();

    let error = workflow
        .apply(WorkflowSignal::FirstAudioFrameWritten { job_id: error_job })
        .unwrap_err();

    assert_eq!(
        error,
        agentdictate_core::WorkflowError::JobMismatch {
            expected: active_job,
            received: error_job,
        }
    );
}

#[test]
fn committed_user_discard_returns_the_workflow_to_ready() {
    let mut workflow = Workflow::new();
    let job_id = JobId::new();
    workflow
        .apply(WorkflowSignal::StartRequested { job_id })
        .unwrap();
    workflow
        .apply(WorkflowSignal::FirstAudioFrameWritten { job_id })
        .unwrap();

    let discarded = workflow
        .apply(WorkflowSignal::DiscardCommitted { job_id })
        .unwrap();

    assert_eq!(discarded.phase, WorkflowPhase::Ready);
}

#[test]
fn recovery_retry_enters_processing_from_needs_attention_for_another_job() {
    let mut workflow = Workflow::new();
    let failed_job = JobId::new();
    workflow
        .apply(WorkflowSignal::StartRequested { job_id: failed_job })
        .unwrap();
    workflow
        .apply(WorkflowSignal::Interrupted {
            job_id: failed_job,
            at: JobStage::Failed,
        })
        .unwrap();
    let older_job = JobId::new();

    let retrying = workflow
        .apply(WorkflowSignal::RetryRequested { job_id: older_job })
        .unwrap();

    assert_eq!(
        retrying.phase,
        WorkflowPhase::Processing {
            job_id: older_job,
            stage: agentdictate_core::ProcessingStage::Transcribing,
        }
    );
}

#[test]
fn a_recoverable_failure_does_not_block_the_next_recording() {
    let mut workflow = Workflow::new();
    let failed_job = JobId::new();
    workflow
        .apply(WorkflowSignal::StartRequested { job_id: failed_job })
        .unwrap();
    workflow
        .apply(WorkflowSignal::Interrupted {
            job_id: failed_job,
            at: JobStage::Interrupted,
        })
        .unwrap();
    let next_job = JobId::new();

    let next = workflow
        .apply(WorkflowSignal::StartRequested { job_id: next_job })
        .unwrap();

    assert_eq!(next.phase, WorkflowPhase::Starting { job_id: next_job });
}

#[test]
fn cancelling_processing_returns_to_ready_and_rejects_the_late_signals() {
    let mut workflow = Workflow::new();
    let job_id = JobId::new();
    for signal in [
        WorkflowSignal::StartRequested { job_id },
        WorkflowSignal::FirstAudioFrameWritten { job_id },
        WorkflowSignal::StopRequested,
        WorkflowSignal::CaptureFinalized { job_id },
    ] {
        workflow.apply(signal).unwrap();
    }

    let cancelled = workflow
        .apply(WorkflowSignal::ProcessingCancelled { job_id })
        .unwrap();

    assert_eq!(cancelled.phase, WorkflowPhase::Ready);
    assert!(
        workflow
            .apply(WorkflowSignal::TranscriptStored { job_id })
            .is_err()
    );
}
