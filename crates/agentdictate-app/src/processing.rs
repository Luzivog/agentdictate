//! Transcription of one captured job, away from the daemon lock.

use std::time::Instant;

use agentdictate_core::{JobId, Settings};
use agentdictate_runtime::{ExternalError, RecordingJob, Transcript, TranscriptionOutcome};

use crate::FinishingEncode;

/// Turns one captured job into text. The daemon keeps one transcriber and
/// clones it for every job, whose transcription then runs without the daemon
/// lock, so an implementation must never touch the database.
pub trait Transcriber: Clone + Send + 'static {
    /// Transcribes `job`. `encoding` is its upload audio, encoded while it
    /// recorded; without it, the saved WAV is encoded now.
    fn transcribe(
        &mut self,
        job: &RecordingJob,
        encoding: Option<FinishingEncode>,
    ) -> Result<Transcript, ExternalError>;

    /// Follows saved settings: the API key, and the options of jobs recorded
    /// before options were stored with them.
    fn update_settings(&mut self, _settings: &Settings) {}
}

/// Everything one job's transcription needs, moved out from under the daemon
/// lock. Only the daemon creates one, after the job's `transcribing`
/// checkpoint, and each ticket yields exactly one completion.
#[must_use]
pub struct ProcessingTicket<T> {
    job: RecordingJob,
    /// Cloned when the job stopped, so settings saved meanwhile never change
    /// the job's transcription.
    transcriber: T,
    /// The upload audio encoded while the job recorded, if any.
    encoding: Option<FinishingEncode>,
}

impl<T> std::fmt::Debug for ProcessingTicket<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessingTicket")
            .field("job_id", &self.job.id)
            .finish_non_exhaustive()
    }
}

impl<T: Transcriber> ProcessingTicket<T> {
    pub(crate) const fn new(
        job: RecordingJob,
        transcriber: T,
        encoding: Option<FinishingEncode>,
    ) -> Self {
        Self {
            job,
            transcriber,
            encoding,
        }
    }

    #[must_use]
    pub const fn job_id(&self) -> JobId {
        self.job.id
    }

    /// Transcribes the job. A transcript an earlier attempt already stored is
    /// reused without another paid request.
    pub fn run(mut self) -> TranscriptionCompletion {
        let started = Instant::now();
        let outcome = self.outcome();
        tracing::info!(
            job_id = %self.job.id,
            transcription_ms = started.elapsed().as_millis() as u64,
            outcome = match &outcome {
                TranscriptionOutcome::Text(_) => "text",
                TranscriptionOutcome::NoSpeech => "no_speech",
                TranscriptionOutcome::Failed { .. } => "failed",
            },
            "transcription finished"
        );
        TranscriptionCompletion {
            job_id: self.job.id,
            outcome,
            finished_at: Instant::now(),
        }
    }

    fn outcome(&mut self) -> TranscriptionOutcome {
        if !self.job.raw_transcript.trim().is_empty() {
            return TranscriptionOutcome::Text(Transcript {
                text: self.job.raw_transcript.clone(),
                model: self.job.transcription_model.clone(),
            });
        }
        match self.transcriber.transcribe(&self.job, self.encoding.take()) {
            Ok(transcript) => TranscriptionOutcome::Text(transcript),
            Err(ExternalError::NoSpeech) => TranscriptionOutcome::NoSpeech,
            Err(ExternalError::Failure { message }) => TranscriptionOutcome::Failed { message },
        }
    }
}

/// The result of one ticket, handed back to the daemon under its lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptionCompletion {
    pub job_id: JobId,
    pub outcome: TranscriptionOutcome,
    pub finished_at: Instant,
}

impl TranscriptionCompletion {
    /// A transcription that could not run at all, for example because its
    /// thread panicked. The job fails and keeps its audio for Recovery.
    #[must_use]
    pub fn failed(job_id: JobId, message: impl Into<String>) -> Self {
        Self {
            job_id,
            outcome: TranscriptionOutcome::Failed {
                message: message.into(),
            },
            finished_at: Instant::now(),
        }
    }
}
