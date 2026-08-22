use std::path::PathBuf;

use agentdictate_app::{
    CleanupRequest, CleanupTransport, SpeechRouter, SpeechTransport, TranscriptionPipeline,
    TranscriptionRequest,
};
use agentdictate_core::{JobId, JobStage, Settings, TranscriptionProvider};
use agentdictate_runtime::{DeliveryStatus, ExternalError, RecordingJob, Transcriber};
use chrono::Utc;

struct FixedOpenAi;

struct FailingCleanupOpenAi;

struct SubscriptionSpeech;

struct PaidApiMustNotRun;

impl SpeechTransport for FixedOpenAi {
    fn transcribe_audio(
        &mut self,
        _request: TranscriptionRequest<'_>,
    ) -> Result<String, ExternalError> {
        Ok("hello um world".into())
    }
}

impl CleanupTransport for FixedOpenAi {
    fn cleanup_text(&mut self, _request: CleanupRequest<'_>) -> Result<String, ExternalError> {
        Ok("Hello world.".into())
    }
}

impl SpeechTransport for FailingCleanupOpenAi {
    fn transcribe_audio(
        &mut self,
        _request: TranscriptionRequest<'_>,
    ) -> Result<String, ExternalError> {
        Ok("raw words survive".into())
    }
}

impl CleanupTransport for FailingCleanupOpenAi {
    fn cleanup_text(&mut self, _request: CleanupRequest<'_>) -> Result<String, ExternalError> {
        Err(ExternalError::new("cleanup unavailable"))
    }
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
fn transcription_and_cleanup_are_one_deep_runtime_operation() {
    let settings = Settings {
        cleanup_enabled: true,
        cleanup_model: "gpt-5.4-nano".into(),
        ..Settings::default()
    };
    let mut transcriber = TranscriptionPipeline::new(settings, FixedOpenAi, FixedOpenAi);
    let now = Utc::now();
    let job = RecordingJob {
        id: JobId::new(),
        legacy_id: 1,
        started_at: now,
        updated_at: now,
        stage: JobStage::Transcribing,
        audio_path: PathBuf::from("speech.wav"),
        duration_seconds: 12.0,
        transcription_provider: TranscriptionProvider::OpenAiApi,
        transcription_model: "gpt-transcribe".into(),
        raw_transcript: String::new(),
        final_text: String::new(),
        copied_to_clipboard: false,
        paste_triggered: false,
        delivery_status: DeliveryStatus::NotAttempted,
        error_message: None,
        cleanup_error: None,
    };

    let result = transcriber.transcribe(&job).unwrap();

    assert_eq!(result.raw, "hello um world");
    assert_eq!(result.final_text, "Hello world.");
    assert_eq!(result.cleaned_text.as_deref(), Some("Hello world."));
    assert_eq!(result.cleanup_error, None);
}

#[test]
fn cleanup_failure_returns_the_successful_raw_transcript() {
    let settings = Settings {
        cleanup_enabled: true,
        ..Settings::default()
    };
    let mut transcriber =
        TranscriptionPipeline::new(settings, FailingCleanupOpenAi, FailingCleanupOpenAi);
    let now = Utc::now();
    let job = RecordingJob {
        id: JobId::new(),
        legacy_id: 1,
        started_at: now,
        updated_at: now,
        stage: JobStage::Transcribing,
        audio_path: PathBuf::from("speech.wav"),
        duration_seconds: 2.0,
        transcription_provider: TranscriptionProvider::OpenAiApi,
        transcription_model: "gpt-transcribe".into(),
        raw_transcript: String::new(),
        final_text: String::new(),
        copied_to_clipboard: false,
        paste_triggered: false,
        delivery_status: DeliveryStatus::NotAttempted,
        error_message: None,
        cleanup_error: None,
    };

    let result = transcriber.transcribe(&job).unwrap();

    assert_eq!(result.raw, "raw words survive");
    assert_eq!(result.final_text, "raw words survive");
    assert_eq!(result.cleaned_text, None);
    assert_eq!(result.cleanup_error.as_deref(), Some("cleanup unavailable"));
}

#[test]
fn subscription_jobs_never_fall_back_to_the_paid_api_transport() {
    let settings = Settings {
        cleanup_enabled: false,
        ..Settings::default()
    };
    let speech = SpeechRouter::new(PaidApiMustNotRun, SubscriptionSpeech);
    let mut transcriber = TranscriptionPipeline::new(settings, speech, FixedOpenAi);
    let now = Utc::now();
    let job = RecordingJob {
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
        cleanup_error: None,
    };

    let result = transcriber.transcribe(&job).unwrap();

    assert_eq!(result.raw, "subscription transcript");
    assert_eq!(result.final_text, "subscription transcript");
}
