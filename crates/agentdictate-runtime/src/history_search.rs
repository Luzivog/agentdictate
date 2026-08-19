use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::NaiveDate;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::RuntimeError;
use crate::history::{
    HistoryCursor, HistoryMatch, HistoryPage, HistoryQuery, history_select, row_to_history,
};

const MAX_PAGE_SIZE: usize = 100;
const SEARCH_SCHEMA_VERSION: i64 = 2;
const PREVIEW_CHARACTERS: usize = 160;
const MAX_CORRECTIONS_PER_TOKEN: usize = 3;
const MAX_QUERY_TOKENS: usize = 12;
const MAX_QUERY_CHARACTERS: usize = 256;

const SEARCH_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS history_search_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    schema_version INTEGER NOT NULL,
    ready INTEGER NOT NULL DEFAULT 0
);

CREATE VIRTUAL TABLE IF NOT EXISTS transcript_history_fts USING fts5(
    final_text,
    content='transcript_history',
    content_rowid='id',
    tokenize='unicode61 remove_diacritics 2',
    prefix='2 3'
);

CREATE VIRTUAL TABLE IF NOT EXISTS transcript_history_fts_vocab USING fts5vocab(
    transcript_history_fts,
    'row'
);

CREATE VIRTUAL TABLE IF NOT EXISTS transcript_history_fts_trigram USING fts5(
    final_text,
    content='transcript_history',
    content_rowid='id',
    tokenize='trigram'
);

CREATE TRIGGER IF NOT EXISTS transcript_history_fts_insert
AFTER INSERT ON transcript_history BEGIN
    INSERT INTO transcript_history_fts(rowid, final_text)
    VALUES (new.id, new.final_text);
END;

CREATE TRIGGER IF NOT EXISTS transcript_history_fts_delete
AFTER DELETE ON transcript_history BEGIN
    INSERT INTO transcript_history_fts(transcript_history_fts, rowid, final_text)
    VALUES ('delete', old.id, old.final_text);
END;

CREATE TRIGGER IF NOT EXISTS transcript_history_fts_update
AFTER UPDATE OF final_text ON transcript_history BEGIN
    INSERT INTO transcript_history_fts(transcript_history_fts, rowid, final_text)
    VALUES ('delete', old.id, old.final_text);
    INSERT INTO transcript_history_fts(rowid, final_text)
    VALUES (new.id, new.final_text);
END;

CREATE TRIGGER IF NOT EXISTS transcript_history_fts_trigram_insert
AFTER INSERT ON transcript_history BEGIN
    INSERT INTO transcript_history_fts_trigram(rowid, final_text)
    VALUES (new.id, new.final_text);
END;

CREATE TRIGGER IF NOT EXISTS transcript_history_fts_trigram_delete
AFTER DELETE ON transcript_history BEGIN
    INSERT INTO transcript_history_fts_trigram(
        transcript_history_fts_trigram, rowid, final_text
    ) VALUES ('delete', old.id, old.final_text);
END;

CREATE TRIGGER IF NOT EXISTS transcript_history_fts_trigram_update
AFTER UPDATE OF final_text ON transcript_history BEGIN
    INSERT INTO transcript_history_fts_trigram(
        transcript_history_fts_trigram, rowid, final_text
    ) VALUES ('delete', old.id, old.final_text);
    INSERT INTO transcript_history_fts_trigram(rowid, final_text)
    VALUES (new.id, new.final_text);
END;
"#;

#[derive(Clone, Debug)]
struct VocabularyTerm {
    term: String,
    documents: u64,
    characters: usize,
}

#[derive(Default)]
pub(crate) struct SearchCache {
    vocabulary: Option<Arc<VocabularyIndex>>,
    vocabulary_data_version: Option<i64>,
}

struct VocabularyIndex {
    alphabetic: Vec<VocabularyTerm>,
    by_characters: BTreeMap<usize, Vec<usize>>,
}

impl SearchCache {
    pub(crate) fn invalidate(&mut self) {
        self.vocabulary = None;
        self.vocabulary_data_version = None;
    }
}

#[derive(Clone, Debug)]
struct CursorPosition {
    created_at: String,
    id: i64,
}

#[derive(Clone, Copy)]
struct PageContext<'a> {
    normalized_query: &'a str,
    selected_day: Option<NaiveDate>,
    day: &'a str,
    cursor: Option<&'a CursorPosition>,
    limit: usize,
    search_plan_fingerprint: u64,
}

#[derive(Clone, Debug)]
enum SearchPlan {
    Indexed {
        word_expression: String,
        trigram_expression: Option<String>,
        preview_terms: Vec<String>,
    },
    Literal {
        pattern: String,
        preview_terms: Vec<String>,
    },
}

/// Installs only the empty FTS schema and synchronization triggers. Existing
/// transcript backfill is deliberately deferred to `ensure_index` so opening
/// the daemon never places nonessential search work before hotkey readiness.
pub(crate) fn ensure_schema(connection: &mut Connection) -> Result<(), RuntimeError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(SEARCH_SCHEMA)?;
    transaction.execute(
        r#"
        INSERT OR IGNORE INTO history_search_state (id, schema_version, ready)
        VALUES (1, ?1, 0)
        "#,
        [SEARCH_SCHEMA_VERSION],
    )?;
    transaction.execute(
        r#"
        UPDATE history_search_state
        SET ready = 0, schema_version = ?1
        WHERE id = 1 AND schema_version <> ?1
        "#,
        [SEARCH_SCHEMA_VERSION],
    )?;
    transaction.commit()?;
    Ok(())
}

/// Rebuilds and verifies the external-content index transactionally. Callers
/// schedule this after essential daemon listeners are ready.
pub(crate) fn ensure_index(
    connection: &mut Connection,
    cache: &RefCell<SearchCache>,
) -> Result<(), RuntimeError> {
    if is_index_ready(connection)? {
        return Ok(());
    }
    let transaction = connection.transaction()?;
    transaction.execute(
        "INSERT INTO transcript_history_fts(transcript_history_fts) VALUES('rebuild')",
        [],
    )?;
    transaction.execute(
        "INSERT INTO transcript_history_fts_trigram(transcript_history_fts_trigram) VALUES('rebuild')",
        [],
    )?;
    configure_secure_delete(&transaction)?;
    transaction.execute(
        "INSERT INTO transcript_history_fts(transcript_history_fts, rank) VALUES('integrity-check', 1)",
        [],
    )?;
    transaction.execute(
        "INSERT INTO transcript_history_fts_trigram(transcript_history_fts_trigram, rank) VALUES('integrity-check', 1)",
        [],
    )?;
    transaction.execute(
        "UPDATE history_search_state SET ready = 1, schema_version = ?1 WHERE id = 1",
        [SEARCH_SCHEMA_VERSION],
    )?;
    transaction.commit()?;
    cache.borrow_mut().invalidate();
    Ok(())
}

pub(crate) fn history_page(
    connection: &Connection,
    cache: &RefCell<SearchCache>,
    query: HistoryQuery,
) -> Result<HistoryPage, RuntimeError> {
    let normalized = normalize_query(&query.search);
    let day = query
        .day
        .map_or_else(String::new, |value| value.to_string());
    let limit = query.limit.clamp(1, MAX_PAGE_SIZE);
    let index_ready = is_index_ready(connection)?;
    let plan = match build_search_plan(connection, cache, &normalized, index_ready) {
        Ok(plan) => plan,
        Err(error) if index_ready && is_index_failure(&error) => {
            let _ = mark_index_unready(connection);
            cache.borrow_mut().invalidate();
            literal_plan(&normalized)
        }
        Err(error) => return Err(error),
    };
    let search_plan_fingerprint = plan_fingerprint(&plan);
    let cursor = query
        .after
        .as_ref()
        .map(|value| decode_cursor(value, &normalized, query.day, search_plan_fingerprint))
        .transpose()?;

    let page_context = PageContext {
        normalized_query: &normalized,
        selected_day: query.day,
        day: &day,
        cursor: cursor.as_ref(),
        limit,
        search_plan_fingerprint,
    };
    let result = query_page(connection, &plan, page_context);
    match result {
        Ok(page) => Ok(page),
        Err(error) if matches!(&plan, SearchPlan::Indexed { .. }) && is_index_failure(&error) => {
            // Read-only observers cannot persist the degraded state. The
            // literal fallback is still safe for that process; a writable
            // daemon will mark and rebuild it through the maintenance method.
            let _ = mark_index_unready(connection);
            cache.borrow_mut().invalidate();
            let fallback = literal_plan(&normalized);
            query_page(
                connection,
                &fallback,
                PageContext {
                    search_plan_fingerprint: plan_fingerprint(&fallback),
                    ..page_context
                },
            )
        }
        Err(error) => Err(error),
    }
}

fn query_page(
    connection: &Connection,
    plan: &SearchPlan,
    context: PageContext<'_>,
) -> Result<HistoryPage, RuntimeError> {
    let (created_at, id) = context
        .cursor
        .map(|value| (value.created_at.as_str(), value.id))
        .unwrap_or(("", 0));
    let fetch_limit = context.limit.saturating_add(1);
    let (mut matches, total_matches) = match plan {
        SearchPlan::Indexed {
            word_expression,
            trigram_expression,
            preview_terms,
        } => {
            let trigram_expression = trigram_expression.as_deref().unwrap_or("");
            let candidates = if trigram_expression.is_empty() {
                "SELECT rowid FROM transcript_history_fts WHERE transcript_history_fts MATCH ?1"
                    .to_owned()
            } else {
                "SELECT rowid FROM transcript_history_fts WHERE transcript_history_fts MATCH ?1\
                 UNION SELECT rowid FROM transcript_history_fts_trigram \
                 WHERE transcript_history_fts_trigram MATCH ?2"
                    .to_owned()
            };
            let total_matches = connection.query_row(
                &format!(
                    r#"
                SELECT COUNT(*)
                FROM transcript_history h
                JOIN ({candidates}) matched ON matched.rowid = h.id
                WHERE (?3 = '' OR substr(h.created_at, 1, 10) = ?3)
                "#
                ),
                params![word_expression, trigram_expression, context.day],
                |row| row.get::<_, u64>(0),
            )?;
            let sql = format!(
                "{}\n                 JOIN ({candidates}) matched ON matched.rowid = h.id\n                 WHERE (?3 = '' OR substr(h.created_at, 1, 10) = ?3)\n                   AND (?4 = '' OR h.created_at < ?4 OR (h.created_at = ?4 AND h.id < ?5))\n                 ORDER BY h.created_at DESC, h.id DESC\n                 LIMIT ?6",
                history_select(),
            );
            let entries = query_entries(
                connection,
                &sql,
                params![
                    word_expression,
                    trigram_expression,
                    context.day,
                    created_at,
                    id,
                    i64::try_from(fetch_limit).unwrap_or(i64::MAX)
                ],
            )?;
            (
                entries_to_matches(entries, preview_terms, !word_expression.is_empty()),
                total_matches,
            )
        }
        SearchPlan::Literal {
            pattern,
            preview_terms,
        } => {
            let total_matches = connection.query_row(
                r#"
                SELECT COUNT(*)
                FROM transcript_history h
                WHERE (?1 = '' OR h.final_text LIKE ?1 ESCAPE '\')
                  AND (?2 = '' OR substr(h.created_at, 1, 10) = ?2)
                "#,
                params![pattern, context.day],
                |row| row.get::<_, u64>(0),
            )?;
            let sql = format!(
                "{}\n                 WHERE (?1 = '' OR h.final_text LIKE ?1 ESCAPE '\\')\n                   AND (?2 = '' OR substr(h.created_at, 1, 10) = ?2)\n                   AND (?3 = '' OR h.created_at < ?3 OR (h.created_at = ?3 AND h.id < ?4))\n                 ORDER BY h.created_at DESC, h.id DESC\n                 LIMIT ?5",
                history_select()
            );
            let entries = query_entries(
                connection,
                &sql,
                params![
                    pattern,
                    context.day,
                    created_at,
                    id,
                    i64::try_from(fetch_limit).unwrap_or(i64::MAX)
                ],
            )?;
            (
                entries_to_matches(entries, preview_terms, !pattern.is_empty()),
                total_matches,
            )
        }
    };

    let has_more = matches.len() > context.limit;
    matches.truncate(context.limit);
    let next_cursor = if has_more {
        matches
            .last()
            .map(|value| {
                encode_cursor(
                    connection,
                    context.normalized_query,
                    context.selected_day,
                    context.search_plan_fingerprint,
                    &value.entry,
                )
            })
            .transpose()?
    } else {
        None
    };
    Ok(HistoryPage {
        matches,
        total_matches,
        next_cursor,
    })
}

fn query_entries(
    connection: &Connection,
    sql: &str,
    parameters: impl rusqlite::Params,
) -> Result<Vec<crate::HistoryEntry>, RuntimeError> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement
        .query_map(parameters, row_to_history)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().collect()
}

fn build_search_plan(
    connection: &Connection,
    cache: &RefCell<SearchCache>,
    normalized: &str,
    index_ready: bool,
) -> Result<SearchPlan, RuntimeError> {
    if normalized.is_empty() || !index_ready {
        return Ok(literal_plan(normalized));
    }
    let Some(tokens) = fts_tokens(normalized) else {
        return Ok(literal_plan(normalized));
    };
    let supports_trigram = tokens.iter().all(|token| token.chars().count() >= 3);
    let vocabulary = vocabulary(connection, cache)?;
    let mut groups = Vec::with_capacity(tokens.len());
    let mut preview_terms = Vec::new();
    for token in tokens {
        let mut alternatives = vec![token.clone()];
        let original_documents = vocabulary_prefix_documents(&vocabulary.alphabetic, &token);
        let token_characters = token.chars().count();
        if token_characters >= 3 && original_documents <= 1 {
            let maximum_distance = if token_characters <= 5 { 1 } else { 2 };
            let minimum_correction_documents = if original_documents == 0 {
                1
            } else {
                original_documents.saturating_mul(3)
            };
            let mut corrections = (token_characters.saturating_sub(maximum_distance)
                ..=token_characters.saturating_add(maximum_distance))
                .filter_map(|characters| vocabulary.by_characters.get(&characters))
                .flat_map(|indices| indices.iter().map(|index| &vocabulary.alphabetic[*index]))
                .filter(|candidate| candidate.documents >= minimum_correction_documents)
                .filter_map(|candidate| {
                    let distance = osa_distance(&token, &candidate.term);
                    (distance <= maximum_distance).then_some((
                        distance,
                        candidate.documents,
                        candidate.term.as_str(),
                    ))
                })
                .collect::<Vec<_>>();
            corrections.sort_by(|left, right| {
                left.0
                    .cmp(&right.0)
                    .then_with(|| right.1.cmp(&left.1))
                    .then_with(|| left.2.cmp(right.2))
            });
            alternatives.extend(
                corrections
                    .into_iter()
                    .take(MAX_CORRECTIONS_PER_TOKEN)
                    .map(|(_, _, term)| term.to_owned()),
            );
        }
        alternatives.dedup();
        preview_terms.extend(alternatives.iter().cloned());
        let word_terms = alternatives
            .iter()
            .map(|alternative| format!("\"{}\"*", alternative.replace('"', "\"\"")))
            .collect::<Vec<_>>();
        groups.push((
            format!("({})", word_terms.join(" OR ")),
            supports_trigram.then(|| {
                let terms = alternatives
                    .iter()
                    .map(|alternative| format!("\"{}\"", alternative.replace('"', "\"\"")))
                    .collect::<Vec<_>>();
                format!("({})", terms.join(" OR "))
            }),
        ));
    }
    Ok(SearchPlan::Indexed {
        word_expression: groups
            .iter()
            .map(|(word, _)| word.as_str())
            .collect::<Vec<_>>()
            .join(" AND "),
        trigram_expression: supports_trigram.then(|| {
            groups
                .iter()
                .filter_map(|(_, trigram)| trigram.as_deref())
                .collect::<Vec<_>>()
                .join(" AND ")
        }),
        preview_terms,
    })
}

fn literal_plan(normalized: &str) -> SearchPlan {
    let pattern = if normalized.is_empty() {
        String::new()
    } else {
        format!("%{}%", escape_like(normalized))
    };
    SearchPlan::Literal {
        pattern,
        preview_terms: (!normalized.is_empty())
            .then(|| normalized.to_owned())
            .into_iter()
            .collect(),
    }
}

fn fts_tokens(normalized: &str) -> Option<Vec<String>> {
    if normalized
        .chars()
        .any(|character| !character.is_alphanumeric() && !character.is_whitespace())
    {
        return None;
    }
    let tokens = normalized
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    (!tokens.is_empty()).then_some(tokens)
}

fn vocabulary(
    connection: &Connection,
    cache: &RefCell<SearchCache>,
) -> Result<Arc<VocabularyIndex>, RuntimeError> {
    let data_version =
        connection.query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))?;
    {
        let cached = cache.borrow();
        if cached.vocabulary_data_version == Some(data_version)
            && let Some(vocabulary) = cached.vocabulary.as_ref()
        {
            return Ok(vocabulary.clone());
        }
    }
    let mut statement = connection
        .prepare("SELECT term, doc FROM transcript_history_fts_vocab ORDER BY term ASC")?;
    let alphabetic = statement
        .query_map([], |row| {
            let term: String = row.get(0)?;
            Ok(VocabularyTerm {
                characters: term.chars().count(),
                term,
                documents: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut by_characters = BTreeMap::<usize, Vec<usize>>::new();
    for (index, term) in alphabetic.iter().enumerate() {
        by_characters
            .entry(term.characters)
            .or_default()
            .push(index);
    }
    let values = Arc::new(VocabularyIndex {
        alphabetic,
        by_characters,
    });
    let mut cache = cache.borrow_mut();
    cache.vocabulary = Some(values.clone());
    cache.vocabulary_data_version = Some(data_version);
    Ok(values)
}

fn vocabulary_prefix_documents(vocabulary: &[VocabularyTerm], prefix: &str) -> u64 {
    let index = vocabulary.partition_point(|candidate| candidate.term.as_str() < prefix);
    vocabulary
        .iter()
        .skip(index)
        .take_while(|candidate| candidate.term.starts_with(prefix))
        .fold(0_u64, |total, candidate| {
            total.saturating_add(candidate.documents)
        })
}

fn entries_to_matches(
    entries: Vec<crate::HistoryEntry>,
    preview_terms: &[String],
    search_active: bool,
) -> Vec<HistoryMatch> {
    entries
        .into_iter()
        .map(|entry| HistoryMatch {
            preview: preview(&entry.final_text, preview_terms, search_active),
            entry,
        })
        .collect()
}

fn preview(text: &str, terms: &[String], search_active: bool) -> String {
    let characters = text.chars().collect::<Vec<_>>();
    if characters.len() <= PREVIEW_CHARACTERS {
        return text.to_owned();
    }
    let start = if search_active {
        lowercase_match_start(text, terms)
            .map(|character_index| character_index.saturating_sub(36))
            .unwrap_or(0)
    } else {
        0
    };
    let end = (start + PREVIEW_CHARACTERS).min(characters.len());
    let mut value = String::new();
    if start > 0 {
        value.push('…');
    }
    value.extend(characters[start..end].iter());
    if end < characters.len() {
        value.push('…');
    }
    value
}

fn lowercase_match_start(text: &str, terms: &[String]) -> Option<usize> {
    let mut lowercase = String::with_capacity(text.len());
    let mut boundaries = Vec::with_capacity(text.chars().count());
    for (original_index, character) in text.chars().enumerate() {
        for lowercase_character in character.to_lowercase() {
            boundaries.push((lowercase.len(), original_index));
            lowercase.push(lowercase_character);
        }
    }
    terms
        .iter()
        .filter_map(|term| lowercase.find(term))
        .min()
        .and_then(|byte_index| {
            let boundary = boundaries
                .partition_point(|(lowercase_byte, _)| *lowercase_byte <= byte_index)
                .checked_sub(1)?;
            Some(boundaries[boundary].1)
        })
}

fn encode_cursor(
    connection: &Connection,
    normalized_query: &str,
    day: Option<NaiveDate>,
    search_plan_fingerprint: u64,
    entry: &crate::HistoryEntry,
) -> Result<HistoryCursor, RuntimeError> {
    // Preserve the database's exact timestamp spelling. Legacy rows may use
    // `+00:00` while native rows use `Z`; reformatting it would break the same
    // textual ordering used by the keyset query and could repeat a row.
    let created_at = connection.query_row(
        "SELECT created_at FROM transcript_history WHERE id = ?1",
        [entry.id],
        |row| row.get::<_, String>(0),
    )?;
    Ok(HistoryCursor::from_opaque(format!(
        "v2|{:016x}|{:016x}|{}|{}|{}",
        query_fingerprint(normalized_query, day),
        search_plan_fingerprint,
        day.map_or_else(|| "-".to_owned(), |value| value.to_string()),
        created_at,
        entry.id
    )))
}

fn decode_cursor(
    cursor: &HistoryCursor,
    normalized_query: &str,
    day: Option<NaiveDate>,
    search_plan_fingerprint: u64,
) -> Result<CursorPosition, RuntimeError> {
    let parts = cursor.as_str().split('|').collect::<Vec<_>>();
    if parts.len() != 6 || parts[0] != "v2" {
        return Err(invalid_cursor("unsupported cursor format"));
    }
    let fingerprint = u64::from_str_radix(parts[1], 16)
        .map_err(|_| invalid_cursor("invalid query fingerprint"))?;
    if fingerprint != query_fingerprint(normalized_query, day) {
        return Err(invalid_cursor("cursor belongs to a different query"));
    }
    let cursor_plan = u64::from_str_radix(parts[2], 16)
        .map_err(|_| invalid_cursor("invalid search plan fingerprint"))?;
    if cursor_plan != search_plan_fingerprint {
        return Err(invalid_cursor(
            "search index changed; restart from the first page",
        ));
    }
    let expected_day = day.map_or_else(|| "-".to_owned(), |value| value.to_string());
    if parts[3] != expected_day {
        return Err(invalid_cursor("cursor belongs to a different day"));
    }
    chrono::DateTime::parse_from_rfc3339(parts[4])
        .map_err(|_| invalid_cursor("invalid timestamp"))?;
    let id = parts[5]
        .parse::<i64>()
        .map_err(|_| invalid_cursor("invalid row id"))?;
    Ok(CursorPosition {
        created_at: parts[4].to_owned(),
        id,
    })
}

fn plan_fingerprint(plan: &SearchPlan) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let values: Vec<&str> = match plan {
        SearchPlan::Indexed {
            word_expression,
            trigram_expression,
            ..
        } => vec![
            "indexed",
            word_expression,
            trigram_expression.as_deref().unwrap_or(""),
        ],
        SearchPlan::Literal { pattern, .. } => vec!["literal", pattern],
    };
    for value in values {
        for byte in value.bytes().chain([0xff]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

fn invalid_cursor(reason: &str) -> RuntimeError {
    RuntimeError::InvalidHistoryCursor(reason.to_owned())
}

fn query_fingerprint(normalized_query: &str, day: Option<NaiveDate>) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in normalized_query.bytes().chain([0xff]).chain(
        day.map(|value| value.to_string())
            .unwrap_or_default()
            .bytes(),
    ) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn normalize_query(value: &str) -> String {
    let collapsed = value
        .split_whitespace()
        .take(MAX_QUERY_TOKENS)
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    collapsed
        .chars()
        .take(MAX_QUERY_CHARACTERS)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn table_exists(connection: &Connection, name: &str) -> Result<bool, RuntimeError> {
    Ok(connection
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn is_index_ready(connection: &Connection) -> Result<bool, RuntimeError> {
    if !table_exists(connection, "history_search_state")?
        || !table_exists(connection, "transcript_history_fts")?
        || !table_exists(connection, "transcript_history_fts_trigram")?
    {
        return Ok(false);
    }
    Ok(connection
        .query_row(
            "SELECT ready FROM history_search_state WHERE id = 1 AND schema_version = ?1",
            [SEARCH_SCHEMA_VERSION],
            |row| row.get::<_, bool>(0),
        )
        .optional()?
        .unwrap_or(false))
}

fn mark_index_unready(connection: &Connection) -> Result<(), RuntimeError> {
    if table_exists(connection, "history_search_state")? {
        connection.execute("UPDATE history_search_state SET ready = 0 WHERE id = 1", [])?;
    }
    Ok(())
}

fn configure_secure_delete(transaction: &Transaction<'_>) -> Result<(), RuntimeError> {
    transaction.execute(
        "INSERT INTO transcript_history_fts(transcript_history_fts, rank) VALUES('secure-delete', 1)",
        [],
    )?;
    transaction.execute(
        "INSERT INTO transcript_history_fts_trigram(transcript_history_fts_trigram, rank) VALUES('secure-delete', 1)",
        [],
    )?;
    Ok(())
}

fn is_index_failure(error: &RuntimeError) -> bool {
    let RuntimeError::Database(error) = error else {
        return false;
    };
    let message = error.to_string().to_lowercase();
    message.contains("fts5")
        || message.contains("transcript_history_fts")
        || message.contains("malformed")
        || message.contains("corrupt")
        || message.contains("no such table: transcript_history_fts")
}

pub(crate) fn is_search_schema_unavailable(error: &RuntimeError) -> bool {
    let RuntimeError::Database(error) = error else {
        return false;
    };
    let message = error.to_string().to_lowercase();
    message.contains("fts5") || message.contains("transcript_history_fts")
}

fn osa_distance(left: &str, right: &str) -> usize {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    let mut previous_previous = vec![0; right.len() + 1];
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0; right.len() + 1];
    for (left_index, left_character) in left.iter().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_character) in right.iter().enumerate() {
            let substitution =
                previous[right_index] + usize::from(left_character != right_character);
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(substitution);
            if left_index > 0
                && right_index > 0
                && left_character == &right[right_index - 1]
                && &left[left_index - 1] == right_character
            {
                current[right_index + 1] =
                    current[right_index + 1].min(previous_previous[right_index - 1] + 1);
            }
        }
        std::mem::swap(&mut previous_previous, &mut previous);
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use rusqlite::Connection;

    use super::{
        SearchCache, SearchPlan, build_search_plan, ensure_index, ensure_schema, normalize_query,
        osa_distance,
    };

    #[test]
    fn optimal_string_alignment_counts_adjacent_transposition_as_one_edit() {
        assert_eq!(osa_distance("transcirpt", "transcript"), 1);
    }

    #[test]
    fn normalized_queries_have_bounded_tokens_and_characters() {
        let normalized = normalize_query(&format!("{}{}", "needle ".repeat(20), "x".repeat(400)));

        assert!(normalized.split_whitespace().count() <= 12);
        assert!(normalized.chars().count() <= 256);
    }

    #[test]
    fn short_alphanumeric_queries_use_the_word_index_instead_of_literal_scans() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                r#"
                CREATE TABLE transcript_history (
                    id INTEGER PRIMARY KEY,
                    final_text TEXT NOT NULL
                );
                "#,
            )
            .unwrap();
        let cache = RefCell::new(SearchCache::default());
        ensure_schema(&mut connection).unwrap();
        ensure_index(&mut connection, &cache).unwrap();

        for query in ["n", "ne"] {
            let plan = build_search_plan(&connection, &cache, query, true).unwrap();
            assert!(
                matches!(
                    plan,
                    SearchPlan::Indexed {
                        trigram_expression: None,
                        ..
                    }
                ),
                "query {query:?} must avoid the literal full-table path"
            );
        }
    }
}
