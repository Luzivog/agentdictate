use std::fs;

use agentdictate_runtime::{DeliveryStatus, JobStage, Runtime};
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
        vec!["Captured", "Failed", "Interrupted", "ReadyToDeliver"],
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
