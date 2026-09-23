use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use agentdictate_runtime::{
    DeliveryStatus, FinishedJobCleanup, JobId, JobStage, Runtime, Settings, load_settings,
    save_settings,
};
use rusqlite::{Connection, params};
use tempfile::TempDir;

#[test]
fn settings_replacement_is_private_and_leaves_no_partial_file() {
    let directory = TempDir::new().unwrap();
    let settings_path = directory.path().join("config.json");
    let mut settings = Settings {
        openai_api_key: "secret-key".to_owned(),
        ..Settings::default()
    };

    save_settings(&settings_path, &settings).unwrap();
    settings.hotkey = "Alt+Space".parse().unwrap();
    save_settings(&settings_path, &settings).unwrap();

    assert_eq!(
        load_settings(&settings_path).unwrap().hotkey,
        settings.hotkey
    );
    assert_eq!(
        fs::metadata(&settings_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!settings_path.with_extension("json.tmp").exists());
}

/// Inserts a job row, last changed a moment ago, as the version before
/// finished jobs were deleted left it, with a recording when `has_audio`.
/// Returns the audio path.
fn insert_previous_version_job(
    connection: &Connection,
    recordings: &Path,
    id: JobId,
    stage: &str,
    has_audio: bool,
) -> PathBuf {
    let audio_path = recordings.join(format!("dictation-{id}.wav"));
    if has_audio {
        fs::write(&audio_path, b"RIFFprivate speech").unwrap();
    }
    connection
        .execute(
            r#"
            INSERT INTO dictation_jobs (
                runtime_id, started_at, updated_at, state, stage, audio_path,
                duration_seconds, transcription_model, raw_transcript, final_text,
                delivery_status
            ) VALUES (
                ?1, '2026-08-18T12:00:00Z', ?2, ?3, ?3, ?4,
                30, 'gpt-transcribe', 'raw words', 'Final words.', 'submitted'
            )
            "#,
            params![
                id.to_string(),
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                stage,
                audio_path.to_string_lossy()
            ],
        )
        .unwrap();
    audio_path
}

fn age_by_two_hours(path: &Path) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(2 * 60 * 60))
        .unwrap();
}

#[test]
fn startup_cleanup_migrates_finished_jobs_and_sweeps_their_recordings() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let recordings = directory.path().join("recordings");
    fs::create_dir_all(&recordings).unwrap();
    let mut runtime = Runtime::open(&database_path).unwrap();
    let connection = Connection::open(&database_path).unwrap();
    let recorded = JobId::new();
    let interrupted = JobId::new();
    let failed = JobId::new();
    insert_previous_version_job(&connection, &recordings, recorded, "delivered", true);
    // The daemon died after the paste, before recording usage and History.
    insert_previous_version_job(&connection, &recordings, interrupted, "delivered", true);
    insert_previous_version_job(&connection, &recordings, JobId::new(), "no_speech", true);
    insert_previous_version_job(&connection, &recordings, JobId::new(), "deleted", false);
    let failed_audio =
        insert_previous_version_job(&connection, &recordings, failed, "failed", true);
    connection
        .execute(
            r#"
            INSERT INTO dictations (
                job_id, started_at, ended_at, duration_seconds, transcription_provider,
                transcription_model, word_count, character_count, estimated_cost, final_text
            ) VALUES (
                ?1, '2026-08-18T12:00:00Z', '2026-08-18T12:00:30Z', 30, 'openai_api',
                'gpt-transcribe', 2, 12, 0.00225, 'Final words.'
            )
            "#,
            [recorded.to_string()],
        )
        .unwrap();
    let stale_orphan = recordings.join("recording-1787070998007.wav");
    let fresh_orphan = recordings.join("recording-1787071004102.wav");
    fs::write(&stale_orphan, b"RIFFleaked speech").unwrap();
    fs::write(&fresh_orphan, b"RIFFleaked speech").unwrap();
    age_by_two_hours(&stale_orphan);

    let cleanup = runtime
        .clean_up_finished_jobs(&Settings::default(), &recordings)
        .unwrap();

    assert_eq!(
        cleanup,
        FinishedJobCleanup {
            recorded_deliveries: 1,
            removed_jobs: 4,
            removed_recordings: 4,
            ..FinishedJobCleanup::default()
        }
    );
    let remaining_jobs = connection
        .prepare("SELECT runtime_id FROM dictation_jobs")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(remaining_jobs, [failed.to_string()]);
    let mut history_jobs = crate::support::stored_dictations(&database_path)
        .into_iter()
        .map(|entry| entry.job_id.unwrap())
        .collect::<Vec<_>>();
    history_jobs.sort();
    let mut expected_history_jobs = vec![recorded.to_string(), interrupted.to_string()];
    expected_history_jobs.sort();
    assert_eq!(history_jobs, expected_history_jobs);
    let mut remaining_files = fs::read_dir(&recordings)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    remaining_files.sort();
    let mut expected_files = vec![failed_audio, fresh_orphan];
    expected_files.sort();
    assert_eq!(remaining_files, expected_files);
    assert_eq!(
        runtime
            .clean_up_finished_jobs(&Settings::default(), &recordings)
            .unwrap(),
        FinishedJobCleanup::default()
    );
}

#[test]
fn startup_cleanup_keeps_recordings_while_preserve_temporary_audio_is_on() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let recordings = directory.path().join("recordings");
    fs::create_dir_all(&recordings).unwrap();
    let mut runtime = Runtime::open(&database_path).unwrap();
    let connection = Connection::open(&database_path).unwrap();
    let audio_path =
        insert_previous_version_job(&connection, &recordings, JobId::new(), "no_speech", true);
    age_by_two_hours(&audio_path);
    let settings = Settings {
        preserve_temp_audio: true,
        ..Settings::default()
    };

    let cleanup = runtime
        .clean_up_finished_jobs(&settings, &recordings)
        .unwrap();

    assert_eq!(cleanup.removed_jobs, 1);
    assert_eq!(cleanup.removed_recordings, 0);
    assert!(audio_path.is_file());
}

/// The tables an unversioned database had, as the last release before
/// schema versions left them, with the retired full-text index, caches,
/// import ledger and Replacements rules.
const UNVERSIONED_SCHEMA: &str = r#"
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
    error_message TEXT,
    runtime_job_id TEXT,
    transcription_provider TEXT NOT NULL DEFAULT 'openai_api'
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
CREATE TABLE external_dictation_imports (
    source TEXT NOT NULL,
    source_id TEXT NOT NULL,
    imported_at TEXT NOT NULL,
    session_id INTEGER REFERENCES dictation_sessions(id) ON DELETE SET NULL,
    PRIMARY KEY (source, source_id)
);
CREATE TABLE replacement_mappings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_phrase TEXT NOT NULL,
    replacement_phrase TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    case_sensitive INTEGER NOT NULL DEFAULT 0,
    whole_word_only INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE daily_stats (date TEXT PRIMARY KEY, total_sessions INTEGER NOT NULL DEFAULT 0);
CREATE TABLE pricing_settings (model_name TEXT NOT NULL, model_type TEXT NOT NULL);
CREATE TABLE history_search_state (id INTEGER PRIMARY KEY, schema_version INTEGER, ready INTEGER);
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
    error_message TEXT,
    runtime_id TEXT,
    delivery_status TEXT NOT NULL DEFAULT 'not_attempted',
    cleaned_transcript TEXT,
    replacements_applied TEXT NOT NULL DEFAULT '[]',
    cleanup_error TEXT,
    transcription_provider TEXT NOT NULL DEFAULT 'openai_api',
    processing_options TEXT
);
CREATE INDEX idx_sessions_started_at ON dictation_sessions(started_at);
CREATE INDEX idx_history_created_at ON transcript_history(created_at);
CREATE INDEX idx_dictation_jobs_state ON dictation_jobs(state);
CREATE UNIQUE INDEX idx_dictation_jobs_runtime_id ON dictation_jobs(runtime_id);
CREATE UNIQUE INDEX idx_sessions_runtime_job_id ON dictation_sessions(runtime_job_id);
CREATE VIRTUAL TABLE transcript_history_fts USING fts5(
    final_text, content='transcript_history', content_rowid='id'
);
CREATE VIRTUAL TABLE transcript_history_fts_vocab USING fts5vocab(transcript_history_fts, 'row');
CREATE VIRTUAL TABLE transcript_history_fts_trigram USING fts5(
    final_text, content='transcript_history', content_rowid='id', tokenize='trigram'
);
CREATE TRIGGER transcript_history_fts_insert AFTER INSERT ON transcript_history BEGIN
    INSERT INTO transcript_history_fts(rowid, final_text) VALUES (new.id, new.final_text);
END;
CREATE TRIGGER transcript_history_fts_trigram_insert AFTER INSERT ON transcript_history BEGIN
    INSERT INTO transcript_history_fts_trigram(rowid, final_text) VALUES (new.id, new.final_text);
END;
INSERT INTO daily_stats VALUES ('2026-08-18', 3);
INSERT INTO pricing_settings VALUES ('gpt-transcribe', 'transcription');
INSERT INTO history_search_state VALUES (1, 3, 1);
INSERT INTO replacement_mappings (
    source_phrase, replacement_phrase, created_at, updated_at
) VALUES ('lead lord', 'Leadlord', '2026-08-01T00:00:00Z', '2026-08-01T00:00:00Z');
"#;

/// Jobs an unversioned database stores with spellings of older releases.
struct LegacyJobs {
    /// Delivered, stored as `committed`; a crash interrupted its completion.
    committed: JobId,
    /// Stored as `canceled`, with its audio.
    canceled: JobId,
    /// Stored as `delivering` before the paste attempt was recorded.
    delivering: JobId,
}

/// Writes an unversioned database at `path`: a dictation with vocabulary
/// corrections, one from the retired subscription route, a ChatGPT import
/// with its History and one without, an import tombstone, and jobs with
/// legacy spellings. History row ids differ from session ids, as they do in
/// real databases.
fn write_unversioned_database(path: &Path) -> LegacyJobs {
    let connection = Connection::open(path).unwrap();
    connection.execute_batch(UNVERSIONED_SCHEMA).unwrap();
    let session = |started: &str,
                   ended: &str,
                   seconds: f64,
                   provider: &str,
                   model: &str,
                   words: u64,
                   cost: f64| {
        connection
            .execute(
                r#"
                INSERT INTO dictation_sessions (
                    started_at, ended_at, duration_seconds, transcription_provider,
                    transcription_model, raw_word_count, final_word_count,
                    final_character_count, estimated_transcription_cost, estimated_total_cost
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, 20, ?7, ?7)
                "#,
                params![started, ended, seconds, provider, model, words, cost],
            )
            .unwrap();
        connection.last_insert_rowid()
    };
    let corrected = session(
        "2026-08-18T12:00:00+00:00",
        "2026-08-18T12:00:30+00:00",
        30.0,
        "openai_api",
        "gpt-transcribe",
        4,
        0.00225,
    );
    let subscription = session(
        "2026-08-18T12:05:00Z",
        "2026-08-18T12:05:10Z",
        10.0,
        "chatgpt_subscription",
        "gpt-4o-transcribe",
        2,
        0.00075,
    );
    let imported = session(
        "2026-08-18T12:10:00Z",
        "2026-08-18T12:10:20Z",
        20.0,
        "chatgpt_subscription",
        "Managed by ChatGPT",
        3,
        0.0,
    );
    let stats_only = session(
        "2026-08-18T12:20:00Z",
        "2026-08-18T12:20:05Z",
        5.0,
        "chatgpt_subscription",
        "Managed by ChatGPT",
        1,
        0.0,
    );
    let history = |session: i64, created_at: &str, raw: &str, text: &str, corrections: &str| {
        connection
            .execute(
                r#"
                INSERT INTO transcript_history (
                    session_id, created_at, raw_transcript, final_text, replacements_applied
                ) VALUES (?1, ?2, ?3, ?4, ?5)
                "#,
                params![session, created_at, raw, text, corrections],
            )
            .unwrap();
    };
    history(
        imported,
        "2026-08-18T12:10:20Z",
        "",
        "Imported from ChatGPT.",
        "[]",
    );
    history(
        corrected,
        "2026-08-18T12:00:30+00:00",
        "fix the versel deploy",
        "fix the Vercel deploy",
        r#"[{"source_phrase":"versel","replacement_phrase":"Vercel","count":1}]"#,
    );
    history(
        subscription,
        "2026-08-18T12:05:10Z",
        "Plain words.",
        "Plain words.",
        "[]",
    );
    for (receipt, session) in [
        ("receipt-3", Some(imported)),
        ("receipt-4", Some(stats_only)),
        ("receipt-gone", None),
    ] {
        connection
            .execute(
                "INSERT INTO external_dictation_imports VALUES ('chatgpt_desktop', ?1, '2026-08-18T13:00:00Z', ?2)",
                params![receipt, session],
            )
            .unwrap();
    }
    let job = |stage: &str, delivery_status: &str, updated_at: &str| {
        let id = JobId::new();
        connection
            .execute(
                r#"
                INSERT INTO dictation_jobs (
                    runtime_id, started_at, updated_at, state, stage, audio_path,
                    duration_seconds, transcription_model, raw_transcript, final_text,
                    copied_to_clipboard, delivery_status
                ) VALUES (
                    ?1, '2026-08-18T12:30:00Z', ?2, ?3, ?3, ?4, 40, 'gpt-transcribe',
                    'Pasted before the crash.', 'Pasted before the crash.', 1, ?5
                )
                "#,
                params![
                    id.to_string(),
                    updated_at,
                    stage,
                    path.with_file_name(format!("{stage}.wav"))
                        .to_string_lossy(),
                    delivery_status
                ],
            )
            .unwrap();
        id
    };
    LegacyJobs {
        committed: job("delivered", "committed", "2026-08-18T12:30:40Z"),
        canceled: job("canceled", "not_attempted", "2026-08-18T11:00:00Z"),
        delivering: job("delivering", "not_attempted", "2026-08-18T11:30:00Z"),
    }
}

fn user_version(path: &Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn an_unversioned_database_becomes_one_dictations_table_with_the_same_history_and_usage() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let legacy = write_unversioned_database(&database_path);

    let mut runtime = Runtime::open(&database_path).unwrap();

    assert_eq!(user_version(&database_path), 1);
    let connection = Connection::open(&database_path).unwrap();
    let tables = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    // The Replacements rules wait for `retire_replacement_rules`.
    assert_eq!(
        tables,
        [
            "dictation_jobs",
            "dictations",
            "replacement_mappings",
            "sqlite_sequence"
        ]
    );
    let history = crate::support::history_rows(&runtime);
    assert_eq!(
        history
            .iter()
            .map(|row| (row.id, row.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (1, "Imported from ChatGPT."),
            (3, "Plain words."),
            (2, "fix the Vercel deploy")
        ]
    );
    let found = runtime
        .history_page(&agentdictate_core::HistoryPageRequest {
            search: "VERCEL".to_owned(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(found.rows.iter().map(|row| row.id).collect::<Vec<_>>(), [2]);
    let usage = runtime.usage().unwrap().all_time;
    assert_eq!((usage.dictations, usage.words), (4, 10));
    assert_eq!(usage.audio_seconds, 65.0);
    assert!((usage.estimated_cost - 0.003).abs() < 1e-12);
    let stored = connection
        .prepare(
            "SELECT id, source, source_id, started_at, ended_at, transcription_provider,
                    final_text IS NULL, raw_text, vocabulary_corrections IS NOT NULL
             FROM dictations ORDER BY id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, bool>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, bool>(8)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let row = |id: i64,
               source: &str,
               receipt: Option<&str>,
               started: &str,
               ended: &str,
               provider: &str,
               no_text: bool,
               raw: Option<&str>,
               corrected: bool| {
        (
            id,
            source.to_owned(),
            receipt.map(str::to_owned),
            started.to_owned(),
            ended.to_owned(),
            provider.to_owned(),
            no_text,
            raw.map(str::to_owned),
            corrected,
        )
    };
    assert_eq!(
        stored,
        [
            row(
                1,
                "chatgpt_desktop",
                Some("receipt-3"),
                "2026-08-18T12:10:00Z",
                "2026-08-18T12:10:20Z",
                "chatgpt_subscription",
                false,
                Some(""),
                false
            ),
            row(
                2,
                "agentdictate",
                None,
                "2026-08-18T12:00:00Z",
                "2026-08-18T12:00:30Z",
                "openai_api",
                false,
                Some("fix the versel deploy"),
                true
            ),
            row(
                3,
                "agentdictate",
                None,
                "2026-08-18T12:05:00Z",
                "2026-08-18T12:05:10Z",
                "chatgpt_subscription",
                false,
                None,
                false
            ),
            row(
                4,
                "chatgpt_desktop",
                Some("receipt-4"),
                "2026-08-18T12:20:00Z",
                "2026-08-18T12:20:05Z",
                "chatgpt_subscription",
                true,
                None,
                false
            ),
        ]
    );
    // Legacy job spellings are renamed once: a canceled job stays in
    // Recovery, a paste that may have started is never offered again, and
    // the committed delivery reads as submitted.
    let recoveries = runtime.recoveries().unwrap();
    assert_eq!(
        recoveries
            .iter()
            .map(|entry| (entry.job_id, entry.stage, entry.delivery_ambiguous))
            .collect::<Vec<_>>(),
        [
            (legacy.delivering, JobStage::Failed, true),
            (legacy.canceled, JobStage::Interrupted, false)
        ]
    );
    let committed = runtime.job(legacy.committed).unwrap().unwrap();
    assert_eq!(committed.delivery_status, DeliveryStatus::Submitted);
    let cleanup = runtime
        .clean_up_finished_jobs(&Settings::default(), directory.path())
        .unwrap();
    assert_eq!(cleanup.recorded_deliveries, 1);
    assert_eq!(
        crate::support::history_rows(&runtime)[0].text,
        "Pasted before the crash."
    );
    assert_eq!(runtime.usage().unwrap().all_time.dictations, 5);
}

#[test]
fn migrating_keeps_one_private_backup_and_runs_once() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let backup = directory.path().join("agentdictate.db.pre-v1");
    write_unversioned_database(&database_path);
    fs::write(&backup, b"an older backup").unwrap();

    drop(Runtime::open(&database_path).unwrap());

    assert_eq!(
        fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let saved_history: i64 = Connection::open(&backup)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM transcript_history", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(saved_history, 3);
    assert_eq!(user_version(&backup), 0);

    fs::remove_file(&backup).unwrap();
    drop(Runtime::open(&database_path).unwrap());
    assert!(!backup.exists());
    assert_eq!(user_version(&database_path), 1);

    let fresh = directory.path().join("fresh.db");
    drop(Runtime::open(&fresh).unwrap());
    assert!(!directory.path().join("fresh.db.pre-v1").exists());
    assert_eq!(user_version(&fresh), 1);
}

#[test]
fn a_database_from_a_newer_version_is_refused() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    drop(Runtime::open(&database_path).unwrap());
    Connection::open(&database_path)
        .unwrap()
        .execute_batch("PRAGMA user_version = 2")
        .unwrap();

    assert!(matches!(
        Runtime::open(&database_path),
        Err(agentdictate_runtime::RuntimeError::NewerDatabase {
            version: 2,
            latest: 1
        })
    ));
}

#[test]
fn replacements_rules_outlive_the_migration_until_they_move_into_vocabulary() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let config_file = directory.path().join("config.json");
    write_unversioned_database(&database_path);
    let runtime = Runtime::open(&database_path).unwrap();
    let mut settings = Settings::default();

    let retired = runtime
        .retire_replacement_rules(&mut settings, &config_file)
        .unwrap();

    assert_eq!(retired.len(), 1);
    assert_eq!(
        settings.vocabulary,
        agentdictate_core::parse_vocabulary("Leadlord = lead lord").unwrap()
    );
    assert!(
        runtime
            .retire_replacement_rules(&mut settings, &config_file)
            .unwrap()
            .is_empty()
    );
}
