use std::path::PathBuf;

use agentdictate_app::{
    SpeechRouter, SpeechTransport, TranscriptionPipeline, TranscriptionRequest,
};
use agentdictate_core::{JobId, JobStage, Settings, TranscriptionProvider};
use agentdictate_runtime::{DeliveryStatus, ExternalError, RecordingJob, Transcriber};
use chrono::Utc;

struct SubscriptionSpeech;

struct PaidApiMustNotRun;

#[test]
fn subscription_language_list_is_rejected_before_reading_audio_or_authentication() {
    let mut transport = agentdictate_app::CodexSubscriptionTransport::new();
    let error = transport
        .transcribe_audio(TranscriptionRequest {
            keywords: &[],
            audio_path: std::path::Path::new("does-not-exist.wav"),
            provider: TranscriptionProvider::ChatGptSubscription,
            model: "ignored",
            language: "en,fr",
            prompt: "",
            duration_seconds: 1.0,
        })
        .unwrap_err();
    assert!(error.to_string().contains("one language hint"));
}

#[test]
fn a_stored_transcript_is_reused_without_transcribing_again() {
    let now = Utc::now();
    let job = RecordingJob {
        options: None,
        id: JobId::new(),
        legacy_id: 1,
        started_at: now,
        updated_at: now,
        stage: JobStage::Transcribing,
        audio_path: "missing.wav".into(),
        duration_seconds: 2.0,
        transcription_provider: TranscriptionProvider::OpenAiApi,
        transcription_model: "gpt-transcribe".into(),
        raw_transcript: "Do not push.".into(),
        final_text: String::new(),
        copied_to_clipboard: false,
        paste_triggered: false,
        delivery_status: DeliveryStatus::NotAttempted,
        error_message: None,
    };
    let mut pipeline = TranscriptionPipeline::new(Settings::default(), PaidApiMustNotRun);
    let result = pipeline.transcribe(&job).unwrap();
    assert_eq!(result.text, "Do not push.");
    assert_eq!(result.model, "gpt-transcribe");
}

impl SpeechTransport for SubscriptionSpeech {
    fn transcribe_audio(
        &mut self,
        _request: TranscriptionRequest<'_>,
    ) -> Result<String, ExternalError> {
        Ok("subscription transcript".into())
    }
}

impl SpeechTransport for PaidApiMustNotRun {
    fn transcribe_audio(
        &mut self,
        _request: TranscriptionRequest<'_>,
    ) -> Result<String, ExternalError> {
        panic!("subscription transcription must not call the paid API transport")
    }
}

#[test]
fn subscription_jobs_never_fall_back_to_the_paid_api_transport() {
    let speech = SpeechRouter::new(PaidApiMustNotRun, SubscriptionSpeech);
    let mut transcriber = TranscriptionPipeline::new(Settings::default(), speech);
    let now = Utc::now();
    let job = RecordingJob {
        options: None,
        id: JobId::new(),
        legacy_id: 1,
        started_at: now,
        updated_at: now,
        stage: JobStage::Transcribing,
        audio_path: PathBuf::from("speech.wav"),
        duration_seconds: 2.0,
        transcription_provider: TranscriptionProvider::ChatGptSubscription,
        transcription_model: "gpt-transcribe".into(),
        raw_transcript: String::new(),
        final_text: String::new(),
        copied_to_clipboard: false,
        paste_triggered: false,
        delivery_status: DeliveryStatus::NotAttempted,
        error_message: None,
    };

    let result = transcriber.transcribe(&job).unwrap();

    assert_eq!(result.text, "subscription transcript");
}

#[test]
fn empty_results_require_quiet_audio_while_short_words_and_network_errors_survive() {
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
        legacy_id: 1,
        started_at: now,
        updated_at: now,
        stage: JobStage::Transcribing,
        audio_path: audio_path.clone(),
        duration_seconds: 0.1,
        transcription_provider: TranscriptionProvider::OpenAiApi,
        transcription_model: "gpt-transcribe".into(),
        raw_transcript: String::new(),
        final_text: String::new(),
        copied_to_clipboard: false,
        paste_triggered: false,
        delivery_status: DeliveryStatus::NotAttempted,
        error_message: None,
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
            let actual = pipeline.transcribe(&job);
            match original {
                Ok(text) if !text.trim().is_empty() => {
                    assert_eq!(actual.unwrap().text, "Yes.")
                }
                Err(ExternalError::Failure { .. }) => {
                    assert_eq!(actual.unwrap_err().to_string(), "network unavailable")
                }
                _ if sample == 12 => assert_eq!(actual.unwrap_err(), ExternalError::NoSpeech),
                _ => assert!(matches!(actual, Err(ExternalError::Failure { .. }))),
            }
        }
    }
}
