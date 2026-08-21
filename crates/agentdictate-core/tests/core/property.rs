use agentdictate_core::{
    ClientCommand, PROTOCOL_VERSION, ReplacementRule, ServerMessage, Settings, apply_replacements,
    estimate_session_cost,
};
use proptest::prelude::*;

fn rule(source: &str, replacement: &str, enabled: bool) -> ReplacementRule {
    ReplacementRule {
        id: None,
        source_phrase: source.to_owned(),
        replacement_phrase: replacement.to_owned(),
        enabled,
        case_sensitive: false,
        whole_word_only: false,
    }
}

proptest! {
    #[test]
    fn replacing_a_source_with_itself_leaves_text_untouched(
        text in any::<String>(),
        seed in "[a-z]",
    ) {
        let full = format!("{text}{seed}{text}");
        let mut self_rule = rule(&seed, &seed, true);
        // Case-insensitive matching splices the replacement literally, so an
        // identity claim requires exact-case matching.
        self_rule.case_sensitive = true;
        let result = apply_replacements(&full, &[self_rule]).unwrap();
        prop_assert_eq!(result.text, full);
        prop_assert_eq!(result.applied.len(), 1);
    }

    #[test]
    fn disabled_and_empty_rules_never_alter_text(text in any::<String>()) {
        let rules = [
            rule("anything", "else", false),
            rule("", "nothing", true),
        ];
        let result = apply_replacements(&text, &rules).unwrap();
        prop_assert_eq!(result.text, text);
        prop_assert!(result.applied.is_empty());
    }

    #[test]
    fn applying_no_rules_is_the_identity(text in any::<String>()) {
        let result = apply_replacements(&text, &[]).unwrap();
        prop_assert_eq!(result.text, text);
        prop_assert!(result.applied.is_empty());
    }

    #[test]
    fn arbitrary_unicode_sources_and_text_never_panic(
        text in any::<String>(),
        source in any::<String>(),
        replacement in any::<String>(),
    ) {
        let rules = [rule(&source, &replacement, true)];
        let _ = apply_replacements(&text, &rules);
    }

    #[test]
    fn cleanup_disabled_sessions_report_zero_cleanup_tokens(
        duration in 0.0..600.0f64,
        raw in any::<String>(),
        price in 0.0..1.0f64,
    ) {
        let estimate = estimate_session_cost(duration, &raw, None, true, price, 0.05, 0.4);
        prop_assert_eq!(estimate.cleanup_input_tokens, 0);
        prop_assert_eq!(estimate.cleanup_output_tokens, 0);
        prop_assert_eq!(estimate.total_cost, estimate.transcription_cost);
    }

    #[test]
    fn every_client_command_tags_the_protocol_version(request_id in any::<u64>()) {
        for command in [
            ClientCommand::get_snapshot(request_id),
            ClientCommand::start_recording(request_id),
            ClientCommand::stop_recording(request_id),
            ClientCommand::quit(request_id),
        ] {
            prop_assert_eq!(command.protocol_version, PROTOCOL_VERSION);
        }
        let message = ServerMessage::command_rejected(request_id, "no");
        prop_assert_eq!(message.protocol_version, PROTOCOL_VERSION);
    }
}

#[test]
fn settings_round_trip_through_json_preserves_defaults() {
    let settings = Settings::default();
    let json = serde_json::to_string(&settings).unwrap();
    let restored: Settings = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, settings);
}
