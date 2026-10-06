use agentdictate_core::{
    DictationMode, DictationOptions, Settings, VocabularyEntry, normalize_transcript,
};
use proptest::prelude::*;

proptest! {
    #[test]
    fn arbitrary_unicode_aliases_and_text_never_panic(
        text in any::<String>(),
        alias in any::<String>(),
        spelling in any::<String>(),
    ) {
        let options = DictationOptions {
            mode: DictationMode::Dictate,
            language: String::new(),
            context: String::new(),
            vocabulary: vec![VocabularyEntry { spelling, aliases: vec![alias] }],
        };
        let _ = normalize_transcript(&text, &options);
    }

    #[test]
    fn spoken_symbol_words_in_any_arrangement_never_panic(
        parts in proptest::collection::vec(
            (
                prop::sample::select(vec![
                    "dot", "Dash", "dash", "at", "underscore", "slash", "plus", "hello", "the",
                    "md", "AI", "env", "I", "A", "T", "l", "é", "leadlord.ai", "x-y", "Codex", "",
                ]),
                prop::sample::select(vec![" ", "  ", ", ", ".", "-", "@", "\n", "", "é", "`"]),
            ),
            0..16,
        ),
    ) {
        let text: String = parts.iter().flat_map(|(word, gap)| [*word, *gap]).collect();
        let options = DictationOptions {
            mode: DictationMode::Dictate,
            language: String::new(),
            context: String::new(),
            vocabulary: vec![VocabularyEntry {
                spelling: "Codex".into(),
                aliases: vec!["dot".into()],
            }],
        };
        let _ = normalize_transcript(&text, &options);
        let without_words = DictationOptions { vocabulary: Vec::new(), ..options };
        let _ = normalize_transcript(&text, &without_words);
    }
}

#[test]
fn settings_round_trip_through_json_preserves_defaults() {
    let settings = Settings::default();
    let json = serde_json::to_string(&settings).unwrap();
    let restored: Settings = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, settings);
}
