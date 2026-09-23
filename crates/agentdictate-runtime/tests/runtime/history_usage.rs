use std::path::Path;

use agentdictate_core::{
    DictationOptions, HistoryPageRequest, KeepTranscripts, Settings, parse_vocabulary,
};
use agentdictate_runtime::{
    DatabaseObserver, Deliverer, DeliveryDisposition, DeliveryMethod, ExternalError,
    HeadlessDeliveryGate, RecordingJob, Runtime, RuntimeError,
};
use tempfile::TempDir;

use crate::support::{
    ReadyRecorder, days_ago, history_rows, request, stored_dictations, transcribe_and_deliver,
};

const TRANSCRIPTION_MODEL: &str = "gpt-transcribe";

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
            consumed: true,
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
    transcribe_and_deliver(
        runtime,
        job.id,
        "fix the versel deploy",
        &mut HeadlessDeliveryGate,
        &mut SubmittedDeliverer,
    )
    .unwrap()
}

#[test]
fn a_delivered_dictation_is_recorded_once_and_feeds_usage() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let delivered = delivered_job(&mut runtime, &directory);
    let settings = Settings::default();

    runtime.complete_delivered(delivered.id, &settings).unwrap();
    runtime.complete_delivered(delivered.id, &settings).unwrap();

    let dictations = stored_dictations(&database_path);
    assert_eq!(dictations.len(), 1);
    let first = &dictations[0];
    assert_eq!(first.job_id, Some(delivered.id.to_string()));
    assert_eq!(first.raw_text.as_deref(), Some("fix the versel deploy"));
    assert_eq!(first.final_text.as_deref(), Some("fix the Vercel deploy"));
    let corrections = first.vocabulary_corrections.as_ref().unwrap();
    assert_eq!(corrections[0]["source_phrase"], "versel");
    assert_eq!(corrections[0]["count"], 1);
    assert_eq!(first.word_count, 4);
    assert_eq!(first.character_count, 21);
    assert!((first.estimated_cost - 0.0045).abs() < f64::EPSILON);
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
    let dictations = stored_dictations(&database_path);
    assert_eq!(dictations.len(), 1);
    assert_eq!(dictations[0].job_id, Some(delivered.id.to_string()));
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

/// Inserts a dictation that ended `days` ago, with `text` as its final and
/// raw text.
fn insert_aged_dictation(connection: &rusqlite::Connection, days: i64, text: &str) {
    connection
        .execute(
            r#"
            INSERT INTO dictations (
                started_at, ended_at, duration_seconds, transcription_provider,
                transcription_model, word_count, character_count, estimated_cost,
                final_text, raw_text
            ) VALUES (?1, ?1, 1, 'openai_api', 'gpt-transcribe', 3, 12, 0, ?2, ?2)
            "#,
            rusqlite::params![days_ago(days), text],
        )
        .unwrap();
}

#[test]
fn completing_a_dictation_deletes_text_keep_transcripts_no_longer_allows() {
    for (keep, kept) in [
        (
            KeepTranscripts::Forever,
            &["fix the Vercel deploy", "ten days old", "forty days old"][..],
        ),
        (
            KeepTranscripts::Days30,
            &["fix the Vercel deploy", "ten days old"][..],
        ),
        (KeepTranscripts::Never, &[][..]),
    ] {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("agentdictate.db");
        let mut runtime = Runtime::open(&database_path).unwrap();
        let connection = rusqlite::Connection::open(&database_path).unwrap();
        insert_aged_dictation(&connection, 40, "forty days old");
        insert_aged_dictation(&connection, 10, "ten days old");
        let delivered = delivered_job(&mut runtime, &directory);
        let settings = Settings {
            keep_transcripts: keep,
            ..Settings::default()
        };

        runtime.complete_delivered(delivered.id, &settings).unwrap();

        let texts = history_rows(&runtime)
            .into_iter()
            .map(|row| row.text)
            .collect::<Vec<_>>();
        assert_eq!(texts, kept, "{keep:?}");
        assert_eq!(runtime.usage().unwrap().all_time.dictations, 3, "{keep:?}");
        assert_eq!(
            tables_containing(&database_path, "forty").is_empty(),
            keep != KeepTranscripts::Forever,
            "{keep:?}"
        );
    }
}

#[test]
fn startup_deletes_text_older_than_keep_transcripts_allows() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    insert_aged_dictation(&connection, 31, "a month old");
    insert_aged_dictation(&connection, 29, "almost a month old");
    let settings = Settings {
        keep_transcripts: KeepTranscripts::Days30,
        ..Settings::default()
    };

    let cleanup = runtime
        .clean_up_finished_jobs(&settings, directory.path())
        .unwrap();

    assert_eq!(cleanup.purged_transcripts, 1);
    let texts = history_rows(&runtime)
        .into_iter()
        .map(|row| row.text)
        .collect::<Vec<_>>();
    assert_eq!(texts, ["almost a month old"]);
    assert_eq!(runtime.usage().unwrap().all_time.dictations, 2);
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

/// Saves one dictation with its text, as delivery would.
fn insert_history(connection: &rusqlite::Connection, ended_at: &str, final_text: &str) -> i64 {
    connection
        .execute(
            r#"
            INSERT INTO dictations (
                started_at, ended_at, duration_seconds, transcription_provider,
                transcription_model, word_count, character_count, estimated_cost,
                final_text, raw_text
            ) VALUES (?1, ?1, 1, 'openai_api', 'test-model', 2, ?2, 0, ?3, 'raw-only words')
            "#,
            rusqlite::params![ended_at, final_text.chars().count(), final_text],
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
            let ended_at = format!("2026-08-18T12:{:02}:00Z", index / 2);
            let id = insert_history(&connection, &ended_at, &format!("entry {index}"));
            (ended_at, id)
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

/// Whether any database file on disk holds `needle`, in live rows, free
/// space, or the write-ahead log.
fn database_files_contain(database: &Path, needle: &str) -> bool {
    ["", "-wal"].into_iter().any(|suffix| {
        let mut path = database.as_os_str().to_owned();
        path.push(suffix);
        std::fs::read(path).is_ok_and(|bytes| {
            bytes
                .windows(needle.len())
                .any(|window| window == needle.as_bytes())
        })
    })
}

#[test]
fn deleted_and_cleared_history_text_leaves_the_database_files() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let delivered = delivered_job(&mut runtime, &directory);
    runtime
        .complete_delivered(delivered.id, &Settings::default())
        .unwrap();
    assert!(database_files_contain(&database_path, "Vercel deploy"));

    let entry = history_rows(&runtime).remove(0);
    runtime.delete_history(entry.id).unwrap();

    assert!(!database_files_contain(&database_path, "Vercel deploy"));
    assert!(!database_files_contain(&database_path, "versel deploy"));

    let connection = rusqlite::Connection::open(&database_path).unwrap();
    insert_history(&connection, "2026-08-18T12:00:00Z", "a private note");
    drop(connection);
    runtime.clear_history().unwrap();

    assert!(!database_files_contain(&database_path, "private note"));
    assert!(tables_containing(&database_path, "private note").is_empty());
}

/// The settings window reads what the daemon committed straight from the
/// database, and a file event that committed nothing does not count as a
/// change.
#[test]
fn the_window_sees_each_commit_and_skips_events_that_changed_nothing() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let mut observer = DatabaseObserver::open(&database_path).unwrap();
    assert!(observer.changed().unwrap());
    observer.workspace(&HistoryPageRequest::default()).unwrap();
    assert!(!observer.changed().unwrap());

    let delivered = delivered_job(&mut runtime, &directory);
    runtime
        .complete_delivered(delivered.id, &Settings::default())
        .unwrap();

    assert!(observer.changed().unwrap());
    let workspace = observer
        .workspace(&HistoryPageRequest {
            search: "no such words".into(),
            ..HistoryPageRequest::default()
        })
        .unwrap();
    assert_eq!(workspace.recent.rows[0].text, "fix the Vercel deploy");
    assert!(workspace.history.rows.is_empty());
    assert_eq!(workspace.usage.all_time.dictations, 1);
    assert!(workspace.recoveries.is_empty());
    // SQLite's automatic checkpoint writes the database file this way.
    rusqlite::Connection::open(&database_path)
        .unwrap()
        .execute_batch("PRAGMA wal_checkpoint(PASSIVE);")
        .unwrap();
    assert!(!observer.changed().unwrap());
}

#[test]
fn the_window_refuses_a_database_a_newer_release_migrated() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    drop(Runtime::open(&database_path).unwrap());
    rusqlite::Connection::open(&database_path)
        .unwrap()
        .pragma_update(None, "user_version", 99)
        .unwrap();

    let mut observer = DatabaseObserver::open(&database_path).unwrap();

    assert!(matches!(
        observer.workspace(&HistoryPageRequest::default()),
        Err(RuntimeError::NewerDatabase { version: 99, .. })
    ));
}
