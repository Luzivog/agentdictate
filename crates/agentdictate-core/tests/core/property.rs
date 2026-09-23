use agentdictate_core::{
    ClientCommand, PROTOCOL_VERSION, ServerMessage, Settings, VocabularyEntry, normalize_vocabulary,
};
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

    #[test]
    fn every_client_command_tags_the_protocol_version(request_id in any::<u64>()) {
        for command in [
            ClientCommand::get_snapshot(request_id),
            ClientCommand::start_recording(request_id),
            ClientCommand::stop_recording(request_id),
            ClientCommand::quit(request_id),
        ] {
            prop_assert_eq!(command.protocol_version, PROTOCOL_VERSION);
        }
        let message = ServerMessage::command_rejected(request_id, "no");
        prop_assert_eq!(message.protocol_version, PROTOCOL_VERSION);
    }
}

#[test]
fn settings_round_trip_through_json_preserves_defaults() {
    let settings = Settings::default();
    let json = serde_json::to_string(&settings).unwrap();
    let restored: Settings = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, settings);
}
