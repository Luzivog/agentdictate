use agentdictate_app::{SpeechTransport, Transcriber, TranscriptionPipeline, TranscriptionRequest};
use agentdictate_core::{FailureKind, JobId, JobStage, Settings};
use agentdictate_runtime::{DeliveryStatus, ExternalError, RecordingJob};
use chrono::Utc;

#[test]
fn empty_results_require_quiet_audio_while_short_words_and_network_errors_survive() {
    #[derive(Clone)]
    struct Speech(Result<String, ExternalError>);
    impl SpeechTransport for Speech {
        fn transcribe_audio(
            &mut self,
            _: TranscriptionRequest<'_>,
        ) -> Result<String, ExternalError> {
            self.0.clone()
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let audio_path = dir.path().join("short.wav");
    let now = Utc::now();
    let job = RecordingJob {
        options: None,
        id: JobId::new(),
        started_at: now,
        updated_at: now,
        stage: JobStage::Transcribing,
        audio_path: audio_path.clone(),
        duration_seconds: 0.1,
        transcription_model: "gpt-transcribe".into(),
        raw_transcript: String::new(),
        final_text: String::new(),
        copied_to_clipboard: false,
        paste_triggered: false,
        delivery_status: DeliveryStatus::NotAttempted,
        error_message: None,
        failure: None,
    };
    for sample in [12i16, 2000] {
        let mut wav = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\x3e\0\0\0\x7d\0\0\x02\0\x10\0data\x80\x0c\0\0".to_vec();
        for _ in 0..1600 {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
        std::fs::write(&audio_path, wav).unwrap();
        for result in [
            Err(ExternalError::NoSpeech),
            Ok("  ".into()),
            Ok("Yes.".into()),
            Err(ExternalError::new("network unavailable")),
        ] {
            let original = result.clone();
            let mut pipeline = TranscriptionPipeline::new(Settings::default(), Speech(result));
            let actual = pipeline.transcribe(&job, None);
            match original {
                Ok(text) if !text.trim().is_empty() => {
                    assert_eq!(actual.unwrap().text, "Yes.")
                }
                Err(ExternalError::Failure { .. }) => {
                    assert_eq!(actual.unwrap_err().to_string(), "network unavailable")
                }
                _ if sample == 12 => assert_eq!(actual.unwrap_err(), ExternalError::NoSpeech),
                // Audio with sound but no words stays in Recovery as unheard.
                _ => assert_eq!(actual.unwrap_err().kind(), FailureKind::NoSpeech),
            }
        }
    }
}
