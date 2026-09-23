use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use agentdictate_runtime::{
    DeliveryStatus, FinishedJobCleanup, HistoryQuery, JobId, JobStage, Runtime, Settings,
    load_settings, save_settings,
};
use rusqlite::{Connection, params};
use tempfile::TempDir;

#[test]
fn python_settings_json_keeps_values_and_ignores_retired_fields() {
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
fn stored_inflight_delivery_reopens_as_ambiguous_and_never_safe_to_resume() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    drop(Runtime::open(&database_path).unwrap());
    let id = JobId::new();
    Connection::open(&database_path)
        .unwrap()
        .execute(
            r#"
            INSERT INTO dictation_jobs (
                runtime_id, started_at, updated_at, state, stage, audio_path,
                final_text, copied_to_clipboard
            ) VALUES (
                ?1, '2026-08-18T12:00:00Z', '2026-08-18T12:00:30Z',
                'delivering', 'delivering', ?2, 'Could already be pasted.', 1
            )
            "#,
            params![
                id.to_string(),
                directory
                    .path()
                    .join("possibly-pasted.wav")
                    .to_string_lossy()
            ],
        )
        .unwrap();

    let job = Runtime::open(&database_path)
        .unwrap()
        .job(id)
        .unwrap()
        .unwrap();

    assert_eq!(job.stage, JobStage::Failed);
    assert_eq!(job.delivery_status, DeliveryStatus::Ambiguous);
    assert_eq!(job.final_text, "Could already be pasted.");
}

#[test]
fn committed_deliveries_from_the_first_release_read_as_submitted() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    drop(Runtime::open(&database_path).unwrap());
    let id = JobId::new();
    insert_previous_version_job(
        &Connection::open(&database_path).unwrap(),
        directory.path(),
        id,
        "delivered",
        false,
    );
    Connection::open(&database_path)
        .unwrap()
        .execute(
            "UPDATE dictation_jobs SET delivery_status = 'committed'",
            [],
        )
        .unwrap();

    let job = Runtime::open(&database_path)
        .unwrap()
        .job(id)
        .unwrap()
        .unwrap();

    assert_eq!(job.delivery_status, DeliveryStatus::Submitted);
}

/// Inserts a job row as the version before finished jobs were deleted left
/// it, with a recording when `has_audio`. Returns the audio path.
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
                ?1, '2026-08-18T12:00:00Z', '2026-08-18T12:00:30Z', ?2, ?2, ?3,
                30, 'gpt-transcribe', 'raw words', 'Final words.', 'submitted'
            )
            "#,
            params![id.to_string(), stage, audio_path.to_string_lossy()],
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
            INSERT INTO dictation_sessions (
                started_at, ended_at, transcription_model, runtime_job_id
            ) VALUES ('2026-08-18T12:00:00Z', '2026-08-18T12:00:30Z', 'gpt-transcribe', ?1)
            "#,
            [recorded.to_string()],
        )
        .unwrap();
    connection
        .execute(
            r#"
            INSERT INTO transcript_history (session_id, created_at, final_text)
            VALUES (?1, '2026-08-18T12:00:30Z', 'Final words.')
            "#,
            [connection.last_insert_rowid()],
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
            failed_removals: 0,
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
    let mut history_jobs = runtime
        .list_history(HistoryQuery::default())
        .unwrap()
        .into_iter()
        .map(|entry| entry.job_id.unwrap().to_string())
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
