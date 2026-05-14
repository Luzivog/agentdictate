from __future__ import annotations

import threading
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable

from .audio import AudioError, AudioRecorder, Recording
from .audio_ducking import AudioDucker
from .cleanup import build_cleanup_instruction
from .clipboard import ClipboardPaste
from .config import Settings, load_settings, repair_zero_pricing_defaults, save_settings
from .costs import estimate_session_cost, word_count
from .hotkey import HotkeyError, InputHotkeyListener
from .openai_client import OpenAIClient, OpenAIClientError
from .feedback import play_feedback
from .replacements import apply_replacements
from .startup import set_start_on_login
from .storage import HistoryRecord, Storage, utc_now


StatusCallback = Callable[[str], None]
MessageCallback = Callable[[str, str], None]
RefreshCallback = Callable[[], None]


class AgentDictateController:
    def __init__(
        self,
        status_callback: StatusCallback | None = None,
        message_callback: MessageCallback | None = None,
        refresh_callback: RefreshCallback | None = None,
        settings: Settings | None = None,
        storage: Storage | None = None,
        audio_ducker: AudioDucker | None = None,
    ) -> None:
        self.settings = settings or load_settings()
        self.storage = storage or Storage()
        if repair_zero_pricing_defaults(self.settings):
            save_settings(self.settings)
        self.storage.seed_pricing(self.settings)
        self.storage.reprice_history(self.settings)
        self.status_callback = status_callback
        self.message_callback = message_callback
        self.refresh_callback = refresh_callback
        self.audio = AudioRecorder()
        self.audio_ducker = audio_ducker or AudioDucker()
        self.recording: Recording | None = None
        self.recording_started_at_iso: str | None = None
        self.recording_session_id: int | None = None
        self.processing_session_id: int | None = None
        self.cancelled_sessions: set[int] = set()
        self.next_session_id = 0
        self.max_timer: threading.Timer | None = None
        self.lock = threading.RLock()
        self.hotkey_listener: InputHotkeyListener | None = None
        self.status = "Ready"
        set_start_on_login(self.settings.start_on_login)

    def close(self) -> None:
        self.stop_hotkey()
        self.audio_ducker.restore(wait=True)
        self.storage.close()

    def update_settings(self, settings: Settings) -> None:
        self.settings = settings
        repair_zero_pricing_defaults(self.settings)
        save_settings(settings)
        self.storage.seed_pricing(settings)
        self.storage.reprice_history(settings)
        set_start_on_login(settings.start_on_login)
        if not settings.audio_ducking_enabled:
            self.audio_ducker.restore()
        self.restart_hotkey()
        self.refresh()

    def save_settings(self) -> None:
        repair_zero_pricing_defaults(self.settings)
        save_settings(self.settings)
        set_start_on_login(self.settings.start_on_login)
        self.storage.seed_pricing(self.settings)
        self.storage.reprice_history(self.settings)
        self.refresh()

    def set_status(self, status: str) -> None:
        self.status = status
        if self.status_callback:
            self.status_callback(status)

    def message(self, title: str, body: str = "") -> None:
        if self.message_callback:
            self.message_callback(title, body)

    def refresh(self) -> None:
        if self.refresh_callback:
            self.refresh_callback()

    def start_hotkey(self) -> None:
        if self.hotkey_listener is not None:
            return
        try:
            listener = InputHotkeyListener(
                hotkey=self.settings.hotkey,
                recording_mode=self.settings.recording_mode,
                on_start=self.start_recording,
                on_stop=self.stop_recording,
                on_cancel=self.cancel_current_flow,
                on_error=self._hotkey_error,
            )
        except HotkeyError as exc:
            self._hotkey_error(str(exc))
            return
        self.hotkey_listener = listener
        listener.start()

    def stop_hotkey(self) -> None:
        if self.hotkey_listener:
            self.hotkey_listener.stop()
            self.hotkey_listener = None

    def restart_hotkey(self) -> None:
        self.stop_hotkey()
        self.start_hotkey()

    def recording_elapsed_seconds(self) -> float:
        with self.lock:
            recording = self.recording
            if recording is None:
                return 0.0
            return max(0.0, time.monotonic() - recording.started_at)

    def recording_input_level(self) -> float:
        with self.lock:
            recording = self.recording
            if recording is None:
                return 0.0
            return self.audio.input_level(recording)

    def recording_waveform(self) -> list[float]:
        with self.lock:
            recording = self.recording
            if recording is None:
                return []
            return self.audio.input_waveform(recording)

    def _hotkey_error(self, message: str) -> None:
        self.set_status("Error")
        self.message("Could not register Ctrl+Space. Choose another hotkey.", message)

    def start_recording(self) -> None:
        with self.lock:
            if self.recording is not None:
                return
            self.audio_ducker.duck(self.settings)
            try:
                self.recording = self.audio.start()
            except AudioError as exc:
                self.audio_ducker.restore()
                self.set_status("Error")
                self.message(str(exc))
                return
            self.next_session_id += 1
            self.recording_session_id = self.next_session_id
            self.recording_started_at_iso = utc_now()
            self.set_status("Recording")
            play_feedback(
                "start",
                enabled=self.settings.sound_feedback and self.settings.start_sound,
            )
            self.message("Recording...")
            if self.settings.max_recording_seconds > 0:
                self.max_timer = threading.Timer(
                    self.settings.max_recording_seconds, self._max_recording_reached
                )
                self.max_timer.daemon = True
                self.max_timer.start()

    def _max_recording_reached(self) -> None:
        self.message("Maximum recording length reached. Transcribing now.")
        self.stop_recording()

    def stop_recording(self) -> None:
        with self.lock:
            recording = self.recording
            if recording is None:
                return
            self.recording = None
            if self.max_timer:
                self.max_timer.cancel()
                self.max_timer = None
            started_at = self.recording_started_at_iso or utc_now()
            self.recording_started_at_iso = None
            session_id = self.recording_session_id
            self.recording_session_id = None
        try:
            duration = self.audio.stop(recording)
        except AudioError as exc:
            self.audio_ducker.restore()
            self.set_status("Error")
            self.message(str(exc))
            return
        self.audio_ducker.restore()
        play_feedback(
            "stop",
            enabled=self.settings.sound_feedback and self.settings.stop_sound,
        )
        if duration < 0.3:
            self.audio.delete_temp(recording.path, preserve=False)
            self.set_status("Ready")
            self.message("Recording too short.")
            return
        self.set_status("Transcribing")
        self.message("Transcribing...")
        thread = threading.Thread(
            target=self._process_recording,
            args=(recording.path, duration, started_at, session_id),
            daemon=True,
        )
        thread.start()

    def cancel_current_flow(self) -> None:
        recording: Recording | None = None
        session_id: int | None = None
        with self.lock:
            if self.recording is not None:
                recording = self.recording
                session_id = self.recording_session_id
                self.recording = None
                self.recording_session_id = None
                self.recording_started_at_iso = None
                if self.max_timer:
                    self.max_timer.cancel()
                    self.max_timer = None
                if session_id is not None:
                    self.cancelled_sessions.add(session_id)
            elif self.processing_session_id is not None:
                self.cancelled_sessions.add(self.processing_session_id)
                self.set_status("Ready")
                self.message("Canceled.")
                return
            else:
                return

        if recording is not None:
            try:
                self.audio.stop(recording)
            except AudioError:
                pass
            self.audio_ducker.restore()
            self.audio.delete_temp(recording.path, preserve=False)
            if session_id is not None:
                self._finish_session(session_id)
            self.set_status("Ready")
            self.message("Recording canceled.")
            self.refresh()

    def _is_session_cancelled(self, session_id: int | None) -> bool:
        if session_id is None:
            return False
        with self.lock:
            return session_id in self.cancelled_sessions

    def _finish_session(self, session_id: int | None) -> None:
        if session_id is None:
            return
        with self.lock:
            self.cancelled_sessions.discard(session_id)
            if self.processing_session_id == session_id:
                self.processing_session_id = None

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
        copied = False
        pasted = False
        success = False
        canceled = False
        transcription_model_used = self.settings.active_transcription_model()
        try:
            if self._is_session_cancelled(session_id):
                canceled = True
                return
            client = OpenAIClient(self.settings.openai_api_key)
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
            text_for_replacements = raw_transcript
            cleanup_model = None
            if self.settings.cleanup_enabled:
                cleanup_model = self.settings.active_cleanup_model()
                try:
                    if self._is_session_cancelled(session_id):
                        canceled = True
                        return
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
                    if self._is_session_cancelled(session_id):
                        canceled = True
                        return
                except OpenAIClientError as exc:
                    cleanup_error = str(exc)
                    text_for_replacements = raw_transcript
                    self.message(
                        "Cleanup failed. Raw transcript was pasted instead.",
                        cleanup_error,
                    )
            mappings = self.storage.list_mappings()
            final_text, replacements_applied = apply_replacements(
                text_for_replacements, mappings
            )
            if self._is_session_cancelled(session_id):
                canceled = True
                return
            self.set_status("Pasting")
            paste = ClipboardPaste(self.settings.restore_clipboard_after_paste)
            paste_result = paste.copy_and_paste(final_text)
            copied = paste_result.copied
            pasted = paste_result.paste_triggered
            if paste_result.error:
                self.message(paste_result.error)
            else:
                self.message("Prompt pasted.")
            success = True
        except OpenAIClientError as exc:
            replacements_applied = []
            error_message = self._friendly_openai_error(str(exc))
            self.set_status("Error")
            self.message(error_message)
        except Exception as exc:
            replacements_applied = []
            error_message = str(exc)
            self.set_status("Error")
            self.message("Something went wrong.", error_message)
        finally:
            if canceled:
                self.audio.delete_temp(audio_path, preserve=False)
                self._finish_session(session_id)
                self.set_status("Ready")
                self.message("Canceled.")
                self.refresh()
                return
            cleanup_model_used = (
                self.settings.active_cleanup_model()
                if self.settings.cleanup_enabled
                else None
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
                self.storage.add_history_record(
                    HistoryRecord(
                        started_at=started_at,
                        ended_at=ended_at,
                        duration_seconds=duration,
                        transcription_model=transcription_model_used,
                        cleanup_enabled=self.settings.cleanup_enabled,
                        cleanup_model=cleanup_model_used,
                        cleanup_style=(
                            self.settings.cleanup_style if self.settings.cleanup_enabled else None
                        ),
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
            self.audio.delete_temp(
                audio_path,
                preserve=self.settings.debug_mode or self.settings.preserve_temp_audio,
            )
            if success:
                self.set_status("Ready")
            self._finish_session(session_id)
            self.refresh()

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
                cleanup_model=self.settings.active_cleanup_model()
                if self.settings.cleanup_enabled
                else None,
                cleanup_style=self.settings.cleanup_style
                if self.settings.cleanup_enabled
                else None,
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
