use std::path::Path;

use agentdictate_core::TranscriptionProvider;
use agentdictate_runtime::{ExternalError, Recorder, RecordingJob, RecordingRequest};
use chrono::{TimeZone, Utc};

pub(crate) struct ReadyRecorder;

impl Recorder for ReadyRecorder {
    fn start(&mut self, _job: &RecordingJob) -> Result<(), ExternalError> {
        Ok(())
    }
}

pub(crate) fn request(audio_path: &Path, transcription_model: &str) -> RecordingRequest {
    request_with_provider(
        audio_path,
        TranscriptionProvider::OpenAiApi,
        transcription_model,
    )
}

pub(crate) fn request_with_provider(
    audio_path: &Path,
    transcription_provider: TranscriptionProvider,
    transcription_model: &str,
) -> RecordingRequest {
    RecordingRequest {
        options: None,
        audio_path: audio_path.to_owned(),
        started_at: Utc.with_ymd_and_hms(2026, 8, 18, 12, 0, 0).unwrap(),
        transcription_provider,
        transcription_model: transcription_model.to_owned(),
    }
}
