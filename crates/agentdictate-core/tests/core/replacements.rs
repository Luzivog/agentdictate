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

#[test]
fn replacements_keep_unicode_boundaries_literal_values_and_stored_order() {
    let rule = ReplacementRule {
        id: Some(1),
        source_phrase: "shoe".into(),
        replacement_phrase: "$1\\literal".into(),
        enabled: true,
        case_sensitive: false,
        whole_word_only: true,
    };
    // Combining marks, join controls, and underscores are Unicode regex word characters.
    let result = apply_replacements(
        "shoe éshoe shoe\u{301} shoe\u{200d} _shoe shoe_ (SHOE) shoe",
        &[
            rule.clone(),
            ReplacementRule {
                id: Some(2),
                source_phrase: "$1\\literal".into(),
                replacement_phrase: "done".into(),
                case_sensitive: true,
                whole_word_only: false,
                ..rule.clone()
            },
        ],
    )
    .unwrap();
    assert_eq!(
        result.text,
        "done éshoe shoe\u{301} shoe\u{200d} _shoe shoe_ (done) done"
    );
    assert_eq!(
        result
            .applied
            .iter()
            .map(|entry| (entry.rule_id, entry.count))
            .collect::<Vec<_>>(),
        [(Some(1), 3), (Some(2), 3)]
    );

    let result = apply_replacements(
        "aaaaa",
        &[ReplacementRule {
            source_phrase: "aa".into(),
            replacement_phrase: String::new(),
            whole_word_only: false,
            ..rule
        }],
    )
    .unwrap();
    assert_eq!(result.text, "a");
    assert_eq!(result.applied[0].count, 2);
}
