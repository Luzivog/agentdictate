//! Built-in spoken technical syntax, so common symbols need no Words entry.
//! See [`normalize_spoken_symbols`] for the rules.

use std::{collections::HashSet, ops::Range, sync::LazyLock};

use super::{
    DETERMINERS, MODALS, NormalizedText, PATH_LEADERS, STOPWORDS, SYMBOL_NAMERS, SYMBOL_NOUNS,
    VocabularyEntry, is_capitalized, is_common_english_word, is_inline_space, is_symbol_spelling,
    literal_ranges, record_correction, whole_word_matches,
};

/// Last labels that make a dotted name a web domain, which is lowercased.
/// English words such as "net", "app" or "me" are left out: "the dot net".
const DOMAIN_ENDINGS: &[&str] = &[
    "ai", "ca", "co", "com", "dev", "fr", "gg", "io", "org", "uk", "xyz",
];

/// Last labels that make a dotted name a file name. English words such as
/// "go", "log", "lock" or "swift" are left out: "make the dot go away".
const FILE_EXTENSIONS: &[&str] = &[
    "cfg", "cjs", "conf", "cpp", "cs", "css", "csv", "gif", "gz", "hpp", "html", "ini", "java",
    "jpeg", "jpg", "js", "json", "jsonc", "jsx", "kt", "md", "mdx", "mjs", "mp3", "mp4", "pdf",
    "php", "png", "py", "rb", "rs", "scss", "sh", "sql", "svg", "toml", "ts", "tsx", "txt", "wav",
    "webm", "webp", "yaml", "yml", "zsh",
];

/// Names that a spoken leading "dot" turns into a dotfile: "the dot env file".
const DOTFILES: &[&str] = &[
    "claude",
    "codex",
    "dockerignore",
    "editorconfig",
    "env",
    "eslintrc",
    "git",
    "gitattributes",
    "github",
    "gitignore",
    "npmrc",
    "nvmrc",
    "prettierrc",
    "ssh",
    "vscode",
];

/// Words after "<word> dot com" that make "dot com" the era, not a domain.
const DOT_COM_NOUNS: &[&str] = &[
    "boom",
    "bubble",
    "companies",
    "company",
    "crash",
    "days",
    "era",
    "startup",
    "startups",
];

/// Mailbox names that are rarely anything else, so "hello at leadlord.ai"
/// needs no other cue. Ordinary nouns such as "team" or "support" need one.
const ADDRESS_NAMES: &[&str] = &[
    "admin",
    "contact",
    "hello",
    "hi",
    "info",
    "noreply",
    "postmaster",
    "webmaster",
];

/// Words earlier in the sentence that make "<name> at <domain>" an address:
/// "send it to team at leadlord.ai", "write to james at leadlord.ai".
const EMAIL_CUES: &[&str] = &[
    "account", "accounts", "address", "cc", "contact", "e-mail", "email", "emailed", "emails",
    "forward", "inbox", "mail", "mailbox", "message", "send", "sent", "write",
];

/// Words right after an address that name it: "hello at leadlord.ai account".
const ADDRESS_NOUNS: &[&str] = &["account", "address", "e-mail", "email", "inbox"];

/// Words before "at <domain>" that place something on a site rather than
/// name a mailbox: "look at leadlord.ai", "I work at leadlord.ai".
const NOT_MAILBOXES: &[&str] = &[
    "available",
    "check",
    "checked",
    "deployed",
    "found",
    "go",
    "goes",
    "her",
    "him",
    "home",
    "hosted",
    "launched",
    "live",
    "lives",
    "located",
    "log",
    "logged",
    "look",
    "looked",
    "looking",
    "looks",
    "me",
    "now",
    "online",
    "open",
    "opened",
    "out",
    "page",
    "point",
    "pointed",
    "pointing",
    "points",
    "published",
    "redirect",
    "redirected",
    "redirects",
    "running",
    "served",
    "sign",
    "signed",
    "site",
    "stuck",
    "them",
    "up",
    "us",
    "visit",
    "website",
    "went",
    "work",
    "worked",
    "working",
    "works",
];

/// Words before "the A P I" that let a spelled run start with "A" or "I".
const RUN_ARTICLES: &[&str] = &["my", "our", "that", "the", "their", "this", "your"];

/// Commands that take short flags even though they are common words.
const COMMANDS: &[&str] = &[
    "bun", "cargo", "cat", "cd", "chmod", "cp", "curl", "docker", "git", "grep", "kill", "ls",
    "mkdir", "mv", "node", "npm", "pnpm", "ps", "python", "rm", "ssh", "tar",
];

/// How many words before a mailbox an email cue may be, within its sentence.
const EMAIL_CUE_WINDOW: usize = 5;

/// Writes spoken technical syntax as the symbols it names. Dictate mode runs
/// it before vocabulary corrections (see [`super::normalize_transcript`]).
/// Words are separated by spaces on one line, and a word may already hold `.`
/// or `-` (`leadlord.ai`), so "two-dot" never holds the spoken "dot". The
/// rules, tried in this order at each word:
///
/// - **Relative paths.** "dot slash install dot sh" is `./install.sh`, and
///   "dot dot slash dot dot slash src" is `../../src`.
/// - **Flags.** "dash dash parallel" is `--parallel`, and "dash dash force
///   dash with dash lease" is `--force-with-lease`. The name has two or more
///   characters, is lowercased if capitalized, and does not end on a small
///   word ("dash dash watch dash and then" is `--watch dash and then`). A
///   "dash" hyphenated to a word of the name is the spoken word: "dash dash
///   no-dash isolate" is `--no-isolate`.
/// - **Dotfiles.** At a line start or after a word such as "the", "in", "a" or
///   "and", "dot" before a name in `DOTFILES` starts it, and further "dot"
///   parts join it: "the dot env dot local file" is "the .env.local file".
/// - **Email addresses.** "hello at leadlord dot ai" or "hello at
///   leadlord.ai" is `hello@leadlord.ai`, lowercased, when the domain ends in
///   `DOMAIN_ENDINGS` and one of these holds: the mailbox is in
///   `ADDRESS_NAMES`, an `EMAIL_CUES` word comes earlier in the sentence (at
///   most five words back), or an `ADDRESS_NOUNS` word follows. After a word
///   from `DETERMINERS` only a name from `ADDRESS_NAMES` followed by an
///   `ADDRESS_NOUNS` word qualifies ("the hello at leadlord.ai account", but
///   not "the team at leadlord.ai"). The mailbox is not a small word, a word
///   from `NOT_MAILBOXES`, part of a contraction ("we're"), or after "plus".
/// - **Dotted names.** "agents dot md" is `agents.md` and "name dot sites dot
///   leadlord dot ai" is `name.sites.leadlord.ai`, when the last name ends in
///   `DOMAIN_ENDINGS` (lowercased whole) or `FILE_EXTENSIONS` (extension
///   lowercased: "Next dot JS" is `Next.js`). No part is a small word, and the
///   first is not a word such as "the", "use" or "a". A domain whose name is
///   only common English words stays words unless Words spells one of them
///   ("the early dot com days"), as does any domain before a `DOT_COM_NOUNS`
///   word ("dot com bubble"). A common word capitalized only by a sentence
///   start is lowercased: "Package dot json" is `package.json`, but `.js`
///   names keep it (`Node.js`).
/// - **Underscores.** "snake underscore case" is `snake_case`, with the same
///   limits as a dotted name, never after a word from `MODALS` or
///   `DETERMINERS` or a contraction ("we must underscore safety", "I'd
///   underscore"), and not before a word that names the symbol ("underscore
///   key").
/// - **Short flags.** "ls dash l" is `ls -l`: one letter other than "a" or
///   "i", after a lowercase word that is in `COMMANDS` or is not a common
///   English word ("em dash a model" stays).
/// - **Spelled letters.** Three or more capital letters separated by single
///   spaces are one word: "T O K S" is `TOKS`. A run starts with "A" or "I"
///   only after a word from `RUN_ARTICLES` ("so I A B tested" stays). A final
///   "I" before a lowercase word is the pronoun ("B C D I think" is "BCD I
///   think"), but "use the C L I." is "use the CLI.". A run followed by
///   "or"/"and" and another letter is a list of options and stays. Hyphenated
///   spellings ("Z-E-R-N-I-O") already read as spelled and stay.
///
/// Spoken "slash" is not here: the vocabulary pass handles it like a `/ =
/// slash` Words entry (see `super::builtin_slash`).
///
/// Everything else stays words, including "dot dot dot", "the yellow dot",
/// "a dash", "Plan B I think" and "I'm at the office". Nothing changes inside
/// [`literal_ranges`] or a match of a spelling of several words or of a Sounds
/// like entry, so Words entries keep deciding their own text. Two kinds of
/// entry do not claim their words here: one that only fixes case (`leadlord`
/// for `Leadlord`) and one for a symbol (`- = dash`), which the vocabulary
/// pass applies to whatever these rules leave. Each rewrite is recorded with
/// the text as heard as its alias.
pub(super) fn normalize_spoken_symbols(
    text: &str,
    vocabulary: &[VocabularyEntry],
) -> NormalizedText {
    let mut protected = literal_ranges(text);
    for entry in vocabulary
        .iter()
        .filter(|e| !is_symbol_spelling(&e.spelling))
    {
        let aliases = entry.aliases.iter().filter(|alias| {
            !alias.trim().is_empty() && alias.to_lowercase() != entry.spelling.to_lowercase()
        });
        let phrases = entry
            .spelling
            .contains(char::is_whitespace)
            .then_some(&entry.spelling);
        for term in aliases.chain(phrases) {
            protected.extend(whole_word_matches(text, term));
        }
    }
    let spellings = vocabulary
        .iter()
        .map(|entry| entry.spelling.to_lowercase())
        .collect();
    let words = Words::new(text, protected, spellings);
    let rules: [fn(&Words, usize) -> Option<Conversion>; 8] = [
        relative_path,
        flag,
        dotfile,
        email,
        dotted,
        underscored,
        short_flag,
        spelled_letters,
    ];
    let mut conversions = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let found = rules
            .iter()
            .find_map(|rule| rule(&words, i).filter(|conversion| words.free(conversion)));
        match found {
            Some(conversion) => {
                i = conversion.last + 1;
                conversions.push(conversion);
            }
            None => i += 1,
        }
    }

    let mut output = String::new();
    let mut cursor = 0;
    let mut corrections = Vec::new();
    for conversion in conversions {
        let span = words.span(&conversion);
        output.push_str(&text[cursor..span.start]);
        output.push_str(&conversion.replacement);
        record_correction(
            &mut corrections,
            &text[span.clone()],
            &conversion.replacement,
        );
        cursor = span.end;
    }
    output.push_str(&text[cursor..]);
    NormalizedText {
        text: output,
        corrections,
    }
}

/// Words `first` through `last` (inclusive) become `replacement`.
struct Conversion {
    first: usize,
    last: usize,
    replacement: String,
}

/// The words of a transcript: runs of word characters, joined by single `.`
/// or `-` (`leadlord.ai`, `dry-run`).
struct Words<'a> {
    text: &'a str,
    ranges: Vec<Range<usize>>,
    protected: Vec<Range<usize>>,
    /// The lowercase spellings from Words.
    spellings: HashSet<String>,
}

impl<'a> Words<'a> {
    fn new(text: &'a str, protected: Vec<Range<usize>>, spellings: HashSet<String>) -> Self {
        static WORD: LazyLock<regex::Regex> = LazyLock::new(|| {
            regex::Regex::new(r"\w+(?:[.-]\w+)*").expect("word expression is valid")
        });
        let ranges = WORD.find_iter(text).map(|found| found.range()).collect();
        Self {
            text,
            ranges,
            protected,
            spellings,
        }
    }

    fn len(&self) -> usize {
        self.ranges.len()
    }

    /// Word `i`, or "" past the end.
    fn word(&self, i: usize) -> &'a str {
        self.ranges
            .get(i)
            .map_or("", |range| &self.text[range.clone()])
    }

    fn lower(&self, i: usize) -> String {
        self.word(i).to_lowercase()
    }

    /// Whether word `i` is `expected`, ignoring case.
    fn is(&self, i: usize, expected: &str) -> bool {
        self.word(i).eq_ignore_ascii_case(expected)
    }

    /// Whether word `i` is in `list`, ignoring case.
    fn is_in(&self, i: usize, list: &[&str]) -> bool {
        list.contains(&self.lower(i).as_str())
    }

    /// The text between word `i` and the next one.
    fn gap(&self, i: usize) -> &'a str {
        &self.text[self.ranges[i].end..self.ranges[i + 1].start]
    }

    /// The text between the previous word (or the start) and word `i`.
    fn gap_before(&self, i: usize) -> &'a str {
        let start = i.checked_sub(1).map_or(0, |k| self.ranges[k].end);
        &self.text[start..self.ranges[i].start]
    }

    /// Whether a word follows word `i` after spaces on the same line.
    fn spaced(&self, i: usize) -> bool {
        i + 1 < self.len() && {
            let gap = self.gap(i);
            !gap.is_empty() && gap.chars().all(is_inline_space)
        }
    }

    /// Whether word `i` is followed by `expected` and then another word,
    /// with spaces between them: "dot" in "agents dot md".
    fn links(&self, i: usize, expected: &str) -> bool {
        self.spaced(i) && self.is(i + 1, expected) && self.spaced(i + 1)
    }

    /// Whether word `i` comes right after another word on its line, and that
    /// word is in `list`.
    fn follows(&self, i: usize, list: &[&str]) -> bool {
        i > 0 && self.spaced(i - 1) && self.is_in(i - 1, list)
    }

    /// Whether word `i` is part of a contraction: "re" in "we're".
    fn in_contraction(&self, i: usize) -> bool {
        let apostrophes = ['\'', '’'];
        self.text[..self.ranges[i].start].ends_with(apostrophes)
            || self.text[self.ranges[i].end..].starts_with(apostrophes)
    }

    /// Whether word `i` starts a line or a sentence.
    fn starts_sentence(&self, i: usize) -> bool {
        let before = self.gap_before(i).trim_end();
        (i == 0 && before.is_empty())
            || before.ends_with(['.', '!', '?', '\n'])
            || self.gap_before(i).contains('\n')
    }

    fn span(&self, conversion: &Conversion) -> Range<usize> {
        self.ranges[conversion.first].start..self.ranges[conversion.last].end
    }

    fn free(&self, conversion: &Conversion) -> bool {
        let span = self.span(conversion);
        !self
            .protected
            .iter()
            .any(|range| span.start < range.end && span.end > range.start)
    }
}

/// "dot slash install dot sh" is `./install.sh`; "dot dot slash dot dot
/// slash src" is `../../src`; "dot slash scripts slash build" is
/// `./scripts/build`.
fn relative_path(words: &Words, i: usize) -> Option<Conversion> {
    if i > 0 && words.spaced(i - 1) && words.is(i - 1, "dot") {
        return None;
    }
    let mut prefix = String::new();
    let mut k = i;
    loop {
        if words.is(k, "dot") && words.links(k, "slash") {
            prefix.push_str("./");
            k += 2;
        } else if words.is(k, "dot") && words.links(k, "dot") && words.links(k + 1, "slash") {
            prefix.push_str("../");
            k += 3;
        } else {
            break;
        }
    }
    if prefix.is_empty() || !is_name_part(words, k) {
        return None;
    }
    let mut path = format!("{prefix}{}", words.word(k));
    let mut last = k;
    while words.links(last, "slash") && is_name_part(words, last + 2) {
        path.push('/');
        path.push_str(words.word(last + 2));
        last += 2;
    }
    if let Some((end, Ending::File)) = dotted_chain(words, last) {
        path.truncate(path.len() - words.word(last).len());
        path.push_str(&joined(words, last, end, '.', Ending::File));
        last = end;
    }
    Some(Conversion {
        first: i,
        last,
        replacement: path,
    })
}

/// "dash dash parallel" is `--parallel`; "dash dash no dash verify" is
/// `--no-verify`. "dash dash" only names a flag, so small words may be part
/// of the name, but not end it. A "dash" hyphenated to a word of the name is
/// the spoken word too: "dash dash no-dash isolate" is `--no-isolate`, and
/// "dash dash dry dash-run" is `--dry-run`.
fn flag(words: &Words, i: usize) -> Option<Conversion> {
    if !(words.is(i, "dash") && words.links(i, "dash")) {
        return None;
    }
    let first = FlagWord::new(words.word(i + 2)).filter(|word| !word.dash_before)?;
    let mut name = lowercase_if_capitalized(&first.name);
    let (mut k, mut current) = (i + 2, first);
    // The last word that may end the name, and the name's length there.
    let mut last = None;
    loop {
        if !current.dash_after && !STOPWORDS.contains(&current.name.to_lowercase().as_str()) {
            last = Some((k, name.len()));
        }
        // Word `n` continues the name if it is lowercase and starts with a
        // hyphenated "dash" exactly when no other dash came before it.
        let part = |n: usize, dash_before: bool| {
            FlagWord::new(words.word(n))
                .filter(|word| word.dash_before == dash_before && words.word(n) == words.lower(n))
                .map(|word| (n, word))
        };
        let next = if current.dash_after {
            words.spaced(k).then(|| part(k + 1, false)).flatten()
        } else if words.links(k, "dash") {
            part(k + 2, false)
        } else if words.spaced(k) {
            part(k + 1, true)
        } else {
            None
        };
        let Some((n, word)) = next else {
            break;
        };
        name.push('-');
        name.push_str(&word.name);
        (k, current) = (n, word);
    }
    let (last, length) = last?;
    name.truncate(length);
    (length >= 2).then(|| Conversion {
        first: i,
        last,
        replacement: format!("--{name}"),
    })
}

/// A word of a flag's name without the "dash" parts the model sometimes
/// hyphenates to it: "no-dash" is `no` with a dash after it.
struct FlagWord {
    name: String,
    dash_before: bool,
    dash_after: bool,
}

impl FlagWord {
    /// `None` unless the word is ASCII letters, digits and `-`, and what is
    /// left without its "dash" parts starts with a letter.
    fn new(word: &str) -> Option<Self> {
        if !word.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return None;
        }
        let is_dash = |part: &&str| part.eq_ignore_ascii_case("dash");
        let parts: Vec<&str> = word.split('-').collect();
        let name = parts
            .iter()
            .filter(|part| !is_dash(part))
            .copied()
            .collect::<Vec<_>>()
            .join("-");
        name.starts_with(|c: char| c.is_ascii_alphabetic())
            .then(|| Self {
                dash_before: parts.first().is_some_and(is_dash),
                dash_after: parts.last().is_some_and(is_dash),
                name,
            })
    }
}

/// "the dot env file" is "the .env file"; "dot env dot local" is
/// `.env.local`; "dot eslintrc dot json" is `.eslintrc.json`.
fn dotfile(words: &Words, i: usize) -> Option<Conversion> {
    let before = words.gap_before(i);
    let line_start = before.chars().all(char::is_whitespace) && (i == 0 || before.contains('\n'));
    let after_leader =
        words.follows(i, PATH_LEADERS) || words.follows(i, &["a", "an", "and", "or"]);
    if !(words.is(i, "dot")
        && (line_start || after_leader)
        && words.spaced(i)
        && words.is_in(i + 1, DOTFILES))
    {
        return None;
    }
    let mut name = format!(".{}", words.lower(i + 1));
    let mut last = i + 1;
    while words.links(last, "dot") && is_name_part(words, last + 2) {
        let part = words.word(last + 2);
        if part != part.to_lowercase() && ending(part).is_none() {
            break;
        }
        name.push('.');
        name.push_str(&part.to_lowercase());
        last += 2;
    }
    Some(Conversion {
        first: i,
        last,
        replacement: name,
    })
}

/// "hello at leadlord dot ai" is `hello@leadlord.ai`.
fn email(words: &Words, i: usize) -> Option<Conversion> {
    let word = words.word(i);
    let is_mailbox = word.len() >= 2
        && word.starts_with(|c: char| c.is_ascii_alphabetic())
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        && !words.is_in(i, STOPWORDS)
        && !words.is_in(i, NOT_MAILBOXES)
        && !words.in_contraction(i);
    let after_plus = i > 0 && words.is(i - 1, "plus");
    if !is_mailbox || after_plus || !words.links(i, "at") {
        return None;
    }
    let start = i + 2;
    let (last, domain) = match dotted_name(words, start) {
        Some((last, Ending::Domain)) => (last, joined(words, start, last, '.', Ending::Domain)),
        Some(_) => return None,
        None if ending(words.word(start)) == Some(Ending::Domain)
            && words.word(start).contains('.') =>
        {
            (start, words.lower(start))
        }
        None => return None,
    };
    let address_name = words.is_in(i, ADDRESS_NAMES);
    let named_after = words.spaced(last) && words.is_in(last + 1, ADDRESS_NOUNS);
    let qualifies = if words.follows(i, DETERMINERS) {
        address_name && named_after
    } else {
        address_name || named_after || cued_before(words, i)
    };
    qualifies.then(|| Conversion {
        first: i,
        last,
        replacement: format!("{}@{domain}", word.to_lowercase()),
    })
}

/// Whether an `EMAIL_CUES` word comes at most `EMAIL_CUE_WINDOW` words
/// before word `i`, in the same sentence.
fn cued_before(words: &Words, i: usize) -> bool {
    for k in (i.saturating_sub(EMAIL_CUE_WINDOW)..i).rev() {
        if words.gap(k).contains(['.', '!', '?', '\n']) {
            return false;
        }
        if words.is_in(k, EMAIL_CUES) {
            return true;
        }
    }
    false
}

/// "agents dot md" is `agents.md`; "Leadlord dot AI" is `leadlord.ai`.
fn dotted(words: &Words, i: usize) -> Option<Conversion> {
    let (last, ending) = dotted_name(words, i)?;
    let mut name = joined(words, i, last, '.', ending);
    if ending == Ending::Domain {
        // Every label but the top-level domain: "early" in "early dot com".
        let labels: Vec<String> = name.split('.').map(str::to_owned).collect();
        let name_labels = &labels[..labels.len() - 1];
        let ordinary = name_labels
            .iter()
            .all(|label| is_common_english_word(label) && !words.spellings.contains(label));
        let era = words.spaced(last) && words.is_in(last + 1, DOT_COM_NOUNS);
        if ordinary || era {
            return None;
        }
    }
    let first = words.word(i);
    let sentence_case = ending == Ending::File
        && words.starts_sentence(i)
        && is_capitalized(first)
        && is_common_english_word(&first.to_lowercase())
        && !words.spellings.contains(&first.to_lowercase())
        && !name.to_lowercase().ends_with(".js");
    if sentence_case {
        name = format!("{}{}", first.to_lowercase(), &name[first.len()..]);
    }
    Some(Conversion {
        first: i,
        last,
        replacement: name,
    })
}

/// The last word and ending of a dotted name starting at word `i`, whose
/// first word may start a name.
fn dotted_name(words: &Words, i: usize) -> Option<(usize, Ending)> {
    let starts =
        is_name_part(words, i) && !words.is_in(i, PATH_LEADERS) && !words.is_in(i, SYMBOL_NAMERS);
    starts.then(|| dotted_chain(words, i)).flatten()
}

/// The longest chain "word dot word dot …" after word `i` whose last word
/// has a known ending, as that word's index and the ending.
fn dotted_chain(words: &Words, i: usize) -> Option<(usize, Ending)> {
    let mut best = None;
    let mut k = i;
    while words.links(k, "dot") && is_name_part(words, k + 2) {
        k += 2;
        if let Some(ending) = ending(words.word(k)) {
            best = Some((k, ending));
        }
    }
    best
}

/// "snake underscore case" is `snake_case`.
fn underscored(words: &Words, i: usize) -> Option<Conversion> {
    let is_part = |k: usize| {
        is_name_part(words, k)
            && !words.word(k).contains(['.', '-'])
            && !words.is(k, "underscore")
            && !words.in_contraction(k)
            && !words.is_in(k, SYMBOL_NAMERS)
            && !words.is_in(k, SYMBOL_NOUNS)
            && !words.is_in(k, MODALS)
            && !words.is_in(k, DETERMINERS)
    };
    if !is_part(i) || words.is_in(i, PATH_LEADERS) {
        return None;
    }
    let mut last = i;
    while words.links(last, "underscore") && is_part(last + 2) {
        last += 2;
    }
    (last > i).then(|| Conversion {
        first: i,
        last,
        replacement: joined(words, i, last, '_', Ending::AsHeard),
    })
}

/// "ls dash l" is "ls -l".
fn short_flag(words: &Words, i: usize) -> Option<Conversion> {
    let command = words.word(i);
    let is_command = !command.is_empty()
        && command
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && (words.is_in(i, COMMANDS) || !is_common_english_word(command))
        && !words.is_in(i, STOPWORDS)
        && !words.is(i, "dash");
    let letter = words.word(i + 2);
    let is_letter = letter.len() == 1
        && letter.chars().all(|c| c.is_ascii_alphabetic())
        && !letter.eq_ignore_ascii_case("a")
        && !letter.eq_ignore_ascii_case("i");
    (is_command && is_letter && words.links(i, "dash")).then(|| Conversion {
        first: i + 1,
        last: i + 2,
        replacement: format!("-{letter}"),
    })
}

/// "T O K S" is `TOKS`.
fn spelled_letters(words: &Words, i: usize) -> Option<Conversion> {
    let is_letter = |k: usize| {
        let word = words.word(k);
        word.len() == 1 && word.chars().all(|c| c.is_ascii_uppercase())
    };
    let pronoun_or_article = matches!(words.word(i), "A" | "I");
    if !is_letter(i) || (pronoun_or_article && !words.follows(i, RUN_ARTICLES)) {
        return None;
    }
    let mut last = i;
    while last + 1 < words.len() && words.gap(last) == " " && is_letter(last + 1) {
        last += 1;
    }
    let before_lowercase = |k: usize| {
        words.spaced(k)
            && words
                .word(k + 1)
                .starts_with(|c: char| c.is_ascii_lowercase())
    };
    if last > i && words.word(last) == "I" && (last < i + 2 || before_lowercase(last)) {
        last -= 1;
    }
    let options = words.spaced(last)
        && words.is_in(last + 1, &["and", "or"])
        && words.spaced(last + 1)
        && is_letter(last + 2);
    (last >= i + 2 && !options).then(|| Conversion {
        first: i,
        last,
        replacement: (i..=last).map(|k| words.word(k)).collect(),
    })
}

/// What a dotted name's last label makes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    /// A web domain, lowercased whole.
    Domain,
    /// A file name; only its extension is lowercased.
    File,
    /// Not a dotted name; joined as heard.
    AsHeard,
}

/// The ending of a word by its last `.`-separated label.
fn ending(word: &str) -> Option<Ending> {
    let label = word.rsplit('.').next().unwrap_or(word).to_ascii_lowercase();
    if DOMAIN_ENDINGS.contains(&label.as_str()) {
        Some(Ending::Domain)
    } else if FILE_EXTENSIONS.contains(&label.as_str()) {
        Some(Ending::File)
    } else {
        None
    }
}

/// A word that may be part of a dotted or underscored name: ASCII letters,
/// digits, `.` and `-`, and not a small word or another spoken symbol.
fn is_name_part(words: &Words, k: usize) -> bool {
    let word = words.word(k);
    !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && !words.is_in(k, STOPWORDS)
        && !words.is_in(k, &["dot", "dash", "underscore", "slash"])
}

/// Words `first`, `first + 2`, … `last` joined by `separator`, cased for
/// `ending`.
fn joined(words: &Words, first: usize, last: usize, separator: char, ending: Ending) -> String {
    let mut name = (first..=last)
        .step_by(2)
        .map(|k| words.word(k))
        .collect::<Vec<_>>()
        .join(&separator.to_string());
    match ending {
        Ending::Domain => name = name.to_lowercase(),
        Ending::File => {
            if let Some(dot) = name.rfind('.') {
                name = format!("{}{}", &name[..dot], name[dot..].to_lowercase());
            }
        }
        Ending::AsHeard => {}
    }
    name
}

/// "Parallel" is "parallel", but "HTTP" and "dryRun" keep their case.
fn lowercase_if_capitalized(word: &str) -> String {
    if is_capitalized(word) {
        word.to_lowercase()
    } else {
        word.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        DictationMode, DictationOptions, VocabularyCorrection, normalize_transcript,
        parse_vocabulary,
    };

    fn options(mode: DictationMode, vocabulary: &str) -> DictationOptions {
        DictationOptions {
            mode,
            language: String::new(),
            context: String::new(),
            vocabulary: parse_vocabulary(vocabulary).expect("test vocabulary is valid"),
        }
    }

    fn dictated(text: &str, vocabulary: &str) -> String {
        normalize_transcript(text, &options(DictationMode::Dictate, vocabulary)).text
    }

    #[test]
    fn spoken_syntax_becomes_symbols() {
        for (heard, expected) in [
            (
                "Wasn't there an issue with the dash dash parallel?",
                "Wasn't there an issue with the --parallel?",
            ),
            (
                "run it with dash dash no dash verify",
                "run it with --no-verify",
            ),
            ("Dash dash Help", "--help"),
            (
                "So we just upgraded the Leadlord hello at Leadlord dot AI email account",
                "So we just upgraded the Leadlord hello@leadlord.ai email account",
            ),
            ("Use hello at LeadLord.ai.", "Use hello@leadlord.ai."),
            (
                "write to james at leadlord.ai about the email",
                "write to james@leadlord.ai about the email",
            ),
            (
                "so that the name dot sites dot leadlord.ai is the one that's live",
                "so that the name.sites.leadlord.ai is the one that's live",
            ),
            ("open leadlord dot ai", "open leadlord.ai"),
            ("a dot b dot com", "a dot b.com"),
            (
                "Edit package dot json and Next dot JS",
                "Edit package.json and Next.js",
            ),
            ("the dot env file", "the .env file"),
            ("Dot eslintrc dot json", ".eslintrc.json"),
            ("rename snake underscore case", "rename snake_case"),
            ("run ls dash l", "run ls -l"),
            (
                "the same design as T O K S, where",
                "the same design as TOKS, where",
            ),
            ("like B C D I think", "like BCD I think"),
            ("run dot slash install dot SH", "run ./install.sh"),
            ("cd dot dot slash dot dot slash src", "cd ../../src"),
            ("dot slash scripts slash build", "./scripts/build"),
            (
                "use dash dash force dash with dash lease",
                "use --force-with-lease",
            ),
            (
                "run dash dash watch dash and then stop",
                "run --watch dash and then stop",
            ),
            (
                "But I thought that using dash dash parallel with dash dash no-dash isolate still increased CPU cost by 1.6x.",
                "But I thought that using --parallel with --no-isolate still increased CPU cost by 1.6x.",
            ),
            ("try dash dash dry-dash run", "try --dry-run"),
            ("try dash dash dry dash-run", "try --dry-run"),
            ("try dash dash dry-run mode", "try --dry-run mode"),
            ("the dot env dot local file", "the .env.local file"),
            ("add it to dot gitignore", "add it to .gitignore"),
            (
                "send it to team at leadlord.ai",
                "send it to team@leadlord.ai",
            ),
            (
                "mail it to admin at leadlord dot co",
                "mail it to admin@leadlord.co",
            ),
            (
                "I'd like the hello at leadlord.ai account",
                "I'd like the hello@leadlord.ai account",
            ),
            (
                "the leadlord dot ai landing page",
                "the leadlord.ai landing page",
            ),
            ("Package dot json is stale", "package.json is stale"),
            ("Node dot JS works", "Node.js works"),
            ("use the C L I.", "use the CLI."),
            ("the A P I", "the API"),
            ("U S A I think", "USA I think"),
        ] {
            assert_eq!(dictated(heard, ""), expected, "{heard}");
        }
        // The result still gets its casing from Words.
        assert_eq!(
            dictated("update agents dot md", "AGENTS.md"),
            "update AGENTS.md"
        );
    }

    #[test]
    fn ordinary_words_stay() {
        for text in [
            "No need to put the colored dot next to the name.",
            "the dot being on a new line after the email",
            "Analyze thoroughly what's going on in every single dot dot dot.",
            "make the yellow dot go away",
            "the plus before the at sign",
            "we should have a dash review for the one that needs to be reviewed",
            "Delete OCX dash star stuff.",
            "I'm at the office, look at leadlord.ai",
            "website updates at leadlord.ai or something",
            "hello plus one at leadlord.ai account",
            "Plan B I think",
            "the next two-dot landing page",
            "Zernio, Z-E-R-N-I-O.",
            "press the underscore key, a dash, the dot com era",
            // Not email addresses.
            "I work at leadlord.ai and my email is",
            "The dashboard at app.leadlord.ai shows every email we sent",
            "The app at leadlord.ai sends the email",
            "the team at leadlord.ai is shipping fast",
            "the docs at docs.leadlord.ai, and send me the link",
            "they're at leadlord.ai so send them a message",
            "The sales at acme.com went up",
            "security at leadlord.ai matters, check the account",
            "James is at leadlord.ai, email him there",
            "email me at noon, ping support at 3 pm",
            // "dot com" as a word, and dots that end nothing.
            "back in the early dot com days",
            "the whole dot com era was wild",
            "nineties dot com crash",
            "remove the dot md from the file",
            "dot net and Python three dot twelve, see you at ten dot",
            // Dashes, underscores and letters that are words.
            "replace every em dash a model writes",
            "use an en dash a lot less",
            "a quick dash a few blocks away",
            "I made a mad dash to the store, go dash board",
            "a dash dash b",
            "run dash dash no-dash and then stop",
            "I'd underscore security here",
            "we must underscore safety first",
            "so I A B tested the landing page",
            "Is it A B C or D, pick X Y Z or W",
            "A B I think, A I agents, I O U one, F Y I the build broke",
            "the A P I is down, the U S and the U K",
        ] {
            let result = normalize_transcript(text, &options(DictationMode::Dictate, ""));
            assert_eq!(result.text, text);
            assert!(result.corrections.is_empty(), "{text}");
        }
    }

    #[test]
    fn protected_spans_words_entries_and_literal_mode_keep_spoken_syntax() {
        for text in [
            "`dash dash parallel`",
            "\"leadlord dot ai\"",
            "see /docs/agents dot md",
        ] {
            assert_eq!(dictated(text, ""), text);
        }
        // A Sounds like entry decides its own words, unless it only fixes case.
        assert_eq!(
            dictated("use plan dash b", "Plan-B = plan dash b"),
            "use Plan-B"
        );
        assert_eq!(
            dictated("hello at leadlord dot ai", "Leadlord = leadlord"),
            "hello@leadlord.ai"
        );
        let literal = "dash dash parallel at leadlord dot ai, T O K S";
        let result = normalize_transcript(literal, &options(DictationMode::Literal, ""));
        assert_eq!(result.text, literal);
        assert!(result.corrections.is_empty());
    }

    #[test]
    fn a_domain_without_an_address_still_joins() {
        for (heard, expected) in [
            (
                "our waitlist at leadlord dot ai collects emails",
                "our waitlist at leadlord.ai collects emails",
            ),
            (
                "we're at leadlord dot ai now, email us",
                "we're at leadlord.ai now, email us",
            ),
            (
                "help at leadlord dot ai is down",
                "help at leadlord.ai is down",
            ),
        ] {
            assert_eq!(dictated(heard, ""), expected, "{heard}");
        }
        // A Words spelling makes a common word a name: "Stripe dot com".
        assert_eq!(dictated("open stripe dot com", ""), "open stripe dot com");
        assert_eq!(dictated("open stripe dot com", "Stripe"), "open stripe.com");
    }

    #[test]
    fn built_in_symbols_win_over_symbol_words_entries() {
        let entries = "- = dash\n. = dot\n@ = at\n/ = slash";
        for (heard, expected) in [
            ("the dash dash parallel flag", "the --parallel flag"),
            ("run dot slash install dot sh", "run ./install.sh"),
            ("hello at leadlord dot ai", "hello@leadlord.ai"),
            // The entry still applies to what the built-ins leave.
            ("read dash only", "read-only"),
        ] {
            assert_eq!(dictated(heard, entries), expected, "{heard}");
        }
    }

    #[test]
    fn slash_is_built_in_and_a_words_entry_changes_nothing() {
        assert_eq!(dictated("apps slash landing", ""), "apps/landing");
        let result = normalize_transcript(
            "apps slash landing",
            &options(DictationMode::Dictate, "/ = slash"),
        );
        assert_eq!(result.text, "apps/landing");
        assert_eq!(result.corrections.len(), 1);
        // An entry that claims the word turns the built-in off.
        assert_eq!(dictated("slash home", "Slash = slash"), "Slash home");
    }

    #[test]
    fn rewrites_are_recorded_as_heard() {
        let result = normalize_transcript(
            "dash dash parallel and dash dash parallel, agents dot md",
            &options(DictationMode::Dictate, "AGENTS.md"),
        );
        assert_eq!(result.text, "--parallel and --parallel, AGENTS.md");
        let correction = |alias: &str, spelling: &str, count| VocabularyCorrection {
            alias: alias.into(),
            spelling: spelling.into(),
            count,
        };
        assert_eq!(
            result.corrections,
            [
                correction("dash dash parallel", "--parallel", 2),
                correction("agents dot md", "agents.md", 1),
                correction("agents.md", "AGENTS.md", 1),
            ]
        );
    }
}
