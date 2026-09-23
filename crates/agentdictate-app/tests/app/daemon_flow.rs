use std::{
    collections::VecDeque,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use agentdictate_app::{
    AppPaths, CapturedRecording, Daemon, DaemonError, DaemonStatus, OverlayUpdate, RecorderEvent,
    RecordingController, Transcriber, TranscriptionCompletion, start_overlay_presenter,
};
use agentdictate_core::{
    HistoryPageRequest, HistorySnapshot, HotkeyReadiness, JobStage, ProcessingStage, Settings,
    WorkflowPhase, parse_vocabulary,
};
use agentdictate_runtime::{
    Deliverer, DeliveryDisposition, DeliveryMethod, ExternalError, Recorder, RecordingJob, Runtime,
    Transcript,
};
use rusqlite::params;
use tempfile::tempdir;

use super::support::{FailingStartRecorder, FixedTranscriber, InspectingRecorder, finish};

struct FailingFinishRecorder;

#[derive(Default)]
struct PreservingRecorder {
    finish_attempts: usize,
}

impl Recorder for PreservingRecorder {
    fn start(&mut self, job: &RecordingJob) -> Result<(), ExternalError> {
        std::fs::write(&job.audio_path, b"RIFFpreserved audio").unwrap();
        Ok(())
    }
}

impl RecordingController for PreservingRecorder {
    fn finish(&mut self, _job: &RecordingJob) -> Result<CapturedRecording, ExternalError> {
        self.finish_attempts += 1;
        Ok(CapturedRecording {
            duration_seconds: 27.5,
        })
    }
}

impl Recorder for FailingFinishRecorder {
    fn start(&mut self, job: &RecordingJob) -> Result<(), ExternalError> {
        std::fs::write(&job.audio_path, b"RIFFpartial audio").unwrap();
        Ok(())
    }
}

impl RecordingController for FailingFinishRecorder {
    fn finish(&mut self, _job: &RecordingJob) -> Result<CapturedRecording, ExternalError> {
        Err(ExternalError::new("recorder disappeared"))
    }
}

/// Writes audio and reports a recording of `seconds`.
struct TimedRecorder {
    seconds: f64,
}

impl Recorder for TimedRecorder {
    fn start(&mut self, job: &RecordingJob) -> Result<(), ExternalError> {
        std::fs::write(&job.audio_path, b"RIFFtimed audio").unwrap();
        Ok(())
    }
}

impl RecordingController for TimedRecorder {
    fn finish(&mut self, _job: &RecordingJob) -> Result<CapturedRecording, ExternalError> {
        Ok(CapturedRecording {
            duration_seconds: self.seconds,
        })
    }
}

#[derive(Default)]
struct SubmittedDelivery {
    attempts: usize,
    methods: Vec<DeliveryMethod>,
}

#[test]
fn empty_dictation_finishes_without_delivery_history_or_recovery_and_allows_the_next_start() {
    #[derive(Clone)]
    struct Empty;
    impl Transcriber for Empty {
        fn transcribe(&mut self, _: &RecordingJob) -> Result<Transcript, ExternalError> {
            Err(ExternalError::NoSpeech)
        }
    }
    let dir = tempdir().unwrap();
    let paths = app_paths(dir.path());
    std::fs::create_dir_all(paths.database_file.parent().unwrap()).unwrap();
    let runtime = Runtime::open(&paths.database_file).unwrap();
    let mut daemon = Daemon::new(
        runtime,
        Settings::default(),
        paths.clone(),
        PreservingRecorder::default(),
        Empty,
        SubmittedDelivery::default(),
    );
    daemon.start_recording().unwrap();
    let finished = finish(&mut daemon);
    assert_eq!(finished.stage, JobStage::NoSpeech);
    assert_eq!(daemon.snapshot().workflow.phase, WorkflowPhase::Ready);
    assert_eq!(daemon.snapshot().recoverable_count, 0);
    assert_eq!(daemon.deliverer().attempts, 0);
    assert!(!finished.audio_path.exists());
    assert!(daemon.workspace_snapshot().unwrap().recoveries.is_empty());
    let observer = Runtime::open_observer(&paths.database_file).unwrap();
    assert!(history_rows(&observer).is_empty());
    assert!(observer.recoverable_jobs().unwrap().is_empty());
    assert!(observer.job(finished.id).unwrap().is_none());
    daemon.start_recording().unwrap();
}

impl Deliverer for SubmittedDelivery {
    fn deliver(
        &mut self,
        _job: &RecordingJob,
        method: DeliveryMethod,
    ) -> Result<DeliveryDisposition, ExternalError> {
        self.attempts += 1;
        self.methods.push(method);
        Ok(DeliveryDisposition::Submitted {
            copied_to_clipboard: true,
            paste_triggered: method == DeliveryMethod::Paste,
        })
    }
}

struct ExitInspectingDelivery {
    helper_exited: PathBuf,
    delivered_after_exit: bool,
}

impl Deliverer for ExitInspectingDelivery {
    fn deliver(
        &mut self,
        _job: &RecordingJob,
        _: DeliveryMethod,
    ) -> Result<DeliveryDisposition, ExternalError> {
        self.delivered_after_exit = self.helper_exited.is_file();
        Ok(DeliveryDisposition::Submitted {
            copied_to_clipboard: true,
            paste_triggered: true,
        })
    }
}

/// Records whether the daemon already reported recording while the recorder
/// started, so an Esc pressed during a start is not dropped.
#[derive(Default)]
struct FlagInspectingRecorder {
    status: Option<Arc<DaemonStatus>>,
    flag_set_during_start: bool,
}

impl Recorder for FlagInspectingRecorder {
    fn start(&mut self, job: &RecordingJob) -> Result<(), ExternalError> {
        self.flag_set_during_start = self
            .status
            .as_ref()
            .is_some_and(|status| status.is_recording());
        std::fs::write(&job.audio_path, b"RIFFcaptured audio").unwrap();
        Ok(())
    }
}

impl RecordingController for FlagInspectingRecorder {
    fn finish(&mut self, _job: &RecordingJob) -> Result<CapturedRecording, ExternalError> {
        Ok(CapturedRecording {
            duration_seconds: 2.0,
        })
    }
}

#[test]
fn recording_flag_is_set_only_while_a_recording_starts_or_runs() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    std::fs::create_dir_all(paths.database_file.parent().unwrap()).unwrap();
    let mut daemon = Daemon::new(
        Runtime::open(&paths.database_file).unwrap(),
        Settings::default(),
        paths,
        FlagInspectingRecorder::default(),
        FixedTranscriber,
        SubmittedDelivery::default(),
    );
    let status = daemon.status();
    daemon.recorder_mut().status = Some(Arc::clone(&status));
    assert!(!status.is_recording());

    daemon.start_recording().unwrap();
    assert!(daemon.recorder().flag_set_during_start);
    assert!(status.is_recording());
    daemon.discard_recording().unwrap();
    assert!(!status.is_recording());

    daemon.start_recording().unwrap();
    assert!(status.is_recording());
    finish(&mut daemon);
    assert!(!status.is_recording());
}

#[test]
fn daemon_checkpoints_audio_before_capture_and_transcript_before_delivery() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    std::fs::create_dir_all(paths.database_file.parent().unwrap()).unwrap();
    let runtime = Runtime::open(&paths.database_file).unwrap();
    let recorder = InspectingRecorder {
        database: paths.database_file.clone(),
        started_after_checkpoint: false,
    };
    let mut daemon = Daemon::new(
        runtime,
        Settings::default(),
        paths.clone(),
        recorder,
        FixedTranscriber,
        SubmittedDelivery::default(),
    );

    let started = daemon.start_recording().unwrap();
    assert!(daemon.recorder().started_after_checkpoint);
    assert_eq!(started.stage, JobStage::Recording);
    assert!(matches!(
        daemon.snapshot().workflow.phase,
        WorkflowPhase::Recording { job_id } if job_id == started.id
    ));
    assert_eq!(daemon.snapshot().recoverable_count, 0);
    assert!(daemon.workspace_snapshot().unwrap().recoveries.is_empty());

    let delivered = finish(&mut daemon);

    assert_eq!(delivered.stage, JobStage::Delivered);
    assert_eq!(delivered.raw_transcript, "Final transcript.");
    assert_eq!(delivered.final_text, "Final transcript.");
    assert!(!delivered.audio_path.exists());
    assert_eq!(daemon.deliverer().attempts, 1);
    assert_eq!(daemon.snapshot().workflow.phase, WorkflowPhase::Ready);
    assert_eq!(
        daemon.snapshot().last_transcript.as_deref(),
        Some("Final transcript.")
    );
    assert_eq!(daemon.snapshot().hotkey, HotkeyReadiness::Starting);
    let observer = Runtime::open_observer(&paths.database_file).unwrap();
    let history = history_rows(&observer);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].preview_text, "Final transcript.");
    // The transcript now lives only in History.
    assert!(observer.job(delivered.id).unwrap().is_none());
}

#[test]
fn daemon_waits_for_an_unconfirmed_overlay_to_exit_before_delivery() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    std::fs::create_dir_all(paths.database_file.parent().unwrap()).unwrap();
    let executable = directory.path().join("overlay-helper");
    let received = directory.path().join("received");
    let helper_exited = directory.path().join("helper-exited");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\nwhile IFS= read -r line; do printf '%s\\n' \"$line\" >> '{}'; done\nprintf 'exited' > '{}'\n",
            received.display(),
            helper_exited.display(),
        ),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&executable, permissions).unwrap();
    let (overlay, presenter) = start_overlay_presenter(executable).unwrap();
    let runtime = Runtime::open(&paths.database_file).unwrap();
    let recorder = InspectingRecorder {
        database: paths.database_file.clone(),
        started_after_checkpoint: false,
    };
    let mut daemon = Daemon::new(
        runtime,
        Settings::default(),
        paths,
        recorder,
        FixedTranscriber,
        ExitInspectingDelivery {
            helper_exited: helper_exited.clone(),
            delivered_after_exit: false,
        },
    );
    daemon.set_overlay_controller(overlay);

    let started = daemon.start_recording().unwrap();
    let delivered = finish(&mut daemon);

    assert_eq!(delivered.stage, JobStage::Delivered);
    assert!(daemon.deliverer().delivered_after_exit);
    assert_eq!(std::fs::read_to_string(helper_exited).unwrap(), "exited");
    let received = std::fs::read_to_string(received).unwrap();
    let updates = received
        .lines()
        .map(|line| serde_json::from_str::<OverlayUpdate>(line).unwrap())
        .collect::<Vec<_>>();
    // The helper launches at the start request, before the microphone is ready.
    assert_eq!(
        updates[0].workflow.phase,
        WorkflowPhase::Starting { job_id: started.id }
    );
    assert_eq!(updates[0].active_recording, None);
    let encoded = received.lines().nth(1).unwrap();
    let recording = &updates[1];
    assert_eq!(
        recording
            .active_recording
            .as_ref()
            .map(|active| active.audio_path.as_path()),
        Some(started.audio_path.as_path())
    );
    let overlay_started_at = recording
        .active_recording
        .as_ref()
        .map(|active| active.started_at_unix_millis)
        .unwrap();
    assert!(overlay_started_at >= started.started_at.timestamp_millis());
    assert!(matches!(
        recording.workflow.phase,
        WorkflowPhase::Recording { job_id } if job_id == started.id
    ));
    assert!(!encoded.contains("last_transcript"));
    assert!(!encoded.contains("recoverable_count"));
    drop(daemon);
    presenter.join().unwrap();
}

#[test]
fn recorder_finalize_failure_is_immediately_durable_and_recoverable() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    std::fs::create_dir_all(paths.database_file.parent().unwrap()).unwrap();
    let runtime = Runtime::open(&paths.database_file).unwrap();
    let mut daemon = Daemon::new(
        runtime,
        Settings::default(),
        paths.clone(),
        FailingFinishRecorder,
        FixedTranscriber,
        SubmittedDelivery::default(),
    );
    let started = daemon.start_recording().unwrap();

    assert!(daemon.stop_recording().is_err());

    let observer = Runtime::open_observer(&paths.database_file).unwrap();
    let interrupted = observer.job(started.id).unwrap().unwrap();
    assert_eq!(interrupted.stage, JobStage::Interrupted);
    assert!(interrupted.audio_path.exists());
    assert!(
        interrupted
            .error_message
            .as_deref()
            .unwrap()
            .contains("recorder disappeared")
    );
    assert_eq!(daemon.snapshot().recoverable_count, 1);
}

#[test]
fn stop_capture_checkpoint_failure_clears_the_session_and_preserves_audio() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    std::fs::create_dir_all(paths.database_file.parent().unwrap()).unwrap();
    let runtime = Runtime::open(&paths.database_file).unwrap();
    let mut daemon = Daemon::new(
        runtime,
        Settings::default(),
        paths.clone(),
        PreservingRecorder::default(),
        FixedTranscriber,
        SubmittedDelivery::default(),
    );
    let started = daemon.start_recording().unwrap();
    let connection = rusqlite::Connection::open(&paths.database_file).unwrap();
    reject_capture_checkpoint(&connection);

    let error = daemon.stop_recording().unwrap_err();

    assert!(error.to_string().contains("capture checkpoint unavailable"));
    assert_eq!(daemon.recorder().finish_attempts, 1);
    assert!(matches!(
        daemon.snapshot().workflow.phase,
        WorkflowPhase::NeedsAttention {
            job_id,
            at: JobStage::Interrupted,
        } if job_id == started.id
    ));
    assert_eq!(daemon.snapshot().recoverable_count, 1);
    let observer = Runtime::open_observer(&paths.database_file).unwrap();
    let interrupted = observer.job(started.id).unwrap().unwrap();
    assert_eq!(interrupted.stage, JobStage::Interrupted);
    assert!(interrupted.audio_path.is_file());
    drop(observer);

    connection
        .execute_batch("DROP TRIGGER reject_capture_checkpoint")
        .unwrap();
    let next = daemon.start_recording().unwrap();
    assert_ne!(next.id, started.id);
    daemon.discard_recording().unwrap();
}

/// Pastes never get through and copies always do. Records every request.
#[derive(Default)]
struct PasteFailsCopyWorks {
    methods: Vec<DeliveryMethod>,
}

impl Deliverer for PasteFailsCopyWorks {
    fn deliver(
        &mut self,
        _job: &RecordingJob,
        method: DeliveryMethod,
    ) -> Result<DeliveryDisposition, ExternalError> {
        self.methods.push(method);
        Ok(match method {
            DeliveryMethod::Paste => DeliveryDisposition::NotSent {
                copied_to_clipboard: false,
                reason: "could not find the focused window, so nothing was pasted".to_owned(),
            },
            DeliveryMethod::CopyOnly => DeliveryDisposition::Submitted {
                copied_to_clipboard: true,
                paste_triggered: false,
            },
        })
    }
}

#[test]
fn recovery_retries_copy_the_text_and_never_paste_into_the_focused_window() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    std::fs::create_dir_all(paths.database_file.parent().unwrap()).unwrap();
    let runtime = Runtime::open(&paths.database_file).unwrap();
    let mut daemon = Daemon::new(
        runtime,
        Settings::default(),
        paths.clone(),
        PreservingRecorder::default(),
        FixedTranscriber,
        PasteFailsCopyWorks::default(),
    );
    daemon.start_recording().unwrap();
    let unpasted = finish(&mut daemon);
    assert_eq!(unpasted.stage, JobStage::ReadyToDeliver);
    let interrupted = daemon.start_recording().unwrap();
    exit_recorder(&mut daemon, interrupted.id);

    let copied = daemon.retry_delivery(unpasted.id).unwrap();
    let ticket = daemon.retry_transcription(interrupted.id).unwrap();
    let transcribed = daemon.complete_transcription(ticket.run()).unwrap();

    assert_eq!(
        daemon.deliverer().methods,
        [
            DeliveryMethod::Paste,
            DeliveryMethod::CopyOnly,
            DeliveryMethod::CopyOnly
        ]
    );
    assert_eq!(copied.stage, JobStage::Delivered);
    assert_eq!(transcribed.stage, JobStage::Delivered);
    assert_eq!(daemon.snapshot().workflow.phase, WorkflowPhase::Ready);
    assert_eq!(daemon.snapshot().recoverable_count, 0);
    let recorded: i64 = rusqlite::Connection::open(&paths.database_file)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM dictations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(recorded, 2);
}

#[test]
fn failure_after_transcription_keeps_the_raw_transcript_in_recovery_and_the_next_start_works() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    std::fs::create_dir_all(paths.database_file.parent().unwrap()).unwrap();
    let runtime = Runtime::open(&paths.database_file).unwrap();
    let mut daemon = Daemon::new(
        runtime,
        Settings::default(),
        paths.clone(),
        PreservingRecorder::default(),
        FixedTranscriber,
        SubmittedDelivery::default(),
    );
    let started = daemon.start_recording().unwrap();
    let connection = rusqlite::Connection::open(&paths.database_file).unwrap();
    connection
        .execute_batch(
            r#"
            CREATE TRIGGER reject_ready_checkpoint
            BEFORE UPDATE OF stage ON dictation_jobs
            WHEN NEW.stage = 'ready_to_deliver'
            BEGIN
                SELECT RAISE(FAIL, 'ready checkpoint unavailable');
            END;
            "#,
        )
        .unwrap();

    let ticket = daemon.stop_recording().unwrap();
    let error = daemon.complete_transcription(ticket.run()).unwrap_err();

    assert!(error.to_string().contains("ready checkpoint unavailable"));
    assert_eq!(daemon.deliverer().attempts, 0);
    assert!(matches!(
        daemon.snapshot().workflow.phase,
        WorkflowPhase::NeedsAttention {
            job_id,
            at: JobStage::Failed,
        } if job_id == started.id
    ));
    let observer = Runtime::open_observer(&paths.database_file).unwrap();
    let failed = observer.job(started.id).unwrap().unwrap();
    assert_eq!(failed.stage, JobStage::Failed);
    assert_eq!(failed.raw_transcript, "Final transcript.");
    assert!(
        observer
            .recoveries()
            .unwrap()
            .iter()
            .any(|entry| entry.job_id == started.id)
    );
    drop(observer);
    let next = daemon.start_recording().unwrap();
    assert_ne!(next.id, started.id);
}

#[test]
fn deleting_an_older_recovery_item_while_recording_keeps_the_recording_stoppable() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    std::fs::create_dir_all(paths.database_file.parent().unwrap()).unwrap();
    let runtime = Runtime::open(&paths.database_file).unwrap();
    let mut daemon = Daemon::new(
        runtime,
        Settings::default(),
        paths,
        PreservingRecorder::default(),
        FixedTranscriber,
        SubmittedDelivery::default(),
    );
    let older = daemon.start_recording().unwrap();
    exit_recorder(&mut daemon, older.id);
    let recording = daemon.start_recording().unwrap();

    daemon.delete_recovery(older.id).unwrap();

    assert!(matches!(
        daemon.snapshot().workflow.phase,
        WorkflowPhase::Recording { job_id } if job_id == recording.id
    ));
    let delivered = finish(&mut daemon);
    assert_eq!(delivered.id, recording.id);
    assert_eq!(delivered.stage, JobStage::Delivered);
    // Once for the older recording, once for the stop.
    assert_eq!(daemon.recorder().finish_attempts, 2);
    assert_eq!(daemon.snapshot().workflow.phase, WorkflowPhase::Ready);
}

/// Five seconds is the longest recording Esc deletes outright.
#[test]
fn escape_discards_a_short_dictation_and_keeps_its_audio_only_when_retention_is_enabled() {
    for preserve_temp_audio in [false, true] {
        let directory = tempdir().unwrap();
        let paths = app_paths(directory.path());
        std::fs::create_dir_all(paths.database_file.parent().unwrap()).unwrap();
        let runtime = Runtime::open(&paths.database_file).unwrap();
        let settings = Settings {
            preserve_temp_audio,
            ..Settings::default()
        };
        let mut daemon = Daemon::new(
            runtime,
            settings,
            paths.clone(),
            TimedRecorder { seconds: 5.0 },
            FixedTranscriber,
            SubmittedDelivery::default(),
        );
        let started = daemon.start_recording().unwrap();

        let discarded = daemon.discard_recording().unwrap();

        assert_eq!(discarded.stage, JobStage::Deleted);
        assert_eq!(started.audio_path.exists(), preserve_temp_audio);
        assert_eq!(daemon.snapshot().workflow.phase, WorkflowPhase::Ready);
        assert_eq!(daemon.snapshot().recoverable_count, 0);
        assert!(daemon.workspace_snapshot().unwrap().recoveries.is_empty());
        let observer = Runtime::open_observer(&paths.database_file).unwrap();
        assert!(observer.job(started.id).unwrap().is_none());
    }
}

/// Esc after a long take keeps it in Recovery for a day without asking for
/// attention; "Transcribe" there copies the text like any Recovery retry.
#[test]
fn escape_after_a_long_recording_keeps_it_in_recovery_and_transcribing_it_copies() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    let mut daemon = daemon_with(&paths, Settings::default(), FixedTranscriber);
    let started = daemon.start_recording().unwrap();

    let cancelled = daemon.discard_recording().unwrap();

    assert_eq!(cancelled.stage, JobStage::Cancelled);
    assert!(started.audio_path.is_file());
    assert_eq!(daemon.phase(), WorkflowPhase::Ready);
    assert_eq!(daemon.snapshot().recoverable_count, 1);
    let recovery = daemon.workspace_snapshot().unwrap().recoveries.remove(0);
    assert_eq!(recovery.job_id, started.id);
    assert_eq!(recovery.stage, JobStage::Cancelled);
    assert_eq!(
        recovery.expires_at - recovery.updated_at,
        chrono::TimeDelta::days(1)
    );

    let ticket = daemon.retry_transcription(started.id).unwrap();
    let copied = daemon.complete_transcription(ticket.run()).unwrap();

    assert_eq!(copied.stage, JobStage::Delivered);
    assert_eq!(daemon.deliverer().methods, [DeliveryMethod::CopyOnly]);
    assert_eq!(daemon.snapshot().recoverable_count, 0);
    assert_eq!(daemon.phase(), WorkflowPhase::Ready);
}

#[test]
fn failed_escape_delete_restores_audio_and_surfaces_recovery_attention() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    std::fs::create_dir_all(paths.database_file.parent().unwrap()).unwrap();
    let runtime = Runtime::open(&paths.database_file).unwrap();
    let mut daemon = Daemon::new(
        runtime,
        Settings::default(),
        paths.clone(),
        TimedRecorder { seconds: 2.0 },
        FixedTranscriber,
        SubmittedDelivery::default(),
    );
    let started = daemon.start_recording().unwrap();
    rusqlite::Connection::open(&paths.database_file)
        .unwrap()
        .execute_batch(
            r#"
            CREATE TRIGGER reject_escape_delete
            BEFORE DELETE ON dictation_jobs
            BEGIN
                SELECT RAISE(ABORT, 'forced discard failure');
            END;
            "#,
        )
        .unwrap();

    let error = daemon.discard_recording().unwrap_err();

    assert!(error.to_string().contains("forced discard failure"));
    assert!(started.audio_path.is_file());
    assert!(matches!(
        daemon.snapshot().workflow.phase,
        WorkflowPhase::NeedsAttention {
            job_id,
            at: JobStage::Captured,
        } if job_id == started.id
    ));
    assert_eq!(daemon.snapshot().recoverable_count, 1);
}

#[test]
fn escape_finalize_failure_preserves_partial_audio_for_recovery() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    std::fs::create_dir_all(paths.database_file.parent().unwrap()).unwrap();
    let runtime = Runtime::open(&paths.database_file).unwrap();
    let mut daemon = Daemon::new(
        runtime,
        Settings::default(),
        paths.clone(),
        FailingFinishRecorder,
        FixedTranscriber,
        SubmittedDelivery::default(),
    );
    let started = daemon.start_recording().unwrap();

    let interrupted = daemon.discard_recording().unwrap();

    assert_eq!(interrupted.stage, JobStage::Interrupted);
    assert!(interrupted.audio_path.is_file());
    assert!(matches!(
        daemon.snapshot().workflow.phase,
        WorkflowPhase::NeedsAttention {
            job_id,
            at: JobStage::Interrupted,
        } if job_id == started.id
    ));
    assert_eq!(daemon.snapshot().recoverable_count, 1);
}

#[test]
fn discard_capture_checkpoint_failure_clears_the_session_and_preserves_audio() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    std::fs::create_dir_all(paths.database_file.parent().unwrap()).unwrap();
    let runtime = Runtime::open(&paths.database_file).unwrap();
    let mut daemon = Daemon::new(
        runtime,
        Settings::default(),
        paths.clone(),
        PreservingRecorder::default(),
        FixedTranscriber,
        SubmittedDelivery::default(),
    );
    let started = daemon.start_recording().unwrap();
    let connection = rusqlite::Connection::open(&paths.database_file).unwrap();
    reject_capture_checkpoint(&connection);

    let error = daemon.discard_recording().unwrap_err();

    assert!(error.to_string().contains("capture checkpoint unavailable"));
    assert_eq!(daemon.recorder().finish_attempts, 1);
    assert!(matches!(
        daemon.snapshot().workflow.phase,
        WorkflowPhase::NeedsAttention {
            job_id,
            at: JobStage::Interrupted,
        } if job_id == started.id
    ));
    assert_eq!(daemon.snapshot().recoverable_count, 1);
    let observer = Runtime::open_observer(&paths.database_file).unwrap();
    let interrupted = observer.job(started.id).unwrap().unwrap();
    assert_eq!(interrupted.stage, JobStage::Interrupted);
    assert!(interrupted.audio_path.is_file());
    drop(observer);

    connection
        .execute_batch("DROP TRIGGER reject_capture_checkpoint")
        .unwrap();
    let next = daemon.start_recording().unwrap();
    assert_ne!(next.id, started.id);
    daemon.discard_recording().unwrap();
}

#[test]
fn recorder_start_failure_is_published_as_recoverable_attention() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    std::fs::create_dir_all(paths.database_file.parent().unwrap()).unwrap();
    let runtime = Runtime::open(&paths.database_file).unwrap();
    let mut daemon = Daemon::new(
        runtime,
        Settings::default(),
        paths,
        FailingStartRecorder,
        FixedTranscriber,
        SubmittedDelivery::default(),
    );

    assert!(daemon.start_recording().is_err());

    assert_eq!(daemon.snapshot().recoverable_count, 1);
    assert!(matches!(
        daemon.snapshot().workflow.phase,
        WorkflowPhase::NeedsAttention {
            at: JobStage::Interrupted,
            ..
        }
    ));
}

#[test]
fn unexpected_recorder_exit_preserves_audio_for_recovery_without_transcribing() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    std::fs::create_dir_all(paths.database_file.parent().unwrap()).unwrap();
    let runtime = Runtime::open(&paths.database_file).unwrap();
    let recorder = InspectingRecorder {
        database: paths.database_file.clone(),
        started_after_checkpoint: false,
    };
    let mut daemon = Daemon::new(
        runtime,
        Settings::default(),
        paths.clone(),
        recorder,
        FixedTranscriber,
        SubmittedDelivery::default(),
    );
    let started = daemon.start_recording().unwrap();

    exit_recorder(&mut daemon, started.id);
    let recovered = Runtime::open_observer(&paths.database_file)
        .unwrap()
        .job(started.id)
        .unwrap()
        .unwrap();

    assert_eq!(recovered.stage, JobStage::Interrupted);
    assert!(recovered.audio_path.is_file());
    assert!(recovered.final_text.is_empty());
    assert!(matches!(
        daemon.snapshot().workflow.phase,
        WorkflowPhase::NeedsAttention {
            job_id,
            at: JobStage::Interrupted,
        } if job_id == started.id
    ));
    assert_eq!(daemon.snapshot().recoverable_count, 1);
    assert!(
        daemon
            .recorder_event(RecorderEvent::Exited { job_id: started.id })
            .unwrap()
            .is_none()
    );
    let observer = Runtime::open_observer(&paths.database_file).unwrap();
    assert!(history_rows(&observer).is_empty());
}

#[test]
fn graceful_shutdown_finalizes_and_preserves_active_audio_for_recovery() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    std::fs::create_dir_all(paths.database_file.parent().unwrap()).unwrap();
    let runtime = Runtime::open(&paths.database_file).unwrap();
    let recorder = InspectingRecorder {
        database: paths.database_file.clone(),
        started_after_checkpoint: false,
    };
    let mut daemon = Daemon::new(
        runtime,
        Settings::default(),
        paths.clone(),
        recorder,
        FixedTranscriber,
        SubmittedDelivery::default(),
    );
    let started = daemon.start_recording().unwrap();

    daemon.shutdown().unwrap();

    let observer = Runtime::open_observer(&paths.database_file).unwrap();
    let preserved = observer.job(started.id).unwrap().unwrap();
    assert_eq!(preserved.stage, JobStage::Interrupted);
    assert!(preserved.audio_path.is_file());
    assert!(
        preserved
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("shut down"))
    );
    assert!(matches!(
        daemon.snapshot().workflow.phase,
        WorkflowPhase::NeedsAttention {
            job_id,
            at: JobStage::Interrupted,
        } if job_id == started.id
    ));
    assert_eq!(daemon.snapshot().recoverable_count, 1);
}

#[test]
fn workspace_history_is_bounded_even_when_the_archive_is_large() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    std::fs::create_dir_all(paths.database_file.parent().unwrap()).unwrap();
    drop(Runtime::open(&paths.database_file).unwrap());
    let mut connection = rusqlite::Connection::open(&paths.database_file).unwrap();
    let transaction = connection.transaction().unwrap();
    let full_body = "é".repeat(200);
    for index in 0..251 {
        let timestamp = format!("2026-08-18T12:{:02}:{:02}Z", index / 60, index % 60);
        transaction
            .execute(
                r#"
                INSERT INTO dictations (
                    started_at, ended_at, duration_seconds, transcription_provider,
                    transcription_model, word_count, character_count, estimated_cost,
                    final_text
                ) VALUES (?1, ?1, 1, 'openai_api', 'test-model', 1, ?2, 0, ?3)
                "#,
                params![timestamp, full_body.chars().count() as u64, full_body],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    drop(connection);
    let runtime = Runtime::open(&paths.database_file).unwrap();
    let daemon = Daemon::new(
        runtime,
        Settings::default(),
        paths.clone(),
        InspectingRecorder {
            database: paths.database_file,
            started_after_checkpoint: false,
        },
        FixedTranscriber,
        SubmittedDelivery::default(),
    );

    let workspace = daemon.workspace_snapshot().unwrap();

    let history = workspace.history;
    assert_eq!(history.rows.len(), 30);
    assert_eq!(history.total_matches, 251);
    assert!(history.next_cursor.is_some());
    assert!(!history.rows.iter().any(|entry| entry.id == 1));
    assert!(history.rows.iter().all(|entry| {
        entry.preview_text.chars().count() == 161
            && entry.preview_text.ends_with('…')
            && entry.text == full_body
    }));
}

/// Replies with its script in order, counting calls across clones, and
/// reports the API key it was configured with when the job started.
#[derive(Clone, Default)]
struct ScriptedTranscriber {
    replies: Arc<Mutex<VecDeque<Result<String, ExternalError>>>>,
    calls: Arc<AtomicUsize>,
    api_key: String,
}

impl ScriptedTranscriber {
    fn replying(replies: impl IntoIterator<Item = Result<String, ExternalError>>) -> Self {
        Self {
            replies: Arc::new(Mutex::new(replies.into_iter().collect())),
            ..Self::default()
        }
    }
}

impl Transcriber for ScriptedTranscriber {
    fn transcribe(&mut self, _job: &RecordingJob) -> Result<Transcript, ExternalError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(format!("versel, sent with key {}.", self.api_key)));
        reply.map(|text| Transcript {
            text,
            model: "gpt-transcribe".into(),
        })
    }

    fn update_settings(&mut self, settings: &Settings) {
        self.api_key.clone_from(&settings.openai_api_key);
    }
}

fn daemon_with<T: Transcriber>(
    paths: &AppPaths,
    settings: Settings,
    transcriber: T,
) -> Daemon<PreservingRecorder, T, SubmittedDelivery> {
    std::fs::create_dir_all(paths.database_file.parent().unwrap()).unwrap();
    Daemon::new(
        Runtime::open(&paths.database_file).unwrap(),
        settings,
        paths.clone(),
        PreservingRecorder::default(),
        transcriber,
        SubmittedDelivery::default(),
    )
}

#[test]
fn stop_checkpoints_transcribing_and_returns_before_any_transcription() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    let transcriber = ScriptedTranscriber::default();
    let calls = Arc::clone(&transcriber.calls);
    let mut daemon = daemon_with(&paths, Settings::default(), transcriber);
    let started = daemon.start_recording().unwrap();

    let ticket = daemon.stop_recording().unwrap();

    let observer = Runtime::open_observer(&paths.database_file).unwrap();
    assert_eq!(
        observer.job(started.id).unwrap().unwrap().stage,
        JobStage::Transcribing
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        daemon.phase(),
        WorkflowPhase::Processing {
            job_id: started.id,
            stage: ProcessingStage::Transcribing,
        }
    );
    let delivered = daemon.complete_transcription(ticket.run()).unwrap();
    assert_eq!(delivered.stage, JobStage::Delivered);
    assert_eq!(daemon.deliverer().methods, [DeliveryMethod::Paste]);
    assert_eq!(daemon.phase(), WorkflowPhase::Ready);
}

#[test]
fn cancel_during_processing_keeps_the_transcript_for_recovery_and_never_pastes() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    let mut daemon = daemon_with(&paths, Settings::default(), FixedTranscriber);
    let started = daemon.start_recording().unwrap();
    let ticket = daemon.stop_recording().unwrap();

    assert_eq!(daemon.cancel_processing().unwrap(), started.id);
    assert_eq!(daemon.phase(), WorkflowPhase::Ready);
    let stored = daemon.complete_transcription(ticket.run()).unwrap();

    assert_eq!(daemon.deliverer().attempts, 0);
    assert_eq!(stored.stage, JobStage::ReadyToDeliver);
    assert!(stored.audio_path.is_file());
    let recovery = daemon.workspace_snapshot().unwrap().recoveries.remove(0);
    assert_eq!(recovery.job_id, started.id);
    assert_eq!(recovery.final_text, "Final transcript.");
    assert_eq!(
        recovery.error_message.as_deref(),
        Some("Cancelled before paste")
    );
    assert_eq!(daemon.snapshot().recoverable_count, 1);
}

#[test]
fn late_result_of_a_cancelled_job_does_not_disturb_the_next_recording() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    let mut daemon = daemon_with(&paths, Settings::default(), FixedTranscriber);
    daemon.start_recording().unwrap();
    let cancelled = daemon.stop_recording().unwrap();
    daemon.cancel_processing().unwrap();
    let next = daemon.start_recording().unwrap();

    daemon.complete_transcription(cancelled.run()).unwrap();

    assert_eq!(daemon.phase(), WorkflowPhase::Recording { job_id: next.id });
    assert_eq!(daemon.deliverer().attempts, 0);
    let delivered = finish(&mut daemon);
    assert_eq!(delivered.id, next.id);
    assert_eq!(daemon.deliverer().methods, [DeliveryMethod::Paste]);
}

#[test]
fn start_during_processing_is_rejected_as_busy_without_a_new_job() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    let mut daemon = daemon_with(&paths, Settings::default(), FixedTranscriber);
    daemon.start_recording().unwrap();
    let ticket = daemon.stop_recording().unwrap();

    let error = daemon.start_recording().unwrap_err();

    assert!(matches!(error, DaemonError::Busy { .. }));
    let jobs: i64 = rusqlite::Connection::open(&paths.database_file)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM dictation_jobs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(jobs, 1);
    daemon.complete_transcription(ticket.run()).unwrap();
    daemon.start_recording().unwrap();
}

#[test]
fn settings_saved_during_processing_do_not_change_the_in_flight_job() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    let recorded_with = Settings {
        openai_api_key: "first".into(),
        vocabulary: parse_vocabulary("Vercel = versel").unwrap(),
        ..Settings::default()
    };
    let mut transcriber = ScriptedTranscriber::default();
    transcriber.update_settings(&recorded_with);
    let mut daemon = daemon_with(&paths, recorded_with, transcriber);
    daemon.start_recording().unwrap();
    let ticket = daemon.stop_recording().unwrap();
    let saved_meanwhile = Settings {
        openai_api_key: "second".into(),
        vocabulary: parse_vocabulary("Versailles = versel").unwrap(),
        ..Settings::default()
    };
    daemon.transcriber_mut().update_settings(&saved_meanwhile);
    daemon.update_settings(saved_meanwhile);

    let delivered = daemon.complete_transcription(ticket.run()).unwrap();

    assert_eq!(delivered.final_text, "Vercel, sent with key first.");
    daemon.start_recording().unwrap();
    assert_eq!(
        finish(&mut daemon).final_text,
        "Versailles, sent with key second."
    );
}

#[test]
fn late_result_for_a_job_that_left_transcribing_is_dropped() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    let mut daemon = daemon_with(&paths, Settings::default(), FixedTranscriber);
    let started = daemon.start_recording().unwrap();
    let ticket = daemon.stop_recording().unwrap();
    let connection = rusqlite::Connection::open(&paths.database_file).unwrap();
    // As a restart's reconciliation would.
    connection
        .execute(
            "UPDATE dictation_jobs SET state = 'interrupted', stage = 'interrupted'",
            [],
        )
        .unwrap();

    let error = daemon.complete_transcription(ticket.run()).unwrap_err();

    assert!(matches!(error, DaemonError::StaleResult { job_id } if job_id == started.id));
    assert_eq!(daemon.deliverer().attempts, 0);
    let observer = Runtime::open_observer(&paths.database_file).unwrap();
    let job = observer.job(started.id).unwrap().unwrap();
    assert_eq!(job.stage, JobStage::Interrupted);
    assert!(job.raw_transcript.is_empty());
    assert_eq!(daemon.phase(), WorkflowPhase::Ready);
}

#[test]
fn result_after_the_stale_paste_limit_is_copied_not_pasted() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    let mut daemon = daemon_with(&paths, Settings::default(), FixedTranscriber);
    daemon.start_recording().unwrap();
    let ticket = daemon.stop_recording().unwrap();
    let late = TranscriptionCompletion {
        finished_at: Instant::now() + Duration::from_secs(9),
        ..ticket.run()
    };

    let delivered = daemon.complete_transcription(late).unwrap();

    assert_eq!(daemon.deliverer().methods, [DeliveryMethod::CopyOnly]);
    assert_eq!(delivered.stage, JobStage::Delivered);
    assert!(!delivered.paste_triggered);
    assert_eq!(daemon.phase(), WorkflowPhase::Ready);
}

#[test]
fn transcription_retry_copies_only_and_blocks_a_concurrent_start() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    let transcriber = ScriptedTranscriber::replying([
        Err(ExternalError::new("OpenAI is unreachable")),
        Ok("Second attempt.".into()),
    ]);
    let mut daemon = daemon_with(&paths, Settings::default(), transcriber);
    daemon.start_recording().unwrap();
    let failed = finish(&mut daemon);
    assert_eq!(failed.stage, JobStage::Failed);

    let ticket = daemon.retry_transcription(failed.id).unwrap();

    assert!(matches!(
        daemon.phase(),
        WorkflowPhase::Processing { job_id, .. } if job_id == failed.id
    ));
    assert!(matches!(
        daemon.start_recording(),
        Err(DaemonError::Busy { .. })
    ));
    let copied = daemon.complete_transcription(ticket.run()).unwrap();
    assert_eq!(daemon.deliverer().methods, [DeliveryMethod::CopyOnly]);
    assert_eq!(copied.stage, JobStage::Delivered);
    assert!(!copied.paste_triggered);
    assert_eq!(daemon.phase(), WorkflowPhase::Ready);
}

#[test]
fn retrying_a_job_with_a_stored_transcript_never_transcribes_again() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    let transcriber = ScriptedTranscriber::replying([Ok("Paid for once.".into())]);
    let calls = Arc::clone(&transcriber.calls);
    let mut daemon = daemon_with(&paths, Settings::default(), transcriber);
    daemon.start_recording().unwrap();
    let connection = rusqlite::Connection::open(&paths.database_file).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_ready BEFORE UPDATE OF stage ON dictation_jobs
             WHEN NEW.stage = 'ready_to_deliver'
             BEGIN SELECT RAISE(FAIL, 'ready checkpoint unavailable'); END;",
        )
        .unwrap();
    let ticket = daemon.stop_recording().unwrap();
    let job_id = ticket.job_id();
    assert!(daemon.complete_transcription(ticket.run()).is_err());
    connection
        .execute_batch("DROP TRIGGER reject_ready")
        .unwrap();

    let ticket = daemon.retry_transcription(job_id).unwrap();
    let copied = daemon.complete_transcription(ticket.run()).unwrap();

    assert_eq!(copied.final_text, "Paid for once.");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn panicking_transcription_fails_the_job_and_frees_the_daemon() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    let mut daemon = daemon_with(&paths, Settings::default(), FixedTranscriber);
    daemon.start_recording().unwrap();
    let ticket = daemon.stop_recording().unwrap();
    let job_id = ticket.job_id();
    drop(ticket);

    let failed = daemon
        .complete_transcription(TranscriptionCompletion::failed(
            job_id,
            "transcription stopped unexpectedly; audio is saved",
        ))
        .unwrap();

    assert_eq!(failed.stage, JobStage::Failed);
    assert!(failed.audio_path.is_file());
    assert_eq!(
        daemon.workspace_snapshot().unwrap().recoveries[0].job_id,
        job_id
    );
    daemon.start_recording().unwrap();
}

#[test]
fn deleting_another_recovery_during_processing_keeps_the_processing_job() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    let mut daemon = daemon_with(&paths, Settings::default(), FixedTranscriber);
    let older = daemon.start_recording().unwrap();
    exit_recorder(&mut daemon, older.id);
    let processing = daemon.start_recording().unwrap();
    let ticket = daemon.stop_recording().unwrap();

    daemon.delete_recovery(older.id).unwrap();

    assert!(matches!(
        daemon.phase(),
        WorkflowPhase::Processing { job_id, .. } if job_id == processing.id
    ));
    let delivered = daemon.complete_transcription(ticket.run()).unwrap();
    assert_eq!(delivered.stage, JobStage::Delivered);
    assert_eq!(daemon.deliverer().methods, [DeliveryMethod::Paste]);
}

#[test]
fn max_duration_event_stops_only_the_matching_recording() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    let mut daemon = daemon_with(&paths, Settings::default(), FixedTranscriber);
    let earlier = daemon.start_recording().unwrap();
    daemon.discard_recording().unwrap();
    let current = daemon.start_recording().unwrap();

    let stale = daemon
        .recorder_event(RecorderEvent::MaxDurationReached { job_id: earlier.id })
        .unwrap();
    assert!(stale.is_none());
    assert_eq!(
        daemon.phase(),
        WorkflowPhase::Recording { job_id: current.id }
    );

    let ticket = daemon
        .recorder_event(RecorderEvent::MaxDurationReached { job_id: current.id })
        .unwrap()
        .expect("the matching recording stops");
    let delivered = daemon.complete_transcription(ticket.run()).unwrap();
    assert_eq!(delivered.id, current.id);
    assert_eq!(delivered.stage, JobStage::Delivered);
}

#[test]
fn stalled_recorder_preserves_audio_for_recovery() {
    let directory = tempdir().unwrap();
    let paths = app_paths(directory.path());
    let mut daemon = daemon_with(&paths, Settings::default(), FixedTranscriber);
    let started = daemon.start_recording().unwrap();

    let ticket = daemon
        .recorder_event(RecorderEvent::Stalled { job_id: started.id })
        .unwrap();

    assert!(ticket.is_none());
    assert_eq!(daemon.deliverer().attempts, 0);
    assert!(matches!(
        daemon.phase(),
        WorkflowPhase::NeedsAttention { job_id, at: JobStage::Interrupted } if job_id == started.id
    ));
    let recovery = daemon.workspace_snapshot().unwrap().recoveries.remove(0);
    assert_eq!(recovery.stage, JobStage::Interrupted);
    assert!(recovery.audio_present);
    assert!(
        recovery
            .error_message
            .is_some_and(|message| message.contains("microphone stopped sending audio"))
    );
}

/// The recorder process of `job_id` exits by itself, as the owner reports it.
fn exit_recorder<R, T, D>(daemon: &mut Daemon<R, T, D>, job_id: agentdictate_core::JobId)
where
    R: RecordingController,
    T: Transcriber,
    D: Deliverer,
{
    assert!(
        daemon
            .recorder_event(RecorderEvent::Exited { job_id })
            .unwrap()
            .is_none()
    );
}

fn app_paths(root: &Path) -> AppPaths {
    AppPaths::from_roots(
        root.join("config"),
        root.join("data"),
        root.join("state"),
        root.join("cache"),
        root.join("runtime"),
    )
}

fn reject_capture_checkpoint(connection: &rusqlite::Connection) {
    connection
        .execute_batch(
            r#"
            CREATE TRIGGER reject_capture_checkpoint
            BEFORE UPDATE OF stage ON dictation_jobs
            WHEN NEW.stage = 'captured'
            BEGIN
                SELECT RAISE(FAIL, 'capture checkpoint unavailable');
            END;
            "#,
        )
        .unwrap();
}

/// Every History row the History page lists, newest first.
fn history_rows(runtime: &Runtime) -> Vec<HistorySnapshot> {
    runtime
        .history_page(&HistoryPageRequest {
            page_size: 100,
            ..HistoryPageRequest::default()
        })
        .unwrap()
        .rows
}
