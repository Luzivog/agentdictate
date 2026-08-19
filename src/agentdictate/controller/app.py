from __future__ import annotations

import threading
import time
from collections.abc import Callable
from datetime import datetime, timezone
from pathlib import Path

from agentdictate.audio import AudioError, AudioRecorder, Recording
from agentdictate.audio_ducking import AudioDucker
from agentdictate.clipboard import ClipboardPaste
from agentdictate.config import Settings, load_settings, repair_zero_pricing_defaults, save_settings
from agentdictate.diagnostics import log_event
from agentdictate.hotkey import (
    HotkeyError,
    HotkeyEvent,
    HotkeyEventKind,
    InputHotkeyListener,
)
from agentdictate.paths import recordings_dir
from agentdictate.startup import set_start_on_login
from agentdictate.storage import Storage

from .processing import ProcessingMixin
from .recording import RecordingMixin
from .support import SupportMixin
from .types import MessageCallback, RefreshCallback, StatusCallback


class AgentDictateController(RecordingMixin, ProcessingMixin, SupportMixin):
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
        storage_is_injected = storage is not None
        self.storage = storage if storage is not None else Storage()
        self._recordings_directory = (
            self.storage.path.parent / "recordings"
            if storage_is_injected
            else recordings_dir()
        )
        self._manage_startup = not storage_is_injected
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
        self.recording_job_id: int | None = None
        self.recording_started_at_iso: str | None = None
        self.recording_session_id: int | None = None
        self.processing_session_id: int | None = None
        self.cancelled_sessions: set[int] = set()
        self._processing_threads: set[threading.Thread] = set()
        self._storage_closed = False
        self.next_session_id = 0
        self.max_timer: threading.Timer | None = None
        self.lock = threading.RLock()
        self.hotkey_listener: InputHotkeyListener | None = None
        self.hotkey_available = False
        self._hotkey_unavailable_reported = False
        self._closed = False
        self.status = "Ready"
        self._reconcile_dictation_jobs()
        if self._manage_startup:
            set_start_on_login(self.settings.start_on_login)
        log_event("controller_started", recoverable_count=len(self.list_recoverable_dictations()))

    def _reconcile_dictation_jobs(self) -> None:
        """Make every durable recording discoverable after an unclean exit."""
        jobs = self.storage.list_dictation_jobs()
        known_paths = {Path(row["audio_path"]) for row in jobs}
        inflight_states = {
            "recording",
            "captured",
            "transcribing",
            "transcribed",
            "delivering",
        }
        interrupted_count = 0
        for row in jobs:
            if row["state"] in inflight_states:
                self.storage.update_dictation_job(
                    int(row["id"]),
                    state="interrupted",
                    stage=str(row["stage"]),
                    error_message="AgentDictate restarted before this dictation finished.",
                )
                interrupted_count += 1

        directory = self._recordings_directory
        if not directory.exists():
            if interrupted_count:
                log_event(
                    "startup_recovery",
                    interrupted_jobs=interrupted_count,
                    orphan_recordings=0,
                )
            return
        orphan_count = 0
        for audio_path in directory.glob("*.wav"):
            if audio_path in known_paths or not audio_path.is_file():
                continue
            started_at = datetime.fromtimestamp(
                audio_path.stat().st_mtime,
                tz=timezone.utc,
            ).isoformat()
            job_id = self.storage.ensure_dictation_job(
                audio_path,
                started_at,
                self.settings.active_transcription_model(),
            )
            self.storage.update_dictation_job(
                job_id,
                state="interrupted",
                stage="recording",
                error_message="AgentDictate restarted before this recording was registered.",
            )
            orphan_count += 1
        if interrupted_count or orphan_count:
            log_event(
                "startup_recovery",
                interrupted_jobs=interrupted_count,
                orphan_recordings=orphan_count,
            )

    def close(self) -> None:
        with self.lock:
            if self._closed:
                return
            self._closed = True
            recording = self.recording
            recording_job_id = self.recording_job_id
            processing_session_id = self.processing_session_id
            if recording is not None:
                self._clear_active_recording()
            if processing_session_id is not None:
                self.cancelled_sessions.add(processing_session_id)
        self.stop_hotkey()
        ClipboardPaste.close_sources()
        if recording is not None:
            try:
                duration = self.audio.stop(recording)
            except AudioError:
                duration = 0.0
            if recording_job_id is not None:
                self.storage.update_dictation_job(
                    recording_job_id,
                    state="interrupted",
                    stage="recording",
                    duration_seconds=duration,
                    error_message="Application closed; captured audio was preserved.",
                )
        if processing_session_id is not None:
            row = self.storage.get_dictation_job(processing_session_id)
            if row is not None:
                self.storage.update_dictation_job(
                    processing_session_id,
                    state="interrupted",
                    stage=str(row["stage"]),
                    error_message="Application closed while this dictation was processing.",
                )
        self.audio_ducker.restore(wait=True)
        with self.lock:
            close_storage = not self._processing_threads
        if close_storage:
            self._close_storage_once()
        log_event(
            "controller_closed",
            active_recording=recording is not None,
            active_processing=processing_session_id is not None,
        )

    def _start_processing_thread(
        self,
        target: Callable[..., None],
        args: tuple[object, ...],
    ) -> None:
        def run() -> None:
            try:
                target(*args)
            except Exception as exc:
                log_event(
                    "background_task_failed",
                    task=getattr(target, "__name__", type(target).__name__),
                    error=str(exc),
                )
                if not self._closed:
                    self.set_status("Error")
                    self.message("A background dictation task failed.", str(exc))
                    self.refresh()
            finally:
                current = threading.current_thread()
                with self.lock:
                    self._processing_threads.discard(current)
                    close_storage = self._closed and not self._processing_threads
                if close_storage:
                    self._close_storage_once()

        thread = threading.Thread(
            target=run,
            name="agentdictate-processing",
            daemon=True,
        )
        with self.lock:
            self._processing_threads.add(thread)
        try:
            thread.start()
        except Exception:
            with self.lock:
                self._processing_threads.discard(thread)
            raise

    def _close_storage_once(self) -> None:
        with self.lock:
            if self._storage_closed:
                return
            self._storage_closed = True
        self.storage.close()

    def update_settings(self, settings: Settings) -> None:
        self.settings = settings
        repair_zero_pricing_defaults(self.settings)
        save_settings(settings)
        self.storage.seed_pricing(settings)
        self.storage.reprice_history(settings)
        if self._manage_startup:
            set_start_on_login(settings.start_on_login)
        if not settings.audio_ducking_enabled:
            self.audio_ducker.restore()
        self.restart_hotkey()
        self.refresh()

    def save_settings(self) -> None:
        repair_zero_pricing_defaults(self.settings)
        save_settings(self.settings)
        if self._manage_startup:
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

    def list_recoverable_dictations(self):
        return self.storage.list_recoverable_dictations()

    def retry_dictation(self, job_id: int) -> bool:
        row = self.storage.get_dictation_job(job_id)
        if row is None:
            self.message("Recovery item no longer exists.")
            return False
        retry_delivery = row["state"] == "delivery_failed" and bool(row["final_text"])
        audio_path = Path(row["audio_path"])
        if not retry_delivery and not audio_path.is_file():
            self.storage.update_dictation_job(
                job_id,
                state="failed",
                stage=str(row["stage"]),
                error_message="The saved audio file is missing.",
            )
            self.message("Saved audio is missing.", str(audio_path))
            self.refresh()
            return False
        with self.lock:
            if self._closed or self.recording is not None or self.processing_session_id is not None:
                self.message("Finish the current dictation before retrying this one.")
                return False
            self.processing_session_id = job_id
            self.cancelled_sessions.discard(job_id)
        self.set_status("Transcribing")
        self.message("Retrying saved dictation...")
        try:
            self._start_processing_thread(
                target=(self._redeliver_dictation if retry_delivery else self._process_recording),
                args=(job_id, str(row["final_text"]))
                if retry_delivery
                else (
                    audio_path,
                    float(row["duration_seconds"] or 0.0),
                    str(row["started_at"]),
                    job_id,
                ),
            )
        except Exception as exc:
            self._finish_session(job_id)
            self.storage.update_dictation_job(
                job_id,
                state=str(row["state"]),
                stage=str(row["stage"]),
                error_message=f"Could not start retry worker: {exc}",
            )
            self.set_status("Error")
            self.message("Could not retry saved dictation.", str(exc))
            log_event("dictation_retry_start_failed", job_id=job_id, error=str(exc))
            self.refresh()
            return False
        log_event("dictation_retry_started", job_id=job_id, delivery_only=retry_delivery)
        return True

    def delete_recoverable_dictation(self, job_id: int) -> bool:
        row = self.storage.get_dictation_job(job_id)
        if row is None or row["state"] in {"delivered", "deleted"}:
            return False
        with self.lock:
            if job_id in {self.recording_job_id, self.processing_session_id}:
                self.message("Cancel the active dictation before deleting its saved audio.")
                return False
        audio_path = Path(row["audio_path"])
        try:
            audio_path.unlink(missing_ok=True)
        except OSError as exc:
            self.message("Could not delete saved audio.", str(exc))
            return False
        self.storage.update_dictation_job(
            job_id,
            state="deleted",
            stage="deleted",
            error_message=None,
        )
        log_event("dictation_deleted", job_id=job_id)
        self.refresh()
        return True

    def start_hotkey(self) -> None:
        if self.hotkey_listener is not None:
            return
        try:
            listener = InputHotkeyListener(
                hotkey=self.settings.hotkey,
                on_event=self._handle_hotkey_event,
            )
        except HotkeyError as exc:
            self._hotkey_error(str(exc))
            return
        self.hotkey_listener = listener
        listener.start()

    def stop_hotkey(self) -> None:
        if self.hotkey_listener:
            self.hotkey_listener.close()
            self.hotkey_listener = None
        self.hotkey_available = False

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
        self.hotkey_available = False
        self._hotkey_unavailable_reported = True
        self.set_status("Error")
        self.message(f"Could not register {self.settings.hotkey}.", message)

    def _handle_hotkey_event(self, event: HotkeyEvent) -> None:
        with self.lock:
            if self._closed:
                return
        if event.kind == HotkeyEventKind.AVAILABLE:
            self.hotkey_available = True
            if self._hotkey_unavailable_reported:
                self.message(f"{self.settings.hotkey} is ready.")
                self._hotkey_unavailable_reported = False
            self.refresh()
            return
        if event.kind == HotkeyEventKind.UNAVAILABLE:
            self.hotkey_available = False
            self._hotkey_unavailable_reported = True
            self.message(f"Could not register {self.settings.hotkey}.", event.message)
            self.refresh()
            return
        if event.kind == HotkeyEventKind.CANCELLED:
            self.cancel_current_flow()
            return
        if event.kind == HotkeyEventKind.PRESSED:
            if self.settings.recording_mode == "toggle":
                self._toggle_recording()
            else:
                self.start_recording()
            return
        if (
            event.kind == HotkeyEventKind.RELEASED
            and self.settings.recording_mode == "hold"
        ):
            self.stop_recording()

    def _toggle_recording(self) -> None:
        with self.lock:
            recording_active = self.recording is not None
        if recording_active:
            self.stop_recording()
        else:
            self.start_recording()
