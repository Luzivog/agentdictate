use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agentdictate_core::{DictationOptions, Settings};
use agentdictate_linux::command::{
    PlatformCapability, PlatformCommandError, PlatformExecutable, PlatformTool, SystemCommandRunner,
};
use agentdictate_runtime::{ExternalError, RecordingJob, Transcript};

use crate::Transcriber;
use reqwest::StatusCode;
use serde_json::Value;

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

pub struct TranscriptionRequest<'a> {
    pub keywords: &'a [String],
    pub audio_path: &'a Path,
    pub model: &'a str,
    pub language: &'a str,
    pub prompt: &'a str,
    pub duration_seconds: f64,
}

/// The speech-to-text service boundary; tests substitute a fake.
pub trait SpeechTransport: Clone + Send + 'static {
    fn set_api_key(&mut self, _api_key: &str) {}

    fn transcribe_audio(
        &mut self,
        request: TranscriptionRequest<'_>,
    ) -> Result<String, ExternalError>;
}

/// The production `Transcriber`: sends the recording to the speech transport
/// with the options stored on its job.
#[derive(Clone)]
pub struct TranscriptionPipeline<S> {
    settings: Settings,
    speech: S,
}

impl<S> TranscriptionPipeline<S> {
    #[must_use]
    pub const fn new(settings: Settings, speech: S) -> Self {
        Self { settings, speech }
    }
}

impl<S: SpeechTransport> Transcriber for TranscriptionPipeline<S> {
    /// Transcribes the saved audio. An empty result counts as no speech only
    /// when the audio is near-silent; otherwise the audio is kept for another
    /// attempt, so a misheard dictation is never silently dropped.
    fn transcribe(&mut self, job: &RecordingJob) -> Result<Transcript, ExternalError> {
        let options = job
            .options
            .clone()
            .unwrap_or_else(|| DictationOptions::from_settings(&self.settings));
        let keywords = options.keywords();
        let text = match self.speech.transcribe_audio(TranscriptionRequest {
            keywords: &keywords,
            audio_path: &job.audio_path,
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
        };
        if text.trim().is_empty() {
            return Err(if crate::captured_audio::is_near_silent(&job.audio_path) {
                ExternalError::NoSpeech
            } else {
                ExternalError::new("Transcription returned an empty result; audio is saved")
            });
        }
        Ok(Transcript {
            text,
            model: job.transcription_model.clone(),
        })
    }

    fn update_settings(&mut self, settings: &Settings) {
        self.speech.set_api_key(&settings.openai_api_key);
        self.settings = settings.clone();
    }
}

/// OpenAI's HTTP transcription endpoint. Clones share one connection pool.
#[derive(Clone)]
pub struct ReqwestOpenAiTransport {
    client: reqwest::blocking::Client,
    api_key: String,
    api_base: String,
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
    fn set_api_key(&mut self, api_key: &str) {
        self.api_key = api_key.trim().to_owned();
    }

    fn transcribe_audio(
        &mut self,
        request: TranscriptionRequest<'_>,
    ) -> Result<String, ExternalError> {
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

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    use agentdictate_linux::command::{PlatformExecutable, PlatformTool};

    use super::{UploadFormat, encode_webm_opus, prepare_upload_audio};

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
}
