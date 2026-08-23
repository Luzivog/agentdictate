use agentdictate_core::AppliedReplacement;
use agentdictate_runtime::{
    ExternalDictationImportOutcome, ExternalDictationReceipt, ExternalDictationSource,
    ExternalError, JobStage, Recorder, RecordingJob, RecordingRequest, Runtime,
};
use chrono::{TimeZone, Utc};
use tempfile::TempDir;

fn receipt(source_id: &str) -> ExternalDictationReceipt {
    ExternalDictationReceipt {
        source: ExternalDictationSource::ChatGptDesktop,
        source_id: source_id.to_owned(),
        started_at: Utc.with_ymd_and_hms(2026, 8, 23, 1, 2, 3).unwrap(),
        duration_seconds: 30.5,
        transcription_model: "Managed by ChatGPT".to_owned(),
        raw_transcript: "Ship the versel usage importer.".to_owned(),
        final_text: "Ship the Vercel usage importer.".to_owned(),
        replacements_applied: vec![AppliedReplacement {
            rule_id: Some(7),
            source_phrase: "versel".to_owned(),
            replacement_phrase: "Vercel".to_owned(),
            count: 1,
        }],
    }
}

struct ReadyRecorder;

impl Recorder for ReadyRecorder {
    fn start(&mut self, _job: &RecordingJob) -> Result<(), ExternalError> {
        Ok(())
    }
}

#[test]
fn external_dictation_import_is_idempotent_and_feeds_usage_and_history() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.sqlite");
    let mut runtime = Runtime::open(&database_path).unwrap();

    let first = runtime
        .import_external_dictation(&receipt("dictation-one"))
        .unwrap();
    let duplicate = runtime
        .import_external_dictation(&receipt("dictation-one"))
        .unwrap();

    assert!(matches!(
        first,
        ExternalDictationImportOutcome::Imported { word_count: 5, .. }
    ));
    assert_eq!(duplicate, ExternalDictationImportOutcome::AlreadyImported);

    let usage = runtime.usage_summary().unwrap();
    assert_eq!(usage.all_time.total_sessions, 1);
    assert_eq!(usage.all_time.total_words, 5);
    assert_eq!(usage.all_time.total_audio_seconds, 30.5);
    assert_eq!(
        usage.most_used_transcription_model.as_deref(),
        Some("Managed by ChatGPT")
    );

    let connection = rusqlite::Connection::open(database_path).unwrap();
    let stored: (String, u64, u64, f64) = connection
        .query_row(
            r#"
            SELECT transcription_provider, final_word_count,
                   final_character_count, estimated_total_cost
            FROM dictation_sessions
            "#,
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(stored.0, "chatgpt_subscription");
    assert_eq!(stored.1, 5);
    assert_eq!(stored.2, 31);
    assert_eq!(stored.3, 0.0);
    let history = runtime.list_history(Default::default()).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].raw_transcript, "Ship the versel usage importer.");
    assert_eq!(history[0].final_text, "Ship the Vercel usage importer.");
    assert_eq!(history[0].replacements_applied.len(), 1);
    assert_eq!(history[0].replacements_applied[0].rule_id, Some(7));
    assert!(!history[0].copied_to_clipboard);
    assert!(!history[0].paste_triggered);
}

#[test]
fn clearing_history_does_not_make_old_external_receipts_importable_again() {
    let directory = TempDir::new().unwrap();
    let mut runtime = Runtime::open(directory.path().join("agentdictate.sqlite")).unwrap();
    let receipt = receipt("dictation-to-clear");
    runtime.import_external_dictation(&receipt).unwrap();

    runtime.clear_history().unwrap();

    assert_eq!(
        runtime.import_external_dictation(&receipt).unwrap(),
        ExternalDictationImportOutcome::AlreadyImported
    );
    assert_eq!(runtime.usage_summary().unwrap().all_time.total_sessions, 0);
}

#[test]
fn invalid_external_receipts_are_rejected_before_writing_usage() {
    let directory = TempDir::new().unwrap();
    let mut runtime = Runtime::open(directory.path().join("agentdictate.sqlite")).unwrap();
    let mut invalid = receipt("dictation-invalid");
    invalid.duration_seconds = f64::NAN;

    let error = runtime.import_external_dictation(&invalid).unwrap_err();

    assert!(error.to_string().contains("duration"));
    assert_eq!(runtime.usage_summary().unwrap().all_time.total_sessions, 0);
}

#[test]
fn background_writer_open_does_not_reconcile_the_live_recording() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.sqlite");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut recorder = ReadyRecorder;
    let job = runtime
        .start_recording(
            RecordingRequest {
                audio_path: directory.path().join("active.wav"),
                started_at: Utc::now(),
                transcription_provider: Default::default(),
                transcription_model: "test-model".to_owned(),
            },
            &mut recorder,
        )
        .unwrap();

    let _worker = Runtime::open_background_writer(&database_path).unwrap();

    assert_eq!(
        runtime.job(job.id).unwrap().unwrap().stage,
        JobStage::Recording
    );
}
