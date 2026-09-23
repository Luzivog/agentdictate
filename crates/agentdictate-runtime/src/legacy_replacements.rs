//! One-time move of the retired Replacements feature into vocabulary.

use std::path::Path;

use agentdictate_core::{Settings, VocabularyEntry, parse_vocabulary, vocabulary_text};

use crate::{Runtime, RuntimeError, save_settings};

/// An enabled rule of the retired Replacements feature, and what became of it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetiredReplacement {
    pub source_phrase: String,
    pub replacement_phrase: String,
    pub outcome: RetiredReplacementOutcome,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetiredReplacementOutcome {
    /// The source phrase is now an alias of the replacement's spelling.
    BecameAlias,
    /// The rule also matched inside words, which vocabulary never does.
    NotWholeWord,
    /// Core's vocabulary rules, which the Words screen applies, reject it: a
    /// blank or oversized phrase, a comma or `=`, an alias another spelling
    /// owns, or an alias identical to its spelling.
    NotValidVocabulary,
}

impl Runtime {
    /// Moves the enabled rules of the retired Replacements feature into
    /// `settings.vocabulary`, saves that to `config_file`, then drops the
    /// `replacement_mappings` table so this runs once. A whole-word rule
    /// becomes an alias (its source phrase) of a spelling (its replacement
    /// phrase); vocabulary ignores case, so a case-sensitive rule now matches
    /// every case. Settings are saved before the table is dropped, and a
    /// repeated merge changes nothing, so a crash in between is harmless.
    pub fn retire_replacement_rules(
        &self,
        settings: &mut Settings,
        config_file: &Path,
    ) -> Result<Vec<RetiredReplacement>, RuntimeError> {
        let table_exists: bool = self.connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'replacement_mappings'
            )",
            [],
            |row| row.get(0),
        )?;
        if !table_exists {
            return Ok(Vec::new());
        }
        let rules = self
            .connection
            .prepare(
                "SELECT source_phrase, replacement_phrase, whole_word_only
                 FROM replacement_mappings WHERE enabled != 0 ORDER BY id",
            )?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<Vec<(String, String, bool)>>>()?;
        let mut vocabulary = settings.vocabulary.clone();
        let retired = rules
            .into_iter()
            .map(|(source_phrase, replacement_phrase, whole_word_only)| {
                let outcome = if !whole_word_only {
                    RetiredReplacementOutcome::NotWholeWord
                } else if add_alias(&mut vocabulary, &replacement_phrase, &source_phrase) {
                    RetiredReplacementOutcome::BecameAlias
                } else {
                    RetiredReplacementOutcome::NotValidVocabulary
                };
                RetiredReplacement {
                    source_phrase,
                    replacement_phrase,
                    outcome,
                }
            })
            .collect();
        if vocabulary != settings.vocabulary {
            let migrated = Settings {
                vocabulary,
                ..settings.clone()
            };
            save_settings(config_file, &migrated)?;
            *settings = migrated;
        }
        self.connection
            .execute("DROP TABLE replacement_mappings", [])?;
        Ok(retired)
    }
}

/// Adds `alias` to the entry spelled `spelling`, creating the entry when
/// needed. Returns false and leaves `vocabulary` unchanged when the result
/// would not survive the vocabulary text form's round trip, which also
/// applies core's vocabulary rules.
fn add_alias(vocabulary: &mut Vec<VocabularyEntry>, spelling: &str, alias: &str) -> bool {
    let (spelling, alias) = (spelling.trim(), alias.trim());
    let mut candidate = vocabulary.clone();
    match candidate
        .iter_mut()
        .find(|entry| entry.spelling.to_lowercase() == spelling.to_lowercase())
    {
        Some(entry)
            if entry
                .aliases
                .iter()
                .any(|existing| existing.to_lowercase() == alias.to_lowercase()) =>
        {
            return true;
        }
        Some(entry) => entry.aliases.push(alias.to_owned()),
        None => candidate.push(VocabularyEntry {
            spelling: spelling.to_owned(),
            aliases: vec![alias.to_owned()],
        }),
    }
    if parse_vocabulary(&vocabulary_text(&candidate)).as_ref() != Ok(&candidate) {
        return false;
    }
    *vocabulary = candidate;
    true
}

#[cfg(test)]
mod tests {
    use agentdictate_core::parse_vocabulary;
    use rusqlite::params;
    use tempfile::tempdir;

    use super::*;
    use crate::load_settings;

    fn insert_rule(runtime: &Runtime, source: &str, replacement: &str, enabled: bool, whole: bool) {
        runtime
            .connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS replacement_mappings (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    source_phrase TEXT NOT NULL,
                    replacement_phrase TEXT NOT NULL,
                    enabled INTEGER NOT NULL DEFAULT 1,
                    case_sensitive INTEGER NOT NULL DEFAULT 0,
                    whole_word_only INTEGER NOT NULL DEFAULT 1,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );",
            )
            .unwrap();
        runtime
            .connection
            .execute(
                "INSERT INTO replacement_mappings (
                    source_phrase, replacement_phrase, enabled, whole_word_only,
                    created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, '2026-09-01T00:00:00Z', '2026-09-01T00:00:00Z')",
                params![source, replacement, enabled, whole],
            )
            .unwrap();
    }

    #[test]
    fn enabled_whole_word_rules_become_aliases_once() {
        let directory = tempdir().unwrap();
        let config_file = directory.path().join("config.json");
        let runtime = Runtime::open(directory.path().join("agentdictate.sqlite")).unwrap();
        insert_rule(&runtime, "versel", "Vercel", true, true);
        insert_rule(&runtime, "lead load", "Leadlord", true, true);
        insert_rule(&runtime, "kube", "kubectl", true, false);
        insert_rule(&runtime, "a, b", "Commas", true, true);
        insert_rule(&runtime, "postgress", "Postgres", false, true);
        let mut settings = Settings {
            vocabulary: parse_vocabulary("Leadlord = lead lord").unwrap(),
            ..Settings::default()
        };

        let retired = runtime
            .retire_replacement_rules(&mut settings, &config_file)
            .unwrap();

        let outcomes = retired
            .iter()
            .map(|rule| (rule.source_phrase.as_str(), rule.outcome))
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes,
            [
                ("versel", RetiredReplacementOutcome::BecameAlias),
                ("lead load", RetiredReplacementOutcome::BecameAlias),
                ("kube", RetiredReplacementOutcome::NotWholeWord),
                ("a, b", RetiredReplacementOutcome::NotValidVocabulary),
            ]
        );
        let expected =
            parse_vocabulary("Leadlord = lead lord, lead load\nVercel = versel").unwrap();
        assert_eq!(settings.vocabulary, expected);
        assert_eq!(load_settings(&config_file).unwrap().vocabulary, expected);
        assert!(
            runtime
                .retire_replacement_rules(&mut settings, &config_file)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_database_without_rules_leaves_settings_unsaved() {
        let directory = tempdir().unwrap();
        let config_file = directory.path().join("config.json");
        let runtime = Runtime::open(directory.path().join("agentdictate.sqlite")).unwrap();
        let mut settings = Settings::default();

        let retired = runtime
            .retire_replacement_rules(&mut settings, &config_file)
            .unwrap();

        assert!(retired.is_empty());
        assert!(!config_file.exists());
    }
}
