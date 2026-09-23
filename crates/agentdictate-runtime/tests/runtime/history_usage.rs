use agentdictate_core::{HistoryPageRequest, ReplacementRule, Settings, TranscriptionProvider};
use agentdictate_runtime::{
    Deliverer, DeliveryDisposition, DeliveryMethod, ExternalError, HeadlessDeliveryGate, JobStage,
    RecordingJob, Runtime, Transcriber, Transcript,
};
use tempfile::TempDir;

use crate::support::{ReadyRecorder, history_rows, request, request_with_provider, stored_history};

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
    delivered_job_with_provider(runtime, directory, TranscriptionProvider::OpenAiApi)
}

fn delivered_job_with_provider(
    runtime: &mut Runtime,
    directory: &TempDir,
    transcription_provider: TranscriptionProvider,
) -> RecordingJob {
    runtime
        .create_replacement(ReplacementRule {
            id: None,
            source_phrase: "versel".to_owned(),
            replacement_phrase: "Vercel".to_owned(),
            enabled: true,
            case_sensitive: false,
            whole_word_only: true,
        })
        .unwrap();
    let mut recorder = ReadyRecorder;
    let job = runtime
        .start_recording(
            request_with_provider(
                &directory.path().join("recordings/history.wav"),
                transcription_provider,
                TRANSCRIPTION_MODEL,
            ),
            &mut recorder,
        )
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
fn subscription_history_keeps_its_route_and_has_zero_marginal_transcription_cost() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let delivered = delivered_job_with_provider(
        &mut runtime,
        &directory,
        TranscriptionProvider::ChatGptSubscription,
    );

    assert_eq!(
        delivered.transcription_provider,
        TranscriptionProvider::ChatGptSubscription
    );
    runtime
        .complete_delivered(delivered.id, &Settings::default())
        .unwrap();
    let recorded = &stored_history(&database_path)[0];
    assert_eq!(recorded.transcription_provider, "chatgpt_subscription");
    assert_eq!(recorded.estimated_total_cost, 0.0);
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

#[test]
fn history_page_is_bounded_searchable_and_reports_more() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    runtime.ensure_history_search_index().unwrap();
    let mut connection = rusqlite::Connection::open(&database_path).unwrap();
    let transaction = connection.transaction().unwrap();
    for index in 0..25 {
        let timestamp = format!("2026-08-18T12:{index:02}:00Z");
        transaction
            .execute(
                r#"
                INSERT INTO dictation_sessions (
                    started_at, ended_at, duration_seconds, transcription_model,
                    raw_word_count, final_word_count, final_character_count
                ) VALUES (?1, ?1, 1, 'test-model', 2, 2, 20)
                "#,
                [&timestamp],
            )
            .unwrap();
        let session_id = transaction.last_insert_rowid();
        let final_text = if index % 10 == 2 {
            format!("needle result {index}")
        } else {
            format!("ordinary result {index}")
        };
        transaction
            .execute(
                r#"
                INSERT INTO transcript_history (
                    session_id, created_at, raw_transcript, final_text
                ) VALUES (?1, ?2, ?3, ?3)
                "#,
                rusqlite::params![session_id, timestamp, final_text],
            )
            .unwrap();
    }
    transaction.commit().unwrap();

    let zero_page_size = runtime
        .history_page(&HistoryPageRequest {
            page_size: 0,
            ..HistoryPageRequest::default()
        })
        .unwrap();
    assert_eq!(zero_page_size.rows.len(), 1);
    let oversized_page_size = runtime
        .history_page(&HistoryPageRequest {
            page_size: usize::MAX,
            ..HistoryPageRequest::default()
        })
        .unwrap();
    assert_eq!(oversized_page_size.rows.len(), 25);

    let first_page = runtime
        .history_page(&HistoryPageRequest {
            page_size: 10,
            ..HistoryPageRequest::default()
        })
        .unwrap();
    assert_eq!(first_page.rows.len(), 10);
    assert_eq!(first_page.total_matches, 25);
    assert!(first_page.next_cursor.is_some());
    let second_page = runtime
        .history_page(&HistoryPageRequest {
            page_size: 10,
            after: first_page.next_cursor.clone(),
            ..HistoryPageRequest::default()
        })
        .unwrap();
    assert_eq!(second_page.rows.len(), 10);
    assert_eq!(second_page.total_matches, 25);
    assert_eq!(second_page.rows[0].preview_text, "ordinary result 14");
    assert_eq!(second_page.rows[9].preview_text, "ordinary result 5");
    assert!(second_page.next_cursor.is_some());

    let foreign_cursor = runtime
        .history_page(&HistoryPageRequest {
            search: "ordinary".to_owned(),
            page_size: 10,
            after: first_page.next_cursor,
        })
        .unwrap();
    assert!(foreign_cursor.cursor_restarted);
    assert_eq!(foreign_cursor.rows[0].preview_text, "ordinary result 24");

    let matches = runtime
        .history_page(&HistoryPageRequest {
            search: "nedle".into(),
            page_size: 10,
            ..HistoryPageRequest::default()
        })
        .unwrap();
    assert_eq!(matches.total_matches, 3);
    assert!(matches.next_cursor.is_none());
    assert_eq!(matches.rows[0].preview_text, "needle result 22");
    assert_eq!(matches.rows[1].preview_text, "needle result 12");
    assert_eq!(matches.rows[2].preview_text, "needle result 2");
    assert!(matches.rows[0].preview_text.contains("needle"));

    let oversized_query = runtime
        .history_page(&HistoryPageRequest {
            search: "needle ".repeat(1_000),
            page_size: 10,
            ..HistoryPageRequest::default()
        })
        .unwrap();
    assert_eq!(oversized_query.total_matches, 3);
}

#[test]
fn history_search_handles_typos_symbols_and_match_aware_previews() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    runtime.ensure_history_search_index().unwrap();
    let mut connection = rusqlite::Connection::open(&database_path).unwrap();
    let transaction = connection.transaction().unwrap();
    let fixtures = [
        (
            "2026-08-18T12:03:00Z",
            "raw-only-secret",
            format!(
                "{}Transcript canonical phrase for the C++, AI, 100%, and foo_bar search demo.",
                "unrelated opening words ".repeat(12)
            ),
        ),
        (
            "2026-08-18T12:02:00Z",
            "ordinary raw",
            "Transcript without the second required token.".to_owned(),
        ),
        (
            "2026-08-18T12:01:00Z",
            "product names",
            "TokScope integrates with AgentDictate.".to_owned(),
        ),
        (
            "2026-08-18T12:00:00Z",
            "unicode expansion",
            format!("{} needle at the end", "İ".repeat(200)),
        ),
        (
            "2026-08-18T11:59:00Z",
            "diacritic folding",
            "A résumé beside the café.".to_owned(),
        ),
    ];
    for (created_at, raw, final_text) in fixtures {
        transaction
            .execute(
                r#"
                INSERT INTO dictation_sessions (
                    started_at, ended_at, duration_seconds, transcription_model,
                    raw_word_count, final_word_count, final_character_count
                ) VALUES (?1, ?1, 1, 'test-model', 2, 9, ?2)
                "#,
                rusqlite::params![created_at, final_text.chars().count()],
            )
            .unwrap();
        let session_id = transaction.last_insert_rowid();
        transaction
            .execute(
                r#"
                INSERT INTO transcript_history (
                    session_id, created_at, raw_transcript, final_text
                ) VALUES (?1, ?2, ?3, ?4)
                "#,
                rusqlite::params![session_id, created_at, raw, final_text],
            )
            .unwrap();
    }
    transaction.commit().unwrap();

    let fuzzy = runtime
        .history_page(&HistoryPageRequest {
            search: "transcirpt canoncal".to_owned(),
            page_size: 10,
            ..HistoryPageRequest::default()
        })
        .unwrap();
    assert_eq!(fuzzy.total_matches, 1);
    assert_eq!(fuzzy.rows.len(), 1);
    assert!(fuzzy.rows[0].preview_text.contains("Transcript canonical"));
    assert!(!fuzzy.rows[0].preview_text.starts_with("unrelated opening"));

    for literal in ["C++", "AI", "%", "_"] {
        let page = runtime
            .history_page(&HistoryPageRequest {
                search: literal.to_owned(),
                page_size: 10,
                ..HistoryPageRequest::default()
            })
            .unwrap();
        assert_eq!(page.total_matches, 1, "literal query {literal}");
    }

    for infix in ["scope", "dictate"] {
        let page = runtime
            .history_page(&HistoryPageRequest {
                search: infix.to_owned(),
                page_size: 10,
                ..HistoryPageRequest::default()
            })
            .unwrap();
        assert_eq!(page.total_matches, 1, "infix query {infix}");
        assert_eq!(
            page.rows[0].preview_text,
            "TokScope integrates with AgentDictate."
        );
    }

    for (query, expected_fragment) in [("resume", "résumé"), ("cafe", "café")] {
        let page = runtime
            .history_page(&HistoryPageRequest {
                search: query.to_owned(),
                page_size: 10,
                ..HistoryPageRequest::default()
            })
            .unwrap();
        assert_eq!(page.total_matches, 1, "diacritic query {query}");
        assert!(page.rows[0].preview_text.contains(expected_fragment));
    }

    let unicode_preview = runtime
        .history_page(&HistoryPageRequest {
            search: "needle".to_owned(),
            page_size: 10,
            ..HistoryPageRequest::default()
        })
        .unwrap();
    assert_eq!(unicode_preview.total_matches, 1);
    assert!(unicode_preview.rows[0].preview_text.contains("needle"));
    assert!(unicode_preview.rows[0].preview_text.chars().count() <= 162);

    let raw_only = runtime
        .history_page(&HistoryPageRequest {
            search: "raw-only-secret".to_owned(),
            page_size: 10,
            ..HistoryPageRequest::default()
        })
        .unwrap();
    assert!(raw_only.rows.is_empty());
}

#[test]
fn history_search_corrects_a_first_character_typo_and_a_rare_misspelling() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    runtime.ensure_history_search_index().unwrap();
    let mut connection = rusqlite::Connection::open(&database_path).unwrap();
    let transaction = connection.transaction().unwrap();
    for (index, final_text) in [
        "transcript canonical one",
        "transcript canonical two",
        "transcript canonical three",
        "transcript canonical four",
        "transcirpt literal artifact",
    ]
    .into_iter()
    .enumerate()
    {
        let created_at = format!("2026-08-18T12:0{index}:00Z");
        transaction
            .execute(
                r#"
                INSERT INTO dictation_sessions (
                    started_at, ended_at, duration_seconds, transcription_model,
                    raw_word_count, final_word_count, final_character_count
                ) VALUES (?1, ?1, 1, 'test-model', 3, 3, ?2)
                "#,
                rusqlite::params![created_at, final_text.chars().count()],
            )
            .unwrap();
        transaction
            .execute(
                r#"
                INSERT INTO transcript_history (
                    session_id, created_at, raw_transcript, final_text
                ) VALUES (?1, ?2, ?3, ?3)
                "#,
                rusqlite::params![transaction.last_insert_rowid(), created_at, final_text],
            )
            .unwrap();
    }
    transaction.commit().unwrap();

    let first_character = runtime
        .history_page(&HistoryPageRequest {
            search: "xranscript".to_owned(),
            page_size: 10,
            ..HistoryPageRequest::default()
        })
        .unwrap();
    assert_eq!(first_character.total_matches, 5);
    assert!(
        first_character
            .rows
            .iter()
            .any(|matched| matched.preview_text == "transcript canonical four")
    );

    let rare_misspelling = runtime
        .history_page(&HistoryPageRequest {
            search: "transcirpt".to_owned(),
            page_size: 10,
            ..HistoryPageRequest::default()
        })
        .unwrap();
    assert_eq!(rare_misspelling.total_matches, 5);
    assert!(
        rare_misspelling
            .rows
            .iter()
            .any(|matched| matched.preview_text == "transcirpt literal artifact")
    );
    assert!(
        rare_misspelling
            .rows
            .iter()
            .any(|matched| matched.preview_text == "transcript canonical four")
    );
}

#[test]
fn recording_history_invalidates_the_fuzzy_vocabulary_cache() {
    let directory = TempDir::new().unwrap();
    let mut runtime = Runtime::open(directory.path().join("agentdictate.db")).unwrap();
    runtime.ensure_history_search_index().unwrap();
    assert!(
        runtime
            .history_page(&HistoryPageRequest {
                search: "vrceel".to_owned(),
                page_size: 10,
                ..HistoryPageRequest::default()
            })
            .unwrap()
            .rows
            .is_empty()
    );

    let delivered = delivered_job(&mut runtime, &directory);
    runtime
        .complete_delivered(delivered.id, &Settings::default())
        .unwrap();
    let entry = history_rows(&runtime).remove(0);
    let found = runtime
        .history_page(&HistoryPageRequest {
            search: "vrceel".to_owned(),
            page_size: 10,
            ..HistoryPageRequest::default()
        })
        .unwrap();
    assert_eq!(found.rows.len(), 1);
    assert_eq!(found.rows[0].id, entry.id);

    runtime.ensure_history_search_index().unwrap();
    assert!(runtime.delete_history(entry.id).unwrap());
    assert!(
        runtime
            .history_page(&HistoryPageRequest {
                search: "Vercel".to_owned(),
                page_size: 10,
                ..HistoryPageRequest::default()
            })
            .unwrap()
            .rows
            .is_empty()
    );
}

#[test]
fn external_history_writes_refresh_the_fuzzy_vocabulary_cache() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    runtime.ensure_history_search_index().unwrap();
    assert!(
        runtime
            .history_page(&HistoryPageRequest {
                search: "vrceel".to_owned(),
                page_size: 10,
                ..HistoryPageRequest::default()
            })
            .unwrap()
            .rows
            .is_empty()
    );

    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute(
            r#"
            INSERT INTO dictation_sessions (
                started_at, ended_at, duration_seconds, transcription_model,
                raw_word_count, final_word_count, final_character_count
            ) VALUES ('2026-08-18T12:00:00Z', '2026-08-18T12:00:01Z', 1,
                'test-model', 1, 1, 6)
            "#,
            [],
        )
        .unwrap();
    let session_id = connection.last_insert_rowid();
    connection
        .execute(
            r#"
            INSERT INTO transcript_history (
                session_id, created_at, raw_transcript, final_text
            ) VALUES (?1, '2026-08-18T12:00:01Z', 'Vercel', 'Vercel')
            "#,
            [session_id],
        )
        .unwrap();

    let found = runtime
        .history_page(&HistoryPageRequest {
            search: "vrceel".to_owned(),
            page_size: 10,
            ..HistoryPageRequest::default()
        })
        .unwrap();
    assert_eq!(found.rows.len(), 1);
    assert_eq!(found.rows[0].preview_text, "Vercel");

    connection
        .execute(
            "UPDATE transcript_history SET final_text = 'Cloudflare' WHERE session_id = ?1",
            [session_id],
        )
        .unwrap();
    assert!(
        runtime
            .history_page(&HistoryPageRequest {
                search: "vrceel".to_owned(),
                page_size: 10,
                ..HistoryPageRequest::default()
            })
            .unwrap()
            .rows
            .is_empty()
    );
    let updated = runtime
        .history_page(&HistoryPageRequest {
            search: "clodflare".to_owned(),
            page_size: 10,
            ..HistoryPageRequest::default()
        })
        .unwrap();
    assert_eq!(updated.rows.len(), 1);
    assert_eq!(updated.rows[0].preview_text, "Cloudflare");

    connection
        .execute("DELETE FROM dictation_sessions WHERE id = ?1", [session_id])
        .unwrap();
    assert!(
        runtime
            .history_page(&HistoryPageRequest {
                search: "clodflare".to_owned(),
                page_size: 10,
                ..HistoryPageRequest::default()
            })
            .unwrap()
            .rows
            .is_empty()
    );
}

#[test]
fn fuzzy_cursor_expires_when_vocabulary_changes_its_candidate_plan() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    runtime.ensure_history_search_index().unwrap();
    let mut connection = rusqlite::Connection::open(&database_path).unwrap();
    for (index, final_text) in [
        "needle first",
        "needle second",
        "needle third",
        "ordinary transcript",
    ]
    .into_iter()
    .enumerate()
    {
        insert_external_history(&mut connection, index, final_text);
    }

    let first_page = runtime
        .history_page(&HistoryPageRequest {
            search: "nedle".to_owned(),
            page_size: 1,
            ..HistoryPageRequest::default()
        })
        .unwrap();
    assert_eq!(first_page.total_matches, 3);
    let cursor = first_page.next_cursor.expect("first fuzzy page cursor");

    insert_external_history(&mut connection, 10, "nedle exact one");
    insert_external_history(&mut connection, 11, "nedle exact two");

    let restarted = runtime
        .history_page(&HistoryPageRequest {
            search: "nedle".to_owned(),
            page_size: 1,
            after: Some(cursor),
        })
        .unwrap();
    assert!(restarted.cursor_restarted);
    assert_eq!(restarted.rows[0].preview_text, "nedle exact two");
}

fn insert_external_history(
    connection: &mut rusqlite::Connection,
    timestamp_offset: usize,
    final_text: &str,
) {
    let timestamp = format!("2026-08-18T13:{timestamp_offset:02}:00Z");
    let transaction = connection.transaction().unwrap();
    transaction
        .execute(
            r#"
            INSERT INTO dictation_sessions (
                started_at, ended_at, duration_seconds, transcription_model,
                raw_word_count, final_word_count, final_character_count
            ) VALUES (?1, ?1, 1, 'test-model', 2, 2, ?2)
            "#,
            rusqlite::params![timestamp, final_text.chars().count()],
        )
        .unwrap();
    transaction
        .execute(
            r#"
            INSERT INTO transcript_history (
                session_id, created_at, raw_transcript, final_text
            ) VALUES (?1, ?2, ?3, ?3)
            "#,
            rusqlite::params![transaction.last_insert_rowid(), timestamp, final_text],
        )
        .unwrap();
    transaction.commit().unwrap();
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

#[test]
fn replacement_mutations_validate_sources_and_preserve_stored_order() {
    let directory = TempDir::new().unwrap();
    let mut runtime = Runtime::open(directory.path().join("agentdictate.db")).unwrap();

    assert!(
        runtime
            .create_replacement(ReplacementRule {
                id: None,
                source_phrase: "   ".to_owned(),
                replacement_phrase: "ignored".to_owned(),
                enabled: true,
                case_sensitive: false,
                whole_word_only: true,
            })
            .is_err()
    );
    let mut first = runtime
        .create_replacement(ReplacementRule {
            id: None,
            source_phrase: "  versel  ".to_owned(),
            replacement_phrase: "Vercel".to_owned(),
            enabled: true,
            case_sensitive: false,
            whole_word_only: true,
        })
        .unwrap();
    runtime
        .create_replacement(ReplacementRule {
            id: None,
            source_phrase: "postgress".to_owned(),
            replacement_phrase: "Postgres".to_owned(),
            enabled: true,
            case_sensitive: false,
            whole_word_only: true,
        })
        .unwrap();

    assert_eq!(first.source_phrase, "versel");
    first.replacement_phrase = "Vercel Inc.".to_owned();
    first.enabled = false;
    let updated = runtime.update_replacement(first.clone()).unwrap();
    assert_eq!(updated, first);
    assert_eq!(runtime.replacement_rules().unwrap()[0], first);
    assert!(runtime.delete_replacement(first.id.unwrap()).unwrap());
    assert!(!runtime.delete_replacement(first.id.unwrap()).unwrap());
    assert_eq!(
        runtime.replacement_rules().unwrap()[0].source_phrase,
        "postgress"
    );
}
