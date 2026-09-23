use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agentdictate_core::{JobId, ReasoningEffort, Settings, TranscriptionProvider};
use agentdictate_linux::command::{
    PlatformCapability, PlatformCommandError, PlatformExecutable, PlatformTool, SystemCommandRunner,
};
use agentdictate_runtime::{ExternalError, RecordingJob, Transcriber, Transcript};
use reqwest::StatusCode;
use serde_json::{Value, json};

/// 32 kbps Opus reduces the 256 kbps PCM payload by roughly 8x before container
/// overhead. Recognition quality still depends on the audio and selected model.
const UPLOAD_OPUS_BITRATE: &str = "32k";

/// Container of the audio sent to the transcription endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UploadFormat {
    /// Opus in WebM, a format OpenAI documents for transcription uploads.
    WebmOpus,
    /// The captured recording itself, about 8x larger.
    Wav,
}

impl UploadFormat {
    const fn file_name(self) -> &'static str {
        match self {
            Self::WebmOpus => "recording.webm",
            Self::Wav => "recording.wav",
        }
    }

    const fn mime(self) -> &'static str {
        match self {
            Self::WebmOpus => "audio/webm",
            Self::Wav => "audio/wav",
        }
    }
}

/// Audio payload actually sent to the transcription endpoint: the WebM/Opus
/// encoding of the captured WAV when ffmpeg is available, or the raw WAV
/// bytes as a fallback so a missing encoder can never lose a dictation.
struct UploadAudio {
    bytes: Vec<u8>,
    format: UploadFormat,
    encode_ms: Option<u64>,
}

/// Bounds a hung encoder. ffmpeg normally needs ~13 ms per audio-second, so
/// this only fires when it is stuck; the WAV is uploaded instead.
fn encode_deadline(audio_seconds: f64) -> Instant {
    let scaled = Duration::try_from_secs_f64(audio_seconds.max(0.0) / 2.0).unwrap_or_default();
    Instant::now() + Duration::from_secs(10) + scaled
}

fn encode_webm_opus(
    ffmpeg: &PlatformExecutable,
    audio_path: &Path,
    deadline: Instant,
) -> Result<Vec<u8>, PlatformCommandError> {
    let mut arguments = vec![OsString::from("-loglevel"), "error".into(), "-i".into()];
    arguments.push(audio_path.into());
    // `-application voip` keeps libopus in its speech-optimized mode.
    arguments.extend(
        [
            "-ac",
            "1",
            "-ar",
            "16000",
            "-c:a",
            "libopus",
            "-b:a",
            UPLOAD_OPUS_BITRATE,
            "-application",
            "voip",
            "-f",
            "webm",
            "pipe:1",
        ]
        .map(OsString::from),
    );
    let encoded = SystemCommandRunner.run_output(
        PlatformCapability::AudioCompression,
        ffmpeg,
        &arguments,
        deadline,
    )?;
    if encoded.is_empty() {
        return Err(PlatformCommandError::UnexpectedOutput {
            tool: PlatformTool::Ffmpeg,
            detail: "no audio",
        });
    }
    Ok(encoded)
}

fn prepare_upload_audio(
    ffmpeg: &PlatformExecutable,
    audio_path: &Path,
    deadline: Instant,
) -> Result<UploadAudio, ExternalError> {
    let encode_started = Instant::now();
    match encode_webm_opus(ffmpeg, audio_path, deadline) {
        Ok(bytes) => Ok(UploadAudio {
            bytes,
            format: UploadFormat::WebmOpus,
            encode_ms: Some(encode_started.elapsed().as_millis() as u64),
        }),
        Err(error) => {
            tracing::warn!(%error, "audio compression unavailable; uploading raw WAV");
            wav_upload(audio_path)
        }
    }
}

fn wav_upload(audio_path: &Path) -> Result<UploadAudio, ExternalError> {
    let bytes = std::fs::read(audio_path).map_err(|error| {
        ExternalError::new(format!("Could not read the captured recording: {error}"))
    })?;
    Ok(UploadAudio {
        bytes,
        format: UploadFormat::Wav,
        encode_ms: None,
    })
}

/// True when OpenAI answered HTTP 400 with an error about the uploaded file
/// or its format. The WAV original is then worth one more attempt.
fn rejects_upload_format(status: StatusCode, body: &str) -> bool {
    if status != StatusCode::BAD_REQUEST {
        return false;
    }
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|payload| {
            payload
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| body.to_owned())
        .to_ascii_lowercase();
    ["file", "format", "audio"]
        .iter()
        .any(|word| message.contains(word))
}

/// Builds the multipart body for one transcription attempt, in the request
/// shape of `gpt-transcribe`: a JSON response, a `languages[]` list and
/// vocabulary `keywords[]`. A multipart form is consumed by sending, so every
/// attempt builds its own.
fn transcription_form(
    request: &TranscriptionRequest<'_>,
    upload: &UploadAudio,
) -> Result<reqwest::blocking::multipart::Form, ExternalError> {
    let file = reqwest::blocking::multipart::Part::bytes(upload.bytes.clone())
        .file_name(upload.format.file_name())
        .mime_str(upload.format.mime())
        .map_err(|error| ExternalError::new(format!("Invalid audio upload: {error}")))?;
    let mut form = reqwest::blocking::multipart::Form::new()
        .text("model", request.model.to_owned())
        .text("response_format", "json")
        .part("file", file);
    let prompt = request.prompt.trim();
    for language in request
        .language
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        form = form.text("languages[]", language.to_owned());
    }
    for keyword in request.keywords {
        form = form.text("keywords[]", keyword.clone());
    }
    if !prompt.is_empty() {
        form = form.text("prompt", prompt.to_owned());
    }
    Ok(form)
}

/// The shared HTTP client configuration for OpenAI requests.
fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(180))
        .build()
        .expect("the rustls HTTP client must be constructible")
}

/// True when a request failed before OpenAI returned any status, so OpenAI
/// cannot have answered it. The total 180 s timeout is excluded: the upload
/// may already be processing, and a second full wait is worse than handing
/// the audio to Recovery.
fn failed_before_status(error: &reqwest::Error) -> bool {
    (error.is_request() || error.is_body()) && (error.is_connect() || !error.is_timeout())
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
    ffmpeg: PlatformExecutable,
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
            client: http_client(),
            api_key: api_key.into().trim().to_owned(),
            api_base: api_base.into().trim_end_matches('/').to_owned(),
            actual_model: None,
            live: None,
            ffmpeg: PlatformExecutable::discover(PlatformTool::Ffmpeg),
        }
    }

    /// Uses `program` instead of the `ffmpeg` found on `PATH` to compress
    /// uploads, so tests can exercise the encoder boundary with a fake.
    #[must_use]
    pub fn with_audio_encoder(mut self, program: impl Into<PathBuf>) -> Self {
        self.ffmpeg = PlatformExecutable::at(PlatformTool::Ffmpeg, program);
        self
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

    /// Posts one transcription form and returns OpenAI's status and body.
    /// A failure before any status arrives (connecting, sending the request,
    /// or streaming its body) is retried once, immediately, on a fresh
    /// connection pool; the classic cause is a pooled keep-alive connection
    /// that the network silently dropped. Nothing is retried once a status
    /// arrives, so an HTTP error is never repeated here.
    fn send_transcription(
        &mut self,
        form: impl Fn() -> Result<reqwest::blocking::multipart::Form, ExternalError>,
    ) -> Result<(StatusCode, String), ExternalError> {
        let url = format!("{}/audio/transcriptions", self.api_base);
        let authorization = self.authorization()?;
        let send = |client: &reqwest::blocking::Client, form| {
            client
                .post(&url)
                .header("Authorization", &authorization)
                .multipart(form)
                .send()
        };
        let response = match send(&self.client, form()?) {
            Ok(response) => response,
            Err(error) if failed_before_status(&error) => {
                tracing::warn!(%error, "transcription request retried");
                self.client = http_client();
                send(&self.client, form()?).map_err(|error| {
                    ExternalError::new(format!("Could not reach OpenAI: {error}"))
                })?
            }
            Err(error) => {
                return Err(ExternalError::new(format!(
                    "Could not reach OpenAI: {error}"
                )));
            }
        };
        let status = response.status();
        let body = response.text().map_err(|error| {
            ExternalError::new(format!("Could not read OpenAI's response: {error}"))
        })?;
        Ok((status, body))
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
        let mut upload = prepare_upload_audio(
            &self.ffmpeg,
            request.audio_path,
            encode_deadline(request.duration_seconds),
        )?;
        let request_started = Instant::now();
        let (mut status, mut body) =
            self.send_transcription(|| transcription_form(&request, &upload))?;
        if upload.format == UploadFormat::WebmOpus && rejects_upload_format(status, &body) {
            tracing::warn!(
                error = %Self::response_error(status, &body),
                "OpenAI rejected the compressed audio; retrying with the original WAV"
            );
            upload = wav_upload(request.audio_path)?;
            (status, body) = self.send_transcription(|| transcription_form(&request, &upload))?;
        }
        let request_ms = request_started.elapsed().as_millis() as u64;
        if !status.is_success() {
            return Err(Self::response_error(status, &body));
        }
        let text = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|payload| {
                payload
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .ok_or_else(|| {
                ExternalError::new("OpenAI returned an invalid transcription response")
            })?;
        let text = text.trim().to_owned();
        tracing::info!(
            model = request.model,
            audio_seconds = request.duration_seconds,
            upload_format = ?upload.format,
            upload_bytes = upload.bytes.len(),
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
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    use agentdictate_linux::command::{PlatformExecutable, PlatformTool};

    use super::{UploadFormat, cleanup_reasoning_effort, encode_webm_opus, prepare_upload_audio};

    #[test]
    fn a_valid_wav_encodes_to_a_webm_opus_payload() {
        let ffmpeg = PlatformExecutable::discover(PlatformTool::Ffmpeg);
        if ffmpeg.path().is_none() {
            eprintln!("skipping: ffmpeg is not installed");
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let audio_path = directory.path().join("recording.wav");
        std::fs::write(&audio_path, tiny_wav()).unwrap();

        let encoded = encode_webm_opus(
            &ffmpeg,
            &audio_path,
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap();

        // Every WebM file starts with the EBML magic number.
        assert!(encoded.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]));
    }

    #[test]
    fn a_hung_encoder_is_stopped_at_its_deadline_and_the_wav_is_uploaded() {
        let directory = tempfile::tempdir().unwrap();
        let audio_path = directory.path().join("recording.wav");
        std::fs::write(&audio_path, tiny_wav()).unwrap();
        let ffmpeg = directory.path().join("ffmpeg");
        std::fs::write(&ffmpeg, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = Instant::now();

        let upload = prepare_upload_audio(
            &PlatformExecutable::at(PlatformTool::Ffmpeg, ffmpeg),
            &audio_path,
            started + Duration::from_millis(200),
        )
        .unwrap();

        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(upload.format, UploadFormat::Wav);
        assert_eq!(upload.bytes, tiny_wav());
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
