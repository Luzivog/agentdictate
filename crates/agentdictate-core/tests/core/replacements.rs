use agentdictate_core::{ReplacementRule, apply_replacements};

#[test]
fn replacement_rules_preserve_python_whole_word_and_case_behavior() {
    let rules = [
        ReplacementRule {
            id: Some(7),
            source_phrase: "shoe".into(),
            replacement_phrase: "SHU".into(),
            enabled: true,
            case_sensitive: false,
            whole_word_only: true,
        },
        ReplacementRule {
            id: Some(8),
            source_phrase: "next js".into(),
            replacement_phrase: "Next.js".into(),
            enabled: true,
            case_sensitive: false,
            whole_word_only: true,
        },
    ];

    let result = apply_replacements("Shoe and shoelace, then NEXT JS.", &rules).unwrap();

    assert_eq!(result.text, "SHU and shoelace, then Next.js.");
    assert_eq!(result.applied[0].rule_id, Some(7));
    assert_eq!(result.applied[0].count, 1);
    assert_eq!(result.applied[1].rule_id, Some(8));
    assert_eq!(result.applied[1].count, 1);
}
