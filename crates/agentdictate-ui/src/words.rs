//! The Words screen: the user's spellings (`Settings::vocabulary`) and the
//! edits that change them. Core's `validate_vocabulary` decides what is valid.

use agentdictate_core::{VocabularyEntry, VocabularyError, validate_vocabulary};

/// One change to the vocabulary from the Words screen. `sounds_like` fields
/// hold comma-separated aliases.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WordsEdit {
    Add {
        spelling: String,
        sounds_like: String,
    },
    /// Replaces the word at `index` in the vocabulary.
    Update {
        index: usize,
        spelling: String,
        sounds_like: String,
    },
    Delete {
        index: usize,
    },
}

impl WordsEdit {
    /// Returns the whole vocabulary after this edit, or why it is refused.
    /// An index that no longer exists leaves the vocabulary unchanged.
    pub fn apply(
        &self,
        vocabulary: &[VocabularyEntry],
    ) -> Result<Vec<VocabularyEntry>, VocabularyError> {
        let mut next = vocabulary.to_vec();
        match self {
            Self::Add {
                spelling,
                sounds_like,
            } => next.push(entry(spelling, sounds_like)),
            Self::Update {
                index,
                spelling,
                sounds_like,
            } => {
                if let Some(word) = next.get_mut(*index) {
                    *word = entry(spelling, sounds_like);
                }
            }
            Self::Delete { index } => {
                if *index < next.len() {
                    next.remove(*index);
                }
            }
        }
        validate_vocabulary(&next)?;
        Ok(next)
    }
}

fn entry(spelling: &str, sounds_like: &str) -> VocabularyEntry {
    VocabularyEntry {
        spelling: spelling.trim().to_owned(),
        aliases: sounds_like
            .split(',')
            .map(str::trim)
            .filter(|alias| !alias.is_empty())
            .map(str::to_owned)
            .collect(),
    }
}

/// One row of the Words list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WordRowViewModel {
    /// The word's position in the vocabulary, which edits refer to.
    pub index: usize,
    pub spelling: String,
    pub sounds_like: String,
}

/// The words whose spelling or Sounds like contains `filter`, ignoring case,
/// in the order the vocabulary stores them.
pub fn word_rows(vocabulary: &[VocabularyEntry], filter: &str) -> Vec<WordRowViewModel> {
    let filter = filter.trim().to_lowercase();
    vocabulary
        .iter()
        .enumerate()
        .filter(|(_, word)| {
            filter.is_empty()
                || word.spelling.to_lowercase().contains(&filter)
                || word
                    .aliases
                    .iter()
                    .any(|alias| alias.to_lowercase().contains(&filter))
        })
        .map(|(index, word)| WordRowViewModel {
            index,
            spelling: word.spelling.clone(),
            sounds_like: word.aliases.join(", "),
        })
        .collect()
}
