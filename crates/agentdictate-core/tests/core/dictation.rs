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
    assert_eq!(result.corrections.iter().map(|a| a.count).sum::<usize>(), 2);
    assert_eq!(
        normalize_vocabulary("éagent agent_name agent\u{301}", &terms).text,
        "éagent agent_name agent\u{301}"
    );
}

#[test]
fn vocabulary_rejects_conflicting_aliases_and_round_trips_hints() {
    let terms = parse_vocabulary("Claude Code\nBrightlane = bright lane, bright lain").unwrap();
    assert_eq!(parse_vocabulary(&vocabulary_text(&terms)).unwrap(), terms);
    assert!(parse_vocabulary("A = common\nB = COMMON").is_err());
    assert!(parse_vocabulary("<bad>").is_err());
    assert_eq!(
        normalize_vocabulary("My landlord compared audio codecs", &terms).text,
        "My landlord compared audio codecs"
    );
}

#[test]
fn vocabulary_rejects_blank_spellings_self_aliases_and_aliases_the_text_form_cannot_hold() {
    let entry = |spelling: &str, aliases: &[&str]| VocabularyEntry {
        spelling: spelling.to_owned(),
        aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
    };

    assert_eq!(
        validate_vocabulary(&[entry("  ", &[])]),
        Err(VocabularyError::BlankSpelling)
    );
    assert_eq!(
        validate_vocabulary(&[entry("Brightlane", &["Brightlane"])]),
        Err(VocabularyError::AliasIsSpelling)
    );
    assert_eq!(
        validate_vocabulary(&[entry("Brightlane", &["bright, lane"])]),
        Err(VocabularyError::InvalidAlias("bright, lane".to_owned()))
    );
    assert_eq!(
        validate_vocabulary(&[entry("Brightlane", &[]), entry("brightlane", &[])]),
        Err(VocabularyError::DuplicateSpelling("brightlane".to_owned()))
    );
    // A different casing is a deliberate case fix.
    assert_eq!(
        validate_vocabulary(&[entry("GitHub", &["github", "git hub"])]),
        Ok(())
    );
}

#[test]
fn literal_options_never_include_automatic_corrections() {
    let settings = Settings {
        dictation_mode: DictationMode::Literal,
        vocabulary: parse_vocabulary("Codex = codecs").unwrap(),
        ..Settings::default()
    };
    let options = DictationOptions::from_settings(&settings);
    assert!(options.keywords().is_empty());
    assert!(options.vocabulary.is_empty());
    assert!(options.context.is_empty());
}

#[test]
fn configuration_is_credential_free() {
    let settings = Settings {
        openai_api_key: "never-snapshot-this".into(),
        vocabulary: parse_vocabulary("UniqueName = unique name").unwrap(),
        ..Settings::default()
    };
    let options = DictationOptions::from_settings(&settings);
    let json = serde_json::to_string(&options).unwrap();
    assert!(!json.contains("never-snapshot-this"));
}

#[test]
fn options_stored_with_the_retired_organize_mode_and_cleanup_load_as_dictate() {
    let options: DictationOptions = serde_json::from_str(
        r#"{
            "mode": "organize",
            "language": "en",
            "context": "",
            "vocabulary": [],
            "cleanup_enabled": true,
            "cleanup_model": "gpt-5.4-nano",
            "cleanup_instruction": "Edit the transcript.",
            "streaming": false,
            "replacements": []
        }"#,
    )
    .unwrap();

    assert_eq!(options.mode, DictationMode::Dictate);
    let settings: Settings = serde_json::from_str(r#"{"dictation_mode":"organize"}"#).unwrap();
    assert_eq!(settings.dictation_mode, DictationMode::Dictate);
}
