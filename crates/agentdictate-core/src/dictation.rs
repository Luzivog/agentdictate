use std::{collections::BTreeSet, fmt, ops::Range, str::FromStr, sync::LazyLock};

use serde::{Deserialize, Serialize};

use crate::Settings;

mod symbols;

/// How a dictation is processed. `Literal` skips context hints and automatic
/// corrections (see [`normalize_transcript`]).
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
/// Sounds like entry that matched, or for a spoken symbol, a casing fix or a
/// number the text as heard (`dash dash parallel` for `--parallel`,
/// `agents.md` for `AGENTS.md`, `Wave one` for `Wave 1`). Jobs
/// and History store these as JSON in `vocabulary_corrections`; rows written
/// before schema version 3 used the retired Replacements names, which still
/// deserialize.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VocabularyCorrection {
    #[serde(alias = "source_phrase")]
    pub alias: String,
    #[serde(alias = "replacement_phrase")]
    pub spelling: String,
    pub count: usize,
}

/// A transcript after normalization, with the corrections that produced it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedText {
    pub text: String,
    pub corrections: Vec<VocabularyCorrection>,
}

/// Counts one applied rewrite of `alias` to `spelling`.
fn record_correction(corrections: &mut Vec<VocabularyCorrection>, alias: &str, spelling: &str) {
    if let Some(hit) = corrections
        .iter_mut()
        .find(|hit| hit.alias == alias && hit.spelling == spelling)
    {
        hit.count += 1;
    } else {
        corrections.push(VocabularyCorrection {
            alias: alias.to_owned(),
            spelling: spelling.to_owned(),
            count: 1,
        });
    }
}

/// The text a dictation delivers for what the model heard. Dictate mode runs
/// three passes, each on the previous one's output and outside
/// [`literal_ranges`]:
///
/// 1. Spoken symbols become the symbols (`symbols::normalize_spoken_symbols`):
///    "agents dot md" is `agents.md`. It runs first so the result still gets
///    its casing, and it leaves the matches of Sounds like entries to them.
/// 2. Vocabulary corrections ([`normalize_vocabulary`]): `AGENTS.md`.
/// 3. Spoken numbers as digits (`normalize_numbers`), outside the spellings
///    the vocabulary pass placed.
///
/// Literal mode returns the text as heard.
pub fn normalize_transcript(text: &str, options: &DictationOptions) -> NormalizedText {
    match options.mode {
        DictationMode::Literal => NormalizedText {
            text: text.to_owned(),
            corrections: Vec::new(),
        },
        DictationMode::Dictate => {
            let symbols = symbols::normalize_spoken_symbols(text, &options.vocabulary);
            let (vocabulary, spellings) = apply_vocabulary(&symbols.text, &options.vocabulary);
            let mut protected = literal_ranges(&vocabulary.text);
            protected.extend(spellings);
            let numbers = normalize_numbers(&vocabulary.text, &protected);
            let mut corrections = symbols.corrections;
            corrections.extend(vocabulary.corrections);
            corrections.extend(numbers.corrections);
            NormalizedText {
                text: numbers.text,
                corrections,
            }
        }
    }
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
/// [`literal_ranges`]; every spelling found claims its span, even unchanged.
/// Symbol spellings join words instead of standing alone (see
/// `symbol_rewrite_range`), and spoken "slash" is one even without an entry
/// (see `builtin_slash`); casing fixes skip ambiguous cases (see
/// `needs_casing_fix`). Dictations run it through [`normalize_transcript`].
pub fn normalize_vocabulary(text: &str, vocabulary: &[VocabularyEntry]) -> NormalizedText {
    apply_vocabulary(text, vocabulary).0
}

/// [`normalize_vocabulary`], plus the spans of the output that hold a
/// spelling it found, changed or not.
fn apply_vocabulary(
    text: &str,
    vocabulary: &[VocabularyEntry],
) -> (NormalizedText, Vec<Range<usize>>) {
    let protected = literal_ranges(text);
    let is_free = |range: &Range<usize>| {
        !protected
            .iter()
            .any(|r| range.start < r.end && range.end > r.start)
    };
    let mut rewrites = Vec::new();
    for entry in vocabulary.iter().chain(builtin_slash(vocabulary)) {
        let spelling = entry.spelling.as_str();
        let symbol = is_symbol_spelling(spelling);
        for alias in entry.aliases.iter().filter(|alias| !alias.is_empty()) {
            // A case-only alias, such as `codex` for `Codex`, fixes case and
            // so keeps the casing exemption for longer names: `leadlord.ai`.
            let case_only = alias.to_lowercase() == spelling.to_lowercase();
            let matches = whole_word_matches(text, alias)
                .filter(&is_free)
                .filter(|found| !(case_only && in_longer_name(text, found.clone(), spelling)));
            for found in matches {
                let range = if symbol {
                    // A join may take a comma before the word, which a
                    // protected span such as `apps/landing,` keeps.
                    match symbol_rewrite_range(text, found.clone(), spelling) {
                        Some(range) if is_free(&(range.start..found.start)) => range,
                        _ => continue,
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
    let mut corrections = Vec::new();
    let mut spellings = Vec::new();
    for rewrite in rewrites {
        if rewrite.range.start < cursor {
            continue;
        }
        output.push_str(&text[cursor..rewrite.range.start]);
        let start = output.len();
        output.push_str(rewrite.replacement);
        spellings.push(start..output.len());
        if text[rewrite.range.clone()] != *rewrite.replacement {
            record_correction(&mut corrections, rewrite.source, rewrite.replacement);
        }
        cursor = rewrite.range.end;
    }
    output.push_str(&text[cursor..]);
    let normalized = NormalizedText {
        text: output,
        corrections,
    };
    (normalized, spellings)
}

/// Words that number what follows them, matched without case: "wave one".
const NUMBER_LABELS: &[&str] = &[
    "step",
    "phase",
    "question",
    "option",
    "decision",
    "issue",
    "item",
    "lane",
    "wave",
    "tier",
    "level",
    "part",
    "section",
    "version",
    "round",
    "ticket",
    "task",
    "chapter",
    "page",
    "slide",
    "lecture",
    "module",
    "week",
    "stage",
    "milestone",
    "sprint",
    "plan",
    "case",
    "test",
    "pr",
];

/// Number words below twenty, at their value's index.
const SMALL_NUMBERS: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];

/// Tens from twenty, at index `value / 10 - 2`.
const TENS: [&str; 8] = [
    "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];

/// Writes spoken numbers as digits in two places the transcription model
/// leaves them as words. Words are whole and separated by single spaces.
///
/// - A word from `NUMBER_LABELS`, optionally followed by "number", then a
///   number from zero to ninety-nine: "Wave one" is "Wave 1", "question
///   twenty-one" is "question 21", "issue number four" is "issue number 4".
///   "dot" or "point" and another number make it a decimal: "version two dot
///   five" is "version 2.5"; before another word the label stays words.
/// - A run of three or more number words from zero to twenty, separated by
///   a space or ", ": "One two three" is "1 2 3", "two, three, four" is
///   "2, 3, 4". A run never starts on the second word of "forty four".
///
/// Everything else stays words: counts ("two minutes", "these two", "one
/// more"), a pair ("one two", "Test one two"), "issue one by one", a number
/// joined to a word by `-`, `.` or an apostrophe ("step one-liner"), and
/// anything overlapping `protected`. Each conversion is recorded with the
/// text as heard as its alias.
fn normalize_numbers(text: &str, protected: &[Range<usize>]) -> NormalizedText {
    static WORD: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"\w+").expect("word expression is valid"));
    let words: Vec<Range<usize>> = WORD.find_iter(text).map(|m| m.range()).collect();
    let word = |i: usize| &text[words[i].clone()];
    // The text between word `i` and the next one.
    let gap = |i: usize| &text[words[i].end..words[i + 1].start];
    // Whether word `i` exists and follows the previous word after one space.
    let spaced = |i: usize| i < words.len() && gap(i - 1) == " ";
    let free = |range: &Range<usize>| {
        let apostrophe = ['\'', '’'];
        !protected
            .iter()
            .any(|r| range.start < r.end && range.end > r.start)
            && !joined_to_word(text, range.clone())
            && !text[..range.start].ends_with(apostrophe)
            && !text[range.end..].starts_with(apostrophe)
    };
    let small = |i: usize| {
        SMALL_NUMBERS
            .iter()
            .position(|number| word(i).eq_ignore_ascii_case(number))
    };
    let tens = |i: usize| {
        TENS.iter()
            .position(|number| word(i).eq_ignore_ascii_case(number))
            .map(|index| 20 + 10 * index)
    };
    // The value of a run word, zero to twenty.
    let run_value = |i: usize| small(i).or(tens(i).filter(|&value| value == 20));
    // The end (exclusive) of the run of number words starting at word `i`,
    // which is empty on the unit of a compound such as "forty four".
    let run_end = |i: usize| {
        if i > 0 && tens(i - 1).is_some() && matches!(gap(i - 1), " " | "-") {
            return i;
        }
        let mut end = i;
        while end < words.len()
            && run_value(end).is_some()
            && free(&words[end])
            && (end == i || matches!(gap(end - 1), " " | ", "))
        {
            end += 1;
        }
        end
    };
    // The value of the number starting at word `i` and its last word.
    let number = |i: usize| {
        if let Some(value) = small(i) {
            return Some((value, i));
        }
        let value = tens(i)?;
        let unit = (i + 1 < words.len() && matches!(gap(i), " " | "-"))
            .then(|| small(i + 1))
            .flatten()
            .filter(|unit| (1..=9).contains(unit));
        Some(unit.map_or((value, i), |unit| (value + unit, i + 1)))
    };
    // A label at word `i` and its number: the span, its digits, and the
    // word after it. More numbers after it are a pair or a run, not a label.
    let labeled = |i: usize| {
        let is_label = NUMBER_LABELS
            .iter()
            .any(|label| word(i).eq_ignore_ascii_case(label));
        if !is_label {
            return None;
        }
        let mut k = i + 1;
        if spaced(k) && word(k).eq_ignore_ascii_case("number") {
            k += 1;
        }
        if !spaced(k) {
            return None;
        }
        let (value, mut last) = number(k)?;
        // "version two dot five" is "version 2.5". A spoken point before
        // anything but a number leaves the label as words, never "version 2
        // point something".
        let mut fraction = String::new();
        let point = spaced(last + 1)
            && (word(last + 1).eq_ignore_ascii_case("dot")
                || word(last + 1).eq_ignore_ascii_case("point"));
        if point && spaced(last + 2) {
            if let Some((part, part_last)) = number(last + 2) {
                fraction = format!(".{part}");
                last = part_last;
            } else if word(last + 2).chars().all(|c| c.is_ascii_digit()) {
                fraction = format!(".{}", word(last + 2));
                last += 2;
            } else {
                return None;
            }
        }
        let after = last + 1;
        let continues =
            after < words.len() && matches!(gap(last), " " | ", ") && run_value(after).is_some();
        let by_one = spaced(after)
            && word(after).eq_ignore_ascii_case("by")
            && spaced(after + 1)
            && small(after + 1).is_some();
        if continues || by_one {
            return None;
        }
        let span = words[i].start..words[last].end;
        free(&span).then(|| {
            let label = &text[words[i].start..words[k].start];
            let digits = format!("{label}{value}{fraction}");
            (span, digits, last + 1)
        })
    };

    let mut conversions = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let end = run_end(i);
        if end - i >= 3 {
            let mut digits = String::new();
            for k in i..end {
                if k > i {
                    digits.push_str(gap(k - 1));
                }
                digits.push_str(&run_value(k).expect("run words are numbers").to_string());
            }
            conversions.push((words[i].start..words[end - 1].end, digits));
            i = end;
        } else if let Some((span, digits, next)) = labeled(i) {
            conversions.push((span, digits));
            i = next;
        } else {
            i += 1;
        }
    }

    let mut output = String::new();
    let mut cursor = 0;
    let mut corrections = Vec::new();
    for (span, digits) in conversions {
        output.push_str(&text[cursor..span.start]);
        output.push_str(&digits);
        record_correction(&mut corrections, &text[span.clone()], &digits);
        cursor = span.end;
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

/// Spoken "slash" needs no Words entry: unless an entry already uses the word
/// "slash", the vocabulary pass behaves as if Words held `/ = slash`.
fn builtin_slash(vocabulary: &[VocabularyEntry]) -> Option<&'static VocabularyEntry> {
    static SLASH: LazyLock<VocabularyEntry> = LazyLock::new(|| VocabularyEntry {
        spelling: PATH_ROOT.to_owned(),
        aliases: vec!["slash".to_owned()],
    });
    let claimed = vocabulary.iter().any(|entry| {
        entry.spelling.eq_ignore_ascii_case("slash")
            || entry
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case("slash"))
    });
    (!claimed).then(|| &*SLASH)
}

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

/// Determiners, possessives and pronouns. A spoken symbol never joins one to
/// the next word ("slash our burn rate"), and no mailbox follows one ("the
/// team at leadlord.ai").
const DETERMINERS: &[&str] = &[
    "a", "all", "an", "any", "each", "every", "he", "her", "his", "i", "it", "its", "my", "no",
    "our", "she", "some", "that", "the", "their", "these", "they", "this", "those", "we", "you",
    "your",
];

/// Modal and helper words before a verb that is also a symbol's name: "we can
/// slash prices", "we must underscore safety".
const MODALS: &[&str] = &[
    "also", "can", "could", "just", "may", "might", "must", "really", "shall", "should", "will",
    "would",
];

/// Where a symbol spelling's alias match (`alias`) is replaced, swallowing the
/// spaces that the symbol joins, or `None` to leave the spoken word as it is.
/// The rule, with `/` spoken as "slash":
///
/// - The next word must follow on the same line after spaces, and must not
///   name the symbol ("slash command"). Otherwise the word stays, so a stray
///   "slash." never becomes " / ".
/// - The symbol never joins or starts a next word from `STOPWORDS` or
///   `DETERMINERS`, except that "and" and "or" may be joined ("and slash or"
///   is "and/or"), so "to slash our burn rate" stays.
/// - A comma right after the previous word goes, and the rules below apply
///   as if it were not there, except that `/` always joins: "code, slash
///   refactor" is "code/refactor", "delete, slash update" is
///   "delete/update".
/// - After a word from `SYMBOL_NAMERS` ("a slash", "forward slash"), `MODALS`
///   ("we can slash prices"), after "dot" ("dot slash", see the spoken
///   symbols pass) or after other punctuation ("PRs? slash do we") the word
///   stays.
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
    let mut before = text[..alias.start].trim_end_matches(is_inline_space);
    // The model often writes a pause before "slash" as a comma: "code, slash
    // refactor".
    let after_comma = before
        .strip_suffix(',')
        .filter(|rest| !trailing_word(rest).is_empty());
    if let Some(rest) = after_comma {
        before = rest;
    }
    let previous = trailing_word(before).to_lowercase();
    let next_is_stopword =
        STOPWORDS.contains(&next.as_str()) || DETERMINERS.contains(&next.as_str());
    let starts_path = spelling == PATH_ROOT && !next_is_stopword;
    if previous.is_empty() {
        let line_start = before.is_empty() || before.ends_with(['\n', '\r']);
        return (line_start && starts_path).then_some(alias.start..end);
    }
    if SYMBOL_NAMERS.contains(&previous.as_str())
        || MODALS.contains(&previous.as_str())
        || previous == "dot"
    {
        return None;
    }
    if after_comma.is_none() && PATH_LEADERS.contains(&previous.as_str()) {
        return starts_path.then_some(alias.start..end);
    }
    let joins = !next_is_stopword || matches!(next.as_str(), "and" | "or");
    joins.then_some(before.len()..end)
}

/// Whether the whole-word, case-insensitive occurrence of `spelling` at
/// `found` should be rewritten to it. It stays when:
///
/// - It is part of a longer name (see [`in_longer_name`]): `openai.rs`,
///   `agentdictate-core`, `.codex`, `hello@leadlord.ai`.
/// - It differs only by capital initials on lowercase letters of the
///   spelling, as at a sentence start or in a title: "Read-only" and
///   "Read-Only Mode" stay for `read-only`.
/// - The spelling is one word of letters cased like an ordinary word, its
///   lowercase form is a common English word (`Rust`, `Go`, `IT`; see
///   [`is_common_english_word`]), and the occurrence is lowercase or
///   capitalized, so it may be that word ("rust", "go", "It's"). A Sounds
///   like entry such as `rust` opts into that fix. Other plain words, such as
///   `Codex`, are fixed: "the codex config" becomes "the Codex config".
fn needs_casing_fix(text: &str, found: Range<usize>, spelling: &str) -> bool {
    let heard = &text[found.clone()];
    if heard == spelling
        || in_longer_name(text, found, spelling)
        || only_capital_initials(heard, spelling)
    {
        return false;
    }
    let may_be_common = is_plain_word(spelling) && is_common_english_word(&spelling.to_lowercase());
    !(may_be_common && (is_lowercase(heard) || is_capitalized(heard)))
}

/// True for a lowercase word in the bundled list of common English words
/// (SCOWL size 35; see `data/README.md`).
fn is_common_english_word(word: &str) -> bool {
    static WORDS: LazyLock<std::collections::HashSet<&'static str>> = LazyLock::new(|| {
        include_str!("../data/common-english-words.txt")
            .lines()
            .collect()
    });
    WORDS.contains(word)
}

/// One word of letters cased like an ordinary word: `Rust`, `IT`, `pnpm`.
fn is_plain_word(spelling: &str) -> bool {
    spelling.chars().all(char::is_alphabetic)
        && (is_lowercase(spelling) || is_uppercase(spelling) || is_capitalized(spelling))
}

/// True when the occurrence of `spelling` at `range` is part of a longer name
/// whose case it must keep: it is [`joined_to_word`] (`openai.rs`,
/// `agentdictate-core`), or `spelling` is a plain word and a `.` starts it
/// (`.codex`) or an `@` touches it (`hello@leadlord.ai`, `@codex`). Names with
/// their own punctuation are still fixed there: `@agents.md` is `@AGENTS.md`.
/// `_` needs no rule: it is a word character, so `agent_name` never holds
/// `agent` as a whole word.
fn in_longer_name(text: &str, range: Range<usize>, spelling: &str) -> bool {
    let before = text[..range.start].chars().next_back();
    let after = text[range.end..].chars().next();
    joined_to_word(text, range)
        || (is_plain_word(spelling) && (matches!(before, Some('.' | '@')) || after == Some('@')))
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
    fn spoken_slash_after_a_comma_joins_as_without_it() {
        for (heard, expected) in [
            (
                "If by the same time you can clean up code, slash refactor, slash delete some custom machinery, do it.",
                "If by the same time you can clean up code/refactor/delete some custom machinery, do it.",
            ),
            // "delete" would start a path without the comma.
            (
                "what should we remove, delete, slash update to fix these issues?",
                "what should we remove, delete/update to fix these issues?",
            ),
            (
                "Are there Bun caches, Turbo caches, slash other type of caches?",
                "Are there Bun caches, Turbo caches/other type of caches?",
            ),
            (
                "Are there native ways, slash frameworks, slash ways from existing libraries",
                "Are there native ways/frameworks/ways from existing libraries",
            ),
        ] {
            // The built-in slash and a Words entry share the rule.
            for words in ["", "/ = slash"] {
                assert_eq!(normalized(heard, words), expected, "{words:?}");
            }
        }
    }

    #[test]
    fn spoken_slash_stays_a_word_when_it_names_the_symbol_or_cannot_join() {
        for text in [
            "PRs? slash do we have any rules",
            "posted, slash can you access it?",
            "We need to cut costs, slash the budget",
            "what do we need to update slash do to merge it?",
            "see apps/landing, slash docs",
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
            for words in ["", "/ = slash"] {
                let result = normalize_vocabulary(text, &vocabulary(words));
                assert_eq!(result.text, text, "{words:?}");
                assert!(result.corrections.is_empty(), "{text}");
            }
        }
    }

    #[test]
    fn spoken_slash_stays_after_dot_modals_and_before_determiners() {
        for text in [
            "We need to slash our burn rate",
            "we can slash prices",
            "we should slash every budget",
            "the yellow dot slash green",
        ] {
            assert_eq!(normalized(text, "/ = slash"), text);
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
        let words = "read-only\nkubectl\nRust\nIT\nEffect\nGo\nAGENTS.md";
        let text = "Read-only. Kubectl works. It's rust, a side effect. Go on. myagents.mdx";
        assert_eq!(normalized(text, words), text);
        assert_eq!(
            normalized("RUST and READ-ONLY", words),
            "Rust and read-only"
        );
        // A lowercase Sounds like entry opts a common word into the fix.
        assert_eq!(normalized("use rust", "Rust = rust"), "use Rust");
    }

    #[test]
    fn casing_fixes_names_that_are_not_common_words() {
        let words = "Codex\nLeadlord\nAGENTS.md\nAgentDictate";
        assert_eq!(
            normalized(
                "the codex config, the leadlord vault, leadlord GitHub organization",
                words
            ),
            "the Codex config, the Leadlord vault, Leadlord GitHub organization"
        );
        let names = "leadlord.ai hello@leadlord.ai .codex @codex agentdictate-core";
        assert_eq!(normalized(names, words), names);
        // Spellings with their own punctuation are still fixed after `.` or `@`.
        assert_eq!(
            normalized("import @agents.md and .agents.md", words),
            "import @AGENTS.md and .AGENTS.md"
        );
        let result = normalize_vocabulary("codex and codex", &vocabulary(words));
        assert_eq!(
            result.corrections,
            [VocabularyCorrection {
                alias: "codex".into(),
                spelling: "Codex".into(),
                count: 2,
            }]
        );
    }

    #[test]
    fn case_only_aliases_skip_longer_names_but_other_aliases_join() {
        let words = "Leadlord = leadlord\nsub-worktrees = sub-work trees";
        assert_eq!(
            normalized("leadlord at leadlord.ai and @leadlord", words),
            "Leadlord at leadlord.ai and @leadlord"
        );
        assert_eq!(normalized("the sub-work trees", words), "the sub-worktrees");
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

    fn options(mode: DictationMode, vocabulary_text: &str) -> DictationOptions {
        DictationOptions {
            mode,
            language: String::new(),
            context: String::new(),
            vocabulary: vocabulary(vocabulary_text),
        }
    }

    fn dictated(text: &str) -> String {
        normalize_transcript(text, &options(DictationMode::Dictate, "")).text
    }

    #[test]
    fn a_label_keeps_its_words_and_numbers_its_number() {
        for (heard, expected) in [
            (
                "Okay, go to wave two and then wave three.",
                "Okay, go to wave 2 and then wave 3.",
            ),
            (
                "Do step one, then step two, and skip step five.",
                "Do step 1, then step 2, and skip step 5.",
            ),
            (
                "Question 2. Yes. Question three, no.",
                "Question 2. Yes. Question 3, no.",
            ),
            ("Wave one", "Wave 1"),
            ("question twenty-one", "question 21"),
            ("question twenty one", "question 21"),
            ("issue number four", "issue number 4"),
            ("option two is better", "option 2 is better"),
            ("part one of three", "part 1 of three"),
            ("PR ninety", "PR 90"),
            ("version two dot five", "version 2.5"),
            ("step one dot two", "step 1.2"),
            ("version two point twenty five", "version 2.25"),
            ("version three point 1", "version 3.1"),
        ] {
            assert_eq!(dictated(heard), expected);
        }
    }

    #[test]
    fn runs_of_three_or_more_become_digits_with_their_separators() {
        for (heard, expected) in [
            ("One two three one two three.", "1 2 3 1 2 3."),
            ("two, three, four", "2, 3, 4"),
            ("Test one two one two", "Test 1 2 1 2"),
            ("count to twenty, nineteen, eighteen", "count to 20, 19, 18"),
            (
                "It's plus forty four, seven three five.",
                "It's plus forty four, 7 3 5.",
            ),
        ] {
            assert_eq!(dictated(heard), expected);
        }
    }

    #[test]
    fn counts_pairs_joined_words_and_ordinary_phrases_stay_words() {
        for text in [
            "one two",
            "No one said one of these two takes two minutes, one second.",
            "The one thing: one more time, at one point, day one.",
            "someone, anyone, often, a tone",
            "step one-liner, step one's, multi-step one",
            "Test one two. Test, one two.",
            "every single issue one by one",
            "step 3 and version 3.2",
            "twenty-one two three",
            "step one point is clear",
            "two dot five seconds",
        ] {
            let result = normalize_transcript(text, &options(DictationMode::Dictate, ""));
            assert_eq!(result.text, text);
            assert!(result.corrections.is_empty(), "{text}");
        }
    }

    #[test]
    fn numbers_skip_protected_spans_and_vocabulary_spellings() {
        for text in ["`step one`", "\"one two three\"", "see /docs/step one"] {
            assert_eq!(dictated(text), text);
        }
        let vocabulary_options = options(DictationMode::Dictate, "Phase One = face one");
        assert_eq!(
            normalize_transcript("Ship face one, then phase two.", &vocabulary_options).text,
            "Ship Phase One, then phase 2."
        );
    }

    #[test]
    fn literal_mode_delivers_the_text_as_heard() {
        let text = "wave two, one two three, the codex";
        let result = normalize_transcript(text, &options(DictationMode::Literal, "Codex = codex"));
        assert_eq!(result.text, text);
        assert!(result.corrections.is_empty());
    }

    #[test]
    fn number_conversions_are_recorded_as_heard() {
        let result = normalize_transcript(
            "the codex: Wave one. Wave one. One two three.",
            &options(DictationMode::Dictate, "Codex = codex"),
        );
        assert_eq!(result.text, "the Codex: Wave 1. Wave 1. 1 2 3.");
        let correction = |alias: &str, spelling: &str, count| VocabularyCorrection {
            alias: alias.into(),
            spelling: spelling.into(),
            count,
        };
        assert_eq!(
            result.corrections,
            [
                correction("codex", "Codex", 1),
                correction("Wave one", "Wave 1", 2),
                correction("One two three", "1 2 3", 1),
            ]
        );
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
