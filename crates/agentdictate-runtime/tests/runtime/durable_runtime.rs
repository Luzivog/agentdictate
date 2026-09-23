use std::path::PathBuf;

use agentdictate_core::{DictationOptions, Settings, parse_vocabulary};
use agentdictate_runtime::{
    Deliverer, DeliveryDisposition, DeliveryGate, DeliveryGateError, DeliveryMethod,
    DeliveryStatus, ExternalError, HeadlessDeliveryGate, JobId, JobStage, Recorder, RecordingJob,
    Runtime, RuntimeError, StoredTranscript, Transcript, TranscriptionOutcome,
};
use tempfile::TempDir;

use crate::support::{request, transcribe_and_deliver, transcript};

const TRANSCRIPTION_MODEL: &str = "gpt-transcribe";

#[test]
fn raw_checkpoint_and_options_survive_failure_and_database_reopen() {
    let directory = TempDir::new().unwrap();
    let db = directory.path().join("history.sqlite3");
    let mut runtime = Runtime::open(&db).unwrap();
    let mut request = request(&directory.path().join("audio.wav"), TRANSCRIPTION_MODEL);
    let options =
        agentdictate_core::DictationOptions::from_settings(&agentdictate_core::Settings {
            transcription_prompt: "Original project".into(),
            openai_api_key: "must-not-persist".into(),
            ..Default::default()
        });
    request.options = Some(options.clone());
    let job = runtime
        .start_recording(request, &mut crate::support::ReadyRecorder)
        .unwrap();
    runtime.capture_recording(job.id, 2.0).unwrap();
    runtime.begin_transcription(job.id).unwrap();
    rusqlite::Connection::open(&db)
        .unwrap()
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
    let transcribed = TranscriptionOutcome::Text(Transcript {
        text: "Do not push.".into(),
        model: "gpt-transcribe-preview".into(),
    });
    assert!(runtime.store_transcript(job.id, transcribed, None).is_err());
    drop(runtime);
    let runtime = Runtime::open(&db).unwrap();
    let recovered = runtime.job(job.id).unwrap().unwrap();
    assert_eq!(recovered.stage, JobStage::Failed);
    assert_eq!(recovered.raw_transcript, "Do not push.");
    assert_eq!(recovered.transcription_model, "gpt-transcribe-preview");
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
    fn deliver(
        &mut self,
        _job: &RecordingJob,
        _: DeliveryMethod,
    ) -> Result<DeliveryDisposition, ExternalError> {
        self.attempts += 1;
        Ok(DeliveryDisposition::Ambiguous {
            copied_to_clipboard: true,
        })
    }
}

struct CountingSubmittedDeliverer {
    attempts: usize,
}

impl Deliverer for CountingSubmittedDeliverer {
    fn deliver(
        &mut self,
        _job: &RecordingJob,
        _: DeliveryMethod,
    ) -> Result<DeliveryDisposition, ExternalError> {
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
    fn deliver(
        &mut self,
        job: &RecordingJob,
        _: DeliveryMethod,
    ) -> Result<DeliveryDisposition, ExternalError> {
        let reader = Runtime::open_observer(&self.database_path)?;
        self.saw_persisted_transcript = reader.job(job.id)?.is_some_and(|persisted| {
            persisted.stage == JobStage::ReadyToDeliver
                && persisted.raw_transcript == "Durable final words."
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
    let mut deliverer = InspectingDeliverer {
        database_path: database_path.clone(),
        saw_persisted_transcript: false,
        saw_durable_delivery_attempt: false,
    };
    let mut delivery_gate = InspectingDeliveryGate {
        database_path: database_path.clone(),
        saw_ready_without_attempt: false,
    };

    let delivered = transcribe_and_deliver(
        &mut runtime,
        job.id,
        "Durable final words.",
        &mut delivery_gate,
        &mut deliverer,
    )
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
    assert!(restarted.recoveries().unwrap().is_empty());
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

    let error = transcribe_and_deliver(
        &mut runtime,
        job.id,
        "Durable final words.",
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
fn begin_transcription_is_durable_before_the_ticket_leaves_the_lock() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/network.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut crate::support::ReadyRecorder,
        )
        .unwrap();
    runtime.capture_recording(job.id, 4.0).unwrap();

    runtime.begin_transcription(job.id).unwrap();

    let observer = Runtime::open_observer(&database_path).unwrap();
    assert_eq!(
        observer.job(job.id).unwrap().unwrap().stage,
        JobStage::Transcribing
    );
    assert!(runtime.begin_transcription(job.id).is_err());
}

#[test]
fn transcript_is_accepted_only_while_the_job_is_transcribing() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let start = |runtime: &mut Runtime, name: &str| {
        let job = runtime
            .start_recording(
                request(&directory.path().join(name), TRANSCRIPTION_MODEL),
                &mut crate::support::ReadyRecorder,
            )
            .unwrap();
        runtime.capture_recording(job.id, 3.0).unwrap();
        runtime.begin_transcription(job.id).unwrap();
        job.id
    };
    let reconciled = start(&mut runtime, "reconciled.wav");
    let deleted = start(&mut runtime, "deleted.wav");
    let delivered = start(&mut runtime, "delivered.wav");
    let StoredTranscript::Ready(ready) = runtime
        .store_transcript(delivered, transcript("First words."), None)
        .unwrap()
    else {
        panic!("a transcribing job accepts its transcript");
    };
    runtime
        .deliver_ready(
            ready,
            DeliveryMethod::Paste,
            &mut HeadlessDeliveryGate,
            &mut CountingSubmittedDeliverer { attempts: 0 },
        )
        .unwrap();
    drop(runtime);
    // A restart reconciles the other two transcribing jobs to interrupted.
    let mut runtime = Runtime::open(&database_path).unwrap();
    runtime.delete_recovery(deleted).unwrap();
    let before = runtime.job(reconciled).unwrap().unwrap();

    for id in [reconciled, deleted, delivered] {
        for outcome in [
            transcript("Late words."),
            TranscriptionOutcome::NoSpeech,
            TranscriptionOutcome::Failed {
                message: "late failure".into(),
            },
        ] {
            assert_eq!(
                runtime.store_transcript(id, outcome, None).unwrap(),
                StoredTranscript::Stale
            );
        }
    }

    assert_eq!(runtime.job(reconciled).unwrap().unwrap(), before);
    assert!(runtime.job(deleted).unwrap().is_none());
    let delivered = runtime.job(delivered).unwrap().unwrap();
    assert_eq!(delivered.stage, JobStage::Delivered);
    assert_eq!(delivered.final_text, "First words.");
}

#[test]
fn transcribing_job_left_by_a_crash_is_interrupted_and_retryable() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/crash.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut crate::support::ReadyRecorder,
        )
        .unwrap();
    runtime.capture_recording(job.id, 9.0).unwrap();
    runtime.begin_transcription(job.id).unwrap();
    drop(runtime);

    let mut restarted = Runtime::open(&database_path).unwrap();

    assert_eq!(
        restarted.recoveries().unwrap()[0].stage,
        JobStage::Interrupted
    );
    let retrying = restarted.prepare_transcription_retry(job.id).unwrap();
    assert_eq!(retrying.stage, JobStage::Transcribing);
}

#[test]
fn cancelled_result_is_a_recoverable_ready_transcript() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/cancelled.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut crate::support::ReadyRecorder,
        )
        .unwrap();
    runtime.capture_recording(job.id, 9.0).unwrap();
    runtime.begin_transcription(job.id).unwrap();

    let stored = runtime
        .store_transcript(
            job.id,
            transcript("Kept for later."),
            Some("Cancelled before paste"),
        )
        .unwrap();

    assert!(matches!(stored, StoredTranscript::Ready(_)));
    let recovery = runtime.recoveries().unwrap().remove(0);
    assert_eq!(recovery.stage, JobStage::ReadyToDeliver);
    assert_eq!(recovery.final_text, "Kept for later.");
    assert_eq!(
        recovery.error_message.as_deref(),
        Some("Cancelled before paste")
    );
    let copied = runtime.prepare_delivery_retry(job.id).unwrap();
    let copied = runtime
        .deliver_ready(
            copied,
            DeliveryMethod::CopyOnly,
            &mut HeadlessDeliveryGate,
            &mut CountingSubmittedDeliverer { attempts: 0 },
        )
        .unwrap();
    assert_eq!(copied.stage, JobStage::Delivered);
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
    let mut deliverer = AmbiguousDeliverer { attempts: 0 };

    let ambiguous = transcribe_and_deliver(
        &mut runtime,
        job.id,
        "Durable final words.",
        &mut HeadlessDeliveryGate,
        &mut deliverer,
    )
    .unwrap();
    drop(runtime);
    let restarted = Runtime::open(&database_path).unwrap();

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
    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/once.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut crate::support::ReadyRecorder,
        )
        .unwrap();
    runtime.capture_recording(job.id, 2.0).unwrap();
    let mut deliverer = CountingSubmittedDeliverer { attempts: 0 };
    transcribe_and_deliver(
        &mut runtime,
        job.id,
        "Only once.",
        &mut HeadlessDeliveryGate,
        &mut deliverer,
    )
    .unwrap();

    assert!(runtime.capture_recording(job.id, 2.0).is_err());
    assert!(
        transcribe_and_deliver(
            &mut runtime,
            job.id,
            "Only once.",
            &mut HeadlessDeliveryGate,
            &mut deliverer,
        )
        .is_err()
    );
    assert_eq!(
        runtime
            .store_transcript(job.id, transcript("Again."), None)
            .unwrap(),
        StoredTranscript::Stale
    );
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
    drop(runtime);
    let restarted = Runtime::open(&database_path).unwrap();
    assert_eq!(
        restarted.job(job.id).unwrap().unwrap().stage,
        JobStage::Interrupted
    );
}

/// Esc on a recording of at most five seconds deletes it with its audio. A
/// longer one waits in Recovery as cancelled, audio kept, for a day, and
/// "Transcribe again" can still take it.
#[test]
fn explicit_discard_deletes_short_recordings_and_keeps_longer_ones_for_a_day() {
    for (seconds, kept) in [(5.0, false), (5.5, true)] {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("agentdictate.db");
        let audio_path = directory.path().join("discarded.wav");
        let mut runtime = Runtime::open(&database_path).unwrap();
        let job = runtime
            .start_recording(
                request(&audio_path, TRANSCRIPTION_MODEL),
                &mut crate::support::ReadyRecorder,
            )
            .unwrap();
        std::fs::write(&audio_path, b"RIFF").unwrap();
        runtime.capture_recording(job.id, seconds).unwrap();

        let discarded = runtime.discard_recording(job.id, false).unwrap();
        drop(runtime);

        let mut restarted = Runtime::open(&database_path).unwrap();
        let recoveries = restarted.recoveries().unwrap();
        assert_eq!(audio_path.exists(), kept, "{seconds} s");
        if !kept {
            assert_eq!(discarded.stage, JobStage::Deleted);
            assert!(restarted.job(job.id).unwrap().is_none());
            assert!(recoveries.is_empty());
            continue;
        }
        assert_eq!(discarded.stage, JobStage::Cancelled);
        assert_eq!(recoveries.len(), 1);
        assert_eq!(recoveries[0].stage, JobStage::Cancelled);
        assert_eq!(
            recoveries[0].expires_at - recoveries[0].updated_at,
            chrono::TimeDelta::days(1)
        );
        let retrying = restarted.prepare_transcription_retry(job.id).unwrap();
        assert_eq!(retrying.stage, JobStage::Transcribing);
    }
}

/// Also covers rows from the retired ChatGPT subscription route: their
/// stored provider is never read, so they still load and retry.
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
            request(
                &directory.path().join("recordings/captured-restart.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut recorder,
        )
        .unwrap();
    runtime.capture_recording(job.id, 18.0).unwrap();
    drop(runtime);
    rusqlite::Connection::open(&database_path)
        .unwrap()
        .execute(
            "UPDATE dictation_jobs SET transcription_provider = 'chatgpt_subscription'",
            [],
        )
        .unwrap();
    let mut runtime = Runtime::open(&database_path).unwrap();
    let restarted = runtime.job(job.id).unwrap().unwrap();
    assert_eq!(restarted.stage, JobStage::Captured);

    let retrying = runtime.prepare_transcription_retry(job.id).unwrap();

    assert_eq!(retrying.stage, JobStage::Transcribing);
    assert!(matches!(
        runtime
            .store_transcript(job.id, transcript("Recovered words."), None)
            .unwrap(),
        StoredTranscript::Ready(_)
    ));
}

#[test]
fn failed_transcription_can_be_retried_explicitly() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/retry.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut crate::support::ReadyRecorder,
        )
        .unwrap();
    runtime.capture_recording(job.id, 18.0).unwrap();
    runtime.begin_transcription(job.id).unwrap();
    let StoredTranscript::Failed(failed) = runtime
        .store_transcript(
            job.id,
            TranscriptionOutcome::Failed {
                message: "temporary transcription failure".into(),
            },
            None,
        )
        .unwrap()
    else {
        panic!("a failed attempt fails the job");
    };
    assert_eq!(
        failed.error_message.as_deref(),
        Some("temporary transcription failure")
    );

    runtime.prepare_transcription_retry(job.id).unwrap();
    let StoredTranscript::Ready(ready) = runtime
        .store_transcript(job.id, transcript("Only once."), None)
        .unwrap()
    else {
        panic!("the retried job accepts its transcript");
    };
    let mut deliverer = CountingSubmittedDeliverer { attempts: 0 };
    let delivered = runtime
        .deliver_ready(
            ready,
            DeliveryMethod::CopyOnly,
            &mut HeadlessDeliveryGate,
            &mut deliverer,
        )
        .unwrap();

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
    let mut ambiguous = AmbiguousDeliverer { attempts: 0 };
    transcribe_and_deliver(
        &mut runtime,
        job.id,
        "Only once.",
        &mut HeadlessDeliveryGate,
        &mut ambiguous,
    )
    .unwrap();
    let mut submitted = CountingSubmittedDeliverer { attempts: 0 };

    let delivered = copy_again(&mut runtime, job.id, &mut submitted).unwrap();

    assert_eq!(ambiguous.attempts, 1);
    assert_eq!(submitted.attempts, 1);
    assert_eq!(delivered.stage, JobStage::Delivered);
    assert_eq!(delivered.final_text, "Only once.");
}

#[test]
fn delivery_that_fails_before_any_paste_stays_ready_and_can_be_retried() {
    struct NotSentDeliverer;
    impl Deliverer for NotSentDeliverer {
        fn deliver(
            &mut self,
            _: &RecordingJob,
            _: DeliveryMethod,
        ) -> Result<DeliveryDisposition, ExternalError> {
            Ok(DeliveryDisposition::NotSent {
                copied_to_clipboard: false,
                reason: "the clipboard was not ready, so nothing was pasted".to_owned(),
            })
        }
    }
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/not-sent.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut crate::support::ReadyRecorder,
        )
        .unwrap();
    runtime.capture_recording(job.id, 2.0).unwrap();

    let not_sent = transcribe_and_deliver(
        &mut runtime,
        job.id,
        "Durable final words.",
        &mut HeadlessDeliveryGate,
        &mut NotSentDeliverer,
    )
    .unwrap();

    assert_eq!(not_sent.stage, JobStage::ReadyToDeliver);
    assert_eq!(not_sent.delivery_status, DeliveryStatus::NotAttempted);
    assert_eq!(
        not_sent.error_message.as_deref(),
        Some("the clipboard was not ready, so nothing was pasted")
    );
    let mut submitted = CountingSubmittedDeliverer { attempts: 0 };
    let delivered = copy_again(&mut runtime, job.id, &mut submitted).unwrap();
    assert_eq!(delivered.stage, JobStage::Delivered);
    assert_eq!(submitted.attempts, 1);
}

#[test]
fn failed_write_after_a_paste_attempt_marks_it_ambiguous_instead_of_stranding_it() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/submitted.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut crate::support::ReadyRecorder,
        )
        .unwrap();
    runtime.capture_recording(job.id, 2.0).unwrap();
    rusqlite::Connection::open(&database_path)
        .unwrap()
        .execute_batch(
            r#"
            CREATE TRIGGER reject_delivered_checkpoint
            BEFORE UPDATE OF stage ON dictation_jobs
            WHEN NEW.stage = 'delivered'
            BEGIN
                SELECT RAISE(FAIL, 'delivered checkpoint unavailable');
            END;
            "#,
        )
        .unwrap();
    let mut deliverer = CountingSubmittedDeliverer { attempts: 0 };

    let error = transcribe_and_deliver(
        &mut runtime,
        job.id,
        "Durable final words.",
        &mut HeadlessDeliveryGate,
        &mut deliverer,
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("delivered checkpoint unavailable")
    );
    assert_eq!(deliverer.attempts, 1);
    let failed = runtime.job(job.id).unwrap().unwrap();
    assert_eq!(failed.stage, JobStage::Failed);
    assert_eq!(failed.delivery_status, DeliveryStatus::Ambiguous);
    assert_eq!(failed.final_text, "Durable final words.");
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
    let mut ambiguous = AmbiguousDeliverer { attempts: 0 };
    transcribe_and_deliver(
        &mut runtime,
        job.id,
        "Only once.",
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
    let error = runtime.prepare_delivery_retry(job.id).unwrap_err();

    assert!(error.to_string().contains("no durable outcome"));
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
            BEFORE DELETE ON dictation_jobs
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
    restarted
        .reconcile_recovery_deletions(audio_path.parent().unwrap())
        .unwrap();

    assert_eq!(std::fs::read(&audio_path).unwrap(), audio);
    assert!(!quarantine.exists());
    assert_eq!(
        restarted.job(job.id).unwrap().unwrap().stage,
        JobStage::Interrupted
    );
}

#[test]
fn startup_removes_audio_left_in_quarantine_by_a_committed_delete() {
    let directory = TempDir::new().unwrap();
    let recordings = directory.path().join("recordings");
    std::fs::create_dir_all(&recordings).unwrap();
    let runtime = Runtime::open(directory.path().join("agentdictate.db")).unwrap();
    // The job row is gone, so the delete committed before the unlink.
    let quarantine = recordings.join(format!(".agentdictate-delete-{}.pending", JobId::new()));
    std::fs::write(&quarantine, b"RIFFaudio the user deleted").unwrap();

    runtime.reconcile_recovery_deletions(&recordings).unwrap();

    assert!(!quarantine.exists());
}

#[test]
fn vocabulary_aliases_are_corrected_before_delivery() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut recorder = InspectingRecorder {
        database_path,
        saw_durable_starting_job: false,
    };
    let mut request = request(
        &directory.path().join("recordings/vocabulary.wav"),
        TRANSCRIPTION_MODEL,
    );
    request.options = Some(DictationOptions::from_settings(&Settings {
        vocabulary: parse_vocabulary("AgentDictate = durable final words").unwrap(),
        ..Settings::default()
    }));
    let job = runtime.start_recording(request, &mut recorder).unwrap();
    runtime.capture_recording(job.id, 4.0).unwrap();
    let mut deliverer = CountingSubmittedDeliverer { attempts: 0 };

    let delivered = transcribe_and_deliver(
        &mut runtime,
        job.id,
        "Durable final words.",
        &mut HeadlessDeliveryGate,
        &mut deliverer,
    )
    .unwrap();

    assert_eq!(delivered.raw_transcript, "Durable final words.");
    assert_eq!(delivered.final_text, "AgentDictate.");
}

/// Recovery's "Paste again": the stored transcript, copied once.
fn copy_again(
    runtime: &mut Runtime,
    id: JobId,
    deliverer: &mut impl Deliverer,
) -> Result<RecordingJob, RuntimeError> {
    let ready = runtime.prepare_delivery_retry(id)?;
    runtime.deliver_ready(
        ready,
        DeliveryMethod::CopyOnly,
        &mut HeadlessDeliveryGate,
        deliverer,
    )
}

#[test]
fn garbage_database_is_set_aside_and_dictation_can_start() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.sqlite");
    std::fs::write(&database_path, b"not a database, only garbage bytes").unwrap();

    let (mut runtime, set_aside) = Runtime::open_or_set_aside(&database_path).unwrap();

    let set_aside = set_aside.expect("the unreadable file is set aside");
    assert!(
        set_aside
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("agentdictate.sqlite.corrupt-")
    );
    assert_eq!(
        std::fs::read(&set_aside).unwrap(),
        b"not a database, only garbage bytes"
    );
    let job = runtime
        .start_recording(
            request(
                &directory.path().join("recordings/after.wav"),
                TRANSCRIPTION_MODEL,
            ),
            &mut crate::support::ReadyRecorder,
        )
        .unwrap();
    assert_eq!(job.stage, JobStage::Recording);
    drop(runtime);
    let (_, again) = Runtime::open_or_set_aside(&database_path).unwrap();
    assert!(again.is_none());
}
