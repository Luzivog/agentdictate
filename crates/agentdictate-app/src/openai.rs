use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use agentdictate_core::{JobId, ReasoningEffort, Settings, TranscriptionProvider};
use agentdictate_runtime::{ExternalError, RecordingJob, Transcriber, Transcript};
use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::model_catalog::{TranscriptionProfile, transcription_profile};

/// 32 kbps Opus reduces the 256 kbps PCM payload by roughly 8x before container
/// overhead. Recognition quality still depends on the audio and selected model.
const UPLOAD_OPUS_BITRATE: &str = "32k";

/// Audio payload actually sent to the transcription endpoint: the Opus/OGG
/// encoding of the captured WAV when ffmpeg is available, or the raw WAV
/// bytes as a fallback so a missing encoder can never lose a dictation.
struct UploadAudio {
    bytes: Vec<u8>,
    file_name: String,
    mime: &'static str,
    encode_ms: Option<u64>,
}

fn upload_file_name(audio_path: &Path, extension: &str) -> String {
    let stem = audio_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("recording");
    format!("{stem}.{extension}")
}

fn encode_ogg_opus(audio_path: &Path) -> Result<Vec<u8>, ExternalError> {
    let output = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-i"])
        .arg(audio_path)
        .args(["-ac", "1", "-ar", "16000", "-c:a", "libopus"])
        .args(["-b:a", UPLOAD_OPUS_BITRATE, "-f", "ogg", "pipe:1"])
        .output()
        .map_err(|error| ExternalError::new(format!("could not run ffmpeg: {error}")))?;
    if !output.status.success() {
        return Err(ExternalError::new(format!(
            "ffmpeg exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    if output.stdout.is_empty() {
        return Err(ExternalError::new("ffmpeg produced no audio"));
    }
    Ok(output.stdout)
}

fn prepare_upload_audio(audio_path: &Path) -> Result<UploadAudio, ExternalError> {
    let encode_started = Instant::now();
    match encode_ogg_opus(audio_path) {
        Ok(bytes) => Ok(UploadAudio {
            bytes,
            file_name: upload_file_name(audio_path, "ogg"),
            mime: "audio/ogg",
            encode_ms: Some(encode_started.elapsed().as_millis() as u64),
        }),
        Err(error) => {
            tracing::warn!(%error, "audio compression unavailable; uploading raw WAV");
            let bytes = std::fs::read(audio_path).map_err(|error| {
                ExternalError::new(format!("Could not read the captured recording: {error}"))
            })?;
            Ok(UploadAudio {
                bytes,
                file_name: upload_file_name(audio_path, "wav"),
                mime: "audio/wav",
                encode_ms: None,
            })
        }
    }
}

fn cleanup_reasoning_effort(value: &str) -> Option<&str> {
    ReasoningEffort::from_settings_value(value).and_then(ReasoningEffort::openai_value)
}

pub struct TranscriptionRequest<'a> {
    pub keywords: &'a [String],
    pub audio_path: &'a Path,
    pub provider: TranscriptionProvider,
    pub model: &'a str,
    pub language: &'a str,
    pub prompt: &'a str,
    pub duration_seconds: f64,
}

pub struct CleanupRequest<'a> {
    pub timeout: Duration,
    pub transcript: &'a str,
    pub model: &'a str,
    pub instruction: &'a str,
    pub reasoning_effort: Option<&'a str>,
}

pub trait SpeechTransport {
    fn begin_recording(
        &mut self,
        _job: &RecordingJob,
        _options: &agentdictate_core::DictationOptions,
    ) {
    }
    fn cancel_recording(&mut self, _id: JobId) {}
    fn actual_model(&self) -> Option<&str> {
        None
    }

    fn transcribe_audio(
        &mut self,
        request: TranscriptionRequest<'_>,
    ) -> Result<String, ExternalError>;
}

pub trait CleanupTransport {
    fn cleanup_text(&mut self, request: CleanupRequest<'_>) -> Result<String, ExternalError>;
}

pub struct SpeechRouter<A, C> {
    openai: A,
    chatgpt: C,
}

impl<A, C> SpeechRouter<A, C> {
    #[must_use]
    pub const fn new(openai: A, chatgpt: C) -> Self {
        Self { openai, chatgpt }
    }

    pub const fn openai_mut(&mut self) -> &mut A {
        &mut self.openai
    }
}

impl<A: SpeechTransport, C: SpeechTransport> SpeechTransport for SpeechRouter<A, C> {
    fn begin_recording(
        &mut self,
        job: &RecordingJob,
        options: &agentdictate_core::DictationOptions,
    ) {
        if job.transcription_provider == TranscriptionProvider::OpenAiApi {
            self.openai.begin_recording(job, options);
        }
    }
    fn cancel_recording(&mut self, id: JobId) {
        self.openai.cancel_recording(id);
    }
    fn actual_model(&self) -> Option<&str> {
        self.openai.actual_model()
    }

    fn transcribe_audio(
        &mut self,
        request: TranscriptionRequest<'_>,
    ) -> Result<String, ExternalError> {
        match request.provider {
            TranscriptionProvider::OpenAiApi => self.openai.transcribe_audio(request),
            TranscriptionProvider::ChatGptSubscription => self.chatgpt.transcribe_audio(request),
        }
    }
}

pub struct TranscriptionPipeline<S, C> {
    settings: Settings,
    speech: S,
    cleanup: C,
    /// Called with the job id right before the cleanup model runs, so the UI
    /// can show the cleaning phase while `transcribe` is still blocking.
    cleanup_started_observer: Option<Box<dyn Fn(JobId) + Send>>,
}

impl<S, C> TranscriptionPipeline<S, C> {
    #[must_use]
    pub fn new(settings: Settings, speech: S, cleanup: C) -> Self {
        Self {
            settings,
            speech,
            cleanup,
            cleanup_started_observer: None,
        }
    }

    pub fn set_cleanup_started_observer(&mut self, observer: impl Fn(JobId) + Send + 'static) {
        self.cleanup_started_observer = Some(Box::new(observer));
    }

    pub fn update_settings(&mut self, settings: Settings) {
        self.settings = settings;
    }

    pub const fn speech_mut(&mut self) -> &mut S {
        &mut self.speech
    }

    pub const fn cleanup_mut(&mut self) -> &mut C {
        &mut self.cleanup
    }
}

impl<S: SpeechTransport, C: CleanupTransport> Transcriber for TranscriptionPipeline<S, C> {
    fn begin_recording(&mut self, job: &RecordingJob) {
        if let Some(options) = &job.options {
            self.speech.begin_recording(job, options);
        }
    }
    fn cancel_recording(&mut self, id: JobId) {
        self.speech.cancel_recording(id);
    }

    fn transcribe(&mut self, job: &RecordingJob) -> Result<Transcript, ExternalError> {
        self.transcribe_checkpointed(job, &mut |_, _| Ok(()))
    }

    fn transcribe_checkpointed(
        &mut self,
        job: &RecordingJob,
        checkpoint: &mut agentdictate_runtime::TranscriptCheckpoint<'_>,
    ) -> Result<Transcript, ExternalError> {
        let options = job.options.clone().unwrap_or_else(|| {
            agentdictate_core::DictationOptions::from_settings(&self.settings, Vec::new())
        });
        let keywords = options.keywords();
        let raw = if !job.raw_transcript.trim().is_empty() {
            job.raw_transcript.clone()
        } else {
            match self.speech.transcribe_audio(TranscriptionRequest {
                keywords: &keywords,
                audio_path: &job.audio_path,
                provider: job.transcription_provider,
                model: &job.transcription_model,
                language: &options.language,
                prompt: &options.context,
                duration_seconds: job.duration_seconds,
            }) {
                Err(ExternalError::NoSpeech)
                    if !crate::captured_audio::is_near_silent(&job.audio_path) =>
                {
                    return Err(ExternalError::new(
                        "No speech was recognized. Audio is saved for another attempt.",
                    ));
                }
                result => result?,
            }
        };
        if raw.trim().is_empty() {
            return Err(if crate::captured_audio::is_near_silent(&job.audio_path) {
                ExternalError::NoSpeech
            } else {
                ExternalError::new("Transcription returned an empty result; audio is saved")
            });
        }
        checkpoint(
            &raw,
            if job.raw_transcript.is_empty()
                && job.transcription_provider == TranscriptionProvider::OpenAiApi
            {
                self.speech.actual_model()
            } else {
                None
            },
        )?;
        let (final_text, cleaned_text, cleanup_error) = if options.cleanup_enabled {
            if let Some(observer) = &self.cleanup_started_observer {
                observer(job.id);
            }
            let cleaned = self
                .cleanup
                .cleanup_text(CleanupRequest {
                    timeout: Duration::from_millis(u64::from(options.cleanup_timeout_ms)),
                    transcript: &raw,
                    model: &options.cleanup_model,
                    instruction: &options.cleanup_instruction,
                    reasoning_effort: cleanup_reasoning_effort(&options.cleanup_effort),
                })
                .and_then(|cleaned| {
                    agentdictate_core::validate_cleanup(&raw, &cleaned)
                        .map_err(ExternalError::new)?;
                    Ok(cleaned)
                });
            match cleaned {
                Ok(cleaned) => (cleaned.clone(), Some(cleaned), None),
                Err(error) => {
                    tracing::warn!(%error, "cleanup failed; using checkpointed raw transcript");
                    (raw.clone(), None, Some(error.to_string()))
                }
            }
        } else {
            (raw.clone(), None, None)
        };
        Ok(Transcript {
            raw,
            final_text,
            cleaned_text,
            cleanup_error,
        })
    }
}

pub struct ReqwestOpenAiTransport {
    client: reqwest::blocking::Client,
    api_key: String,
    api_base: String,
    live: Option<crate::live_transcription::LiveTranscription>,
    actual_model: Option<String>,
}

impl ReqwestOpenAiTransport {
    #[must_use]
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_api_base(api_key, "https://api.openai.com/v1")
    }

    /// Creates a transport for an OpenAI-compatible endpoint. Exposing the
    /// base URL keeps the HTTP boundary testable without real network calls.
    #[must_use]
    pub fn with_api_base(api_key: impl Into<String>, api_base: impl Into<String>) -> Self {
        Self {
            client: reqwest::blocking::Client::builder()
                .connect_timeout(Duration::from_secs(20))
                .timeout(Duration::from_secs(180))
                .build()
                .expect("the rustls HTTP client must be constructible"),
            api_key: api_key.into().trim().to_owned(),
            api_base: api_base.into().trim_end_matches('/').to_owned(),
            actual_model: None,
            live: None,
        }
    }

    pub fn set_api_key(&mut self, api_key: impl Into<String>) {
        self.api_key = api_key.into().trim().to_owned();
    }

    fn authorization(&self) -> Result<String, ExternalError> {
        if self.api_key.is_empty() {
            return Err(ExternalError::new(
                "OpenAI API key missing. Paste your API key in AgentDictate settings.",
            ));
        }
        Ok(format!("Bearer {}", self.api_key))
    }

    fn response_error(status: StatusCode, body: &str) -> ExternalError {
        if status == StatusCode::UNAUTHORIZED {
            return ExternalError::new("OpenAI authentication failed. Check your API key.");
        }
        let message = serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|payload| {
                payload
                    .pointer("/error/message")
                    .or_else(|| payload.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .filter(|message| !message.trim().is_empty())
            .unwrap_or_else(|| body.trim().to_owned());
        if matches!(
            status,
            StatusCode::REQUEST_TIMEOUT
                | StatusCode::CONFLICT
                | StatusCode::TOO_MANY_REQUESTS
                | StatusCode::INTERNAL_SERVER_ERROR
                | StatusCode::BAD_GATEWAY
                | StatusCode::SERVICE_UNAVAILABLE
                | StatusCode::GATEWAY_TIMEOUT
        ) {
            return ExternalError::new(format!(
                "Could not reach OpenAI or the request was not accepted: {message}"
            ));
        }
        ExternalError::new(if message.is_empty() {
            format!("OpenAI request failed with status {status}")
        } else {
            message
        })
    }
}

impl SpeechTransport for ReqwestOpenAiTransport {
    fn begin_recording(
        &mut self,
        job: &RecordingJob,
        options: &agentdictate_core::DictationOptions,
    ) {
        self.live = None;
        self.actual_model = None;
        if options.streaming && job.transcription_provider == TranscriptionProvider::OpenAiApi {
            let url = format!(
                "{}/realtime?intent=transcription",
                self.api_base
                    .replacen("https://", "wss://", 1)
                    .replacen("http://", "ws://", 1)
            );
            match crate::live_transcription::LiveTranscription::start(
                job.id,
                job.audio_path.clone(),
                options.clone(),
                self.api_key.clone(),
                url,
            ) {
                Ok(live) => self.live = Some(live),
                Err(error) => {
                    tracing::warn!(%error, "live transcription startup failed; buffered audio remains available")
                }
            }
        }
    }
    fn cancel_recording(&mut self, id: JobId) {
        if self.live.as_ref().is_some_and(|live| live.job_id == id) {
            self.live = None;
        }
    }
    fn actual_model(&self) -> Option<&str> {
        self.actual_model.as_deref()
    }

    fn transcribe_audio(
        &mut self,
        request: TranscriptionRequest<'_>,
    ) -> Result<String, ExternalError> {
        if let Some(live) = self
            .live
            .take()
            .filter(|live| live.audio_path == request.audio_path)
        {
            match live.finish() {
                Ok(text) => {
                    self.actual_model = Some("gpt-live-transcribe".into());
                    tracing::info!(
                        model = "gpt-live-transcribe",
                        "live transcription completed"
                    );
                    return Ok(text);
                }
                Err(error) => {
                    tracing::warn!(%error, "live transcription failed; falling back to file transcription")
                }
            }
        }
        self.actual_model = Some(request.model.to_owned());
        if request.model.trim().is_empty() {
            return Err(ExternalError::new(
                "The selected transcription model could not be used. Choose another model or check the custom model name.",
            ));
        }
        let profile =
            transcription_profile(request.model).unwrap_or(TranscriptionProfile::Standard);
        if profile != TranscriptionProfile::AgentDictateGpt && request.language.contains(',') {
            return Err(ExternalError::new(
                "This model accepts one language hint; choose one language or automatic detection",
            ));
        }
        let upload = prepare_upload_audio(request.audio_path)?;
        let upload_bytes = upload.bytes.len();
        let file = reqwest::blocking::multipart::Part::bytes(upload.bytes)
            .file_name(upload.file_name)
            .mime_str(upload.mime)
            .map_err(|error| ExternalError::new(format!("Invalid audio upload: {error}")))?;
        let mut form = reqwest::blocking::multipart::Form::new()
            .text("model", request.model.to_owned())
            .part("file", file);
        let language = request.language.trim();
        let prompt = request.prompt.trim();
        match profile {
            TranscriptionProfile::AgentDictateGpt => {
                form = form.text("response_format", "json");
                if !language.is_empty() {
                    for language in language.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                        form = form.text("languages[]", language.to_owned());
                    }
                }
                for keyword in request.keywords {
                    form = form.text("keywords[]", keyword.clone());
                }
            }
            TranscriptionProfile::OpenAiGpt => {
                form = form.text("response_format", "json");
                if !language.is_empty() {
                    form = form.text("language", language.to_owned());
                }
            }
            TranscriptionProfile::Standard => {
                form = form.text("response_format", "text");
                if !language.is_empty() {
                    form = form.text("language", language.to_owned());
                }
            }
        }
        if !prompt.is_empty() {
            form = form.text("prompt", prompt.to_owned());
        }
        let request_started = Instant::now();
        let response = self
            .client
            .post(format!("{}/audio/transcriptions", self.api_base))
            .header("Authorization", self.authorization()?)
            .multipart(form)
            .send()
            .map_err(|error| ExternalError::new(format!("Could not reach OpenAI: {error}")))?;
        let status = response.status();
        let body = response.text().map_err(|error| {
            ExternalError::new(format!("Could not read OpenAI's response: {error}"))
        })?;
        let request_ms = request_started.elapsed().as_millis() as u64;
        if !status.is_success() {
            return Err(Self::response_error(status, &body));
        }
        let text =
            if profile != TranscriptionProfile::Standard || body.trim_start().starts_with('{') {
                serde_json::from_str::<Value>(&body)
                    .ok()
                    .and_then(|payload| {
                        payload
                            .get("text")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .ok_or_else(|| {
                        ExternalError::new("OpenAI returned an invalid transcription response")
                    })?
            } else {
                body.trim().to_owned()
            };
        let text = text.trim().to_owned();
        tracing::info!(
            model = request.model,
            audio_seconds = request.duration_seconds,
            upload_bytes,
            encode_ms = upload.encode_ms,
            request_ms,
            transcript_chars = text.chars().count(),
            "transcription request completed"
        );
        if text.is_empty() {
            return Err(ExternalError::NoSpeech);
        }
        Ok(text)
    }
}

impl CleanupTransport for ReqwestOpenAiTransport {
    fn cleanup_text(&mut self, request: CleanupRequest<'_>) -> Result<String, ExternalError> {
        if request.model.trim().is_empty() {
            return Err(ExternalError::new(
                "The selected cleanup model could not be used. Choose another model or check the custom model name.",
            ));
        }
        let mut payload = json!({
            "model": request.model,
            "instructions": request.instruction,
            "input": request.transcript,
            "text": {"format": {"type": "text"}},
        });
        if let Some(effort) = request.reasoning_effort {
            payload["reasoning"] = json!({"effort": effort});
        }
        let request_started = Instant::now();
        let response = self
            .client
            .post(format!("{}/responses", self.api_base))
            .timeout(request.timeout)
            .header("Authorization", self.authorization()?)
            .json(&payload)
            .send()
            .map_err(|error| ExternalError::new(format!("Could not reach OpenAI: {error}")))?;
        let status = response.status();
        let body = response.text().map_err(|error| {
            ExternalError::new(format!("Could not read OpenAI's response: {error}"))
        })?;
        let request_ms = request_started.elapsed().as_millis() as u64;
        if !status.is_success() {
            return Err(Self::response_error(status, &body));
        }
        let payload: Value = serde_json::from_str(&body)
            .map_err(|_| ExternalError::new("OpenAI returned an invalid cleanup response."))?;
        if payload.get("status").and_then(Value::as_str) != Some("completed") {
            return Err(ExternalError::new(
                "Cleanup did not complete; using the transcript",
            ));
        }
        if payload
            .get("output")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| item.get("content").and_then(Value::as_array))
            .flatten()
            .any(|part| part.get("type").and_then(Value::as_str) == Some("refusal"))
        {
            return Err(ExternalError::new(
                "Cleanup refused the edit; using the transcript",
            ));
        }
        let text = payload
            .get("output")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| item.get("content").and_then(Value::as_array))
            .flatten()
            .filter(|content| content.get("type").and_then(Value::as_str) == Some("output_text"))
            .filter_map(|content| content.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("")
            .trim()
            .to_owned();
        let input_tokens = payload
            .pointer("/usage/input_tokens")
            .and_then(Value::as_u64);
        let output_tokens = payload
            .pointer("/usage/output_tokens")
            .and_then(Value::as_u64);
        tracing::info!(
            model = request.model,
            effort = request.reasoning_effort,
            request_ms,
            input_tokens,
            output_tokens,
            output_chars = text.chars().count(),
            "cleanup request completed"
        );
        if text.is_empty() {
            return Err(ExternalError::new("Cleanup returned an empty response."));
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::{cleanup_reasoning_effort, encode_ogg_opus, upload_file_name};

    #[test]
    fn encoding_an_unreadable_recording_reports_an_error_instead_of_panicking() {
        let directory = tempfile::tempdir().unwrap();
        let audio_path = directory.path().join("recording.wav");
        std::fs::write(&audio_path, b"not a wav file").unwrap();

        assert!(encode_ogg_opus(&audio_path).is_err());
    }

    #[test]
    fn a_valid_wav_encodes_to_an_ogg_opus_payload() {
        if !std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            eprintln!("skipping: ffmpeg is not installed");
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let audio_path = directory.path().join("recording.wav");
        std::fs::write(&audio_path, tiny_wav()).unwrap();

        let encoded = encode_ogg_opus(&audio_path).unwrap();

        assert!(encoded.starts_with(b"OggS"));
        assert_eq!(upload_file_name(&audio_path, "ogg"), "recording.ogg");
    }

    /// 100 ms of 16 kHz mono s16 silence with a canonical 44-byte header.
    fn tiny_wav() -> Vec<u8> {
        let samples: u32 = 1600;
        let data_len = samples * 2;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&16000_u32.to_le_bytes());
        bytes.extend_from_slice(&32000_u32.to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        bytes.resize(bytes.len() + data_len as usize, 0);
        bytes
    }

    #[test]
    fn every_reasoning_effort_exposed_by_the_catalog_is_forwarded() {
        for effort in ["none", "minimal", "low", "medium", "high", "xhigh", "max"] {
            assert_eq!(cleanup_reasoning_effort(effort), Some(effort));
        }
        assert_eq!(cleanup_reasoning_effort("default"), None);
        assert_eq!(cleanup_reasoning_effort("unsupported"), None);
    }
}
