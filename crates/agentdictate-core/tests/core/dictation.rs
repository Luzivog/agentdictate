use agentdictate_core::*;

#[test]
fn vocabulary_uses_longest_original_spans_and_preserves_literals() {
    let terms = parse_vocabulary("Alpha = agent\nBeta = agent dictate\nGamma = Beta").unwrap();
    let result = normalize_vocabulary(
        "agent dictate then agent. `agent` \"agent\" https://host/agent /agent --agent",
        &terms,
    );
    assert_eq!(
        result.text,
        "Beta then Alpha. `agent` \"agent\" https://host/agent /agent --agent"
    );
    assert_eq!(result.applied.iter().map(|a| a.count).sum::<usize>(), 2);
    assert_eq!(
        normalize_vocabulary("éagent agent_name agent\u{301}", &terms).text,
        "éagent agent_name agent\u{301}"
    );
}

#[test]
fn vocabulary_rejects_conflicting_aliases_and_round_trips_hints() {
    let terms = parse_vocabulary("Claude Code\nLeadlord = lead lord, lead load").unwrap();
    assert_eq!(parse_vocabulary(&vocabulary_text(&terms)).unwrap(), terms);
    assert!(parse_vocabulary("A = common\nB = COMMON").is_err());
    assert!(parse_vocabulary("<bad>").is_err());
    assert_eq!(
        normalize_vocabulary("My landlord compared audio codecs", &terms).text,
        "My landlord compared audio codecs"
    );
}

#[test]
fn critical_edits_fall_back_but_punctuation_can_improve() {
    for (raw, cleaned) in [
        ("Do not push", "Push"),
        ("Maybe change it after checking", "Change it"),
        ("Use 15 percent", "Use 50 percent"),
        ("Use -15 percent", "Use 15 percent"),
        ("Use x < 15", "Use x > 15"),
        ("Check HQ first", "Check UI first"),
        ("Keep 'cloud code'", "Keep 'Claude Code'"),
        ("Keep src/cloud/config.rs", "Keep src/Claude/config.rs"),
        ("Keep `cloud code`", "Keep `Claude Code`"),
        ("Could this work?", "This works."),
        ("It is not not working", "It is not working"),
    ] {
        assert!(validate_cleanup(raw, cleaned).is_err(), "{raw} → {cleaned}");
    }
    assert!(
        validate_cleanup(
            "maybe fix it but do not push",
            "Maybe fix it, but do not push."
        )
        .is_ok()
    );
    assert!(validate_cleanup("hello", "").is_err());
}

#[test]
fn literal_options_never_include_automatic_corrections_or_cleanup() {
    let settings = Settings {
        dictation_mode: DictationMode::Literal,
        vocabulary: parse_vocabulary("Codex = codecs").unwrap(),
        ..Settings::default()
    };
    let options = DictationOptions::from_settings(
        &settings,
        vec![ReplacementRule {
            id: None,
            source_phrase: "hello".into(),
            replacement_phrase: "changed".into(),
            enabled: true,
            case_sensitive: false,
            whole_word_only: true,
        }],
    );
    assert!(!options.cleanup_enabled);
    assert!(options.keywords().is_empty());
    assert!(options.replacements.is_empty());
    assert!(options.context.is_empty());
}

#[test]
fn organize_is_explicit_and_does_not_inherit_the_default_structure_prohibition() {
    let settings = Settings {
        cleanup_enabled: false,
        dictation_mode: DictationMode::Organize,
        ..Settings::default()
    };
    let options = DictationOptions::from_settings(&settings, vec![]);
    assert!(options.cleanup_enabled);
    assert!(
        options
            .cleanup_instruction
            .contains("paragraphs or bullets")
    );
    assert!(
        !options
            .cleanup_instruction
            .contains("Do not summarize, reorder requests")
    );
    assert!(!options.cleanup_instruction.contains("Testing section"));
}

#[test]
fn configuration_is_credential_free_and_vocabulary_is_generated_once() {
    let settings = Settings {
        openai_api_key: "never-snapshot-this".into(),
        vocabulary: parse_vocabulary("UniqueName = unique name").unwrap(),
        language: "en,fr".into(),
        ..Settings::default()
    };
    let options = DictationOptions::from_settings(&settings, Vec::new());
    let json = serde_json::to_string(&options).unwrap();
    assert!(!json.contains("never-snapshot-this"));
    assert_eq!(options.languages(), ["en", "fr"]);
    assert_eq!(options.cleanup_instruction.matches("UniqueName").count(), 1);
}
