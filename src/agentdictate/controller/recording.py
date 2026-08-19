from __future__ import annotations

import threading
import time
from collections.abc import Callable

from agentdictate.audio import AudioError, Recording
from agentdictate.diagnostics import log_event
from agentdictate.feedback import play_feedback
from agentdictate.storage import utc_now


class RecordingMixin:
    def start_recording(self) -> None:
        with self.lock:
            if self._closed or self.recording is not None:
                return
            if self.processing_session_id is not None:
                self.message("Wait for the current dictation to finish processing.")
                return
            self.audio_ducker.duck(self.settings)
            try:
                self.recording = self.audio.start()
            except AudioError as exc:
                self.audio_ducker.restore()
                self.set_status("Error")
                self.message(str(exc))
                return
            self.recording_started_at_iso = utc_now()
            self.recording_job_id = self.storage.ensure_dictation_job(
                self.recording.path,
                self.recording_started_at_iso,
                self.settings.active_transcription_model(),
            )
            self.storage.update_dictation_job(
                self.recording_job_id,
                state="recording",
                stage="recording",
            )
            self.recording_session_id = self.recording_job_id
            log_event(
                "recording_started",
                job_id=self.recording_job_id,
                recorder=self.recording.command_name,
            )
            wait_until_stopped = getattr(self.audio, "wait_until_stopped", None)
            if wait_until_stopped is not None:
                watcher = threading.Thread(
                    target=self._watch_recording_process,
                    args=(self.recording, self.recording_job_id, wait_until_stopped),
                    name="agentdictate-recorder-watch",
                    daemon=True,
                )
                watcher.start()
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

    def _watch_recording_process(
        self,
        recording: Recording,
        job_id: int,
        wait_until_stopped: Callable[[Recording], int],
    ) -> None:
        try:
            return_code = wait_until_stopped(recording)
        except Exception as exc:
            log_event(
                "recorder_watch_failed",
                job_id=job_id,
                error=str(exc),
            )
            return
        with self.lock:
            if self._closed or self.recording is not recording:
                return
            duration = max(0.0, time.monotonic() - recording.started_at)
            self._clear_active_recording()
        self.audio_ducker.restore()
        self.storage.update_dictation_job(
            job_id,
            state="interrupted",
            stage="recording",
            duration_seconds=duration,
            error_message=f"The audio recorder stopped unexpectedly (exit {return_code}).",
        )
        log_event(
            "recording_interrupted",
            job_id=job_id,
            recorder=recording.command_name,
            return_code=return_code,
            duration_seconds=round(duration, 3),
        )
        self.set_status("Error")
        self.message(
            "Recording stopped unexpectedly. Audio saved for recovery.",
            f"{recording.command_name} exited with status {return_code}.",
        )
        self.refresh()

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
            self.recording_job_id = None
            if session_id is not None:
                self.processing_session_id = session_id
        try:
            duration = self.audio.stop(recording)
        except AudioError as exc:
            self.audio_ducker.restore()
            if session_id is not None:
                self.storage.update_dictation_job(
                    session_id,
                    state="interrupted",
                    stage="recording",
                    error_message=str(exc),
                )
                log_event("recording_stop_failed", job_id=session_id, error=str(exc))
                self._finish_session(session_id)
            self.set_status("Error")
            self.message(str(exc))
            return
        self.audio_ducker.restore()
        if session_id is not None:
            self.storage.update_dictation_job(
                session_id,
                state="captured",
                stage="captured",
                duration_seconds=duration,
            )
            log_event(
                "recording_captured",
                job_id=session_id,
                duration_seconds=round(duration, 3),
            )
        play_feedback(
            "stop",
            enabled=self.settings.sound_feedback and self.settings.stop_sound,
        )
        self.set_status("Transcribing")
        self.message("Transcribing...")
        try:
            self._start_processing_thread(
                self._process_recording,
                (recording.path, duration, started_at, session_id),
            )
        except Exception as exc:
            if session_id is not None:
                self.storage.update_dictation_job(
                    session_id,
                    state="interrupted",
                    stage="captured",
                    duration_seconds=duration,
                    error_message=f"Could not start transcription worker: {exc}",
                )
                self._finish_session(session_id)
                log_event(
                    "dictation_worker_start_failed",
                    job_id=session_id,
                    error=str(exc),
                )
            self.set_status("Error")
            self.message("Could not start transcription. Audio saved for recovery.", str(exc))
            self.refresh()

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
        self.recording_job_id = None
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
        if session_id is not None:
            self.storage.update_dictation_job(
                session_id,
                state="canceled",
                stage="recording",
                error_message="Canceled by user; audio preserved.",
            )
            log_event("recording_canceled", job_id=session_id)
            self._finish_session(session_id)
        self.set_status("Ready")
        self.message("Recording canceled. Audio saved for recovery.")
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
