//! Workspace view-model contracts.

use agentdictate_core::{
    HistoryPageSnapshot, HistorySnapshot, JobId, JobStage, RecoverySnapshot, UsageDaySnapshot,
    UsageSnapshot, UsageTotalsSnapshot, WorkspaceSnapshot,
};
use agentdictate_ui::{
    HistoryViewModel, RecoveryItemViewModel, RecoveryStage, TranscriptViewModel, UsageDayViewModel,
    UsagePeriod, UsageTotals, UsageViewModel, WorkspaceAction, WorkspaceViewModel,
};
use chrono::{DateTime, FixedOffset, NaiveDate, TimeDelta, TimeZone, Utc};

/// Wednesday 23 September 2026, 10:00 at UTC+2.
fn now() -> DateTime<FixedOffset> {
    FixedOffset::east_opt(2 * 3600)
        .unwrap()
        .with_ymd_and_hms(2026, 9, 23, 10, 0, 0)
        .unwrap()
}

fn totals(dictations: u64) -> UsageTotalsSnapshot {
    UsageTotalsSnapshot {
        dictations,
        ..UsageTotalsSnapshot::default()
    }
}

#[test]
fn the_database_snapshot_is_shown_on_the_local_clock_for_the_chosen_period() {
    let snapshot = WorkspaceSnapshot {
        recent: HistoryPageSnapshot {
            rows: vec![HistorySnapshot {
                id: 4,
                created_at: Utc.with_ymd_and_hms(2026, 9, 23, 7, 30, 0).unwrap(),
                preview_text: "one two…".into(),
                text: "one two three".into(),
                word_count: 3,
                duration_seconds: 7.0,
            }],
            ..HistoryPageSnapshot::default()
        },
        usage: UsageSnapshot {
            last_7_days: totals(2),
            last_30_days: totals(5),
            all_time: totals(9),
            activity: vec![UsageDaySnapshot {
                date: NaiveDate::from_ymd_opt(2026, 9, 22).unwrap(),
                totals: totals(2),
            }],
            weekly_activity: vec![UsageDaySnapshot {
                date: NaiveDate::from_ymd_opt(2026, 8, 17).unwrap(),
                totals: totals(9),
            }],
        },
        ..WorkspaceSnapshot::default()
    };

    let week = WorkspaceViewModel::from_snapshot(&snapshot, UsagePeriod::Last7Days, &now());
    let all = WorkspaceViewModel::from_snapshot(&snapshot, UsagePeriod::AllTime, &now());

    let recent = &week.recent_transcripts[0];
    assert_eq!(recent.created_at, "Today 09:30");
    assert_eq!(recent.preview, "one two…");
    assert_eq!(recent.text, "one two three");
    assert_eq!(recent.duration, "0:07");
    assert_eq!(week.usage.totals.dictations, 2);
    assert_eq!(week.usage.activity[0].label, "Sep 22");
    assert_eq!(all.usage.totals.dictations, 9);
    assert_eq!(all.usage.activity[0].label, "Week of Aug 17");
}

#[test]
fn recovery_items_offer_the_retry_their_state_needs_and_say_when_they_expire() {
    let updated_at = now().with_timezone(&Utc) - TimeDelta::minutes(5);
    let item = |stage, final_text: &str, lifetime| RecoverySnapshot {
        job_id: JobId::new(),
        stage,
        updated_at,
        expires_at: updated_at + lifetime,
        duration_seconds: 36.0,
        raw_transcript: String::new(),
        final_text: final_text.to_owned(),
        error_message: None,
        failure: None,
        audio_present: true,
        delivery_ambiguous: false,
    };
    let recoveries = [
        item(JobStage::Cancelled, "", TimeDelta::days(1)),
        item(JobStage::Failed, "Stored words.", TimeDelta::days(7)),
        item(JobStage::Failed, "", TimeDelta::days(7)),
    ];

    let history =
        HistoryViewModel::from_snapshots(&recoveries, &HistoryPageSnapshot::default(), &now());

    let [cancelled, paste, transcribe] = &history.recovery.items[..] else {
        panic!("every Recovery item is listed");
    };
    assert_eq!(cancelled.stage, RecoveryStage::Cancelled);
    assert_eq!(cancelled.stage.label(), "Cancelled — transcribe anyway?");
    assert_eq!(cancelled.primary_action_label(), "Transcribe");
    assert_eq!(cancelled.expires.as_deref(), Some("Expires in 24 hours"));
    assert_eq!(cancelled.captured_at, "Today 09:55");
    assert_eq!(paste.stage, RecoveryStage::Delivery);
    assert_eq!(paste.transcript_preview.as_deref(), Some("Stored words."));
    assert_eq!(transcribe.stage, RecoveryStage::Transcription);
    assert_eq!(transcribe.expires.as_deref(), Some("Expires in 7 days"));
}

#[test]
fn history_projects_recoverable_recordings_and_transcripts_without_losing_actions() {
    let recovery = RecoveryItemViewModel::new(
        "018f-recovery",
        RecoveryStage::Delivery,
        "Today, 14:32",
        "2m 08s",
        "Paste target disappeared",
        Some("The transcript is still safe".to_owned()),
    );
    let transcript = TranscriptViewModel::new(
        41,
        "Today, 14:18",
        "Ship the clean recovery flow.",
        6,
        "18s",
    );

    let history = HistoryViewModel::from_page(
        vec![recovery.clone()],
        vec![transcript],
        1,
        String::new(),
        false,
    );

    assert_eq!(history.recovery.item_count, 1);
    assert_eq!(history.recovery.items, vec![recovery]);
    assert_eq!(history.transcript_count, 1);
    assert_eq!(
        history.transcripts[0].preview,
        "Ship the clean recovery flow."
    );
    assert_eq!(
        history.recovery.items[0].primary_action_label(),
        "Paste again"
    );
}

#[test]
fn usage_formats_real_totals_and_preserves_activity_order() {
    let usage = UsageViewModel::new(
        UsagePeriod::Last30Days,
        UsageTotals {
            dictations: 23,
            words: 4_891,
            audio_seconds: 754,
            estimated_cost_usd: 0.1842,
        },
        vec![
            UsageDayViewModel::new("Mon", 4, 820, 113, 0.031),
            UsageDayViewModel::new("Tue", 7, 1_540, 241, 0.058),
        ],
    );

    assert_eq!(usage.period.label(), "Last 30 days");
    assert_eq!(usage.dictations_value(), "23");
    assert_eq!(usage.words_value(), "4,891");
    assert_eq!(usage.audio_value(), "12m 34s");
    assert_eq!(usage.cost_value(), "$0.18");
    assert_eq!(usage.activity[0].label, "Mon");
    assert_eq!(usage.peak_audio_seconds(), 241);
}

#[test]
fn workspace_actions_own_stable_rendering_selectors() {
    let actions = [
        (
            WorkspaceAction::RetryRecovery {
                id: "018f-recovery".to_owned(),
                stage: RecoveryStage::Transcription,
            },
            "history-retry-recovery-018f-recovery",
        ),
        (
            WorkspaceAction::DeleteRecovery {
                id: "018f-recovery".to_owned(),
            },
            "history-delete-recovery-018f-recovery",
        ),
        (
            WorkspaceAction::CopyTranscript { id: 41 },
            "history-copy-transcript-41",
        ),
        (
            WorkspaceAction::DeleteTranscript { id: 41 },
            "history-delete-transcript-41",
        ),
        (WorkspaceAction::ClearHistory, "settings-delete-all-history"),
        (
            WorkspaceAction::SearchHistory {
                query: "database".to_owned(),
            },
            "history-search",
        ),
        (WorkspaceAction::LoadMoreHistory, "history-load-more"),
        (
            WorkspaceAction::SelectUsagePeriod(UsagePeriod::Last7Days),
            "usage-period-7-days",
        ),
    ];

    for (action, expected) in actions {
        assert_eq!(action.selector(), expected);
    }
}
