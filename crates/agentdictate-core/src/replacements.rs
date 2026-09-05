use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

static WORD_CHARACTER: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^\w$").expect("word-character expression is valid"));

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReplacementRule {
    pub id: Option<i64>,
    pub source_phrase: String,
    pub replacement_phrase: String,
    pub enabled: bool,
    pub case_sensitive: bool,
    pub whole_word_only: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AppliedReplacement {
    pub rule_id: Option<i64>,
    pub source_phrase: String,
    pub replacement_phrase: String,
    pub count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReplacementResult {
    pub text: String,
    pub applied: Vec<AppliedReplacement>,
}

/// Applies enabled rules in stored order, matching the existing Python behavior.
pub fn apply_replacements(
    text: &str,
    rules: &[ReplacementRule],
) -> Result<ReplacementResult, regex::Error> {
    let mut text = text.to_owned();
    let mut applied = Vec::new();
    for rule in rules {
        if !rule.enabled || rule.source_phrase.is_empty() {
            continue;
        }
        let expression = regex::RegexBuilder::new(&regex::escape(&rule.source_phrase))
            .case_insensitive(!rule.case_sensitive)
            .build()?;
        let mut matches = expression
            .find_iter(&text)
            .filter(|matched| {
                !rule.whole_word_only
                    || (!neighbor_is_word(&text, matched.start(), true)
                        && !neighbor_is_word(&text, matched.end(), false))
            })
            .peekable();
        if matches.peek().is_none() {
            continue;
        }
        let mut replaced = String::with_capacity(text.len());
        let mut cursor = 0;
        let mut count = 0;
        for matched in matches {
            replaced.push_str(&text[cursor..matched.start()]);
            replaced.push_str(&rule.replacement_phrase);
            cursor = matched.end();
            count += 1;
        }
        replaced.push_str(&text[cursor..]);
        text = replaced;
        applied.push(AppliedReplacement {
            rule_id: rule.id,
            source_phrase: rule.source_phrase.clone(),
            replacement_phrase: rule.replacement_phrase.clone(),
            count,
        });
    }
    Ok(ReplacementResult { text, applied })
}

pub(crate) fn neighbor_is_word(text: &str, byte_index: usize, before: bool) -> bool {
    let neighbor = if before {
        text[..byte_index].chars().next_back()
    } else {
        text[byte_index..].chars().next()
    };
    neighbor.is_some_and(|character| {
        let mut encoded = [0; 4];
        WORD_CHARACTER.is_match(character.encode_utf8(&mut encoded))
    })
}
