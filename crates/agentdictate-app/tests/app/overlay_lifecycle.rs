use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    time::{Duration, Instant},
};

use agentdictate_app::{
    ActiveRecordingUpdate, OverlayProcessAction, OverlayProcessState, OverlayUpdate,
    start_overlay_presenter, start_overlay_presenter_with_timeout,
};
use agentdictate_core::{JobId, Workflow, WorkflowSignal};
use tempfile::tempdir;

fn update(workflow: &Workflow) -> OverlayUpdate {
    OverlayUpdate {
        workflow: workflow.snapshot(),
        active_recording: None,
    }
}

#[test]
fn overlay_process_exists_only_while_a_status_surface_is_visible() {
    let mut state = OverlayProcessState::default();
    assert_eq!(
        state.transition(&update(&Workflow::new())),
        OverlayProcessAction::StayHeadless
    );

    let job_id = JobId::new();
    let mut recording = Workflow::new();
    recording
        .apply(WorkflowSignal::StartRequested { job_id })
        .unwrap();
    assert_eq!(
        state.transition(&update(&recording)),
        OverlayProcessAction::StayHeadless
    );
    recording
        .apply(WorkflowSignal::FirstAudioFrameWritten { job_id })
        .unwrap();
    assert_eq!(
        state.transition(&update(&recording)),
        OverlayProcessAction::Launch
    );
    assert_eq!(
        state.transition(&update(&Workflow::new())),
        OverlayProcessAction::Stop
    );
    assert_eq!(
        state.transition(&update(&Workflow::new())),
        OverlayProcessAction::StayHeadless
    );
}

#[test]
fn visible_workflow_updates_reuse_the_same_notification_process() {
    let job_id = JobId::new();
    let mut recording = Workflow::new();
    recording
        .apply(WorkflowSignal::StartRequested { job_id })
        .unwrap();
    recording
        .apply(WorkflowSignal::FirstAudioFrameWritten { job_id })
        .unwrap();
    let mut state = OverlayProcessState::default();

    assert_eq!(
        state.transition(&update(&recording)),
        OverlayProcessAction::Launch
    );
    assert_eq!(
        state.transition(&update(&recording)),
        OverlayProcessAction::Update
    );
}

#[test]
fn helper_update_serializes_only_overlay_workflow_and_active_recording_metadata() {
    let job_id = JobId::new();
    let mut recording = Workflow::new();
    recording
        .apply(WorkflowSignal::StartRequested { job_id })
        .unwrap();
    recording
        .apply(WorkflowSignal::FirstAudioFrameWritten { job_id })
        .unwrap();
    let update = OverlayUpdate {
        workflow: recording.snapshot(),
        active_recording: Some(ActiveRecordingUpdate {
            audio_path: PathBuf::from("/tmp/recording.wav"),
            started_at_unix_millis: 1_726_000_000_250,
        }),
    };

    let encoded = serde_json::to_string(&update).unwrap();
    let decoded: OverlayUpdate = serde_json::from_str(&encoded).unwrap();

    assert_eq!(decoded, update);
    assert!(encoded.contains("/tmp/recording.wav"));
    assert!(!encoded.contains("last_transcript"));
    assert!(!encoded.contains("recoverable_count"));
    assert_eq!(
        decoded.presentation().active_recording.unwrap().audio_path,
        PathBuf::from("/tmp/recording.wav")
    );
}

#[test]
fn visible_overlay_is_relaunched_when_its_helper_exits_without_an_update() {
    let directory = tempdir().unwrap();
    let executable = directory.path().join("overlay-helper");
    let launches = directory.path().join("launches");
    let received = directory.path().join("received");
    fs::write(
        &executable,
        format!(
            "#!/bin/sh\nprintf 'launch\\n' >> '{}'\ncount=$(wc -l < '{}')\nIFS= read -r line\nprintf '{{\"status\":\"frame_submitted\"}}\\n'\nif [ \"$count\" -eq 1 ]; then\n  exit 17\nfi\nprintf '%s' \"$line\" > '{}'\nwhile IFS= read -r line; do :; done\n",
            launches.display(),
            launches.display(),
            received.display(),
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions).unwrap();

    let job_id = JobId::new();
    let mut recording = Workflow::new();
    recording
        .apply(WorkflowSignal::StartRequested { job_id })
        .unwrap();
    recording
        .apply(WorkflowSignal::FirstAudioFrameWritten { job_id })
        .unwrap();
    let visible = OverlayUpdate {
        workflow: recording.snapshot(),
        active_recording: Some(ActiveRecordingUpdate {
            audio_path: directory.path().join("active-recording.wav"),
            started_at_unix_millis: 1_726_000_000_250,
        }),
    };
    let hidden = update(&Workflow::new());
    let (overlay, presenter) = start_overlay_presenter(executable).unwrap();

    overlay.update(visible);
    let deadline = Instant::now() + Duration::from_secs(2);
    let relaunched = loop {
        let count = fs::read_to_string(&launches)
            .map(|contents| contents.lines().count())
            .unwrap_or_default();
        if count >= 2 && received.exists() {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::yield_now();
    };

    overlay.update(hidden);
    drop(overlay);
    presenter.join().unwrap();
    let observed_launches = fs::read_to_string(&launches).unwrap_or_default();
    assert!(
        relaunched,
        "the visible overlay helper was not relaunched; launches={:?}, received_exists={}",
        observed_launches.lines().count(),
        received.exists(),
    );
    let received = fs::read_to_string(received).unwrap();
    assert!(received.contains("active-recording.wav"));
    assert!(received.contains("1726000000250"));
}

#[test]
fn helper_error_before_readiness_is_killed_and_relaunched() {
    let directory = tempdir().unwrap();
    let executable = directory.path().join("overlay-helper");
    let launches = directory.path().join("launches");
    let received = directory.path().join("received");
    fs::write(
        &executable,
        format!(
            "#!/bin/sh\nprintf 'launch\\n' >> '{}'\ncount=$(wc -l < '{}')\nIFS= read -r line\nif [ \"$count\" -eq 1 ]; then\n  printf '{{\"status\":\"error\",\"message\":\"X11 unavailable\"}}\\n'\n  while IFS= read -r ignored; do :; done\n  exit 17\nfi\nprintf '{{\"status\":\"frame_submitted\"}}\\n'\nprintf '%s' \"$line\" > '{}'\nwhile IFS= read -r ignored; do :; done\n",
            launches.display(),
            launches.display(),
            received.display(),
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions).unwrap();

    let job_id = JobId::new();
    let mut recording = Workflow::new();
    recording
        .apply(WorkflowSignal::StartRequested { job_id })
        .unwrap();
    recording
        .apply(WorkflowSignal::FirstAudioFrameWritten { job_id })
        .unwrap();
    let visible = update(&recording);
    let (overlay, presenter) = start_overlay_presenter(executable).unwrap();

    overlay.update(visible);
    let deadline = Instant::now() + Duration::from_secs(2);
    let recovered = loop {
        let launch_count = fs::read_to_string(&launches)
            .map(|contents| contents.lines().count())
            .unwrap_or_default();
        if launch_count >= 2 && received.exists() {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::yield_now();
    };

    overlay.update(update(&Workflow::new()));
    drop(overlay);
    presenter.join().unwrap();
    assert!(
        recovered,
        "the helper that reported a startup error was not replaced"
    );
}

#[test]
fn created_window_without_a_submitted_frame_is_killed_and_relaunched() {
    let directory = tempdir().unwrap();
    let executable = directory.path().join("overlay-helper");
    let launches = directory.path().join("launches");
    let received = directory.path().join("received");
    fs::write(
        &executable,
        format!(
            "#!/bin/sh\nprintf 'launch\\n' >> '{}'\ncount=$(wc -l < '{}')\nIFS= read -r line\nif [ \"$count\" -eq 1 ]; then\n  printf '{{\"status\":\"window_created\"}}\\n'\n  while IFS= read -r ignored; do :; done\n  exit 17\nfi\nprintf '{{\"status\":\"frame_submitted\"}}\\n'\nprintf '%s' \"$line\" > '{}'\nwhile IFS= read -r ignored; do :; done\n",
            launches.display(),
            launches.display(),
            received.display(),
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions).unwrap();

    let job_id = JobId::new();
    let mut recording = Workflow::new();
    recording
        .apply(WorkflowSignal::StartRequested { job_id })
        .unwrap();
    recording
        .apply(WorkflowSignal::FirstAudioFrameWritten { job_id })
        .unwrap();
    let (overlay, presenter) =
        start_overlay_presenter_with_timeout(executable, Duration::from_millis(50)).unwrap();

    overlay.update(update(&recording));
    let deadline = Instant::now() + Duration::from_secs(2);
    let recovered = loop {
        let launch_count = fs::read_to_string(&launches)
            .map(|contents| contents.lines().count())
            .unwrap_or_default();
        if launch_count >= 2 && received.exists() {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::yield_now();
    };

    overlay.update(update(&Workflow::new()));
    drop(overlay);
    presenter.join().unwrap();
    assert!(
        recovered,
        "the helper that missed its readiness deadline was not replaced"
    );
}

#[test]
fn partial_readiness_message_cannot_bypass_the_startup_deadline() {
    let directory = tempdir().unwrap();
    let executable = directory.path().join("overlay-helper");
    let launches = directory.path().join("launches");
    let received = directory.path().join("received");
    fs::write(
        &executable,
        format!(
            "#!/bin/sh\nprintf 'launch\\n' >> '{}'\ncount=$(wc -l < '{}')\nIFS= read -r line\nif [ \"$count\" -eq 1 ]; then\n  printf '{{\"status\":'\n  while IFS= read -r ignored; do :; done\n  exit 17\nfi\nprintf '{{\"status\":\"frame_submitted\"}}\\n'\nprintf '%s' \"$line\" > '{}'\nwhile IFS= read -r ignored; do :; done\n",
            launches.display(),
            launches.display(),
            received.display(),
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions).unwrap();

    let job_id = JobId::new();
    let mut recording = Workflow::new();
    recording
        .apply(WorkflowSignal::StartRequested { job_id })
        .unwrap();
    recording
        .apply(WorkflowSignal::FirstAudioFrameWritten { job_id })
        .unwrap();
    let (overlay, presenter) =
        start_overlay_presenter_with_timeout(executable, Duration::from_millis(50)).unwrap();

    overlay.update(update(&recording));
    let deadline = Instant::now() + Duration::from_secs(2);
    let recovered = loop {
        let launch_count = fs::read_to_string(&launches)
            .map(|contents| contents.lines().count())
            .unwrap_or_default();
        if launch_count >= 2 && received.exists() {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::yield_now();
    };

    overlay.update(update(&Workflow::new()));
    drop(overlay);
    presenter.join().unwrap();
    assert!(
        recovered,
        "partial helper output bypassed the readiness deadline"
    );
}

#[test]
fn repeated_helper_crashes_are_bounded_until_a_new_visible_update_arrives() {
    let directory = tempdir().unwrap();
    let executable = directory.path().join("crashing-overlay-helper");
    let launches = directory.path().join("launches");
    fs::write(
        &executable,
        format!(
            "#!/bin/sh\nprintf 'launch\\n' >> '{}'\nexit 17\n",
            launches.display(),
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions).unwrap();

    let job_id = JobId::new();
    let mut recording = Workflow::new();
    recording
        .apply(WorkflowSignal::StartRequested { job_id })
        .unwrap();
    recording
        .apply(WorkflowSignal::FirstAudioFrameWritten { job_id })
        .unwrap();
    let visible = update(&recording);
    let (overlay, presenter) = start_overlay_presenter(executable).unwrap();

    overlay.update(visible.clone());
    let first_deadline = Instant::now() + Duration::from_secs(2);
    let mut two_launches_seen_at = None;
    let bounded = loop {
        let count = fs::read_to_string(&launches)
            .map(|contents| contents.lines().count())
            .unwrap_or_default();
        if count > 2 {
            break false;
        }
        if count == 2 {
            let seen_at = two_launches_seen_at.get_or_insert_with(Instant::now);
            if seen_at.elapsed() >= Duration::from_millis(100) {
                break true;
            }
        }
        if Instant::now() >= first_deadline {
            break count == 2;
        }
        std::thread::yield_now();
    };

    let recovered_after_update = if bounded {
        overlay.update(visible);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let count = fs::read_to_string(&launches)
                .map(|contents| contents.lines().count())
                .unwrap_or_default();
            if count >= 4 {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::yield_now();
        }
    } else {
        false
    };

    overlay.update(update(&Workflow::new()));
    drop(overlay);
    presenter.join().unwrap();
    assert!(bounded, "one update caused more than two helper launches");
    assert!(
        recovered_after_update,
        "a new visible update did not reset the helper restart budget"
    );
}

#[test]
fn dismissal_acknowledges_only_after_the_helper_exits() {
    let directory = tempdir().unwrap();
    let executable = directory.path().join("overlay-helper");
    let exited = directory.path().join("exited");
    fs::write(
        &executable,
        format!(
            "#!/bin/sh\nIFS= read -r line\nprintf '{{\"status\":\"frame_submitted\"}}\\n'\nwhile IFS= read -r line; do :; done\nprintf 'exited' > '{}'\n",
            exited.display(),
        ),
    )
    .unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions).unwrap();

    let job_id = JobId::new();
    let mut recording = Workflow::new();
    recording
        .apply(WorkflowSignal::StartRequested { job_id })
        .unwrap();
    recording
        .apply(WorkflowSignal::FirstAudioFrameWritten { job_id })
        .unwrap();
    let (overlay, presenter) = start_overlay_presenter(executable).unwrap();
    overlay.update(update(&recording));

    overlay.dismiss_and_wait().unwrap();

    assert_eq!(fs::read_to_string(exited).unwrap(), "exited");
    drop(overlay);
    presenter.join().unwrap();
}

#[test]
fn dismissal_without_a_helper_is_immediately_acknowledged() {
    let directory = tempdir().unwrap();
    let executable = directory.path().join("unused-overlay-helper");
    let (overlay, presenter) = start_overlay_presenter(executable).unwrap();

    overlay.dismiss_and_wait().unwrap();

    drop(overlay);
    presenter.join().unwrap();
}

#[test]
fn dismissal_timeout_comfortably_covers_the_helper_fade() {
    // The dismissal ack includes the helper's fade-out before process exit.
    assert!(agentdictate_app::OVERLAY_TEARDOWN_TIMEOUT >= 4 * agentdictate_ui::OVERLAY_FADE_HOLD);
}

#[test]
fn presentation_error_after_a_frame_reports_unavailable_until_recovery() {
    let directory = tempdir().unwrap();
    let executable = directory.path().join("overlay-helper");
    let launches = directory.path().join("launches");
    fs::write(
        &executable,
        format!(
            r#"#!/bin/sh
IFS= read -r line
printf 'launch\n' >> '{}'
count=$(wc -l < '{}')
printf '{{"status":"window_created"}}\n'
if [ "$count" -eq 2 ]; then
    IFS= read -r line
fi
printf '{{"status":"frame_submitted"}}\n'
if [ "$count" -eq 1 ]; then
    IFS= read -r line
    printf '{{"status":"error","message":"display connection lost"}}\n'
fi
while IFS= read -r line; do :; done
"#,
            launches.display(),
            launches.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    let (overlay, presenter) = start_overlay_presenter(executable).unwrap();
    let mut recording = Workflow::new();
    let job_id = JobId::new();
    recording
        .apply(WorkflowSignal::StartRequested { job_id })
        .unwrap();
    recording
        .apply(WorkflowSignal::FirstAudioFrameWritten { job_id })
        .unwrap();
    let wait_for = |condition: &dyn Fn() -> bool| {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !condition() {
            assert!(Instant::now() < deadline, "overlay health did not converge");
            std::thread::yield_now();
        }
    };
    overlay.update(update(&recording));
    wait_for(&|| launches.exists());
    overlay.update(update(&recording));
    wait_for(&|| {
        overlay.is_unavailable() && fs::read_to_string(&launches).unwrap().lines().count() == 2
    });
    overlay.update(update(&recording));
    wait_for(&|| !overlay.is_unavailable());
    overlay.dismiss_and_wait().unwrap();
    drop(overlay);
    presenter.join().unwrap();
}
