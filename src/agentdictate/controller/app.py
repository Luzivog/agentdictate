from __future__ import annotations

import threading
import time

from agentdictate.audio import AudioRecorder, Recording
from agentdictate.audio_ducking import AudioDucker
from agentdictate.config import Settings, load_settings, repair_zero_pricing_defaults, save_settings
from agentdictate.hotkey import HotkeyError, InputHotkeyListener
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
