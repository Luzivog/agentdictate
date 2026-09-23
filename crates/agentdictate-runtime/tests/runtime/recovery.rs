use std::fs;
use std::path::Path;

use agentdictate_runtime::{JobStage, Runtime, Settings};
use chrono::Utc;
use rusqlite::Connection;
use tempfile::TempDir;

use crate::support::{ReadyRecorder, days_ago, insert_job, request};

#[test]
fn recovery_projection_lists_recoverable_stages_with_audio_evidence() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("history.db");
    let runtime = Runtime::open(&database).unwrap();

    let kept_audio = directory.path().join("kept.wav");
    fs::write(&kept_audio, b"RIFF").unwrap();

    let connection = Connection::open(&database).unwrap();
    let now = days_ago(0);
    insert_job(&connection, &kept_audio, "captured", "captured", &now);
    for (audio, state, stage) in [
        ("ready.wav", "captured", "ready_to_deliver"),
        ("interrupted.wav", "failed", "interrupted"),
        ("failed.wav", "failed", "failed"),
        ("delivered.wav", "delivered", "delivered"),
    ] {
        insert_job(&connection, Path::new(audio), state, stage, &now);
    }
    drop(connection);

    let entries = runtime.recoveries().unwrap();
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
    assert!(!captured.delivery_ambiguous);
    assert!(
        captured.audio_present,
        "existing files are reported present"
    );

    let missing_audio = entries
        .iter()
        .find(|entry| entry.stage == JobStage::ReadyToDeliver)
        .unwrap();
    assert!(!missing_audio.audio_present);
}

#[test]
fn active_recording_is_not_presented_as_a_recovery() {
    let directory = TempDir::new().unwrap();
    let audio_path = directory.path().join("recordings/active.wav");
    let mut runtime = Runtime::open(directory.path().join("agentdictate.db")).unwrap();
    let mut recorder = ReadyRecorder;

    let job = runtime
        .start_recording(request(&audio_path, "gpt-transcribe"), &mut recorder)
        .unwrap();

    assert_eq!(job.stage, JobStage::Recording);
    assert!(runtime.recoveries().unwrap().is_empty());
    assert_eq!(runtime.recoverable_jobs().unwrap(), vec![job]);
}

#[test]
fn recovery_items_expire_seven_days_after_their_last_change() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database).unwrap();
    let connection = Connection::open(&database).unwrap();
    let stale_audio = directory.path().join("stale.wav");
    let recent_audio = directory.path().join("recent.wav");
    fs::write(&stale_audio, b"RIFF").unwrap();
    fs::write(&recent_audio, b"RIFF").unwrap();
    let stale = insert_job(&connection, &stale_audio, "failed", "failed", &days_ago(8));
    let recent = insert_job(
        &connection,
        &recent_audio,
        "interrupted",
        "interrupted",
        &days_ago(6),
    );

    for entry in runtime.recoveries().unwrap() {
        assert_eq!(
            entry.expires_at - entry.updated_at,
            chrono::TimeDelta::days(7)
        );
    }
    let cleanup = runtime
        .clean_up_finished_jobs(&Settings::default(), directory.path())
        .unwrap();

    assert_eq!(cleanup.expired_recoveries, 1);
    let remaining = runtime
        .recoveries()
        .unwrap()
        .into_iter()
        .map(|entry| entry.job_id)
        .collect::<Vec<_>>();
    assert_eq!(remaining, [recent]);
    assert!(runtime.job(stale).unwrap().is_none());
    assert!(!stale_audio.exists());
    assert!(recent_audio.exists());
    assert!(Utc::now() < runtime.recoveries().unwrap()[0].expires_at);
}
