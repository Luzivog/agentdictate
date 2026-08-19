use std::path::PathBuf;

use agentdictate_app::{CleanupRequest, OpenAiTranscriber, OpenAiTransport, TranscriptionRequest};
use agentdictate_core::{JobId, JobStage, Settings};
use agentdictate_runtime::{DeliveryStatus, ExternalError, RecordingJob, Transcriber};
use chrono::Utc;

struct FixedOpenAi;

struct FailingCleanupOpenAi;

impl OpenAiTransport for FixedOpenAi {
    fn transcribe_audio(
        &mut self,
        _request: TranscriptionRequest<'_>,
    ) -> Result<String, ExternalError> {
        Ok("hello um world".into())
    }

    fn cleanup_text(&mut self, _request: CleanupRequest<'_>) -> Result<String, ExternalError> {
        Ok("Hello world.".into())
    }
}

impl OpenAiTransport for FailingCleanupOpenAi {
    fn transcribe_audio(
        &mut self,
        _request: TranscriptionRequest<'_>,
    ) -> Result<String, ExternalError> {
        Ok("raw words survive".into())
    }

    fn cleanup_text(&mut self, _request: CleanupRequest<'_>) -> Result<String, ExternalError> {
        Err(ExternalError::new("cleanup unavailable"))
    }
}

#[test]
fn transcription_and_cleanup_are_one_deep_runtime_operation() {
    let settings = Settings {
        cleanup_enabled: true,
        cleanup_model: "gpt-5.4-nano".into(),
        ..Settings::default()
    };
    let mut transcriber = OpenAiTranscriber::new(settings, FixedOpenAi);
    let now = Utc::now();
    let job = RecordingJob {
        id: JobId::new(),
        legacy_id: 1,
        started_at: now,
        updated_at: now,
        stage: JobStage::Transcribing,
        audio_path: PathBuf::from("speech.wav"),
        duration_seconds: 12.0,
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
    let mut transcriber = OpenAiTranscriber::new(settings, FailingCleanupOpenAi);
    let now = Utc::now();
    let job = RecordingJob {
        id: JobId::new(),
        legacy_id: 1,
        started_at: now,
        updated_at: now,
        stage: JobStage::Transcribing,
        audio_path: PathBuf::from("speech.wav"),
        duration_seconds: 2.0,
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
