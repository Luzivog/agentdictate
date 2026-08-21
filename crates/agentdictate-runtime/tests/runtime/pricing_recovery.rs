use std::fs;

use agentdictate_runtime::{DeliveryStatus, JobStage, Runtime, Settings};
use chrono::Utc;
use rusqlite::{Connection, params};
use tempfile::TempDir;

fn insert_job(connection: &Connection, audio_path: &str, state: &str, stage: &str) {
    let now = Utc::now().to_rfc3339();
    let runtime_id = agentdictate_core::JobId::new().to_string();
    connection
        .execute(
            r#"
            INSERT INTO dictation_jobs (
                started_at, updated_at, state, stage, audio_path,
                duration_seconds, transcription_model, raw_transcript, final_text,
                delivery_status, runtime_id
            ) VALUES (?1, ?1, ?2, ?3, ?4, 12.5, 'gpt-transcribe', 'raw words', 'final words',
                      'not_attempted', ?5)
            "#,
            params![now, state, stage, audio_path, runtime_id],
        )
        .unwrap();
}

#[test]
fn pricing_sync_upserts_the_python_compatible_table_and_is_idempotent() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("history.db");
    let mut runtime = Runtime::open(&database).unwrap();

    runtime.sync_pricing(&Settings::default()).unwrap();
    let first = read_pricing(&database);
    runtime.sync_pricing(&Settings::default()).unwrap();
    let second = read_pricing(&database);

    assert_eq!(first, second, "repeated syncs converge without duplicates");
    let transcription = first
        .iter()
        .filter(|(_, model_type, _)| model_type == "transcription")
        .count();
    assert_eq!(
        transcription,
        Settings::default().transcription_prices.len(),
        "every default transcription model is priced"
    );
}

fn read_pricing(database: &std::path::Path) -> Vec<(String, String, f64)> {
    let connection = Connection::open(database).unwrap();
    let mut statement = connection
        .prepare("SELECT model_name, model_type, price_per_audio_minute FROM pricing_settings")
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })
        .unwrap();
    let mut entries: Vec<_> = rows.map(|row| row.unwrap()).collect();
    entries.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    entries
}

#[test]
fn recovery_projection_lists_recoverable_stages_with_audio_evidence() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("history.db");
    let runtime = Runtime::open(&database).unwrap();

    let kept_audio = directory.path().join("kept.wav");
    fs::write(&kept_audio, b"RIFF").unwrap();

    let connection = Connection::open(&database).unwrap();
    insert_job(
        &connection,
        kept_audio.to_str().unwrap(),
        "captured",
        "captured",
    );
    insert_job(&connection, "ready.wav", "captured", "ready_to_deliver");
    insert_job(&connection, "interrupted.wav", "failed", "interrupted");
    insert_job(&connection, "canceled.wav", "failed", "canceled");
    insert_job(&connection, "failed.wav", "failed", "failed");
    insert_job(&connection, "delivered.wav", "delivered", "delivered");
    drop(connection);

    let entries = runtime.recovery_entries().unwrap();
    let mut stage_names: Vec<String> = entries
        .iter()
        .map(|entry| format!("{:?}", entry.stage))
        .collect();
    stage_names.sort();
    stage_names.dedup();

    assert_eq!(
        stage_names,
        vec![
            "Canceled",
            "Captured",
            "Failed",
            "Interrupted",
            "ReadyToDeliver",
        ],
        "recoverable stages are listed and delivered work is excluded"
    );

    let captured = entries
        .iter()
        .find(|entry| entry.stage == JobStage::Captured)
        .unwrap();
    assert_eq!(captured.raw_transcript, "raw words");
    assert_eq!(captured.delivery_status, DeliveryStatus::NotAttempted);
    assert!(
        captured.audio_present,
        "existing files are reported present"
    );

    let missing_audio = entries
        .iter()
        .find(|entry| entry.audio_path.ends_with("ready.wav"))
        .unwrap();
    assert!(!missing_audio.audio_present);
}
