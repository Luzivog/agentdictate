use agentdictate_core::{Settings, VocabularyEntry, normalize_vocabulary};
use proptest::prelude::*;

proptest! {
    #[test]
    fn arbitrary_unicode_aliases_and_text_never_panic(
        text in any::<String>(),
        alias in any::<String>(),
        spelling in any::<String>(),
    ) {
        let vocabulary = [VocabularyEntry { spelling, aliases: vec![alias] }];
        let _ = normalize_vocabulary(&text, &vocabulary);
    }
}

#[test]
fn settings_round_trip_through_json_preserves_defaults() {
    let settings = Settings::default();
    let json = serde_json::to_string(&settings).unwrap();
    let restored: Settings = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, settings);
}
