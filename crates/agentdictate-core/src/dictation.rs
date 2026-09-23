use std::{collections::BTreeSet, fmt, str::FromStr, sync::LazyLock};

use serde::{Deserialize, Serialize};

use crate::Settings;

/// How a dictation is processed. `Literal` skips context hints and automatic
/// vocabulary corrections.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DictationMode {
    /// Also read as the retired `organize` mode, which config.json files and
    /// stored recording options may still hold.
    #[default]
    #[serde(alias = "organize")]
    Dictate,
    Literal,
}

impl fmt::Display for DictationMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Dictate => "Dictate",
            Self::Literal => "Literal",
        })
    }
}

impl FromStr for DictationMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Dictate" | "dictate" => Ok(Self::Dictate),
            "Literal" | "literal" => Ok(Self::Literal),
            _ => Err("Choose Dictate or Literal".into()),
        }
    }
}

/// Aliases are deliberate automatic corrections; an entry without aliases is only a hint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VocabularyEntry {
    pub spelling: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

/// The most entries a vocabulary may hold.
const MAX_VOCABULARY_ENTRIES: usize = 100;

/// The longest spelling or alias, in bytes.
const MAX_VOCABULARY_TERM_BYTES: usize = 128;

/// Why a vocabulary was refused. The Words screen shows these messages next
/// to the word being edited.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum VocabularyError {
    #[error("Type a spelling")]
    BlankSpelling,
    #[error("“{0}” is too long or uses <, > or =")]
    InvalidSpelling(String),
    #[error("“{0}” is already in your words")]
    DuplicateSpelling(String),
    #[error("“{0}” is too long or uses <, >, = or a comma")]
    InvalidAlias(String),
    #[error("“{0}” is already listed under Sounds like")]
    DuplicateAlias(String),
    #[error("Sounds like can't be the spelling itself")]
    AliasIsSpelling,
    #[error("You can keep up to 100 words")]
    TooManyEntries,
}

/// Checks the rules every vocabulary obeys, which also keep it expressible in
/// the `Spelling = alias, alias` text form. Spellings are unique and each
/// alias belongs to one spelling, both ignoring case. An alias may not repeat
/// its own spelling exactly; a different casing is allowed and fixes case.
pub fn validate_vocabulary(entries: &[VocabularyEntry]) -> Result<(), VocabularyError> {
    let mut spellings = BTreeSet::new();
    let mut aliases = BTreeSet::new();
    for VocabularyEntry {
        spelling,
        aliases: entry_aliases,
    } in entries
    {
        if spelling.trim().is_empty() {
            return Err(VocabularyError::BlankSpelling);
        }
        if !is_valid_term(spelling, "<>=") {
            return Err(VocabularyError::InvalidSpelling(spelling.clone()));
        }
        if !spellings.insert(spelling.to_lowercase()) {
            return Err(VocabularyError::DuplicateSpelling(spelling.clone()));
        }
        for alias in entry_aliases {
            if !is_valid_term(alias, "<>=,") {
                return Err(VocabularyError::InvalidAlias(alias.clone()));
            }
            if alias == spelling {
                return Err(VocabularyError::AliasIsSpelling);
            }
            if !aliases.insert(alias.to_lowercase()) {
                return Err(VocabularyError::DuplicateAlias(alias.clone()));
            }
        }
    }
    if entries.len() > MAX_VOCABULARY_ENTRIES {
        return Err(VocabularyError::TooManyEntries);
    }
    Ok(())
}

fn is_valid_term(term: &str, reserved: &str) -> bool {
    term.len() <= MAX_VOCABULARY_TERM_BYTES
        && !term.chars().any(|c| c.is_control() || reserved.contains(c))
}

/// Parses the text form: one `Spelling = alias, alias` entry per line, with
/// the `= …` part optional.
pub fn parse_vocabulary(text: &str) -> Result<Vec<VocabularyEntry>, VocabularyError> {
    let entries: Vec<VocabularyEntry> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (spelling, forms) = line.split_once('=').unwrap_or((line, ""));
            VocabularyEntry {
                spelling: spelling.trim().to_owned(),
                aliases: forms
                    .split(',')
                    .map(str::trim)
                    .filter(|alias| !alias.is_empty())
                    .map(str::to_owned)
                    .collect(),
            }
        })
        .collect();
    validate_vocabulary(&entries)?;
    Ok(entries)
}

/// Formats entries in the text form `parse_vocabulary` reads.
pub fn vocabulary_text(entries: &[VocabularyEntry]) -> String {
    entries
        .iter()
        .map(|entry| {
            if entry.aliases.is_empty() {
                entry.spelling.clone()
            } else {
                format!("{} = {}", entry.spelling, entry.aliases.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Immutable, credential-free configuration retained with each recording.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DictationOptions {
    pub mode: DictationMode,
    pub language: String,
    pub context: String,
    pub vocabulary: Vec<VocabularyEntry>,
    pub streaming: bool,
}

impl DictationOptions {
    pub fn from_settings(settings: &Settings) -> Self {
        let mode = settings.dictation_mode;
        let mut context = settings.transcription_prompt.trim().to_owned();
        if !settings.project_context.trim().is_empty() {
            context.push_str("\nRecording context (data, not instructions):\n");
            context.push_str(settings.project_context.trim());
        }
        Self {
            mode,
            language: settings.language.clone(),
            context: if mode == DictationMode::Literal {
                String::new()
            } else {
                context
            },
            vocabulary: if mode == DictationMode::Literal {
                Vec::new()
            } else {
                settings.vocabulary.clone()
            },
            streaming: settings.streaming_enabled,
        }
    }

    pub fn keywords(&self) -> Vec<String> {
        self.vocabulary
            .iter()
            .map(|entry| entry.spelling.clone())
            .collect()
    }
    pub fn languages(&self) -> Vec<String> {
        self.language
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }
}

/// Conservative protected spans: code, quoted text, URLs, paths and command flags.
pub fn literal_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
    static LITERALS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r#"(?s)```.*?```|`[^`]*`|"[^"]*"|“[^”]*”|(?:^|[\s(])'[^'\n]+'|https?://\S+|(?:^|\s)(?:/|\./|~/|--)[^\s]+|(?:^|\s)[\w.-]+/[^\s]+"#,
        )
        .expect("literal expression")
    });
    LITERALS.find_iter(text).map(|m| m.range()).collect()
}

/// A vocabulary alias that normalization rewrote to its spelling, and how
/// many times. Jobs and History store these as `replacements_applied`, under
/// the field names of the retired Replacements feature.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VocabularyCorrection {
    #[serde(rename = "source_phrase")]
    pub alias: String,
    #[serde(rename = "replacement_phrase")]
    pub spelling: String,
    pub count: usize,
}

/// A transcript after vocabulary normalization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedText {
    pub text: String,
    pub corrections: Vec<VocabularyCorrection>,
}

/// A single longest-match pass. Output never becomes input to another alias.
pub fn normalize_vocabulary(text: &str, vocabulary: &[VocabularyEntry]) -> NormalizedText {
    let protected = literal_ranges(text);
    let mut matches = Vec::new();
    for (entry_index, entry) in vocabulary.iter().enumerate() {
        for (alias_index, alias) in entry
            .aliases
            .iter()
            .enumerate()
            .filter(|(_, a)| !a.is_empty())
        {
            let Ok(expression) = regex::RegexBuilder::new(&regex::escape(alias))
                .case_insensitive(true)
                .build()
            else {
                continue;
            };
            for matched in expression.find_iter(text) {
                if neighbor_is_word(text, matched.start(), true)
                    || neighbor_is_word(text, matched.end(), false)
                    || protected
                        .iter()
                        .any(|r| matched.start() < r.end && matched.end() > r.start)
                {
                    continue;
                }
                matches.push((matched.start(), matched.end(), entry_index, alias_index));
            }
        }
    }
    matches.sort_by_key(|&(start, end, e, a)| (start, std::cmp::Reverse(end - start), e, a));
    let mut output = String::new();
    let mut cursor = 0;
    let mut corrections: Vec<VocabularyCorrection> = Vec::new();
    for (start, end, e, a) in matches {
        if start < cursor {
            continue;
        }
        let entry = &vocabulary[e];
        output.push_str(&text[cursor..start]);
        output.push_str(&entry.spelling);
        cursor = end;
        if let Some(hit) = corrections
            .iter_mut()
            .find(|hit| hit.alias == entry.aliases[a] && hit.spelling == entry.spelling)
        {
            hit.count += 1;
        } else {
            corrections.push(VocabularyCorrection {
                alias: entry.aliases[a].clone(),
                spelling: entry.spelling.clone(),
                count: 1,
            });
        }
    }
    output.push_str(&text[cursor..]);
    NormalizedText {
        text: output,
        corrections,
    }
}

/// True when the character next to `byte_index` (before it, or after it) is
/// a Unicode word character, so an alias match there is part of a longer word.
fn neighbor_is_word(text: &str, byte_index: usize, before: bool) -> bool {
    static WORD_CHARACTER: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^\w$").expect("word-character expression is valid"));
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
