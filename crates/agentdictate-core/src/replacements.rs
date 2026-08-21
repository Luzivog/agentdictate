use serde::{Deserialize, Serialize};

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
    let word_character = regex::Regex::new(r"^\w$")?;
    for rule in rules {
        if !rule.enabled || rule.source_phrase.is_empty() {
            continue;
        }
        let expression = regex::RegexBuilder::new(&regex::escape(&rule.source_phrase))
            .case_insensitive(!rule.case_sensitive)
            .build()?;
        let matches = expression
            .find_iter(&text)
            .filter(|matched| {
                !rule.whole_word_only
                    || (!neighbor_is_word(&text, matched.start(), true, &word_character)
                        && !neighbor_is_word(&text, matched.end(), false, &word_character))
            })
            .map(|matched| (matched.start(), matched.end()))
            .collect::<Vec<_>>();
        let count = matches.len();
        if count == 0 {
            continue;
        }
        let mut replaced = String::with_capacity(text.len());
        let mut cursor = 0;
        for (start, end) in matches {
            replaced.push_str(&text[cursor..start]);
            replaced.push_str(&rule.replacement_phrase);
            cursor = end;
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

fn neighbor_is_word(
    text: &str,
    byte_index: usize,
    before: bool,
    word_character: &regex::Regex,
) -> bool {
    let neighbor = if before {
        text[..byte_index].chars().next_back()
    } else {
        text[byte_index..].chars().next()
    };
    neighbor.is_some_and(|character| {
        let mut encoded = [0; 4];
        word_character.is_match(character.encode_utf8(&mut encoded))
    })
}
