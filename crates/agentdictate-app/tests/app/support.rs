use std::path::PathBuf;

use agentdictate_app::{CapturedRecording, RecordingController};
use agentdictate_core::JobStage;
use agentdictate_runtime::{
    ExternalError, Recorder, RecordingJob, Runtime, Transcriber, Transcript,
};

pub(crate) struct InspectingRecorder {
    pub(crate) database: PathBuf,
    pub(crate) started_after_checkpoint: bool,
}

impl Recorder for InspectingRecorder {
    fn start(&mut self, job: &RecordingJob) -> Result<(), ExternalError> {
        let observer = Runtime::open_observer(&self.database).map_err(ExternalError::from)?;
        self.started_after_checkpoint = observer
            .job(job.id)
            .map_err(ExternalError::from)?
            .is_some_and(|stored| stored.stage == JobStage::Starting);
        std::fs::write(&job.audio_path, b"RIFFcaptured audio").unwrap();
        Ok(())
    }
}

impl RecordingController for InspectingRecorder {
    fn finish(&mut self, _job: &RecordingJob) -> Result<CapturedRecording, ExternalError> {
        Ok(CapturedRecording {
            duration_seconds: 12.5,
        })
    }
}

pub(crate) struct FixedTranscriber;

impl Transcriber for FixedTranscriber {
    fn transcribe(&mut self, _job: &RecordingJob) -> Result<Transcript, ExternalError> {
        Ok(Transcript {
            raw: "raw transcript".into(),
            final_text: "Final transcript.".into(),
            cleaned_text: Some("Final transcript.".into()),
            cleanup_error: None,
        })
    }
}

pub(crate) struct FailingStartRecorder;

impl Recorder for FailingStartRecorder {
    fn start(&mut self, _job: &RecordingJob) -> Result<(), ExternalError> {
        Err(ExternalError::new("microphone permission denied"))
    }
}

impl RecordingController for FailingStartRecorder {
    fn finish(&mut self, _job: &RecordingJob) -> Result<CapturedRecording, ExternalError> {
        unreachable!("a recorder that did not start cannot be finalized")
    }
}
