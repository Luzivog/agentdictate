from __future__ import annotations

from pathlib import Path

from agentdictate.cleanup import build_cleanup_instruction
from agentdictate.costs import estimate_session_cost, word_count
from agentdictate.diagnostics import log_event
from agentdictate.openai_client import OpenAIClientError
from agentdictate.replacements import apply_replacements
from agentdictate.storage import HistoryRecord, utc_now


def _openai_client(api_key: str):
    from . import OpenAIClient
    return OpenAIClient(api_key)


def _clipboard_paste(restore_clipboard: bool, shortcut_mode: str):
    from . import ClipboardPaste
    return ClipboardPaste(restore_clipboard, shortcut_mode)


class ProcessingMixin:
    def _redeliver_dictation(self, job_id: int, final_text: str) -> None:
        copied = False
        pasted = False
        error_message: str | None = None
        try:
            if self._is_session_cancelled(job_id):
                return
            self.storage.update_dictation_job(
                job_id,
                state="delivering",
                stage="delivering",
                final_text=final_text,
            )
            copied, pasted, error_message = self._paste_final_text(final_text)
        except Exception as exc:
            error_message = str(exc)
        if self._is_session_cancelled(job_id):
            self._finish_session(job_id)
            return
        success = copied and pasted
        self.storage.update_dictation_job(
            job_id,
            state="delivered" if success else "delivery_failed",
            stage="delivered" if success else "delivering",
            final_text=final_text,
            copied_to_clipboard=copied,
            paste_triggered=pasted,
            error_message=error_message,
        )
        log_event(
            "dictation_delivery_finished",
            job_id=job_id,
            success=success,
            copied=copied,
            pasted=pasted,
            error=error_message or "",
        )
        self._finish_session(job_id)
        self.set_status("Ready" if success else "Error")
        if success:
            self.message("Saved transcript pasted.")
        else:
            self.message("Saved transcript is still available.", error_message or "Paste failed.")
        self.refresh()

    def _process_recording(
        self,
        audio_path: Path,
        duration: float,
        started_at: str,
        session_id: int | None = None,
    ) -> None:
        job_id = self.storage.ensure_dictation_job(
            audio_path,
            started_at,
            self.settings.active_transcription_model(),
        )
        self.storage.update_dictation_job(
            job_id,
            state="transcribing",
            stage="transcribing",
            duration_seconds=duration,
        )
        log_event(
            "dictation_processing_started",
            job_id=job_id,
            duration_seconds=round(duration, 3),
        )
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
        stage = "transcribing"
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
            self.storage.update_dictation_job(
                job_id,
                state="transcribed",
                stage="transcribed",
                raw_transcript=raw_transcript,
            )
            log_event(
                "dictation_transcribed",
                job_id=job_id,
                character_count=len(raw_transcript),
            )
            if self._is_session_cancelled(session_id):
                canceled = True
                return
            stage = "cleanup"
            text_for_replacements = self._cleanup_transcript(
                client, raw_transcript, session_id
            )
            cleaned_transcript, cleanup_error, text_for_replacements = text_for_replacements
            if self._is_session_cancelled(session_id):
                canceled = True
                return
            stage = "replacements"
            final_text, replacements_applied = apply_replacements(
                text_for_replacements, self.storage.list_mappings()
            )
            self.storage.update_dictation_job(
                job_id,
                state="transcribed",
                stage="transcribed",
                raw_transcript=raw_transcript,
                final_text=final_text,
            )
            if self._is_session_cancelled(session_id):
                canceled = True
                return
            stage = "delivering"
            self.storage.update_dictation_job(
                job_id,
                state="delivering",
                stage="delivering",
                raw_transcript=raw_transcript,
                final_text=final_text,
            )
            copied, pasted, delivery_error = self._paste_final_text(final_text)
            success = copied and pasted
            if not success:
                error_message = delivery_error or "Transcript could not be delivered."
        except OpenAIClientError as exc:
            error_message = self._friendly_openai_error(str(exc))
            self.set_status("Error")
            self.message(error_message)
        except Exception as exc:
            error_message = str(exc)
            self.set_status("Error")
            self.message("Something went wrong.", error_message)
        finally:
            canceled = canceled or self._is_session_cancelled(session_id)
            if canceled:
                if self._closed:
                    self._finish_session(session_id)
                    return
                self.storage.update_dictation_job(
                    job_id,
                    state="canceled",
                    stage=stage,
                    raw_transcript=raw_transcript,
                    final_text=final_text,
                    error_message="Canceled.",
                )
                log_event("dictation_processing_canceled", job_id=job_id, stage=stage)
                self._finish_canceled_processing(audio_path, session_id)
                return
            job_state = (
                "delivered"
                if success
                else "delivery_failed"
                if final_text
                else "failed"
            )
            self.storage.update_dictation_job(
                job_id,
                state=job_state,
                stage="delivered" if success else stage,
                raw_transcript=raw_transcript,
                final_text=final_text,
                copied_to_clipboard=copied,
                paste_triggered=pasted,
                error_message=error_message,
            )
            log_event(
                "dictation_processing_finished",
                job_id=job_id,
                state=job_state,
                stage="delivered" if success else stage,
                copied=copied,
                pasted=pasted,
                error=error_message or "",
            )
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

    def _paste_final_text(self, final_text: str) -> tuple[bool, bool, str | None]:
        self.set_status("Pasting")
        paste = _clipboard_paste(
            self.settings.restore_clipboard_after_paste,
            self.settings.paste_shortcut,
        )
        paste_result = paste.deliver(final_text)
        if paste_result.error:
            self.message(paste_result.error)
        else:
            self.message("Paste shortcut sent.")
        return (
            bool(paste_result.copied),
            bool(paste_result.paste_triggered),
            paste_result.error or None,
        )

    def _finish_canceled_processing(self, audio_path: Path, session_id: int | None) -> None:
        self._finish_session(session_id)
        self.set_status("Ready")
        self.message("Canceled. Audio saved for recovery.")
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
        if success:
            self.set_status("Ready")
        self._finish_session(session_id)
        self.refresh()
