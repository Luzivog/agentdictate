use std::path::Path;
use std::time::Duration;

use agentdictate_core::{
    DEFAULT_CLEANUP_PROMPT, ReasoningEffort, Settings, TranscriptionProvider,
    count_words_unicode_alphanumeric,
};
use agentdictate_runtime::{ExternalError, RecordingJob, Transcriber, Transcript};
use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::model_catalog::{TranscriptionProfile, transcription_profile};

const TRANSCRIPTION_COMPLETENESS_PROMPT: &str = "Transcribe the entire recording from beginning to end. Include every spoken sentence and phrase. Do not summarize, omit later sentences, or stop after the first sentence.";
const FALLBACK_TRANSCRIPTION_MODEL: &str = "whisper-1";

fn sentence_count(text: &str) -> usize {
    text.split(['.', '!', '?'])
        .filter(|part| !part.trim().is_empty())
        .count()
}

fn is_better_transcript(candidate: &str, current: &str) -> bool {
    if candidate.trim().is_empty() {
        return false;
    }
    let candidate_words = count_words_unicode_alphanumeric(candidate);
    let current_words = count_words_unicode_alphanumeric(current);
    candidate_words > current_words
        && (sentence_count(candidate) > sentence_count(current)
            || candidate_words >= (current_words + 5).max((current_words * 135) / 100))
}

fn transcript_is_suspiciously_short(text: &str, duration_seconds: f64) -> bool {
    if duration_seconds < 10.0 {
        return false;
    }
    let conservative_minimum = (duration_seconds / 5.0).ceil().max(4.0) as usize;
    count_words_unicode_alphanumeric(text) < conservative_minimum
}

fn cleanup_reasoning_effort(value: &str) -> Option<&str> {
    ReasoningEffort::from_settings_value(value).and_then(ReasoningEffort::openai_value)
}

fn build_cleanup_instruction(style: &str, custom_prompt: &str) -> String {
    let base = if custom_prompt.trim().is_empty() {
        DEFAULT_CLEANUP_PROMPT
    } else {
        custom_prompt.trim()
    };
    if style == "Structured coding prompt" {
        format!(
            "{base}\n\nCleanup style: Structured coding prompt. Use short bullets and sections only when helpful. Possible sections: Goal, Requirements, Constraints, Testing, Notes."
        )
    } else {
        format!(
            "{base}\n\nCleanup style: Light cleanup. Keep wording and structure close to the transcript. Do not invent details."
        )
    }
}

pub struct TranscriptionRequest<'a> {
    pub audio_path: &'a Path,
    pub provider: TranscriptionProvider,
    pub model: &'a str,
    pub language: &'a str,
    pub prompt: &'a str,
    pub duration_seconds: f64,
}

pub struct CleanupRequest<'a> {
    pub transcript: &'a str,
    pub model: &'a str,
    pub instruction: &'a str,
    pub reasoning_effort: Option<&'a str>,
}

pub trait SpeechTransport {
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
}

impl<S, C> TranscriptionPipeline<S, C> {
    #[must_use]
    pub fn new(settings: Settings, speech: S, cleanup: C) -> Self {
        Self {
            settings,
            speech,
            cleanup,
        }
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
    fn transcribe(&mut self, job: &RecordingJob) -> Result<Transcript, ExternalError> {
        let raw = self.speech.transcribe_audio(TranscriptionRequest {
            audio_path: &job.audio_path,
            provider: job.transcription_provider,
            model: &job.transcription_model,
            language: &self.settings.language,
            prompt: &self.settings.transcription_prompt,
            duration_seconds: job.duration_seconds,
        })?;
        if raw.trim().is_empty() {
            return Err(ExternalError::new("Transcription returned an empty result"));
        }

        let (final_text, cleaned_text, cleanup_error) = if self.settings.cleanup_enabled {
            let effort = cleanup_reasoning_effort(&self.settings.cleanup_reasoning_effort);
            let instruction = build_cleanup_instruction(
                &self.settings.cleanup_style,
                &self.settings.cleanup_prompt,
            );
            match self.cleanup.cleanup_text(CleanupRequest {
                transcript: &raw,
                model: self.settings.active_cleanup_model(),
                instruction: &instruction,
                reasoning_effort: effort,
            }) {
                Ok(cleaned) if !cleaned.trim().is_empty() => (cleaned.clone(), Some(cleaned), None),
                Ok(_) => (
                    raw.clone(),
                    None,
                    Some("Cleanup returned an empty response.".to_owned()),
                ),
                Err(error) => {
                    tracing::warn!(%error, "cleanup failed; using durable raw transcript");
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

    fn transcribe_once(
        &self,
        request: &TranscriptionRequest<'_>,
        model: &str,
    ) -> Result<String, ExternalError> {
        let profile = transcription_profile(model).unwrap_or(TranscriptionProfile::Standard);
        let audio = std::fs::read(request.audio_path).map_err(|error| {
            ExternalError::new(format!("Could not read the captured recording: {error}"))
        })?;
        let file_name = request
            .audio_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("recording.wav")
            .to_owned();
        let file = reqwest::blocking::multipart::Part::bytes(audio)
            .file_name(file_name)
            .mime_str("audio/wav")
            .map_err(|error| ExternalError::new(format!("Invalid audio upload: {error}")))?;
        let mut form = reqwest::blocking::multipart::Form::new()
            .text("model", model.to_owned())
            .part("file", file);
        let language = request.language.trim();
        let prompt = request.prompt.trim();
        if profile == TranscriptionProfile::AgentDictateGpt {
            form = form.text("response_format", "json");
            if !language.is_empty() {
                form = form.text("languages[]", language.to_owned());
            }
            if !prompt.is_empty() {
                form = form.text("prompt", prompt.to_owned());
            }
        } else if profile == TranscriptionProfile::OpenAiGpt {
            form = form.text("response_format", "json");
            if !language.is_empty() {
                form = form.text("language", language.to_owned());
            }
            if !prompt.is_empty() {
                form = form.text("prompt", prompt.to_owned());
            }
        } else {
            form = form.text("response_format", "text");
            if !language.is_empty() {
                form = form.text("language", language.to_owned());
            }
            let legacy_prompt = if prompt.is_empty() {
                TRANSCRIPTION_COMPLETENESS_PROMPT.to_owned()
            } else {
                format!("{TRANSCRIPTION_COMPLETENESS_PROMPT}\n\nContext and vocabulary:\n{prompt}")
            };
            form = form.text("prompt", legacy_prompt);
        }
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
                    .unwrap_or_else(|| body.trim().to_owned())
            } else {
                body.trim().to_owned()
            };
        if text.trim().is_empty() {
            return Err(ExternalError::new("No speech detected."));
        }
        Ok(text.trim().to_owned())
    }
}

impl SpeechTransport for ReqwestOpenAiTransport {
    fn transcribe_audio(
        &mut self,
        request: TranscriptionRequest<'_>,
    ) -> Result<String, ExternalError> {
        if request.model.trim().is_empty() {
            return Err(ExternalError::new(
                "The selected transcription model could not be used. Choose another model or check the custom model name.",
            ));
        }
        let primary = self.transcribe_once(&request, request.model)?;
        if request.model == FALLBACK_TRANSCRIPTION_MODEL
            || !transcript_is_suspiciously_short(&primary, request.duration_seconds)
        {
            return Ok(primary);
        }
        let fallback = self
            .transcribe_once(&request, FALLBACK_TRANSCRIPTION_MODEL)
            .unwrap_or_default();
        if is_better_transcript(&fallback, &primary) {
            Ok(fallback)
        } else {
            Ok(primary)
        }
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
        let response = self
            .client
            .post(format!("{}/responses", self.api_base))
            .header("Authorization", self.authorization()?)
            .json(&payload)
            .send()
            .map_err(|error| ExternalError::new(format!("Could not reach OpenAI: {error}")))?;
        let status = response.status();
        let body = response.text().map_err(|error| {
            ExternalError::new(format!("Could not read OpenAI's response: {error}"))
        })?;
        if !status.is_success() {
            return Err(Self::response_error(status, &body));
        }
        let payload: Value = serde_json::from_str(&body)
            .map_err(|_| ExternalError::new("OpenAI returned an invalid cleanup response."))?;
        let text = payload
            .get("output")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| item.get("content").and_then(Value::as_array))
            .flatten()
            .filter_map(|content| content.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("")
            .trim()
            .to_owned();
        if text.is_empty() {
            return Err(ExternalError::new("Cleanup returned an empty response."));
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_cleanup_instruction, cleanup_reasoning_effort, transcript_is_suspiciously_short,
    };

    #[test]
    fn fallback_is_reserved_for_obviously_truncated_audio() {
        assert!(transcript_is_suspiciously_short(
            "Only the opening sentence.",
            30.0
        ));
        assert!(!transcript_is_suspiciously_short(
            "This is a complete one sentence dictation with enough words to match its length and it should return immediately.",
            30.0
        ));
    }

    #[test]
    fn cleanup_style_changes_the_instruction_sent_to_openai() {
        let light = build_cleanup_instruction("Light cleanup", "Preserve my intent.");
        let structured =
            build_cleanup_instruction("Structured coding prompt", "Preserve my intent.");

        assert!(light.contains("Keep wording and structure close"));
        assert!(structured.contains("Goal, Requirements, Constraints, Testing, Notes"));
        assert_ne!(light, structured);
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
