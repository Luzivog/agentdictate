use agentdictate_core::{DictationOptions, HistoryPageRequest, Settings, parse_vocabulary};
use agentdictate_runtime::{
    Deliverer, DeliveryDisposition, DeliveryMethod, ExternalError, HeadlessDeliveryGate, JobStage,
    RecordingJob, Runtime, Transcriber, Transcript,
};
use tempfile::TempDir;

use crate::support::{ReadyRecorder, history_rows, request, stored_history};

const TRANSCRIPTION_MODEL: &str = "gpt-transcribe";

struct FixedTranscriber;

impl Transcriber for FixedTranscriber {
    fn transcribe(&mut self, _job: &RecordingJob) -> Result<Transcript, ExternalError> {
        Ok(Transcript {
            text: "fix the versel deploy".to_owned(),
            model: TRANSCRIPTION_MODEL.to_owned(),
        })
    }
}

struct SubmittedDeliverer;

impl Deliverer for SubmittedDeliverer {
    fn deliver(
        &mut self,
        _job: &RecordingJob,
        _: DeliveryMethod,
    ) -> Result<DeliveryDisposition, ExternalError> {
        Ok(DeliveryDisposition::Submitted {
            copied_to_clipboard: true,
            paste_triggered: true,
        })
    }
}

fn delivered_job(runtime: &mut Runtime, directory: &TempDir) -> RecordingJob {
    let mut request = request(
        &directory.path().join("recordings/history.wav"),
        TRANSCRIPTION_MODEL,
    );
    request.options = Some(DictationOptions::from_settings(&Settings {
        vocabulary: parse_vocabulary("Vercel = versel").unwrap(),
        ..Settings::default()
    }));
    let job = runtime
        .start_recording(request, &mut ReadyRecorder)
        .unwrap();
    runtime.capture_recording(job.id, 60.0).unwrap();
    runtime
        .process_captured(
            job.id,
            &mut FixedTranscriber,
            &mut HeadlessDeliveryGate,
            &mut SubmittedDeliverer,
        )
        .unwrap()
}

#[test]
fn delivered_session_history_is_idempotent_and_feeds_usage() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let delivered = delivered_job(&mut runtime, &directory);
    let settings = Settings::default();

    runtime.complete_delivered(delivered.id, &settings).unwrap();
    runtime.complete_delivered(delivered.id, &settings).unwrap();

    let history = stored_history(&database_path);
    assert_eq!(history.len(), 1);
    let first = &history[0];
    assert_eq!(first.job_id, Some(delivered.id.to_string()));
    assert_eq!(first.raw_transcript, "fix the versel deploy");
    assert_eq!(first.final_text, "fix the Vercel deploy");
    assert_eq!(first.replacements_applied[0]["source_phrase"], "versel");
    assert_eq!(first.replacements_applied[0]["count"], 1);
    assert_eq!(first.raw_word_count, 4);
    assert_eq!(first.final_word_count, 4);
    assert_eq!(first.final_character_count, 21);
    assert!((first.estimated_total_cost - 0.0045).abs() < f64::EPSILON);
    assert!(first.copied_to_clipboard);
    assert!(first.paste_triggered);
    let usage = runtime.usage().unwrap();
    assert_eq!(usage.all_time.dictations, 1);
    assert_eq!(usage.all_time.words, 4);
    assert_eq!(usage.all_time.audio_seconds, 60.0);
}

#[test]
fn delivery_interrupted_before_completion_is_recorded_exactly_once() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    // The paste committed, then the daemon died before completing the job.
    let delivered = delivered_job(&mut runtime, &directory);
    drop(runtime);

    for _ in 0..2 {
        let mut restarted = Runtime::open(&database_path).unwrap();
        restarted
            .clean_up_finished_jobs(&Settings::default(), &directory.path().join("recordings"))
            .unwrap();
    }

    let runtime = Runtime::open(&database_path).unwrap();
    let history = stored_history(&database_path);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].job_id, Some(delivered.id.to_string()));
    assert_eq!(runtime.usage().unwrap().all_time.dictations, 1);
    assert!(runtime.job(delivered.id).unwrap().is_none());
}

#[test]
fn deleted_history_stays_deleted_after_a_restart() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let delivered = delivered_job(&mut runtime, &directory);
    runtime
        .complete_delivered(delivered.id, &Settings::default())
        .unwrap();
    let entry = history_rows(&runtime).remove(0);
    assert!(runtime.delete_history(entry.id).unwrap());
    drop(runtime);

    let mut restarted = Runtime::open(&database_path).unwrap();
    restarted
        .clean_up_finished_jobs(&Settings::default(), &directory.path().join("recordings"))
        .unwrap();

    assert!(history_rows(&restarted).is_empty());
}

#[test]
fn history_off_keeps_usage_numbers_but_no_transcript_after_delivery() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let delivered = delivered_job(&mut runtime, &directory);
    let settings = Settings {
        save_history: false,
        ..Settings::default()
    };

    runtime.complete_delivered(delivered.id, &settings).unwrap();

    let usage = runtime.usage().unwrap();
    assert_eq!(usage.all_time.dictations, 1);
    assert_eq!(usage.all_time.words, 4);
    // Job rows and History are the only tables that hold transcript text.
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    for table in ["dictation_jobs", "transcript_history"] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0,
            "{table} still holds a row"
        );
    }
}

#[test]
fn history_query_and_delete_keep_daily_usage_consistent() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let delivered = delivered_job(&mut runtime, &directory);
    runtime
        .complete_delivered(delivered.id, &Settings::default())
        .unwrap();
    let search = |runtime: &Runtime, text: &str| {
        runtime
            .history_page(&HistoryPageRequest {
                search: text.to_owned(),
                ..HistoryPageRequest::default()
            })
            .unwrap()
            .rows
    };

    let found = search(&runtime, "Vercel");
    assert_eq!(found.len(), 1);
    assert!(search(&runtime, "missing").is_empty());

    assert!(runtime.delete_history(found[0].id).unwrap());
    assert!(!runtime.delete_history(found[0].id).unwrap());
    assert_eq!(runtime.usage().unwrap().all_time.dictations, 0);
    assert!(history_rows(&runtime).is_empty());
}

/// Saves one History row with its usage session, as delivery would.
fn insert_history(connection: &rusqlite::Connection, created_at: &str, final_text: &str) -> i64 {
    connection
        .execute(
            r#"
            INSERT INTO dictation_sessions (
                started_at, ended_at, duration_seconds, transcription_model,
                raw_word_count, final_word_count, final_character_count
            ) VALUES (?1, ?1, 1, 'test-model', 2, 2, ?2)
            "#,
            rusqlite::params![created_at, final_text.chars().count()],
        )
        .unwrap();
    connection
        .execute(
            r#"
            INSERT INTO transcript_history (
                session_id, created_at, raw_transcript, final_text
            ) VALUES (?1, ?2, 'raw-only words', ?3)
            "#,
            rusqlite::params![connection.last_insert_rowid(), created_at, final_text],
        )
        .unwrap();
    connection.last_insert_rowid()
}

fn search(runtime: &Runtime, text: &str) -> Vec<String> {
    runtime
        .history_page(&HistoryPageRequest {
            search: text.to_owned(),
            page_size: 100,
            after: None,
        })
        .unwrap()
        .rows
        .into_iter()
        .map(|row| row.preview_text)
        .collect()
}

/// Every table and column that still holds `needle` in any case.
fn tables_containing(database: &std::path::Path, needle: &str) -> Vec<String> {
    let connection = rusqlite::Connection::open(database).unwrap();
    let tables = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut found = Vec::new();
    for table in tables {
        let columns = connection
            .prepare(&format!("PRAGMA table_info(\"{table}\")"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for column in columns {
            let count: i64 = connection
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM \"{table}\" WHERE CAST(\"{column}\" AS TEXT) LIKE ?1"
                    ),
                    [format!("%{needle}%")],
                    |row| row.get(0),
                )
                .unwrap();
            if count > 0 {
                found.push(format!("{table}.{column}"));
            }
        }
    }
    found
}

#[test]
fn history_search_finds_final_text_containing_the_query_in_any_ascii_case() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let runtime = Runtime::open(&database_path).unwrap();
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    insert_history(&connection, "2026-08-18T12:00:00Z", "Deploy the Vercel app");
    insert_history(
        &connection,
        "2026-08-18T12:01:00Z",
        "TokScope integrates with Vercel",
    );
    insert_history(&connection, "2026-08-18T12:02:00Z", "Postgres migration");

    assert_eq!(
        search(&runtime, "  VERCEL "),
        ["TokScope integrates with Vercel", "Deploy the Vercel app"]
    );
    assert_eq!(
        search(&runtime, "scope"),
        ["TokScope integrates with Vercel"]
    );
    assert_eq!(search(&runtime, "").len(), 3);
    assert!(search(&runtime, "raw-only").is_empty());
    assert!(search(&runtime, "Vercel app Postgres").is_empty());
}

#[test]
fn history_search_matches_wildcards_and_backslashes_literally() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let runtime = Runtime::open(&database_path).unwrap();
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    for (minute, text) in [
        "100% done",
        "100 percent done",
        "rename foo_bar",
        "rename fooXbar",
        r"open C:\temp",
        "open C:temp",
    ]
    .into_iter()
    .enumerate()
    {
        insert_history(&connection, &format!("2026-08-18T12:0{minute}:00Z"), text);
    }

    assert_eq!(search(&runtime, "100%"), ["100% done"]);
    assert_eq!(search(&runtime, "%"), ["100% done"]);
    assert_eq!(search(&runtime, "foo_bar"), ["rename foo_bar"]);
    assert_eq!(search(&runtime, "_"), ["rename foo_bar"]);
    assert_eq!(search(&runtime, r"C:\"), [r"open C:\temp"]);
}

#[test]
fn history_pages_stay_stable_while_new_transcripts_arrive() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let runtime = Runtime::open(&database_path).unwrap();
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    // Pairs of rows share a timestamp, so the row id must break ties.
    let mut expected = (0..25)
        .map(|index| {
            let created_at = format!("2026-08-18T12:{:02}:00Z", index / 2);
            let id = insert_history(&connection, &created_at, &format!("entry {index}"));
            (created_at, id)
        })
        .collect::<Vec<_>>();
    expected.sort_by(|left, right| right.cmp(left));
    let page = |after| {
        runtime
            .history_page(&HistoryPageRequest {
                search: "entry".to_owned(),
                page_size: 10,
                after,
            })
            .unwrap()
    };

    let first = page(None);
    insert_history(&connection, "2026-08-18T13:00:00Z", "entry newest");
    let second = page(first.next_cursor.clone());
    let third = page(second.next_cursor.clone());

    let listed = [&first, &second, &third]
        .into_iter()
        .flat_map(|page| page.rows.iter().map(|row| row.id))
        .collect::<Vec<_>>();
    assert_eq!(
        listed,
        expected.iter().map(|(_, id)| *id).collect::<Vec<_>>()
    );
    assert_eq!(third.total_matches, 26);
    assert!(third.next_cursor.is_none());
    let restarted = page(Some(agentdictate_core::HistoryPageCursor::new(
        "not a cursor",
    )));
    assert!(restarted.cursor_restarted);
    assert_eq!(restarted.rows[0].preview_text, "entry newest");
    let smallest = runtime
        .history_page(&HistoryPageRequest {
            page_size: 0,
            ..HistoryPageRequest::default()
        })
        .unwrap();
    assert_eq!(smallest.rows.len(), 1);
}

#[test]
fn clearing_history_leaves_no_transcript_text_in_any_table() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let delivered = delivered_job(&mut runtime, &directory);
    runtime
        .complete_delivered(delivered.id, &Settings::default())
        .unwrap();
    assert!(!tables_containing(&database_path, "versel").is_empty());

    runtime.clear_history().unwrap();

    assert!(tables_containing(&database_path, "versel").is_empty());
    assert!(tables_containing(&database_path, "Vercel").is_empty());
}

#[test]
fn opening_a_database_with_the_retired_full_text_index_drops_it() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    drop(Runtime::open(&database_path).unwrap());
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute_batch(
            r#"
            CREATE TABLE history_search_state (id INTEGER PRIMARY KEY, ready INTEGER);
            CREATE VIRTUAL TABLE transcript_history_fts USING fts5(
                final_text, content='transcript_history', content_rowid='id'
            );
            CREATE VIRTUAL TABLE transcript_history_fts_vocab USING fts5vocab(
                transcript_history_fts, 'row'
            );
            CREATE VIRTUAL TABLE transcript_history_fts_trigram USING fts5(
                final_text, content='transcript_history', content_rowid='id',
                tokenize='trigram'
            );
            CREATE TRIGGER transcript_history_fts_insert
            AFTER INSERT ON transcript_history BEGIN
                INSERT INTO transcript_history_fts(rowid, final_text)
                VALUES (new.id, new.final_text);
            END;
            CREATE TRIGGER transcript_history_fts_trigram_delete
            AFTER DELETE ON transcript_history BEGIN
                INSERT INTO transcript_history_fts_trigram(
                    transcript_history_fts_trigram, rowid, final_text
                ) VALUES ('delete', old.id, old.final_text);
            END;
            "#,
        )
        .unwrap();
    insert_history(
        &connection,
        "2026-08-18T12:00:00Z",
        "kept through the upgrade",
    );
    drop(connection);

    let runtime = Runtime::open(&database_path).unwrap();

    let leftovers: i64 = rusqlite::Connection::open(&database_path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE name LIKE 'transcript_history_fts%' OR name = 'history_search_state'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(leftovers, 0);
    assert_eq!(search(&runtime, "upgrade"), ["kept through the upgrade"]);
}

#[test]
fn recovery_projection_reports_audio_presence_without_hiding_missing_files() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let audio_path = directory.path().join("recordings/recovery.wav");
    std::fs::create_dir_all(audio_path.parent().unwrap()).unwrap();
    std::fs::write(&audio_path, b"RIFFrecovery").unwrap();
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut recorder = ReadyRecorder;
    let job = runtime
        .start_recording(request(&audio_path, TRANSCRIPTION_MODEL), &mut recorder)
        .unwrap();
    runtime
        .interrupt_job(job.id, JobStage::Recording, "microphone disappeared")
        .unwrap();

    let present = runtime.recoveries().unwrap();
    assert_eq!(present.len(), 1);
    assert_eq!(present[0].job_id, job.id);
    assert_eq!(present[0].stage, JobStage::Interrupted);
    assert_eq!(
        present[0].error_message.as_deref(),
        Some("microphone disappeared")
    );
    assert!(present[0].audio_present);

    std::fs::remove_file(audio_path).unwrap();
    assert!(!runtime.recoveries().unwrap()[0].audio_present);
}

#[test]
fn active_recording_is_not_presented_as_a_recovery() {
    let directory = TempDir::new().unwrap();
    let audio_path = directory.path().join("recordings/active.wav");
    let mut runtime = Runtime::open(directory.path().join("agentdictate.db")).unwrap();
    let mut recorder = ReadyRecorder;

    let job = runtime
        .start_recording(request(&audio_path, TRANSCRIPTION_MODEL), &mut recorder)
        .unwrap();

    assert_eq!(job.stage, JobStage::Recording);
    assert!(runtime.recoveries().unwrap().is_empty());
    assert_eq!(runtime.recoverable_jobs().unwrap(), vec![job]);
}
