from __future__ import annotations

import json
from pathlib import Path

import requests

from .openai_api import (
    FALLBACK_TRANSCRIPTION_MODEL,
    TRANSCRIPTION_COMPLETENESS_PROMPT,
    extract_response_text,
    sentence_count,
    word_count,
)


class OpenAIClientError(RuntimeError):
    pass


class OpenAIClient:
    def __init__(self, api_key: str, api_base: str = "https://api.openai.com/v1") -> None:
        self.api_key = api_key.strip()
        self.api_base = api_base.rstrip("/")
        self.last_transcription_model = ""

    def _headers(self) -> dict[str, str]:
        if not self.api_key:
            raise OpenAIClientError(
                "OpenAI API key missing. Paste your API key in AgentDictate settings."
            )
        return {"Authorization": f"Bearer {self.api_key}"}

    def _raise_for_response(self, response: requests.Response) -> None:
        if response.ok:
            return
        message = response.text
        try:
            payload = response.json()
            message = (
                payload.get("error", {}).get("message")
                or payload.get("message")
                or response.text
            )
        except ValueError:
            pass
        if response.status_code == 401:
            raise OpenAIClientError("OpenAI authentication failed. Check your API key.")
        if response.status_code in (408, 409, 429, 500, 502, 503, 504):
            raise OpenAIClientError(
                f"Could not reach OpenAI or the request was not accepted: {message}"
            )
        raise OpenAIClientError(message)

    def test_api_key(self, timeout: float = 20.0) -> bool:
        response = requests.get(
            f"{self.api_base}/models", headers=self._headers(), timeout=timeout
        )
        self._raise_for_response(response)
        return True

    def transcribe(
        self,
        audio_path: Path,
        model: str,
        language: str = "",
        prompt: str = "",
        audio_duration_seconds: float | None = None,
        timeout: float = 180.0,
    ) -> str:
        if not model:
            raise OpenAIClientError(
                "The selected transcription model could not be used. Choose another model or check the custom model name."
            )
        final_prompt = self._transcription_prompt(prompt)
        text = self._transcribe_once(
            audio_path=audio_path,
            model=model,
            language=language,
            prompt=final_prompt,
            timeout=timeout,
        )
        selected_model = model
        if self._should_retry_transcription(text, audio_duration_seconds, model):
            try:
                fallback_text = self._transcribe_once(
                    audio_path=audio_path,
                    model=FALLBACK_TRANSCRIPTION_MODEL,
                    language=language,
                    prompt=final_prompt,
                    timeout=timeout,
                )
            except OpenAIClientError:
                fallback_text = ""
            if self._is_better_transcript(fallback_text, text):
                text = fallback_text
                selected_model = FALLBACK_TRANSCRIPTION_MODEL
        self.last_transcription_model = selected_model
        return text

    def _transcribe_once(
        self,
        audio_path: Path,
        model: str,
        language: str = "",
        prompt: str = "",
        timeout: float = 180.0,
    ) -> str:
        data: dict[str, str] = {
            "model": model,
            "response_format": "text",
        }
        if language.strip():
            data["language"] = language.strip()
        if prompt.strip():
            data["prompt"] = prompt.strip()
        with audio_path.open("rb") as audio_file:
            response = requests.post(
                f"{self.api_base}/audio/transcriptions",
                headers=self._headers(),
                data=data,
                files={"file": (audio_path.name, audio_file, "audio/wav")},
                timeout=timeout,
        )
        self._raise_for_response(response)
        text = response.text.strip()
        if not text or text.startswith("{"):
            text = self._json_transcription_text(response) or text
        if not text:
            raise OpenAIClientError("No speech detected.")
        return text

    def _json_transcription_text(self, response: requests.Response) -> str:
        try:
            payload = response.json()
        except ValueError:
            return ""
        return str(payload.get("text", "")).strip()

    def _transcription_prompt(self, prompt: str) -> str:
        user_prompt = prompt.strip()
        if not user_prompt:
            return TRANSCRIPTION_COMPLETENESS_PROMPT
        return f"{TRANSCRIPTION_COMPLETENESS_PROMPT}\n\nContext and vocabulary:\n{user_prompt}"

    def _should_retry_transcription(
        self, text: str, audio_duration_seconds: float | None, model: str
    ) -> bool:
        if model == FALLBACK_TRANSCRIPTION_MODEL:
            return False
        if audio_duration_seconds is None or audio_duration_seconds < 6.0:
            return False
        return sentence_count(text) <= 1

    def _is_better_transcript(self, candidate: str, current: str) -> bool:
        if not candidate.strip():
            return False
        candidate_words = word_count(candidate)
        current_words = word_count(current)
        if candidate_words <= current_words:
            return False
        if sentence_count(candidate) > sentence_count(current):
            return True
        return candidate_words >= max(current_words + 5, int(current_words * 1.35))
    def cleanup(
        self,
        transcript: str,
        model: str,
        instruction: str,
        reasoning_effort: str = "",
        timeout: float = 120.0,
    ) -> str:
        if not model:
            raise OpenAIClientError(
                "The selected cleanup model could not be used. Choose another model or check the custom model name."
            )
        payload = {
            "model": model,
            "instructions": instruction,
            "input": transcript,
            "text": {"format": {"type": "text"}},
        }
        effort = reasoning_effort.strip().lower()
        if effort and effort != "default":
            payload["reasoning"] = {"effort": effort}
        response = requests.post(
            f"{self.api_base}/responses",
            headers={**self._headers(), "Content-Type": "application/json"},
            data=json.dumps(payload),
            timeout=timeout,
        )
        self._raise_for_response(response)
        try:
            payload = response.json()
        except ValueError as exc:
            raise OpenAIClientError("OpenAI returned an invalid cleanup response.") from exc
        text = extract_response_text(payload).strip()
        if not text:
            raise OpenAIClientError("Cleanup returned an empty response.")
        return text
