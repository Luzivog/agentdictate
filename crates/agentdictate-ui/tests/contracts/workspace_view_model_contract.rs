//! Workspace view-model contracts.

use agentdictate_ui::{
    HistoryViewModel, RecoveryItemViewModel, RecoveryStage, ReplacementDraft,
    ReplacementRuleViewModel, ReplacementsViewModel, TranscriptViewModel, UsageDayViewModel,
    UsagePeriod, UsageTotals, UsageViewModel, WorkspaceAction, WorkspaceViewModel,
};

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

    let history = HistoryViewModel::from_records(vec![recovery.clone()], vec![transcript]);

    assert_eq!(history.recovery.item_count, 1);
    assert_eq!(history.recovery.items, vec![recovery]);
    assert_eq!(history.transcript_count, 1);
    assert_eq!(
        history.transcripts[0].preview(),
        "Ship the clean recovery flow."
    );
    assert_eq!(
        history.recovery.items[0].primary_action_label(),
        "Paste again"
    );
}

#[test]
fn replacements_expose_enabled_summary_and_exact_match_policy() {
    let replacements = ReplacementsViewModel::new(vec![
        ReplacementRuleViewModel::new(7, "agent dictate", "AgentDictate", true, false, true),
        ReplacementRuleViewModel::new(8, "lead lord", "Leadlord", false, true, false),
    ]);

    assert_eq!(replacements.rule_count(), 2);
    assert_eq!(replacements.enabled_count(), 1);
    assert_eq!(replacements.rules[0].match_policy_label(), "Whole words");
    assert_eq!(
        replacements.rules[1].match_policy_label(),
        "Case-sensitive · Anywhere"
    );
    assert!(ReplacementDraft::new("agent dictate", "AgentDictate").is_valid());
    assert!(!ReplacementDraft::new("  ", "AgentDictate").is_valid());
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
            WorkspaceAction::SearchHistory {
                query: "database".to_owned(),
            },
            "history-search",
        ),
        (WorkspaceAction::LoadMoreHistory, "history-load-more"),
        (
            WorkspaceAction::CreateReplacement {
                draft: ReplacementDraft::new("agent dictate", "AgentDictate"),
            },
            "replacement-save-new",
        ),
        (
            WorkspaceAction::UpdateReplacement {
                id: 7,
                draft: ReplacementDraft::new("agent dictate", "AgentDictate"),
            },
            "replacement-save-7",
        ),
        (
            WorkspaceAction::SetReplacementEnabled {
                id: 7,
                enabled: false,
            },
            "replacement-toggle-7",
        ),
        (
            WorkspaceAction::DeleteReplacement { id: 7 },
            "replacement-delete-7",
        ),
        (
            WorkspaceAction::SelectUsagePeriod(UsagePeriod::Last7Days),
            "usage-period-7-days",
        ),
    ];

    for (action, expected) in actions {
        assert_eq!(action.selector(), expected);
    }

    assert_eq!(
        WorkspaceViewModel::default(),
        WorkspaceViewModel {
            history: HistoryViewModel::default(),
            recent_transcripts: Vec::new(),
            replacements: ReplacementsViewModel::default(),
            usage: UsageViewModel::default(),
            model_catalog: agentdictate_ui::ModelCatalogViewModel::default(),
        }
    );
}
