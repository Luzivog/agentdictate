from __future__ import annotations

import threading
from agentdictate.audio import AudioError, Recording
from agentdictate.feedback import play_feedback
from agentdictate.storage import utc_now


class RecordingMixin:
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
                self._clear_active_recording()
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
            self._cancel_recording(recording, session_id)

    def _clear_active_recording(self) -> None:
        self.recording = None
        self.recording_session_id = None
        self.recording_started_at_iso = None
        if self.max_timer:
            self.max_timer.cancel()
            self.max_timer = None

    def _cancel_recording(self, recording: Recording, session_id: int | None) -> None:
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
