from __future__ import annotations

from pathlib import Path

from agentdictate.cleanup import build_cleanup_instruction
from agentdictate.costs import estimate_session_cost, word_count
from agentdictate.openai_client import OpenAIClientError
from agentdictate.replacements import apply_replacements
from agentdictate.storage import HistoryRecord, utc_now


def _openai_client(api_key: str):
    from . import OpenAIClient
    return OpenAIClient(api_key)


def _clipboard_paste(restore_clipboard: bool):
    from . import ClipboardPaste
    return ClipboardPaste(restore_clipboard)


class ProcessingMixin:
    def _process_recording(
        self,
        audio_path: Path,
        duration: float,
        started_at: str,
        session_id: int | None = None,
    ) -> None:
        if session_id is not None:
            with self.lock:
                self.processing_session_id = session_id
        ended_at = utc_now()
        raw_transcript = ""
        cleaned_transcript: str | None = None
        final_text = ""
        cleanup_error: str | None = None
        error_message: str | None = None
        replacements_applied: list[dict[str, object]] = []
        copied = False
        pasted = False
        success = False
        canceled = False
        transcription_model_used = self.settings.active_transcription_model()
        try:
            if self._is_session_cancelled(session_id):
                canceled = True
                return
            client = _openai_client(self.settings.openai_api_key)
            transcription_model = self.settings.active_transcription_model()
            raw_transcript = client.transcribe(
                audio_path=audio_path,
                model=transcription_model,
                language=self.settings.language,
                prompt=self.settings.transcription_prompt,
                audio_duration_seconds=duration,
            )
            transcription_model_used = (
                getattr(client, "last_transcription_model", "") or transcription_model
            )
            if self._is_session_cancelled(session_id):
                canceled = True
                return
            text_for_replacements = self._cleanup_transcript(
                client, raw_transcript, session_id
            )
            cleaned_transcript, cleanup_error, text_for_replacements = text_for_replacements
            if self._is_session_cancelled(session_id):
                canceled = True
                return
            final_text, replacements_applied = apply_replacements(
                text_for_replacements, self.storage.list_mappings()
            )
            if self._is_session_cancelled(session_id):
                canceled = True
                return
            copied, pasted = self._paste_final_text(final_text)
            success = True
        except OpenAIClientError as exc:
            error_message = self._friendly_openai_error(str(exc))
            self.set_status("Error")
            self.message(error_message)
        except Exception as exc:
            error_message = str(exc)
            self.set_status("Error")
            self.message("Something went wrong.", error_message)
        finally:
            if canceled:
                self._finish_canceled_processing(audio_path, session_id)
                return
            self._record_processing_result(
                audio_path=audio_path,
                duration=duration,
                started_at=started_at,
                ended_at=ended_at,
                transcription_model_used=transcription_model_used,
                raw_transcript=raw_transcript,
                cleaned_transcript=cleaned_transcript,
                final_text=final_text,
                replacements_applied=replacements_applied,
                copied=copied,
                pasted=pasted,
                success=success,
                error_message=error_message,
                cleanup_error=cleanup_error,
                session_id=session_id,
            )

    def _cleanup_transcript(self, client: object, raw_transcript: str, session_id: int | None):
        cleaned_transcript: str | None = None
        cleanup_error: str | None = None
        text_for_replacements = raw_transcript
        if not self.settings.cleanup_enabled:
            return cleaned_transcript, cleanup_error, text_for_replacements
        cleanup_model = self.settings.active_cleanup_model()
        try:
            if self._is_session_cancelled(session_id):
                return cleaned_transcript, cleanup_error, text_for_replacements
            self.set_status("Cleaning up")
            self.message("Cleaning up prompt...")
            cleaned_transcript = client.cleanup(
                transcript=raw_transcript,
                model=cleanup_model,
                instruction=build_cleanup_instruction(
                    self.settings.cleanup_style, self.settings.cleanup_prompt
                ),
                reasoning_effort=self.settings.active_cleanup_reasoning_effort(),
            )
            text_for_replacements = cleaned_transcript
        except OpenAIClientError as exc:
            cleanup_error = str(exc)
            self.message("Cleanup failed. Raw transcript was pasted instead.", cleanup_error)
        return cleaned_transcript, cleanup_error, text_for_replacements

    def _paste_final_text(self, final_text: str) -> tuple[bool, bool]:
        self.set_status("Pasting")
        paste = _clipboard_paste(self.settings.restore_clipboard_after_paste)
        paste_result = paste.copy_and_paste(final_text)
        if paste_result.error:
            self.message(paste_result.error)
        else:
            self.message("Prompt pasted.")
        return bool(paste_result.copied), bool(paste_result.paste_triggered)

    def _finish_canceled_processing(self, audio_path: Path, session_id: int | None) -> None:
        self.audio.delete_temp(audio_path, preserve=False)
        self._finish_session(session_id)
        self.set_status("Ready")
        self.message("Canceled.")
        self.refresh()

    def _record_processing_result(
        self,
        audio_path: Path,
        duration: float,
        started_at: str,
        ended_at: str,
        transcription_model_used: str,
        raw_transcript: str,
        cleaned_transcript: str | None,
        final_text: str,
        replacements_applied: list[dict[str, object]],
        copied: bool,
        pasted: bool,
        success: bool,
        error_message: str | None,
        cleanup_error: str | None,
        session_id: int | None,
    ) -> None:
        cleanup_model_used = (
            self.settings.active_cleanup_model() if self.settings.cleanup_enabled else None
        )
        cleanup_price = self.settings.cleanup_price(cleanup_model_used or "")
        cost = estimate_session_cost(
            duration_seconds=duration,
            raw_transcript=raw_transcript,
            cleaned_transcript=cleaned_transcript,
            cleanup_enabled=self.settings.cleanup_enabled and cleaned_transcript is not None,
            transcription_price_per_minute=self.settings.transcription_price_per_minute(
                transcription_model_used
            ),
            cleanup_input_price_per_1m_tokens=cleanup_price.input_price_per_1m_tokens,
            cleanup_output_price_per_1m_tokens=cleanup_price.output_price_per_1m_tokens,
        )
        if self.settings.save_history:
            self._save_history_record(
                started_at, ended_at, duration, transcription_model_used,
                cleanup_model_used, raw_transcript, cleaned_transcript, final_text,
                replacements_applied, copied, pasted, success, error_message,
                cleanup_error, cost,
            )
        self.audio.delete_temp(
            audio_path,
            preserve=self.settings.debug_mode or self.settings.preserve_temp_audio,
        )
        if success:
            self.set_status("Ready")
        self._finish_session(session_id)
        self.refresh()
