use std::path::Path;

use agentdictate_app::{CodexSubscriptionTransport, SpeechTransport, TranscriptionRequest};
use agentdictate_core::TranscriptionProvider;

/// Manual compatibility probe for the private ChatGPT dictation contract.
/// The normal test suite never sends network traffic or reads Codex auth.
#[test]
#[ignore = "requires a signed-in Codex account and sends a system audio fixture to ChatGPT"]
fn signed_in_codex_account_transcribes_the_system_fixture() {
    let fixture = Path::new("/usr/share/sounds/alsa/Front_Center.wav");
    assert!(fixture.is_file(), "the ALSA speech fixture is unavailable");
    let mut transport = CodexSubscriptionTransport::new();

    let transcript = transport
        .transcribe_audio(TranscriptionRequest {
            keywords: &[],
            audio_path: fixture,
            provider: TranscriptionProvider::ChatGptSubscription,
            model: "gpt-transcribe",
            language: "en",
            prompt: "",
            duration_seconds: 1.428,
        })
        .expect("the signed-in Codex account should transcribe the fixture");

    let normalized = transcript
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    assert!(
        normalized.contains("frontcenter"),
        "unexpected fixture transcript: {transcript:?}"
    );
}
