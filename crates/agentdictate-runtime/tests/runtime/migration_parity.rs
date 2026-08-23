use std::fs;
use std::os::unix::fs::PermissionsExt;

use agentdictate_runtime::{
    DeliveryStatus, HistoryQuery, JobStage, Runtime, Settings, TranscriptionProvider,
    load_settings, save_settings,
};
use rusqlite::Connection;
use tempfile::TempDir;

#[test]
fn python_settings_json_keeps_values_and_repairs_missing_pricing() {
    let directory = TempDir::new().unwrap();
    let settings_path = directory.path().join("config.json");
    fs::write(
        &settings_path,
        r#"{
  "hotkey": "Alt+Space",
  "max_recording_seconds": 45,
  "cleanup_enabled": false,
  "transcription_prices": {},
  "cleanup_prices": {},
  "future_python_field": "ignored"
}
"#,
    )
    .unwrap();

    let settings = load_settings(&settings_path).unwrap();

    assert_eq!(settings.hotkey, "Alt+Space");
    assert_eq!(settings.max_recording_seconds, 45);
    assert!(!settings.cleanup_enabled);
    assert_eq!(
        settings.transcription_prices["gpt-transcribe"].price_per_audio_minute,
        0.0045
    );
    assert_eq!(
        settings.cleanup_prices["gpt-5.4-nano"].input_price_per_1m_tokens,
        0.05
    );
    assert!(!settings_path.with_extension("json.tmp").exists());
}

#[test]
fn settings_replacement_is_private_and_leaves_no_partial_file() {
    let directory = TempDir::new().unwrap();
    let settings_path = directory.path().join("config.json");
    let mut settings = Settings {
        openai_api_key: "secret-key".to_owned(),
        hotkey: "Ctrl+Space".to_owned(),
        ..Settings::default()
    };

    save_settings(&settings_path, &settings).unwrap();
    settings.hotkey = "Alt+Space".to_owned();
    save_settings(&settings_path, &settings).unwrap();

    assert_eq!(load_settings(&settings_path).unwrap().hotkey, "Alt+Space");
    assert_eq!(
        fs::metadata(&settings_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!settings_path.with_extension("json.tmp").exists());
}

#[test]
fn legacy_python_dictation_jobs_are_migrated_without_losing_recovery_data() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let audio_path = directory.path().join("recordings/legacy.wav");
    let connection = Connection::open(&database_path).unwrap();
    connection
        .execute_batch(
            r#"
            CREATE TABLE dictation_sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                started_at TEXT NOT NULL,
                ended_at TEXT NOT NULL,
                duration_seconds REAL NOT NULL DEFAULT 0,
                transcription_model TEXT NOT NULL,
                cleanup_enabled INTEGER NOT NULL DEFAULT 0,
                cleanup_model TEXT,
                cleanup_style TEXT,
                raw_word_count INTEGER NOT NULL DEFAULT 0,
                final_word_count INTEGER NOT NULL DEFAULT 0,
                final_character_count INTEGER NOT NULL DEFAULT 0,
                estimated_transcription_cost REAL NOT NULL DEFAULT 0,
                estimated_cleanup_cost REAL NOT NULL DEFAULT 0,
                estimated_total_cost REAL NOT NULL DEFAULT 0,
                success INTEGER NOT NULL DEFAULT 1,
                error_message TEXT
            );
            CREATE TABLE transcript_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id INTEGER NOT NULL REFERENCES dictation_sessions(id) ON DELETE CASCADE,
                created_at TEXT NOT NULL,
                raw_transcript TEXT NOT NULL DEFAULT '',
                cleaned_transcript TEXT,
                final_text TEXT NOT NULL DEFAULT '',
                replacements_applied TEXT NOT NULL DEFAULT '[]',
                copied_to_clipboard INTEGER NOT NULL DEFAULT 0,
                paste_triggered INTEGER NOT NULL DEFAULT 0,
                cleanup_error TEXT
            );
            CREATE TABLE dictation_jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                started_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                state TEXT NOT NULL,
                stage TEXT NOT NULL,
                audio_path TEXT NOT NULL UNIQUE,
                duration_seconds REAL NOT NULL DEFAULT 0,
                transcription_model TEXT NOT NULL DEFAULT '',
                raw_transcript TEXT NOT NULL DEFAULT '',
                final_text TEXT NOT NULL DEFAULT '',
                copied_to_clipboard INTEGER NOT NULL DEFAULT 0,
                paste_triggered INTEGER NOT NULL DEFAULT 0,
                error_message TEXT
            );
            INSERT INTO dictation_sessions (
                id, started_at, ended_at, transcription_model
            ) VALUES (
                7, '2026-08-18T12:00:00+00:00', '2026-08-18T12:00:30+00:00',
                'gpt-transcribe'
            );
            INSERT INTO transcript_history (
                id, session_id, created_at, raw_transcript, final_text,
                replacements_applied
            ) VALUES (
                9, 7, '2026-08-18T12:00:30+00:00',
                'historic raw', 'Historic final.',
                '[{"id":5,"source_phrase":"versel","replacement_phrase":"Vercel","count":1}]'
            );
            "#,
        )
        .unwrap();
    connection
        .execute(
            r#"
            INSERT INTO dictation_jobs (
                id, started_at, updated_at, state, stage, audio_path,
                duration_seconds, transcription_model, raw_transcript, final_text
            ) VALUES (
                42, '2026-08-18T12:00:00+00:00', '2026-08-18T12:00:30+00:00',
                'captured', 'captured', ?1, 30, 'gpt-transcribe',
                'legacy raw transcript', 'Legacy final transcript.'
            )
            "#,
            [audio_path.to_string_lossy().as_ref()],
        )
        .unwrap();
    drop(connection);

    let mut runtime = Runtime::open(&database_path).unwrap();
    let history = runtime.list_history(HistoryQuery::default()).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].job_id, None);
    assert_eq!(
        history[0].transcription_provider,
        TranscriptionProvider::OpenAiApi
    );
    assert_eq!(history[0].replacements_applied[0].rule_id, Some(5));
    assert_eq!(runtime.usage_summary().unwrap().all_time.total_sessions, 1);
    let literal_before_backfill = runtime
        .history_page(HistoryQuery {
            search: "Historic".to_owned(),
            limit: 10,
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(literal_before_backfill.total_matches, 1);
    assert_eq!(literal_before_backfill.matches[0].entry.id, 9);
    let typo_before_backfill = runtime
        .history_page(HistoryQuery {
            search: "Histroic".to_owned(),
            limit: 10,
            ..HistoryQuery::default()
        })
        .unwrap();
    assert!(typo_before_backfill.matches.is_empty());

    runtime.ensure_history_search_index().unwrap();

    let typo_after_backfill = runtime
        .history_page(HistoryQuery {
            search: "Histroic".to_owned(),
            limit: 10,
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(typo_after_backfill.total_matches, 1);
    assert_eq!(typo_after_backfill.matches[0].entry.id, 9);
    let jobs = runtime.recoverable_jobs().unwrap();
    let migrated_id = jobs[0].id;
    drop(runtime);
    let reopened = Runtime::open(&database_path).unwrap();
    let migrated = reopened.job(migrated_id).unwrap().unwrap();

    assert_eq!(migrated.legacy_id, 42);
    assert_eq!(
        migrated.transcription_provider,
        TranscriptionProvider::OpenAiApi
    );
    assert_eq!(migrated.stage, JobStage::Captured);
    assert_eq!(migrated.audio_path, audio_path);
    assert_eq!(migrated.raw_transcript, "legacy raw transcript");
    assert_eq!(migrated.final_text, "Legacy final transcript.");
    drop(reopened);
    let connection = Connection::open(&database_path).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT id FROM dictation_jobs", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        42
    );
    assert_eq!(
        connection
            .query_row("SELECT final_text FROM transcript_history", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
        "Historic final."
    );
}

#[test]
fn fresh_database_contains_the_complete_python_compatible_schema() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    drop(Runtime::open(&database_path).unwrap());
    let connection = Connection::open(&database_path).unwrap();

    for table in [
        "dictation_sessions",
        "transcript_history",
        "external_dictation_imports",
        "replacement_mappings",
        "daily_stats",
        "pricing_settings",
        "dictation_jobs",
    ] {
        let exists = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                [table],
                |row| row.get::<_, bool>(0),
            )
            .unwrap();
        assert!(exists, "missing compatibility table {table}");
    }
}

#[test]
fn legacy_inflight_delivery_is_migrated_as_ambiguous_and_never_safe_to_resume() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let audio_path = directory.path().join("recordings/possibly-pasted.wav");
    let connection = Connection::open(&database_path).unwrap();
    connection
        .execute_batch(
            r#"
            CREATE TABLE dictation_jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                started_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                state TEXT NOT NULL,
                stage TEXT NOT NULL,
                audio_path TEXT NOT NULL UNIQUE,
                duration_seconds REAL NOT NULL DEFAULT 0,
                transcription_model TEXT NOT NULL DEFAULT '',
                raw_transcript TEXT NOT NULL DEFAULT '',
                final_text TEXT NOT NULL DEFAULT '',
                copied_to_clipboard INTEGER NOT NULL DEFAULT 0,
                paste_triggered INTEGER NOT NULL DEFAULT 0,
                error_message TEXT
            );
            "#,
        )
        .unwrap();
    connection
        .execute(
            r#"
            INSERT INTO dictation_jobs (
                id, started_at, updated_at, state, stage, audio_path,
                final_text, copied_to_clipboard
            ) VALUES (
                73, '2026-08-18T12:00:00+00:00', '2026-08-18T12:00:30+00:00',
                'delivering', 'delivering', ?1, 'Could already be pasted.', 1
            )
            "#,
            [audio_path.to_string_lossy().as_ref()],
        )
        .unwrap();
    drop(connection);

    let runtime = Runtime::open(&database_path).unwrap();
    let jobs = runtime.recoverable_jobs().unwrap();

    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].stage, JobStage::Failed);
    assert_eq!(jobs[0].delivery_status, DeliveryStatus::Ambiguous);
    assert_eq!(jobs[0].final_text, "Could already be pasted.");
    drop(runtime);
    let connection = Connection::open(&database_path).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT id FROM dictation_jobs", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        73
    );
}

#[test]
fn legacy_transcribed_checkpoint_is_migrated_to_safe_delivery_without_changing_its_id() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let audio_path = directory.path().join("recordings/transcribed.wav");
    let connection = Connection::open(&database_path).unwrap();
    connection
        .execute_batch(
            r#"
            CREATE TABLE dictation_jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                started_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                state TEXT NOT NULL,
                stage TEXT NOT NULL,
                audio_path TEXT NOT NULL UNIQUE,
                duration_seconds REAL NOT NULL DEFAULT 0,
                transcription_model TEXT NOT NULL DEFAULT '',
                raw_transcript TEXT NOT NULL DEFAULT '',
                final_text TEXT NOT NULL DEFAULT '',
                copied_to_clipboard INTEGER NOT NULL DEFAULT 0,
                paste_triggered INTEGER NOT NULL DEFAULT 0,
                error_message TEXT
            );
            "#,
        )
        .unwrap();
    connection
        .execute(
            r#"
            INSERT INTO dictation_jobs (
                id, started_at, updated_at, state, stage, audio_path,
                raw_transcript, final_text
            ) VALUES (
                74, '2026-08-18T12:00:00+00:00', '2026-08-18T12:00:30+00:00',
                'transcribed', 'transcribed', ?1,
                'legacy safe raw', 'Legacy safe final.'
            )
            "#,
            [audio_path.to_string_lossy().as_ref()],
        )
        .unwrap();
    drop(connection);

    let runtime = Runtime::open(&database_path).unwrap();
    let jobs = runtime.recoverable_jobs().unwrap();

    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].stage, JobStage::ReadyToDeliver);
    assert_eq!(jobs[0].delivery_status, DeliveryStatus::NotAttempted);
    assert_eq!(jobs[0].raw_transcript, "legacy safe raw");
    assert_eq!(jobs[0].final_text, "Legacy safe final.");
    drop(runtime);
    let connection = Connection::open(&database_path).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT id FROM dictation_jobs", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        74
    );
}

#[test]
fn legacy_cleanup_failure_keeps_its_saved_raw_transcript_as_recoverable() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let audio_path = directory.path().join("recordings/cleanup-failed.wav");
    let connection = Connection::open(&database_path).unwrap();
    connection
        .execute_batch(
            r#"
            CREATE TABLE dictation_jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                started_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                state TEXT NOT NULL,
                stage TEXT NOT NULL,
                audio_path TEXT NOT NULL UNIQUE,
                duration_seconds REAL NOT NULL DEFAULT 0,
                transcription_model TEXT NOT NULL DEFAULT '',
                raw_transcript TEXT NOT NULL DEFAULT '',
                final_text TEXT NOT NULL DEFAULT '',
                copied_to_clipboard INTEGER NOT NULL DEFAULT 0,
                paste_triggered INTEGER NOT NULL DEFAULT 0,
                error_message TEXT
            );
            "#,
        )
        .unwrap();
    connection
        .execute(
            r#"
            INSERT INTO dictation_jobs (
                id, started_at, updated_at, state, stage, audio_path,
                raw_transcript, error_message
            ) VALUES (
                75, '2026-08-18T12:00:00+00:00', '2026-08-18T12:00:30+00:00',
                'failed', 'cleanup', ?1, 'Five minutes of saved words',
                'cleanup response was invalid'
            )
            "#,
            [audio_path.to_string_lossy().as_ref()],
        )
        .unwrap();
    drop(connection);

    let runtime = Runtime::open(&database_path).unwrap();
    let jobs = runtime.recoverable_jobs().unwrap();

    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].stage, JobStage::Interrupted);
    assert_eq!(jobs[0].raw_transcript, "Five minutes of saved words");
    assert_eq!(
        jobs[0].error_message.as_deref(),
        Some("cleanup response was invalid")
    );
    drop(runtime);
    let connection = Connection::open(&database_path).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT id FROM dictation_jobs", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        75
    );
}
