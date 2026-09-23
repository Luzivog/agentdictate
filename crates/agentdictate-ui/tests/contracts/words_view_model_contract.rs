//! Words screen edit contracts.

use agentdictate_core::{VocabularyEntry, VocabularyError};
use agentdictate_ui::{WordRowViewModel, WordsEdit, WordsError, word_rows};

fn word(spelling: &str, aliases: &[&str]) -> VocabularyEntry {
    VocabularyEntry {
        spelling: spelling.to_owned(),
        aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
    }
}

fn vocabulary() -> Vec<VocabularyEntry> {
    vec![
        word("Brightlane", &["bright lane"]),
        word("Claude Code", &[]),
    ]
}

#[test]
fn adding_a_word_trims_it_and_splits_sounds_like_on_commas() {
    let added = WordsEdit::Add {
        spelling: "  serverlord ".to_owned(),
        sounds_like: "server lord, , server load ".to_owned(),
    }
    .apply(&vocabulary())
    .unwrap();

    assert_eq!(
        added.last(),
        Some(&word("serverlord", &["server lord", "server load"]))
    );
    assert_eq!(added.len(), 3);
}

#[test]
fn updating_a_word_replaces_its_spelling_and_sounds_like_in_place() {
    let updated = WordsEdit::Update {
        index: 0,
        spelling: "Brightlane".to_owned(),
        sounds_like: "bright lane, bright lain".to_owned(),
    }
    .apply(&vocabulary())
    .unwrap();

    assert_eq!(
        updated,
        vec![
            word("Brightlane", &["bright lane", "bright lain"]),
            word("Claude Code", &[])
        ]
    );
}

#[test]
fn deleting_a_word_keeps_the_others_in_order() {
    let deleted = WordsEdit::Delete { index: 0 }.apply(&vocabulary()).unwrap();

    assert_eq!(deleted, vec![word("Claude Code", &[])]);
}

#[test]
fn edits_are_refused_for_blank_or_duplicate_spellings_and_self_aliases() {
    let add = |spelling: &str, sounds_like: &str| {
        WordsEdit::Add {
            spelling: spelling.to_owned(),
            sounds_like: sounds_like.to_owned(),
        }
        .apply(&vocabulary())
    };

    assert_eq!(
        add("  ", "bright"),
        Err(WordsError::Invalid(VocabularyError::BlankSpelling))
    );
    assert_eq!(
        add("brightlane", ""),
        Err(WordsError::Invalid(VocabularyError::DuplicateSpelling(
            "brightlane".to_owned()
        )))
    );
    assert_eq!(
        add("Codex", "Codex"),
        Err(WordsError::Invalid(VocabularyError::AliasIsSpelling))
    );
    assert_eq!(
        add("Brightlanes", "bright lane").map_err(|error| error.to_string()),
        Err("“bright lane” is already listed under Sounds like".to_owned())
    );
    // Renaming a word to its own spelling in another case is not a duplicate.
    assert!(
        WordsEdit::Update {
            index: 0,
            spelling: "BrightLane".to_owned(),
            sounds_like: "bright lane".to_owned(),
        }
        .apply(&vocabulary())
        .is_ok()
    );
}

#[test]
fn fixing_a_word_adds_the_heard_phrase_to_an_existing_spelling_or_a_new_word() {
    let fix = |heard: &str, spelling: &str| {
        WordsEdit::FixWord {
            heard: heard.to_owned(),
            spelling: spelling.to_owned(),
        }
        .apply(&vocabulary())
    };

    // An existing spelling, matched ignoring case, keeps its own casing.
    assert_eq!(
        fix(" bright lain ", "brightlane").unwrap()[0],
        word("Brightlane", &["bright lane", "bright lain"])
    );
    // A phrase it already sounds like is not added twice.
    assert_eq!(fix("Bright Lane", "Brightlane").unwrap(), vocabulary());
    assert_eq!(
        fix("codecs", "Codex").unwrap().last(),
        Some(&word("Codex", &["codecs"]))
    );
    assert_eq!(fix("  ", "Codex"), Err(WordsError::BlankHeard));
    assert_eq!(
        fix("bright lane", "Codex"),
        Err(WordsError::Invalid(VocabularyError::DuplicateAlias(
            "bright lane".to_owned()
        )))
    );
}

#[test]
fn the_filter_matches_spellings_and_sounds_like_ignoring_case() {
    let rows = |filter: &str| {
        word_rows(&vocabulary(), filter)
            .into_iter()
            .map(|row| row.index)
            .collect::<Vec<_>>()
    };

    assert_eq!(rows(""), [0, 1]);
    assert_eq!(rows("CLAUDE"), [1]);
    assert_eq!(rows(" lane "), [0]);
    assert!(rows("codex").is_empty());
    assert_eq!(
        word_rows(&vocabulary(), "bright")[0],
        WordRowViewModel {
            index: 0,
            spelling: "Brightlane".to_owned(),
            sounds_like: "bright lane".to_owned(),
        }
    );
}
