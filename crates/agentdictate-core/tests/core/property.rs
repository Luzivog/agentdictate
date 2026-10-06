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
}

#[test]
fn settings_round_trip_through_json_preserves_defaults() {
    let settings = Settings::default();
    let json = serde_json::to_string(&settings).unwrap();
    let restored: Settings = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, settings);
}
