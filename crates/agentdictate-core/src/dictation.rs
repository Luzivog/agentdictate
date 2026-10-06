use std::{collections::BTreeSet, fmt, ops::Range, str::FromStr, sync::LazyLock};

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
}

impl DictationOptions {
    pub fn from_settings(settings: &Settings) -> Self {
        let mode = settings.dictation_mode;
        Self {
            mode,
            language: settings.language.clone(),
            context: if mode == DictationMode::Literal {
                String::new()
            } else {
                settings.transcription_prompt.trim().to_owned()
            },
            vocabulary: if mode == DictationMode::Literal {
                Vec::new()
            } else {
                settings.vocabulary.clone()
            },
        }
    }

    /// The spellings sent to OpenAI as `keywords[]`. Keywords name literal
    /// terms in the audio, so symbol spellings such as `/` are left out.
    pub fn keywords(&self) -> Vec<String> {
        self.vocabulary
            .iter()
            .filter(|entry| has_letter_or_digit(&entry.spelling))
            .map(|entry| entry.spelling.clone())
            .collect()
    }
}

/// Conservative protected spans: code, quoted text, URLs, paths and command flags.
pub fn literal_ranges(text: &str) -> Vec<Range<usize>> {
    static LITERALS: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r#"(?s)```.*?```|`[^`]*`|"[^"]*"|“[^”]*”|(?:^|[\s(])'[^'\n]+'|https?://\S+|(?:^|\s)(?:/|\./|~/|--)[^\s]+|(?:^|\s)[\w.-]+/[^\s]+"#,
        )
        .expect("literal expression")
    });
    LITERALS.find_iter(text).map(|m| m.range()).collect()
}

/// A rewrite that normalization applied, and how many times. `alias` is the
/// Sounds like entry that matched, or for a casing fix the text as heard
/// (`agents.md` for `AGENTS.md`). Jobs and History store these as JSON in
/// `vocabulary_corrections`; rows written before schema version 3 used the
/// retired Replacements names, which still deserialize.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VocabularyCorrection {
    #[serde(alias = "source_phrase")]
    pub alias: String,
    #[serde(alias = "replacement_phrase")]
    pub spelling: String,
    pub count: usize,
}

/// A transcript after vocabulary normalization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedText {
    pub text: String,
    pub corrections: Vec<VocabularyCorrection>,
}

/// Why a span of the transcript is claimed. On equal spans an alias wins,
/// then a casing fix, then a kept spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum RewriteKind {
    Alias,
    Casing,
    /// A spelling that stays as heard. It still claims its span, so a shorter
    /// spelling inside it is not rewritten: `Claude` never touches `CLAUDE.md`.
    Keep,
}

/// A span of the original transcript to replace with `replacement`. A span
/// whose replacement equals its text changes nothing and records nothing.
struct Rewrite<'a> {
    range: Range<usize>,
    kind: RewriteKind,
    source: &'a str,
    replacement: &'a str,
}

/// Rewrites aliases to their spellings and fixes the casing of spellings, in
/// a single longest-match pass over the original text, so output never
/// becomes input to another rewrite. Both need whole words outside
/// [`literal_ranges`]; every spelling found claims its span, even unchanged. Symbol spellings join words instead of standing alone
/// (see `symbol_rewrite_range`); casing fixes skip ambiguous cases (see
/// `needs_casing_fix`).
pub fn normalize_vocabulary(text: &str, vocabulary: &[VocabularyEntry]) -> NormalizedText {
    let protected = literal_ranges(text);
    let is_free = |range: &Range<usize>| {
        !protected
            .iter()
            .any(|r| range.start < r.end && range.end > r.start)
    };
    let mut rewrites = Vec::new();
    for entry in vocabulary {
        let spelling = entry.spelling.as_str();
        let symbol = is_symbol_spelling(spelling);
        for alias in entry.aliases.iter().filter(|alias| !alias.is_empty()) {
            for found in whole_word_matches(text, alias).filter(&is_free) {
                let range = if symbol {
                    match symbol_rewrite_range(text, found, spelling) {
                        Some(range) => range,
                        None => continue,
                    }
                } else {
                    found
                };
                rewrites.push(Rewrite {
                    range,
                    kind: RewriteKind::Alias,
                    source: alias,
                    replacement: spelling,
                });
            }
        }
        if has_letter_or_digit(spelling) {
            for found in whole_word_matches(text, spelling).filter(&is_free) {
                let heard = &text[found.clone()];
                let (kind, replacement) = if needs_casing_fix(text, found.clone(), spelling) {
                    (RewriteKind::Casing, spelling)
                } else {
                    (RewriteKind::Keep, heard)
                };
                rewrites.push(Rewrite {
                    range: found,
                    kind,
                    source: heard,
                    replacement,
                });
            }
        }
    }
    // Stable, so full ties keep vocabulary order.
    rewrites.sort_by_key(|r| (r.range.start, std::cmp::Reverse(r.range.len()), r.kind));
    let mut output = String::new();
    let mut cursor = 0;
    let mut corrections: Vec<VocabularyCorrection> = Vec::new();
    for rewrite in rewrites {
        if rewrite.range.start < cursor {
            continue;
        }
        output.push_str(&text[cursor..rewrite.range.start]);
        output.push_str(rewrite.replacement);
        let unchanged = text[rewrite.range.clone()] == *rewrite.replacement;
        cursor = rewrite.range.end;
        if unchanged {
            continue;
        }
        if let Some(hit) = corrections
            .iter_mut()
            .find(|hit| hit.alias == rewrite.source && hit.spelling == rewrite.replacement)
        {
            hit.count += 1;
        } else {
            corrections.push(VocabularyCorrection {
                alias: rewrite.source.to_owned(),
                spelling: rewrite.replacement.to_owned(),
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

/// Case-insensitive occurrences of `term` that are whole words.
fn whole_word_matches(text: &str, term: &str) -> impl Iterator<Item = Range<usize>> {
    let expression = regex::RegexBuilder::new(&regex::escape(term))
        .case_insensitive(true)
        .build()
        .ok();
    expression
        .into_iter()
        .flat_map(|expression| {
            expression
                .find_iter(text)
                .map(|found| found.range())
                .collect::<Vec<_>>()
        })
        .filter(|found| {
            !neighbor_is_word(text, found.start, true) && !neighbor_is_word(text, found.end, false)
        })
}

/// True when a spelling has a letter or digit. Only those are sent as
/// keywords or have casing to fix.
fn has_letter_or_digit(spelling: &str) -> bool {
    spelling.chars().any(char::is_alphanumeric)
}

/// A spelling made only of symbols, such as `/`, `-` or `.`.
fn is_symbol_spelling(spelling: &str) -> bool {
    !spelling.trim().is_empty() && !has_letter_or_digit(spelling)
}

/// The only symbol that may also start a word, as the root of a path.
const PATH_ROOT: &str = "/";

/// Words after a spoken symbol that make it the symbol's name: "slash command".
const SYMBOL_NOUNS: &[&str] = &[
    "character",
    "characters",
    "command",
    "commands",
    "key",
    "keys",
    "sign",
    "symbol",
];

/// Words before a spoken symbol that make it the symbol's name: "a slash",
/// "trailing slash".
const SYMBOL_NAMERS: &[&str] = &[
    "a", "an", "another", "back", "double", "forward", "leading", "single", "trailing",
];

/// Words after which `/` starts a path: "the slash home" is "the /home".
const PATH_LEADERS: &[&str] = &[
    "at", "call", "cd", "delete", "did", "do", "does", "for", "from", "in", "inside", "into", "my",
    "of", "on", "open", "our", "run", "see", "seeing", "that", "the", "these", "this", "those",
    "to", "try", "type", "under", "use", "using", "with", "your",
];

/// Words that a spoken symbol never joins or starts, because the symbol was
/// more likely a pause or a mistake: "Slash how do we", "in slash since".
const STOPWORDS: &[&str] = &[
    "a", "also", "am", "an", "and", "are", "as", "at", "be", "because", "been", "but", "by", "can",
    "could", "did", "do", "does", "even", "for", "from", "had", "has", "have", "he", "here", "how",
    "i", "if", "in", "instead", "is", "it", "just", "maybe", "no", "not", "of", "on", "or", "she",
    "should", "since", "so", "that", "the", "then", "there", "these", "they", "this", "those",
    "to", "was", "we", "were", "what", "when", "where", "which", "who", "why", "will", "with",
    "would", "yes", "you",
];

/// Where a symbol spelling's alias match (`alias`) is replaced, swallowing the
/// spaces that the symbol joins, or `None` to leave the spoken word as it is.
/// The rule, with `/` spoken as "slash":
///
/// - The next word must follow on the same line after spaces, and must not
///   name the symbol ("slash command"). Otherwise the word stays, so a stray
///   "slash." never becomes " / ".
/// - The symbol never joins or starts a next word from `STOPWORDS`, except
///   that "and" and "or" may be joined ("and slash or" is "and/or").
/// - After a word from `SYMBOL_NAMERS` ("a slash", "forward slash") or after
///   punctuation ("PRs? slash do we") the word stays.
/// - `/` alone starts a path after a word from `PATH_LEADERS` ("seeing slash
///   plans" is "seeing /plans") or at the start of a line ("slash home" is
///   "/home"); other symbols keep the word there.
/// - After any other word, the symbol joins both words: "ChatGPT slash
///   Codex" is "ChatGPT/Codex".
fn symbol_rewrite_range(text: &str, alias: Range<usize>, spelling: &str) -> Option<Range<usize>> {
    let after = &text[alias.end..];
    let next_start = after.len() - after.trim_start_matches(is_inline_space).len();
    let next = leading_word(&after[next_start..]).to_lowercase();
    if next_start == 0 || next.is_empty() || SYMBOL_NOUNS.contains(&next.as_str()) {
        return None;
    }
    let end = alias.end + next_start;
    let before = text[..alias.start].trim_end_matches(is_inline_space);
    let previous = trailing_word(before).to_lowercase();
    let next_is_stopword = STOPWORDS.contains(&next.as_str());
    let starts_path = spelling == PATH_ROOT && !next_is_stopword;
    if previous.is_empty() {
        let line_start = before.is_empty() || before.ends_with(['\n', '\r']);
        return (line_start && starts_path).then_some(alias.start..end);
    }
    if SYMBOL_NAMERS.contains(&previous.as_str()) {
        return None;
    }
    if PATH_LEADERS.contains(&previous.as_str()) {
        return starts_path.then_some(alias.start..end);
    }
    let joins = !next_is_stopword || matches!(next.as_str(), "and" | "or");
    joins.then_some(before.len()..end)
}

/// Whether the whole-word, case-insensitive occurrence of `spelling` at
/// `found` should be rewritten to it. It stays when:
///
/// - `.` or `-` joins it to another word, as in a file or package name:
///   `openai.rs`, `agentdictate-core`.
/// - It differs only by capital initials on lowercase letters of the
///   spelling, as at a sentence start or in a title: "Read-only" and
///   "Read-Only Mode" stay for `read-only`.
/// - The spelling is one word of letters cased like an ordinary word (`Rust`,
///   `Go`, `IT`, `Codex`) and the occurrence is lowercase or capitalized, so
///   it may be the common word ("rust", "go", "It's"). A Sounds like entry
///   such as `codex` opts into that fix.
fn needs_casing_fix(text: &str, found: Range<usize>, spelling: &str) -> bool {
    let heard = &text[found.clone()];
    if heard == spelling || joined_to_word(text, found) || only_capital_initials(heard, spelling) {
        return false;
    }
    let plain_word = spelling.chars().all(char::is_alphabetic)
        && (is_lowercase(spelling) || is_uppercase(spelling) || is_capitalized(spelling));
    !(plain_word && (is_lowercase(heard) || is_capitalized(heard)))
}

/// True when a `.` or `-` right before or after `range` is followed by a
/// word character outside it, as in `openai.rs`.
fn joined_to_word(text: &str, range: Range<usize>) -> bool {
    let joins = |link: Option<char>, word: Option<char>| {
        matches!(link, Some('.' | '-')) && word.is_some_and(is_word_character)
    };
    let mut before = text[..range.start].chars().rev();
    let mut after = text[range.end..].chars();
    joins(before.next(), before.next()) || joins(after.next(), after.next())
}

/// True when `heard` differs from `spelling` only where the spelling has a
/// lowercase letter starting a part and `heard` has it in uppercase.
fn only_capital_initials(heard: &str, spelling: &str) -> bool {
    let mut heard_chars = heard.chars();
    let mut previous: Option<char> = None;
    for wanted in spelling.chars() {
        let Some(got) = heard_chars.next() else {
            return false;
        };
        let part_start = !previous.is_some_and(char::is_alphanumeric);
        if got != wanted && !(part_start && wanted.is_lowercase() && got.is_uppercase()) {
            return false;
        }
        previous = Some(wanted);
    }
    heard_chars.next().is_none()
}

fn is_lowercase(text: &str) -> bool {
    !text.chars().any(char::is_uppercase)
}

fn is_uppercase(text: &str) -> bool {
    !text.chars().any(char::is_lowercase)
}

/// An uppercase first letter followed by no other uppercase letter.
fn is_capitalized(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(char::is_uppercase) && is_lowercase(chars.as_str())
}

/// Spaces and tabs, but not line breaks.
fn is_inline_space(character: char) -> bool {
    character.is_whitespace() && !matches!(character, '\n' | '\r')
}

/// The run of word characters that `text` starts with.
fn leading_word(text: &str) -> &str {
    let end = text
        .char_indices()
        .find(|&(_, character)| !is_word_character(character))
        .map_or(text.len(), |(index, _)| index);
    &text[..end]
}

/// The run of word characters that `text` ends with.
fn trailing_word(text: &str) -> &str {
    let start = text
        .char_indices()
        .rev()
        .find(|&(_, character)| !is_word_character(character))
        .map_or(0, |(index, character)| index + character.len_utf8());
    &text[start..]
}

/// True when the character next to `byte_index` (before it, or after it) is
/// a word character, so a match there is part of a longer word.
fn neighbor_is_word(text: &str, byte_index: usize, before: bool) -> bool {
    let neighbor = if before {
        text[..byte_index].chars().next_back()
    } else {
        text[byte_index..].chars().next()
    };
    neighbor.is_some_and(is_word_character)
}

/// A Unicode word character, as the regex class `\w` defines it.
fn is_word_character(character: char) -> bool {
    static WORD_CHARACTER: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^\w$").expect("word-character expression is valid"));
    let mut encoded = [0; 4];
    WORD_CHARACTER.is_match(character.encode_utf8(&mut encoded))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vocabulary(text: &str) -> Vec<VocabularyEntry> {
        parse_vocabulary(text).expect("test vocabulary is valid")
    }

    fn normalized(text: &str, vocabulary_text: &str) -> String {
        normalize_vocabulary(text, &vocabulary(vocabulary_text)).text
    }

    #[test]
    fn spoken_slash_joins_words_and_starts_paths() {
        let slash = "/ = slash";
        for (heard, expected) in [
            (
                "analysis slash research slash whatever",
                "analysis/research/whatever",
            ),
            ("update slash add slash delete", "update/add/delete"),
            ("ChatGPT slash Codex", "ChatGPT/Codex"),
            ("apps slash landing", "apps/landing"),
            ("and slash or", "and/or"),
            ("slash home is full", "/home is full"),
            ("For the slash home directory", "For the /home directory"),
            ("When we do slash plans", "When we do /plans"),
            ("I'm still seeing slash plans", "I'm still seeing /plans"),
            (
                "one worktree for that slash goal",
                "one worktree for that /goal",
            ),
            ("just delete slash docs.", "just delete /docs."),
            ("in slash new", "in /new"),
            ("of slash tmp", "of /tmp"),
        ] {
            assert_eq!(normalized(heard, slash), expected);
        }
        let result = normalize_vocabulary("and slash or", &vocabulary(slash));
        assert_eq!(
            result.corrections,
            [VocabularyCorrection {
                alias: "slash".into(),
                spelling: "/".into(),
                count: 1,
            }]
        );
    }

    #[test]
    fn spoken_slash_stays_a_word_when_it_names_the_symbol_or_cannot_join() {
        for text in [
            "PRs? slash do we have any rules",
            "posted, slash can you access it?",
            "use slash commands",
            "slash command",
            "add a trailing slash",
            "type a slash and then",
            "remove the slash at the end",
            "it ends with slash.",
            "Slash how do we set it up?",
            "Slash even things that we should",
            "And slash are you doing it?",
            "No need to have it in slash since",
        ] {
            let result = normalize_vocabulary(text, &vocabulary("/ = slash"));
            assert_eq!(result.text, text);
            assert!(result.corrections.is_empty(), "{text}");
        }
    }

    #[test]
    fn other_symbols_join_words_but_never_start_one() {
        assert_eq!(normalized("read dash only", "- = dash"), "read-only");
        assert_eq!(normalized("dash only", "- = dash"), "dash only");
    }

    #[test]
    fn casing_follows_the_spelling() {
        let words = "AGENTS.md\nT3 Code\npnpm\nClaude Code";
        assert_eq!(
            normalized(
                "Read agents.md, open T3 code, run PNPM in claude code.",
                words
            ),
            "Read AGENTS.md, open T3 Code, run pnpm in Claude Code."
        );
        let result = normalize_vocabulary("agents.md and agents.md", &vocabulary(words));
        assert_eq!(
            result.corrections,
            [VocabularyCorrection {
                alias: "agents.md".into(),
                spelling: "AGENTS.md".into(),
                count: 2,
            }]
        );
    }

    #[test]
    fn casing_leaves_sentence_starts_common_words_and_longer_words() {
        let words = "read-only\nkubectl\nRust\nIT\nCodex\nAGENTS.md";
        let text = "Read-only. Kubectl works. It's rust. The codex. myagents.mdx";
        assert_eq!(normalized(text, words), text);
        assert_eq!(
            normalized("CODEX and READ-ONLY", words),
            "Codex and read-only"
        );
        // An explicit Sounds like entry still fixes a plain word.
        assert_eq!(normalized("the codex", "Codex = codex"), "the Codex");
    }

    #[test]
    fn casing_leaves_correct_longer_spellings_file_names_and_titles() {
        let claude = "Claude\nCLAUDE.md";
        let result = normalize_vocabulary("update CLAUDE.md first", &vocabulary(claude));
        assert_eq!(result.text, "update CLAUDE.md first");
        assert!(result.corrections.is_empty());
        assert_eq!(
            normalized("update claude.md first", claude),
            "update CLAUDE.md first"
        );
        // Not a file name, so only the longer spelling shields it.
        assert_eq!(
            normalized("PNPM Workspaces", "pnpm\nPNPM Workspaces"),
            "PNPM Workspaces"
        );
        let text = "openai.rs, agentdictate-core, chatgpt.com, Read-Only Mode, Sub-Agents";
        assert_eq!(
            normalized(text, "OpenAI\nAgentDictate\nChatGPT\nread-only\nsub-agents"),
            text
        );
    }

    #[test]
    fn protected_spans_keep_their_casing_and_slashes() {
        let words = "T3 Code\nAGENTS.md\n/ = slash";
        let text = "\"t3 code\" `agents.md` `and slash or`";
        assert_eq!(normalized(text, words), text);
    }

    #[test]
    fn symbol_spellings_are_not_keywords() {
        let options = DictationOptions {
            mode: DictationMode::Dictate,
            language: String::new(),
            context: String::new(),
            vocabulary: vocabulary("/ = slash\nT3 Code"),
        };
        assert_eq!(options.keywords(), ["T3 Code"]);
    }
}
