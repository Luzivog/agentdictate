from __future__ import annotations

from datetime import datetime, timezone

from agentdictate.costs import estimate_session_cost, word_count
from agentdictate.openai_client import OpenAIClientError
from agentdictate.storage import HistoryRecord


class SupportMixin:
    def _friendly_openai_error(self, message: str) -> str:
        if "No speech detected" in message:
            return "No speech detected."
        if "authentication" in message.lower():
            return "OpenAI authentication failed. Check your API key."
        if "model" in message.lower():
            return message
        if "missing" in message.lower() and "api key" in message.lower():
            return "OpenAI API key missing. Paste your API key in AgentDictate settings."
        return message or "Could not reach OpenAI. Check your internet connection."

    def test_api_key(self) -> tuple[bool, str]:
        from . import OpenAIClient

        try:
            OpenAIClient(self.settings.openai_api_key).test_api_key()
        except OpenAIClientError as exc:
            return False, str(exc)
        return True, "API key works."

    def add_demo_history(self, final_text: str = "Demo AgentDictate transcript.") -> None:
        now = datetime.now(timezone.utc).replace(microsecond=0).isoformat()
        raw = final_text
        cost = estimate_session_cost(
            12.0,
            raw,
            raw if self.settings.cleanup_enabled else None,
            self.settings.cleanup_enabled,
            self.settings.transcription_price_per_minute(),
            self.settings.cleanup_price().input_price_per_1m_tokens,
            self.settings.cleanup_price().output_price_per_1m_tokens,
        )
        self.storage.add_history_record(
            HistoryRecord(
                started_at=now,
                ended_at=now,
                duration_seconds=12.0,
                transcription_model=self.settings.active_transcription_model(),
                cleanup_enabled=self.settings.cleanup_enabled,
                cleanup_model=(
                    self.settings.active_cleanup_model()
                    if self.settings.cleanup_enabled
                    else None
                ),
                cleanup_style=self.settings.cleanup_style if self.settings.cleanup_enabled else None,
                raw_transcript=raw,
                cleaned_transcript=raw if self.settings.cleanup_enabled else None,
                final_text=final_text,
                replacements_applied=[],
                copied_to_clipboard=False,
                paste_triggered=False,
                raw_word_count=word_count(raw),
                final_word_count=word_count(final_text),
                final_character_count=len(final_text),
                estimated_transcription_cost=cost.transcription_cost,
                estimated_cleanup_cost=cost.cleanup_cost,
                estimated_total_cost=cost.total_cost,
                success=True,
            )
        )
        self.refresh()

    def _save_history_record(
        self,
        started_at: str,
        ended_at: str,
        duration: float,
        transcription_model_used: str,
        cleanup_model_used: str | None,
        raw_transcript: str,
        cleaned_transcript: str | None,
        final_text: str,
        replacements_applied: list[dict[str, object]],
        copied: bool,
        pasted: bool,
        success: bool,
        error_message: str | None,
        cleanup_error: str | None,
        cost: object,
    ) -> None:
        self.storage.add_history_record(
            HistoryRecord(
                started_at=started_at,
                ended_at=ended_at,
                duration_seconds=duration,
                transcription_model=transcription_model_used,
                cleanup_enabled=self.settings.cleanup_enabled,
                cleanup_model=cleanup_model_used,
                cleanup_style=self.settings.cleanup_style if self.settings.cleanup_enabled else None,
                raw_transcript=raw_transcript,
                cleaned_transcript=cleaned_transcript,
                final_text=final_text,
                replacements_applied=replacements_applied,
                copied_to_clipboard=copied,
                paste_triggered=pasted,
                raw_word_count=word_count(raw_transcript),
                final_word_count=word_count(final_text),
                final_character_count=len(final_text),
                estimated_transcription_cost=cost.transcription_cost,
                estimated_cleanup_cost=cost.cleanup_cost,
                estimated_total_cost=cost.total_cost,
                success=success,
                error_message=error_message,
                cleanup_error=cleanup_error,
            )
        )
