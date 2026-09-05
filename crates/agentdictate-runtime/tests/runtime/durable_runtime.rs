use std::path::PathBuf;

use agentdictate_core::{ReplacementRule, TranscriptionProvider};
use agentdictate_runtime::{
    Deliverer, DeliveryDisposition, DeliveryGate, DeliveryGateError, DeliveryStatus, ExternalError,
    HeadlessDeliveryGate, JobStage, Recorder, RecordingJob, Runtime, RuntimeError, Transcriber,
    Transcript,
};
use tempfile::TempDir;

use crate::support::{request, request_with_provider};

const TRANSCRIPTION_MODEL: &str = "gpt-transcribe";

#[test]
fn raw_checkpoint_and_options_survive_failure_and_database_reopen() {
    struct CheckpointThenFail;
    impl Transcriber for CheckpointThenFail {
        fn transcribe(&mut self, _: &RecordingJob) -> Result<Transcript, ExternalError> {
            unreachable!()
        }
        fn transcribe_checkpointed(
            &mut self,
            _: &RecordingJob,
            checkpoint: &mut agentdictate_runtime::TranscriptCheckpoint<'_>,
        ) -> Result<Transcript, ExternalError> {
            checkpoint("Do not push.", Some("gpt-live-transcribe"))?;
            Err(ExternalError::new("process failed after speech"))
        }
    }
    let directory = TempDir::new().unwrap();
    let db = directory.path().join("history.sqlite3");
    let mut runtime = Runtime::open(&db).unwrap();
    let mut request = request(&directory.path().join("audio.wav"), TRANSCRIPTION_MODEL);
    let options = agentdictate_core::DictationOptions::from_settings(
        &agentdictate_core::Settings {
            project_context: "Original project".into(),
            cleanup_timeout_ms: 1234,
            openai_api_key: "must-not-persist".into(),
            ..Default::default()
        },
        vec![],
    );
    request.options = Some(options.clone());
    let job = runtime
        .start_recording(request, &mut crate::support::ReadyRecorder)
        .unwrap();
    runtime.capture_recording(job.id, 2.0).unwrap();
    let mut deliverer = CountingSubmittedDeliverer { attempts: 0 };
    assert!(
        runtime
            .process_captured(
                job.id,
                &mut CheckpointThenFail,
                &mut HeadlessDeliveryGate,
                &mut deliverer
            )
            .is_err()
    );
    assert_eq!(deliverer.attempts, 0);
    drop(runtime);
    let runtime = Runtime::open(&db).unwrap();
    let recovered = runtime.job(job.id).unwrap().unwrap();
    assert_eq!(recovered.raw_transcript, "Do not push.");
    assert_eq!(recovered.transcription_model, "gpt-live-transcribe");
    assert_eq!(recovered.options, Some(options));
    assert!(
        !std::fs::read(&db)
            .unwrap()
            .windows(16)
            .any(|part| part == b"must-not-persist")
    );
}

struct InspectingRecorder {
    database_path: PathBuf,
    saw_durable_starting_job: bool,
}

#[derive(Default)]
struct CompensatingRecorder {
    start_attempts: usize,
    abort_attempts: usize,
}

struct FailingStartRecorder;

struct FailingCompensationRecorder;

impl Recorder for FailingStartRecorder {
    fn start(&mut self, _job: &RecordingJob) -> Result<(), ExternalError> {
        Err(ExternalError::new("microphone start failed"))
    }
}

impl Recorder for FailingCompensationRecorder {
    fn start(&mut self, _job: &RecordingJob) -> Result<(), ExternalError> {
        Ok(())
    }

    fn abort_start(&mut self, _job: &RecordingJob) -> Result<(), ExternalError> {
        Err(ExternalError::new("recorder stop failed"))
    }
}

struct AmbiguousDeliverer {
    attempts: usize,
}

impl Deliverer for AmbiguousDeliverer {
    fn deliver(&mut self, _job: &RecordingJob) -> Result<DeliveryDisposition, ExternalError> {
        self.attempts += 1;
        Ok(DeliveryDisposition::Ambiguous {
            copied_to_clipboard: true,
        })
    }
}

struct FixedTranscriber;

impl Transcriber for FixedTranscriber {
    fn transcribe(&mut self, _job: &RecordingJob) -> Result<Transcript, ExternalError> {
        Ok(Transcript {
            raw: "durable raw words".to_owned(),
            final_text: "Durable final words.".to_owned(),
            cleaned_text: Some("Durable final words.".to_owned()),
            cleanup_error: None,
        })
    }
}

struct InspectingTranscriber {
    database_path: PathBuf,
    saw_durable_transcribing_job: bool,
}

impl Transcriber for InspectingTranscriber {
    fn transcribe(&mut self, job: &RecordingJob) -> Result<Transcript, ExternalError> {
        let reader = Runtime::open_observer(&self.database_path)?;
        self.saw_durable_transcribing_job = reader
            .job(job.id)?
            .is_some_and(|persisted| persisted.stage == JobStage::Transcribing);
        Ok(Transcript {
            raw: "network result".to_owned(),
            final_text: "Network result.".to_owned(),
            cleaned_text: Some("Network result.".to_owned()),
            cleanup_error: None,
        })
    }
}

struct CountingTranscriber {
    attempts: usize,
}

struct FailingTranscriber {
    attempts: usize,
}

impl Transcriber for FailingTranscriber {
    fn transcribe(&mut self, _job: &RecordingJob) -> Result<Transcript, ExternalError> {
        self.attempts += 1;
        Err(ExternalError::new("temporary transcription failure"))
    }
}

impl Transcriber for CountingTranscriber {
    fn transcribe(&mut self, _job: &RecordingJob) -> Result<Transcript, ExternalError> {
        self.attempts += 1;
        Ok(Transcript {
            raw: "only once".to_owned(),
            final_text: "Only once.".to_owned(),
            cleaned_text: Some("Only once.".to_owned()),
            cleanup_error: None,
        })
    }
}

struct CountingSubmittedDeliverer {
    attempts: usize,
}

impl Deliverer for CountingSubmittedDeliverer {
    fn deliver(&mut self, _job: &RecordingJob) -> Result<DeliveryDisposition, ExternalError> {
        self.attempts += 1;
        Ok(DeliveryDisposition::Submitted {
            copied_to_clipboard: true,
            paste_triggered: true,
        })
    }
}

struct InspectingDeliverer {
    database_path: PathBuf,
    saw_persisted_transcript: bool,
    saw_durable_delivery_attempt: bool,
}

struct InspectingDeliveryGate {
    database_path: PathBuf,
    saw_ready_without_attempt: bool,
}

impl DeliveryGate for InspectingDeliveryGate {
    fn confirm_ready(&mut self) -> Result<(), DeliveryGateError> {
        let reader = Runtime::open_observer(&self.database_path)
            .map_err(|error| DeliveryGateError::new(error.to_string()))?;
        self.saw_ready_without_attempt = reader
            .recoverable_jobs()
            .map_err(|error| DeliveryGateError::new(error.to_string()))?
            .into_iter()
            .any(|job| {
                job.stage == JobStage::ReadyToDeliver
                    && job.delivery_status == DeliveryStatus::NotAttempted
                    && job.final_text == "Durable final words."
            });
        Ok(())
    }
}

struct FailingDeliveryGate;

impl DeliveryGate for FailingDeliveryGate {
    fn confirm_ready(&mut self) -> Result<(), DeliveryGateError> {
        Err(DeliveryGateError::new("overlay exit was not acknowledged"))
    }
}

impl Deliverer for InspectingDeliverer {
    fn deliver(&mut self, job: &RecordingJob) -> Result<DeliveryDisposition, ExternalError> {
        let reader = Runtime::open_observer(&self.database_path)?;
        self.saw_persisted_transcript = reader.job(job.id)?.is_some_and(|persisted| {
            persisted.stage == JobStage::ReadyToDeliver
                && persisted.raw_transcript == "durable raw words"
                && persisted.final_text == "Durable final words."
        });
        self.saw_durable_delivery_attempt = reader
            .job(job.id)?
            .is_some_and(|persisted| persisted.delivery_status == DeliveryStatus::Attempting);
        Ok(DeliveryDisposition::Submitted {
            copied_to_clipboard: true,
            paste_triggered: true,
        })
    }
}

impl Recorder for InspectingRecorder {
    fn start(&mut self, job: &RecordingJob) -> Result<(), ExternalError> {
        let reader = Runtime::open_observer(&self.database_path)?;
        self.saw_durable_starting_job = reader
            .job(job.id)?
            .is_some_and(|persisted| persisted.stage == JobStage::Starting);
        Ok(())
    }
}

impl Recorder for CompensatingRecorder {
    fn start(&mut self, _job: &RecordingJob) -> Result<(), ExternalError> {
        self.start_attempts += 1;
        Ok(())
    }

    fn abort_start(&mut self, _job: &RecordingJob) -> Result<(), ExternalError> {
        self.abort_attempts += 1;
        Ok(())
    }
}

#[test]
fn recording_job_is_durable_before_the_recorder_starts() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut recorder = InspectingRecorder {
        database_path: database_path.clone(),
        saw_durable_starting_job: false,
    };

    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/first.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut recorder,
        )
        .unwrap();

    assert!(recorder.saw_durable_starting_job);
    assert_eq!(job.stage, JobStage::Recording);
}

#[test]
fn recording_checkpoint_failure_compensates_a_started_recorder() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute_batch(
            r#"
            CREATE TRIGGER reject_recording_checkpoint
            BEFORE UPDATE OF stage ON dictation_jobs
            WHEN NEW.stage = 'recording'
            BEGIN
                SELECT RAISE(FAIL, 'recording checkpoint unavailable');
            END;
            "#,
        )
        .unwrap();
    let audio_path = directory.path().join("recordings/checkpoint-failure.wav");
    let mut recorder = CompensatingRecorder::default();

    let error = runtime
        .start_recording(request(&audio_path, TRANSCRIPTION_MODEL), &mut recorder)
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("recording checkpoint unavailable")
    );
    assert_eq!(recorder.start_attempts, 1);
    assert_eq!(recorder.abort_attempts, 1);
    let failed = runtime
        .recoverable_jobs()
        .unwrap()
        .into_iter()
        .find(|job| job.audio_path == audio_path)
        .unwrap();
    assert_eq!(failed.stage, JobStage::Interrupted);
    assert!(
        failed
            .error_message
            .as_deref()
            .unwrap()
            .contains("recording checkpoint unavailable")
    );
}

#[test]
fn failed_recovery_checkpoint_does_not_mask_the_recorder_start_error() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute_batch(
            r#"
            CREATE TRIGGER reject_start_recovery_checkpoint
            BEFORE UPDATE OF stage ON dictation_jobs
            WHEN NEW.stage = 'interrupted'
            BEGIN
                SELECT RAISE(FAIL, 'start recovery checkpoint unavailable');
            END;
            "#,
        )
        .unwrap();
    let audio_path = directory.path().join("recordings/start-failure.wav");
    let mut recorder = FailingStartRecorder;

    let error = runtime
        .start_recording(request(&audio_path, TRANSCRIPTION_MODEL), &mut recorder)
        .unwrap_err();

    assert!(error.to_string().contains("microphone start failed"));
    let failed = runtime
        .recoverable_jobs()
        .unwrap()
        .into_iter()
        .find(|job| job.audio_path == audio_path)
        .unwrap();
    assert_eq!(failed.stage, JobStage::Starting);
}

#[test]
fn failed_start_compensation_does_not_mask_the_checkpoint_error() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute_batch(
            r#"
            CREATE TRIGGER reject_recording_checkpoint
            BEFORE UPDATE OF stage ON dictation_jobs
            WHEN NEW.stage = 'recording'
            BEGIN
                SELECT RAISE(FAIL, 'recording checkpoint unavailable');
            END;
            "#,
        )
        .unwrap();
    let audio_path = directory.path().join("recordings/failed-compensation.wav");
    let mut recorder = FailingCompensationRecorder;

    let error = runtime
        .start_recording(request(&audio_path, TRANSCRIPTION_MODEL), &mut recorder)
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("recording checkpoint unavailable")
    );
    let failed = runtime
        .recoverable_jobs()
        .unwrap()
        .into_iter()
        .find(|job| job.audio_path == audio_path)
        .unwrap();
    let recovery_message = failed.error_message.as_deref().unwrap();
    assert!(recovery_message.contains("recording checkpoint unavailable"));
    assert!(recovery_message.contains("recorder stop failed"));
}

#[test]
fn dropping_the_ui_subscription_does_not_cancel_the_recording() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut recorder = InspectingRecorder {
        database_path,
        saw_durable_starting_job: false,
    };
    let ui_events = runtime.subscribe();
    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/disconnected.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut recorder,
        )
        .unwrap();

    drop(ui_events);
    let captured = runtime.capture_recording(job.id, 14.5).unwrap();

    assert_eq!(captured.stage, JobStage::Captured);
    assert_eq!(captured.duration_seconds, 14.5);
}

#[test]
fn transcript_is_durable_before_delivery_is_attempted() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut recorder = InspectingRecorder {
        database_path: database_path.clone(),
        saw_durable_starting_job: false,
    };
    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/durable.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut recorder,
        )
        .unwrap();
    runtime.capture_recording(job.id, 31.0).unwrap();
    let mut transcriber = FixedTranscriber;
    let mut deliverer = InspectingDeliverer {
        database_path: database_path.clone(),
        saw_persisted_transcript: false,
        saw_durable_delivery_attempt: false,
    };
    let mut delivery_gate = InspectingDeliveryGate {
        database_path: database_path.clone(),
        saw_ready_without_attempt: false,
    };

    let delivered = runtime
        .process_captured(job.id, &mut transcriber, &mut delivery_gate, &mut deliverer)
        .unwrap();

    assert!(delivery_gate.saw_ready_without_attempt);
    assert!(deliverer.saw_persisted_transcript);
    assert!(deliverer.saw_durable_delivery_attempt);
    assert_eq!(delivered.stage, JobStage::Delivered);
    assert_eq!(delivered.delivery_status, DeliveryStatus::Submitted);
    assert!(delivered.copied_to_clipboard);
    assert!(delivered.paste_triggered);
    drop(runtime);
    assert_eq!(
        rusqlite::Connection::open(&database_path)
            .unwrap()
            .query_row(
                "SELECT delivery_status FROM dictation_jobs WHERE runtime_id = ?1",
                [job.id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "submitted"
    );
    let restarted = Runtime::open(directory.path().join("agentdictate.db")).unwrap();
    assert_eq!(
        restarted.job(job.id).unwrap().unwrap().delivery_status,
        DeliveryStatus::Submitted
    );
    assert!(restarted.recovery_entries().unwrap().is_empty());
}

#[test]
fn delivery_gate_failure_is_safe_to_retry_and_never_calls_the_deliverer() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut recorder = InspectingRecorder {
        database_path: database_path.clone(),
        saw_durable_starting_job: false,
    };
    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/blocked.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut recorder,
        )
        .unwrap();
    runtime.capture_recording(job.id, 6.0).unwrap();
    let mut deliverer = CountingSubmittedDeliverer { attempts: 0 };

    let error = runtime
        .process_captured(
            job.id,
            &mut FixedTranscriber,
            &mut FailingDeliveryGate,
            &mut deliverer,
        )
        .unwrap_err();

    assert!(matches!(error, RuntimeError::DeliveryBlocked(_)));
    assert_eq!(deliverer.attempts, 0);
    let blocked = Runtime::open_observer(&database_path)
        .unwrap()
        .job(job.id)
        .unwrap()
        .unwrap();
    assert_eq!(blocked.stage, JobStage::ReadyToDeliver);
    assert_eq!(blocked.delivery_status, DeliveryStatus::NotAttempted);
    assert!(!blocked.copied_to_clipboard);
    assert!(!blocked.paste_triggered);
    assert!(
        blocked
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("overlay exit was not acknowledged"))
    );
}

#[test]
fn legacy_committed_delivery_is_read_as_submitted_and_not_recovered() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut recorder = InspectingRecorder {
        database_path: database_path.clone(),
        saw_durable_starting_job: false,
    };
    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/legacy-committed.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut recorder,
        )
        .unwrap();
    runtime.capture_recording(job.id, 5.0).unwrap();
    let delivered = runtime
        .process_captured(
            job.id,
            &mut FixedTranscriber,
            &mut HeadlessDeliveryGate,
            &mut CountingSubmittedDeliverer { attempts: 0 },
        )
        .unwrap();
    assert_eq!(delivered.delivery_status, DeliveryStatus::Submitted);
    drop(runtime);
    rusqlite::Connection::open(&database_path)
        .unwrap()
        .execute(
            "UPDATE dictation_jobs SET delivery_status = 'committed' WHERE runtime_id = ?1",
            [job.id.to_string()],
        )
        .unwrap();
    assert_eq!(
        rusqlite::Connection::open(&database_path)
            .unwrap()
            .query_row(
                "SELECT delivery_status FROM dictation_jobs WHERE runtime_id = ?1",
                [job.id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "committed"
    );

    let restarted = Runtime::open(&database_path).unwrap();

    assert_eq!(
        restarted.job(job.id).unwrap().unwrap().delivery_status,
        DeliveryStatus::Submitted
    );
    assert!(restarted.recovery_entries().unwrap().is_empty());
}

#[test]
fn transcribing_stage_is_durable_before_the_network_adapter_runs() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut recorder = InspectingRecorder {
        database_path: database_path.clone(),
        saw_durable_starting_job: false,
    };
    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/network.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut recorder,
        )
        .unwrap();
    runtime.capture_recording(job.id, 4.0).unwrap();
    let mut transcriber = InspectingTranscriber {
        database_path,
        saw_durable_transcribing_job: false,
    };
    let mut deliverer = CountingSubmittedDeliverer { attempts: 0 };

    runtime
        .process_captured(
            job.id,
            &mut transcriber,
            &mut HeadlessDeliveryGate,
            &mut deliverer,
        )
        .unwrap();

    assert!(transcriber.saw_durable_transcribing_job);
}

#[test]
fn ambiguous_delivery_is_not_retried_after_restart() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut recorder = InspectingRecorder {
        database_path: database_path.clone(),
        saw_durable_starting_job: false,
    };
    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/ambiguous.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut recorder,
        )
        .unwrap();
    runtime.capture_recording(job.id, 8.0).unwrap();
    let mut transcriber = FixedTranscriber;
    let mut deliverer = AmbiguousDeliverer { attempts: 0 };

    let ambiguous = runtime
        .process_captured(
            job.id,
            &mut transcriber,
            &mut HeadlessDeliveryGate,
            &mut deliverer,
        )
        .unwrap();
    drop(runtime);
    let mut restarted = Runtime::open(&database_path).unwrap();
    restarted
        .resume_safe_deliveries(&mut HeadlessDeliveryGate, &mut deliverer)
        .unwrap();

    assert_eq!(deliverer.attempts, 1);
    assert_eq!(ambiguous.delivery_status, DeliveryStatus::Ambiguous);
    assert_eq!(ambiguous.stage, JobStage::Failed);
    assert_eq!(
        restarted.job(job.id).unwrap().unwrap().final_text,
        "Durable final words."
    );
}

#[test]
fn interrupted_recording_remains_recoverable_after_restart() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let audio_path = directory.path().join("recordings/interrupted.wav");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut recorder = InspectingRecorder {
        database_path: database_path.clone(),
        saw_durable_starting_job: false,
    };
    let job = runtime
        .start_recording(request(&audio_path, TRANSCRIPTION_MODEL), &mut recorder)
        .unwrap();

    drop(runtime);
    let restarted = Runtime::open(&database_path).unwrap();
    let recoverable = restarted.recoverable_jobs().unwrap();

    assert_eq!(recoverable.len(), 1);
    assert_eq!(recoverable[0].id, job.id);
    assert_eq!(recoverable[0].stage, JobStage::Interrupted);
    assert_eq!(recoverable[0].audio_path, audio_path);
}

#[test]
fn duplicate_processing_signal_cannot_transcribe_or_paste_a_delivered_job_again() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut recorder = InspectingRecorder {
        database_path,
        saw_durable_starting_job: false,
    };
    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/once.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut recorder,
        )
        .unwrap();
    runtime.capture_recording(job.id, 2.0).unwrap();
    let mut transcriber = CountingTranscriber { attempts: 0 };
    let mut deliverer = CountingSubmittedDeliverer { attempts: 0 };
    runtime
        .process_captured(
            job.id,
            &mut transcriber,
            &mut HeadlessDeliveryGate,
            &mut deliverer,
        )
        .unwrap();

    assert!(runtime.capture_recording(job.id, 2.0).is_err());
    assert!(
        runtime
            .process_captured(
                job.id,
                &mut transcriber,
                &mut HeadlessDeliveryGate,
                &mut deliverer,
            )
            .is_err()
    );
    assert_eq!(transcriber.attempts, 1);
    assert_eq!(deliverer.attempts, 1);
    assert_eq!(
        runtime.job(job.id).unwrap().unwrap().stage,
        JobStage::Delivered
    );
}

#[test]
fn capture_finalization_failure_can_interrupt_the_recording_durably() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut recorder = InspectingRecorder {
        database_path: database_path.clone(),
        saw_durable_starting_job: false,
    };
    let events = runtime.subscribe();
    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/finalize-failed.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut recorder,
        )
        .unwrap();

    let interrupted = runtime
        .interrupt_job(
            job.id,
            JobStage::Recording,
            "audio stream stopped before the spool was finalized",
        )
        .unwrap();

    assert_eq!(interrupted.stage, JobStage::Interrupted);
    assert_eq!(
        interrupted.error_message.as_deref(),
        Some("audio stream stopped before the spool was finalized")
    );
    assert!(matches!(
        events.try_iter().last(),
        Some(agentdictate_runtime::RuntimeEvent::JobUpdated(updated))
            if updated.stage == JobStage::Interrupted
    ));
    drop(runtime);
    let restarted = Runtime::open(&database_path).unwrap();
    assert_eq!(
        restarted.job(job.id).unwrap().unwrap().stage,
        JobStage::Interrupted
    );
}

#[test]
fn explicit_discard_deletes_the_captured_job_and_its_audio() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut recorder = InspectingRecorder {
        database_path: database_path.clone(),
        saw_durable_starting_job: false,
    };
    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/discarded.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut recorder,
        )
        .unwrap();
    runtime.capture_recording(job.id, 12.0).unwrap();

    let discarded = runtime.discard_recording(job.id).unwrap();

    assert_eq!(discarded.stage, JobStage::Deleted);
    assert!(!discarded.audio_path.exists());
    assert!(runtime.recoverable_jobs().unwrap().is_empty());
    assert!(runtime.recovery_entries().unwrap().is_empty());
    drop(runtime);
    let restarted = Runtime::open(&database_path).unwrap();
    assert_eq!(
        restarted.job(job.id).unwrap().unwrap().stage,
        JobStage::Deleted
    );
    assert!(restarted.recovery_entries().unwrap().is_empty());
}

#[test]
fn startup_keeps_historical_canceled_jobs_recoverable() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut recorder = InspectingRecorder {
        database_path: database_path.clone(),
        saw_durable_starting_job: false,
    };
    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/historical-canceled.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut recorder,
        )
        .unwrap();
    runtime.capture_recording(job.id, 12.0).unwrap();
    drop(runtime);
    rusqlite::Connection::open(&database_path)
        .unwrap()
        .execute(
            "UPDATE dictation_jobs SET state = 'canceled', stage = 'canceled' WHERE runtime_id = ?1",
            [job.id.to_string()],
        )
        .unwrap();

    let restarted = Runtime::open(&database_path).unwrap();

    assert_eq!(
        restarted.job(job.id).unwrap().unwrap().stage,
        JobStage::Canceled
    );
    assert_eq!(restarted.recovery_entries().unwrap()[0].job_id, job.id);
}

#[test]
fn captured_checkpoint_can_be_retried_after_restart() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut recorder = InspectingRecorder {
        database_path: database_path.clone(),
        saw_durable_starting_job: false,
    };
    let job = runtime
        .start_recording(
            request_with_provider(
                &directory.path().join("recordings/captured-restart.wav"),
                TranscriptionProvider::ChatGptSubscription,
                TRANSCRIPTION_MODEL,
            ),
            &mut recorder,
        )
        .unwrap();
    runtime.capture_recording(job.id, 18.0).unwrap();
    drop(runtime);
    let mut runtime = Runtime::open(&database_path).unwrap();
    let restarted = runtime.job(job.id).unwrap().unwrap();
    assert_eq!(restarted.stage, JobStage::Captured);
    assert_eq!(
        restarted.transcription_provider,
        TranscriptionProvider::ChatGptSubscription
    );
    let mut transcriber = CountingTranscriber { attempts: 0 };
    let mut deliverer = CountingSubmittedDeliverer { attempts: 0 };

    let delivered = runtime
        .retry_transcription(
            job.id,
            &mut transcriber,
            &mut HeadlessDeliveryGate,
            &mut deliverer,
        )
        .unwrap();

    assert_eq!(delivered.stage, JobStage::Delivered);
    assert_eq!(
        delivered.transcription_provider,
        TranscriptionProvider::ChatGptSubscription
    );
    assert_eq!(transcriber.attempts, 1);
    assert_eq!(deliverer.attempts, 1);
}

#[test]
fn failed_transcription_can_be_retried_explicitly_without_a_duplicate_first_attempt() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut recorder = InspectingRecorder {
        database_path,
        saw_durable_starting_job: false,
    };
    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/retry.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut recorder,
        )
        .unwrap();
    runtime.capture_recording(job.id, 18.0).unwrap();
    let mut failing = FailingTranscriber { attempts: 0 };
    let mut deliverer = CountingSubmittedDeliverer { attempts: 0 };
    assert!(
        runtime
            .process_captured(
                job.id,
                &mut failing,
                &mut HeadlessDeliveryGate,
                &mut deliverer,
            )
            .is_err()
    );
    let mut retry = CountingTranscriber { attempts: 0 };

    let delivered = runtime
        .retry_transcription(
            job.id,
            &mut retry,
            &mut HeadlessDeliveryGate,
            &mut deliverer,
        )
        .unwrap();

    assert_eq!(failing.attempts, 1);
    assert_eq!(retry.attempts, 1);
    assert_eq!(deliverer.attempts, 1);
    assert_eq!(delivered.stage, JobStage::Delivered);
}

#[test]
fn delivery_retry_is_explicit_and_reuses_the_durable_transcript() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut recorder = InspectingRecorder {
        database_path,
        saw_durable_starting_job: false,
    };
    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/manual-delivery.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut recorder,
        )
        .unwrap();
    runtime.capture_recording(job.id, 18.0).unwrap();
    let mut transcriber = CountingTranscriber { attempts: 0 };
    let mut ambiguous = AmbiguousDeliverer { attempts: 0 };
    runtime
        .process_captured(
            job.id,
            &mut transcriber,
            &mut HeadlessDeliveryGate,
            &mut ambiguous,
        )
        .unwrap();
    let mut submitted = CountingSubmittedDeliverer { attempts: 0 };

    let delivered = runtime
        .retry_delivery(job.id, &mut HeadlessDeliveryGate, &mut submitted)
        .unwrap();

    assert_eq!(transcriber.attempts, 1);
    assert_eq!(ambiguous.attempts, 1);
    assert_eq!(submitted.attempts, 1);
    assert_eq!(delivered.stage, JobStage::Delivered);
    assert_eq!(delivered.final_text, "Only once.");
}

#[test]
fn delivery_attempt_without_a_durable_outcome_cannot_be_replayed() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut recorder = InspectingRecorder {
        database_path: database_path.clone(),
        saw_durable_starting_job: false,
    };
    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/attempting.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut recorder,
        )
        .unwrap();
    runtime.capture_recording(job.id, 18.0).unwrap();
    let mut transcriber = CountingTranscriber { attempts: 0 };
    let mut ambiguous = AmbiguousDeliverer { attempts: 0 };
    runtime
        .process_captured(
            job.id,
            &mut transcriber,
            &mut HeadlessDeliveryGate,
            &mut ambiguous,
        )
        .unwrap();
    rusqlite::Connection::open(&database_path)
        .unwrap()
        .execute(
            "UPDATE dictation_jobs SET delivery_status = 'attempting' WHERE runtime_id = ?1",
            [job.id.to_string()],
        )
        .unwrap();
    let mut submitted = CountingSubmittedDeliverer { attempts: 0 };

    let error = runtime
        .retry_delivery(job.id, &mut HeadlessDeliveryGate, &mut submitted)
        .unwrap_err();

    assert!(error.to_string().contains("no durable outcome"));
    assert_eq!(submitted.attempts, 0);
}

#[test]
fn deleting_a_recovery_removes_audio_before_marking_the_job_deleted() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let audio_path = directory.path().join("recordings/delete.wav");
    std::fs::create_dir_all(audio_path.parent().unwrap()).unwrap();
    std::fs::write(&audio_path, b"RIFFrecoverable audio").unwrap();
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut recorder = InspectingRecorder {
        database_path,
        saw_durable_starting_job: false,
    };
    let job = runtime
        .start_recording(request(&audio_path, TRANSCRIPTION_MODEL), &mut recorder)
        .unwrap();
    runtime
        .interrupt_job(job.id, JobStage::Recording, "microphone disconnected")
        .unwrap();

    let deleted = runtime.delete_recovery(job.id).unwrap();

    assert_eq!(deleted.stage, JobStage::Deleted);
    assert!(!audio_path.exists());
    assert!(runtime.recoverable_jobs().unwrap().is_empty());
}

#[test]
fn failed_recovery_delete_restores_the_only_audio_copy() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let audio_path = directory.path().join("recordings/delete-failed.wav");
    std::fs::create_dir_all(audio_path.parent().unwrap()).unwrap();
    let audio = b"RIFFthe only recoverable audio";
    std::fs::write(&audio_path, audio).unwrap();
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut recorder = InspectingRecorder {
        database_path: database_path.clone(),
        saw_durable_starting_job: false,
    };
    let job = runtime
        .start_recording(request(&audio_path, TRANSCRIPTION_MODEL), &mut recorder)
        .unwrap();
    runtime
        .interrupt_job(job.id, JobStage::Recording, "microphone disconnected")
        .unwrap();
    rusqlite::Connection::open(&database_path)
        .unwrap()
        .execute_batch(
            r#"
            CREATE TRIGGER reject_recovery_delete
            BEFORE UPDATE OF stage ON dictation_jobs
            WHEN NEW.stage = 'deleted'
            BEGIN
                SELECT RAISE(ABORT, 'forced delete failure');
            END;
            "#,
        )
        .unwrap();

    assert!(runtime.delete_recovery(job.id).is_err());

    assert_eq!(std::fs::read(&audio_path).unwrap(), audio);
    assert_eq!(
        runtime.job(job.id).unwrap().unwrap().stage,
        JobStage::Interrupted
    );
    assert_eq!(
        std::fs::read_dir(audio_path.parent().unwrap())
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn startup_restores_audio_quarantined_before_the_delete_checkpoint() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let audio_path = directory.path().join("recordings/delete-crash.wav");
    std::fs::create_dir_all(audio_path.parent().unwrap()).unwrap();
    let audio = b"RIFFaudio survives a delete crash";
    std::fs::write(&audio_path, audio).unwrap();
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut recorder = InspectingRecorder {
        database_path: database_path.clone(),
        saw_durable_starting_job: false,
    };
    let job = runtime
        .start_recording(request(&audio_path, TRANSCRIPTION_MODEL), &mut recorder)
        .unwrap();
    runtime
        .interrupt_job(job.id, JobStage::Recording, "microphone disconnected")
        .unwrap();
    drop(runtime);
    let quarantine = audio_path.with_file_name(format!(".agentdictate-delete-{}.pending", job.id));
    std::fs::rename(&audio_path, &quarantine).unwrap();

    let restarted = Runtime::open(&database_path).unwrap();

    assert_eq!(std::fs::read(&audio_path).unwrap(), audio);
    assert!(!quarantine.exists());
    assert_eq!(
        restarted.job(job.id).unwrap().unwrap().stage,
        JobStage::Interrupted
    );
}

#[test]
fn enabled_replacements_are_applied_after_cleanup_and_before_delivery() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    runtime
        .create_replacement(ReplacementRule {
            id: None,
            source_phrase: "durable final words".into(),
            replacement_phrase: "AgentDictate".into(),
            enabled: true,
            case_sensitive: false,
            whole_word_only: true,
        })
        .unwrap();
    let mut recorder = InspectingRecorder {
        database_path,
        saw_durable_starting_job: false,
    };
    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/replacements.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut recorder,
        )
        .unwrap();
    runtime.capture_recording(job.id, 4.0).unwrap();
    let mut transcriber = FixedTranscriber;
    let mut deliverer = CountingSubmittedDeliverer { attempts: 0 };

    let delivered = runtime
        .process_captured(
            job.id,
            &mut transcriber,
            &mut HeadlessDeliveryGate,
            &mut deliverer,
        )
        .unwrap();

    assert_eq!(delivered.raw_transcript, "durable raw words");
    assert_eq!(delivered.final_text, "AgentDictate.");
}
