use std::path::PathBuf;

use agentdictate_core::{ReplacementRule, Settings, TranscriptionProvider};
use agentdictate_runtime::{
    Deliverer, DeliveryDisposition, ExternalError, HeadlessDeliveryGate, HistoryQuery, JobStage,
    RecordingJob, Runtime, Transcriber, Transcript, UsageMetric,
};
use chrono::{Datelike, Days, Utc};
use tempfile::TempDir;

use crate::support::{ReadyRecorder, request, request_with_provider};

const TRANSCRIPTION_MODEL: &str = "gpt-4o-transcribe";

struct CleaningTranscriber;

impl Transcriber for CleaningTranscriber {
    fn transcribe(&mut self, _job: &RecordingJob) -> Result<Transcript, ExternalError> {
        Ok(Transcript {
            raw: "fix the versel deploy".to_owned(),
            final_text: "Fix the versel deploy.".to_owned(),
            cleaned_text: Some("Fix the versel deploy.".to_owned()),
            cleanup_error: None,
        })
    }
}

struct SubmittedDeliverer;

impl Deliverer for SubmittedDeliverer {
    fn deliver(&mut self, _job: &RecordingJob) -> Result<DeliveryDisposition, ExternalError> {
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
            &mut CleaningTranscriber,
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
    let recorded = runtime
        .record_delivered_session(
            delivered.id,
            &Settings {
                cleanup_enabled: true,
                ..Settings::default()
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        recorded.transcription_provider,
        TranscriptionProvider::ChatGptSubscription
    );
    assert_eq!(recorded.estimated_transcription_cost, 0.0);
    assert!(recorded.estimated_cleanup_cost > 0.0);

    let mut repriced = Settings::default();
    repriced
        .transcription_prices
        .get_mut("gpt-4o-transcribe")
        .unwrap()
        .price_per_audio_minute = 99.0;
    runtime.sync_pricing(&repriced).unwrap();
    let repriced = runtime.history(recorded.id).unwrap().unwrap();
    assert_eq!(repriced.estimated_transcription_cost, 0.0);
    assert_eq!(
        rusqlite::Connection::open(database_path)
            .unwrap()
            .query_row(
                "SELECT transcription_provider FROM dictation_sessions WHERE id = ?1",
                [recorded.session_id],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "chatgpt_subscription"
    );
}

#[test]
fn delivered_session_history_is_idempotent_and_feeds_usage() {
    let directory = TempDir::new().unwrap();
    let mut runtime = Runtime::open(directory.path().join("agentdictate.db")).unwrap();
    let delivered = delivered_job(&mut runtime, &directory);
    let settings = Settings {
        cleanup_enabled: true,
        cleanup_model: "gpt-5.4-nano".to_owned(),
        cleanup_style: "Light cleanup".to_owned(),
        ..Settings::default()
    };

    let first = runtime
        .record_delivered_session(delivered.id, &settings)
        .unwrap()
        .unwrap();
    let duplicate = runtime
        .record_delivered_session(delivered.id, &settings)
        .unwrap()
        .unwrap();

    assert_eq!(first.id, duplicate.id);
    assert_eq!(first.job_id, Some(delivered.id));
    assert_eq!(first.raw_transcript, "fix the versel deploy");
    assert_eq!(
        first.cleaned_transcript.as_deref(),
        Some("Fix the versel deploy.")
    );
    assert_eq!(first.final_text, "Fix the Vercel deploy.");
    assert_eq!(first.replacements_applied.len(), 1);
    assert_eq!(first.replacements_applied[0].source_phrase, "versel");
    assert_eq!(first.replacements_applied[0].count, 1);
    assert_eq!(first.raw_word_count, 4);
    assert_eq!(first.final_word_count, 4);
    assert_eq!(first.final_character_count, 22);
    assert!((first.estimated_transcription_cost - 0.006).abs() < f64::EPSILON);
    assert!(first.estimated_cleanup_cost > 0.0);
    assert!(first.copied_to_clipboard);
    assert!(first.paste_triggered);
    assert!(first.success);

    let history = runtime.list_history(HistoryQuery::default()).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0], first);
    let usage = runtime.usage_summary().unwrap();
    assert_eq!(usage.all_time.total_sessions, 1);
    assert_eq!(usage.all_time.total_words, 4);
    assert_eq!(usage.all_time.total_audio_seconds, 60.0);
    assert_eq!(usage.all_time.average_wpm, 4.0);
    assert_eq!(
        usage.most_used_transcription_model.as_deref(),
        Some("gpt-4o-transcribe")
    );
    assert_eq!(
        usage.most_used_cleanup_model.as_deref(),
        Some("gpt-5.4-nano")
    );
    assert_eq!(usage.cleanup_mode_usage_count, 1);

    let series = runtime.usage_series(1, UsageMetric::Words).unwrap();
    assert_eq!(series.len(), 1);
}

#[test]
fn all_time_usage_is_aggregated_into_complete_monday_based_weeks() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let runtime = Runtime::open(&database_path).unwrap();
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    let today = Utc::now().date_naive();
    let current_monday = today
        .checked_sub_days(Days::new(today.weekday().num_days_from_monday().into()))
        .unwrap();
    let previous_sunday = current_monday.checked_sub_days(Days::new(1)).unwrap();
    let current_tuesday = current_monday.checked_add_days(Days::new(1)).unwrap();
    for (date, sessions, words, seconds, cost) in [
        (previous_sunday, 2, 20, 60.0, 0.2),
        (current_monday, 3, 30, 90.0, 0.3),
        (current_tuesday, 4, 40, 120.0, 0.4),
    ] {
        connection
            .execute(
                r#"
                INSERT INTO daily_stats (
                    date, total_sessions, total_words, total_audio_seconds,
                    estimated_total_cost
                ) VALUES (?1, ?2, ?3, ?4, ?5)
                "#,
                rusqlite::params![date.to_string(), sessions, words, seconds, cost],
            )
            .unwrap();
    }

    let weeks = runtime.usage_weekly_series().unwrap();

    assert_eq!(weeks.len(), 2);
    assert_eq!(weeks[0].week_start, current_monday - Days::new(7));
    assert_eq!(weeks[0].total_sessions, 2);
    assert_eq!(weeks[0].total_words, 20);
    assert_eq!(weeks[1].week_start, current_monday);
    assert_eq!(weeks[1].total_sessions, 7);
    assert_eq!(weeks[1].total_words, 70);
    assert_eq!(weeks[1].total_audio_seconds, 210.0);
    assert!((weeks[1].estimated_total_cost - 0.7).abs() < f64::EPSILON);
}

#[test]
fn startup_backfill_repairs_a_delivered_job_missing_from_history() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let delivered = delivered_job(&mut runtime, &directory);
    drop(runtime);
    let mut runtime = Runtime::open(&database_path).unwrap();

    let repaired = runtime
        .backfill_delivered_sessions(&Settings::default())
        .unwrap();

    assert_eq!(repaired, 1);
    let history = runtime.list_history(HistoryQuery::default()).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].job_id, Some(delivered.id));
}

#[test]
fn disabled_history_does_not_create_a_session() {
    let directory = TempDir::new().unwrap();
    let mut runtime = Runtime::open(directory.path().join("agentdictate.db")).unwrap();
    let delivered = delivered_job(&mut runtime, &directory);
    let settings = Settings {
        save_history: false,
        ..Settings::default()
    };

    assert!(
        runtime
            .record_delivered_session(delivered.id, &settings)
            .unwrap()
            .is_none()
    );
    assert!(
        runtime
            .list_history(HistoryQuery::default())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn history_query_and_delete_keep_daily_usage_consistent() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let delivered = delivered_job(&mut runtime, &directory);
    let entry = runtime
        .record_delivered_session(delivered.id, &Settings::default())
        .unwrap()
        .unwrap();

    assert_eq!(
        runtime
            .list_history(HistoryQuery {
                search: "Vercel".to_owned(),
                ..HistoryQuery::default()
            })
            .unwrap()
            .len(),
        1
    );
    assert!(
        runtime
            .list_history(HistoryQuery {
                search: "missing".to_owned(),
                ..HistoryQuery::default()
            })
            .unwrap()
            .is_empty()
    );

    assert!(runtime.delete_history(entry.id).unwrap());
    assert!(!runtime.delete_history(entry.id).unwrap());
    assert_eq!(runtime.usage_summary().unwrap().all_time.total_sessions, 0);
    assert!(
        runtime
            .list_history(HistoryQuery::default())
            .unwrap()
            .is_empty()
    );
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
        .history_page(HistoryQuery {
            limit: 0,
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(zero_page_size.matches.len(), 1);
    let oversized_page_size = runtime
        .history_page(HistoryQuery {
            limit: usize::MAX,
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(oversized_page_size.matches.len(), 25);

    let first_page = runtime
        .history_page(HistoryQuery {
            limit: 10,
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(first_page.matches.len(), 10);
    assert_eq!(first_page.total_matches, 25);
    assert!(first_page.next_cursor.is_some());
    let second_page = runtime
        .history_page(HistoryQuery {
            limit: 10,
            after: first_page.next_cursor.clone(),
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(second_page.matches.len(), 10);
    assert_eq!(second_page.total_matches, 25);
    assert_eq!(
        second_page.matches[0].entry.final_text,
        "ordinary result 14"
    );
    assert_eq!(second_page.matches[9].entry.final_text, "ordinary result 5");
    assert!(second_page.next_cursor.is_some());

    let cursor_error = runtime
        .history_page(HistoryQuery {
            search: "different query".to_owned(),
            limit: 10,
            after: first_page.next_cursor,
            ..HistoryQuery::default()
        })
        .unwrap_err();
    assert!(matches!(
        cursor_error,
        agentdictate_runtime::RuntimeError::InvalidHistoryCursor(_)
    ));

    let matches = runtime
        .history_page(HistoryQuery {
            search: "nedle".into(),
            limit: 10,
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(matches.total_matches, 3);
    assert!(matches.next_cursor.is_none());
    assert_eq!(matches.matches[0].entry.final_text, "needle result 22");
    assert_eq!(matches.matches[1].entry.final_text, "needle result 12");
    assert_eq!(matches.matches[2].entry.final_text, "needle result 2");
    assert!(matches.matches[0].preview.contains("needle"));

    let oversized_query = runtime
        .history_page(HistoryQuery {
            search: "needle ".repeat(1_000),
            limit: 10,
            ..HistoryQuery::default()
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
        .history_page(HistoryQuery {
            search: "transcirpt canoncal".to_owned(),
            limit: 10,
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(fuzzy.total_matches, 1);
    assert_eq!(fuzzy.matches.len(), 1);
    assert!(fuzzy.matches[0].preview.contains("Transcript canonical"));
    assert!(!fuzzy.matches[0].preview.starts_with("unrelated opening"));

    for literal in ["C++", "AI", "%", "_"] {
        let page = runtime
            .history_page(HistoryQuery {
                search: literal.to_owned(),
                limit: 10,
                ..HistoryQuery::default()
            })
            .unwrap();
        assert_eq!(page.total_matches, 1, "literal query {literal}");
    }

    for infix in ["scope", "dictate"] {
        let page = runtime
            .history_page(HistoryQuery {
                search: infix.to_owned(),
                limit: 10,
                ..HistoryQuery::default()
            })
            .unwrap();
        assert_eq!(page.total_matches, 1, "infix query {infix}");
        assert_eq!(
            page.matches[0].entry.final_text,
            "TokScope integrates with AgentDictate."
        );
    }

    for (query, expected_fragment) in [("resume", "résumé"), ("cafe", "café")] {
        let page = runtime
            .history_page(HistoryQuery {
                search: query.to_owned(),
                limit: 10,
                ..HistoryQuery::default()
            })
            .unwrap();
        assert_eq!(page.total_matches, 1, "diacritic query {query}");
        assert!(page.matches[0].entry.final_text.contains(expected_fragment));
    }

    let unicode_preview = runtime
        .history_page(HistoryQuery {
            search: "needle".to_owned(),
            limit: 10,
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(unicode_preview.total_matches, 1);
    assert!(unicode_preview.matches[0].preview.contains("needle"));
    assert!(unicode_preview.matches[0].preview.chars().count() <= 162);

    let raw_only = runtime
        .history_page(HistoryQuery {
            search: "raw-only-secret".to_owned(),
            limit: 10,
            ..HistoryQuery::default()
        })
        .unwrap();
    assert!(raw_only.matches.is_empty());
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
        .history_page(HistoryQuery {
            search: "xranscript".to_owned(),
            limit: 10,
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(first_character.total_matches, 5);
    assert!(
        first_character
            .matches
            .iter()
            .any(|matched| matched.entry.final_text == "transcript canonical four")
    );

    let rare_misspelling = runtime
        .history_page(HistoryQuery {
            search: "transcirpt".to_owned(),
            limit: 10,
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(rare_misspelling.total_matches, 5);
    assert!(
        rare_misspelling
            .matches
            .iter()
            .any(|matched| matched.entry.final_text == "transcirpt literal artifact")
    );
    assert!(
        rare_misspelling
            .matches
            .iter()
            .any(|matched| matched.entry.final_text == "transcript canonical four")
    );
}

#[test]
fn recording_history_invalidates_the_fuzzy_vocabulary_cache() {
    let directory = TempDir::new().unwrap();
    let mut runtime = Runtime::open(directory.path().join("agentdictate.db")).unwrap();
    runtime.ensure_history_search_index().unwrap();
    assert!(
        runtime
            .history_page(HistoryQuery {
                search: "vrceel".to_owned(),
                limit: 10,
                ..HistoryQuery::default()
            })
            .unwrap()
            .matches
            .is_empty()
    );

    let delivered = delivered_job(&mut runtime, &directory);
    let entry = runtime
        .record_delivered_session(delivered.id, &Settings::default())
        .unwrap()
        .unwrap();
    let found = runtime
        .history_page(HistoryQuery {
            search: "vrceel".to_owned(),
            limit: 10,
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(found.matches.len(), 1);
    assert_eq!(found.matches[0].entry.id, entry.id);

    runtime.ensure_history_search_index().unwrap();
    assert!(runtime.delete_history(entry.id).unwrap());
    assert!(
        runtime
            .history_page(HistoryQuery {
                search: "Vercel".to_owned(),
                limit: 10,
                ..HistoryQuery::default()
            })
            .unwrap()
            .matches
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
            .history_page(HistoryQuery {
                search: "vrceel".to_owned(),
                limit: 10,
                ..HistoryQuery::default()
            })
            .unwrap()
            .matches
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
        .history_page(HistoryQuery {
            search: "vrceel".to_owned(),
            limit: 10,
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(found.matches.len(), 1);
    assert_eq!(found.matches[0].entry.final_text, "Vercel");

    connection
        .execute(
            "UPDATE transcript_history SET final_text = 'Cloudflare' WHERE session_id = ?1",
            [session_id],
        )
        .unwrap();
    assert!(
        runtime
            .history_page(HistoryQuery {
                search: "vrceel".to_owned(),
                limit: 10,
                ..HistoryQuery::default()
            })
            .unwrap()
            .matches
            .is_empty()
    );
    let updated = runtime
        .history_page(HistoryQuery {
            search: "clodflare".to_owned(),
            limit: 10,
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(updated.matches.len(), 1);
    assert_eq!(updated.matches[0].entry.final_text, "Cloudflare");

    connection
        .execute("DELETE FROM dictation_sessions WHERE id = ?1", [session_id])
        .unwrap();
    assert!(
        runtime
            .history_page(HistoryQuery {
                search: "clodflare".to_owned(),
                limit: 10,
                ..HistoryQuery::default()
            })
            .unwrap()
            .matches
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
        .history_page(HistoryQuery {
            search: "nedle".to_owned(),
            limit: 1,
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(first_page.total_matches, 3);
    let cursor = first_page.next_cursor.expect("first fuzzy page cursor");

    insert_external_history(&mut connection, 10, "nedle exact one");
    insert_external_history(&mut connection, 11, "nedle exact two");

    let error = runtime
        .history_page(HistoryQuery {
            search: "nedle".to_owned(),
            limit: 1,
            after: Some(cursor),
            ..HistoryQuery::default()
        })
        .unwrap_err();
    assert!(matches!(
        error,
        agentdictate_runtime::RuntimeError::InvalidHistoryCursor(_)
    ));
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
fn deleting_cross_midnight_history_repairs_the_session_start_day() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("agentdictate.db");
    let mut runtime = Runtime::open(&database_path).unwrap();
    let delivered = delivered_job(&mut runtime, &directory);
    let entry = runtime
        .record_delivered_session(delivered.id, &Settings::default())
        .unwrap()
        .unwrap();
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute(
            "UPDATE dictation_sessions SET started_at = '2026-08-17T23:59:30Z' WHERE id = ?1",
            [entry.session_id],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE transcript_history SET created_at = '2026-08-18T00:00:30Z' WHERE id = ?1",
            [entry.id],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE daily_stats SET date = '2026-08-17' WHERE date = '2026-08-18'",
            [],
        )
        .unwrap();
    drop(connection);

    assert!(runtime.delete_history(entry.id).unwrap());

    let points = runtime.usage_series(2, UsageMetric::Sessions).unwrap();
    assert!(points.iter().all(|point| point.value == 0.0));
}

#[test]
fn pricing_sync_reprices_existing_history_and_usage() {
    let directory = TempDir::new().unwrap();
    let mut runtime = Runtime::open(directory.path().join("agentdictate.db")).unwrap();
    let delivered = delivered_job(&mut runtime, &directory);
    runtime
        .record_delivered_session(
            delivered.id,
            &Settings {
                cleanup_enabled: true,
                ..Settings::default()
            },
        )
        .unwrap();
    let mut repriced = Settings::default();
    repriced
        .transcription_prices
        .get_mut("gpt-4o-transcribe")
        .unwrap()
        .price_per_audio_minute = 0.012;
    repriced
        .cleanup_prices
        .get_mut("gpt-5.4-nano")
        .unwrap()
        .input_price_per_1m_tokens = 10.0;

    runtime.sync_pricing(&repriced).unwrap();

    let entry = runtime
        .list_history(HistoryQuery::default())
        .unwrap()
        .remove(0);
    assert!((entry.estimated_transcription_cost - 0.012).abs() < f64::EPSILON);
    assert!(entry.estimated_cleanup_cost > 0.000_01);
    let usage = runtime.usage_summary().unwrap();
    assert!(
        (usage.all_time.estimated_total_cost - entry.estimated_total_cost).abs() < f64::EPSILON
    );
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

    let present = runtime.recovery_entries().unwrap();
    assert_eq!(present.len(), 1);
    assert_eq!(present[0].job_id, job.id);
    assert_eq!(present[0].stage, JobStage::Interrupted);
    assert_eq!(
        present[0].error_message.as_deref(),
        Some("microphone disappeared")
    );
    assert_eq!(present[0].audio_path, PathBuf::from(&audio_path));
    assert!(present[0].audio_present);

    std::fs::remove_file(audio_path).unwrap();
    assert!(!runtime.recovery_entries().unwrap()[0].audio_present);
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
    assert!(runtime.recovery_entries().unwrap().is_empty());
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
