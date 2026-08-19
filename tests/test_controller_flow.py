from __future__ import annotations

import os
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

import requests

from agentdictate.audio import AudioError, Recording
from agentdictate.config import Settings
from agentdictate.controller import AgentDictateController
from agentdictate.hotkey import HotkeyEvent, HotkeyEventKind
from agentdictate.openai_client import OpenAIClientError
from agentdictate.storage import Storage


class ControllerFlowTests(unittest.TestCase):
    class FakeDucker:
        def __init__(self) -> None:
            self.events: list[object] = []

        def duck(self, _settings: Settings) -> None:
            self.events.append("duck")

        def restore(self, wait: bool = False) -> None:
            self.events.append(("restore", wait))

    def test_injected_storage_isolated_from_user_recordings_and_startup(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            user_data = root / "user-data"
            user_recordings = user_data / "agentdictate" / "recordings"
            user_recordings.mkdir(parents=True)
            (user_recordings / "real-user-recording.wav").write_bytes(
                b"RIFF" + b"\0" * 128
            )
            xdg_environment = {
                "XDG_CONFIG_HOME": str(root / "user-config"),
                "XDG_DATA_HOME": str(user_data),
                "XDG_STATE_HOME": str(root / "user-state"),
                "XDG_CACHE_HOME": str(root / "user-cache"),
            }

            with patch.dict(os.environ, xdg_environment), patch(
                "agentdictate.controller.app.set_start_on_login"
            ) as set_start_on_login:
                isolated_storage = Storage(root / "isolated" / "agentdictate.sqlite")
                controller = AgentDictateController(
                    settings=Settings(),
                    storage=isolated_storage,
                    audio_ducker=self.FakeDucker(),
                )

            observed_effects = (
                len(controller.list_recoverable_dictations()),
                set_start_on_login.call_count,
            )
            self.assertEqual(observed_effects, (0, 0))
            controller.close()

    def test_network_failure_keeps_captured_audio_recoverable(self) -> None:
        class OfflineClient:
            def __init__(self, _api_key: str) -> None:
                pass

            def transcribe(self, **_kwargs: object) -> str:
                raise requests.ConnectionError(
                    "Failed to resolve api.openai.com"
                )

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            audio_path = root / "speech.wav"
            audio_path.write_bytes(b"RIFF" + b"\0" * 128)
            process = Mock()
            process.poll.return_value = 0
            processing_finished = threading.Event()
            controller = AgentDictateController(
                settings=Settings(
                    openai_api_key="sk-test",
                    cleanup_enabled=False,
                ),
                storage=Storage(root / "agentdictate.sqlite"),
                audio_ducker=self.FakeDucker(),
                refresh_callback=processing_finished.set,
            )
            controller.recording = Recording(
                path=audio_path,
                started_at=time.monotonic() - 30.55,
                process=process,
                command_name="test-recorder",
            )

            with patch("agentdictate.controller.OpenAIClient", OfflineClient):
                controller.stop_recording()
                self.assertTrue(processing_finished.wait(2))

            self.assertTrue(
                audio_path.exists(),
                "captured audio must survive a transcription network failure",
            )
            history = controller.storage.list_history(limit=1)
            self.assertEqual(len(history), 1)
            self.assertEqual(history[0]["success"], 0)
            self.assertIn("resolve api.openai.com", history[0]["error_message"])
            controller.close()

    def test_failed_dictation_is_discoverable_after_restart(self) -> None:
        class OfflineClient:
            def __init__(self, _api_key: str) -> None:
                pass

            def transcribe(self, **_kwargs: object) -> str:
                raise requests.ConnectionError("network unavailable")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            database_path = root / "agentdictate.sqlite"
            audio_path = root / "speech.wav"
            audio_path.write_bytes(b"RIFF" + b"\0" * 128)
            process = Mock()
            process.poll.return_value = 0
            finished = threading.Event()
            controller = AgentDictateController(
                settings=Settings(openai_api_key="sk-test", cleanup_enabled=False),
                storage=Storage(database_path),
                audio_ducker=self.FakeDucker(),
                refresh_callback=finished.set,
            )
            controller.recording = Recording(
                audio_path,
                time.monotonic() - 12.0,
                process,
                "test-recorder",
            )
            with patch("agentdictate.controller.OpenAIClient", OfflineClient):
                controller.stop_recording()
                self.assertTrue(finished.wait(2))
            controller.close()

            restarted = AgentDictateController(
                settings=Settings(openai_api_key="sk-test", cleanup_enabled=False),
                storage=Storage(database_path),
                audio_ducker=self.FakeDucker(),
            )
            recoverable = restarted.list_recoverable_dictations()

            self.assertEqual(len(recoverable), 1)
            self.assertEqual(recoverable[0]["state"], "failed")
            self.assertEqual(recoverable[0]["stage"], "transcribing")
            self.assertEqual(Path(recoverable[0]["audio_path"]), audio_path)
            self.assertIn("network unavailable", recoverable[0]["error_message"])
            restarted.close()

    def test_startup_recovers_inflight_jobs_and_unregistered_audio(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            database_path = root / "agentdictate.sqlite"
            recording_directory = root / "recordings"
            recording_directory.mkdir()
            registered_audio = recording_directory / "recording-registered.wav"
            orphaned_audio = recording_directory / "recording-orphaned.wav"
            registered_audio.write_bytes(b"RIFF" + b"\0" * 128)
            orphaned_audio.write_bytes(b"RIFF" + b"\0" * 128)

            storage = Storage(database_path)
            job_id = storage.ensure_dictation_job(
                registered_audio,
                "2026-08-18T15:00:00+00:00",
                "gpt-transcribe",
            )
            storage.update_dictation_job(
                job_id,
                state="transcribing",
                stage="transcribing",
            )
            storage.close()

            with patch(
                "agentdictate.controller.app.recordings_dir",
                return_value=recording_directory,
            ):
                controller = AgentDictateController(
                    settings=Settings(cleanup_enabled=False),
                    storage=Storage(database_path),
                    audio_ducker=self.FakeDucker(),
                )

            recoverable = controller.list_recoverable_dictations()
            self.assertEqual(len(recoverable), 2)
            by_path = {Path(row["audio_path"]): row for row in recoverable}
            self.assertEqual(by_path[registered_audio]["state"], "interrupted")
            self.assertEqual(by_path[orphaned_audio]["state"], "interrupted")
            self.assertIn("restart", by_path[registered_audio]["error_message"].lower())
            self.assertIn("restart", by_path[orphaned_audio]["error_message"].lower())
            controller.close()

    def test_failed_dictation_can_be_retried_from_preserved_audio(self) -> None:
        class Client:
            last_transcription_model = "gpt-transcribe"

            def __init__(self, _api_key: str) -> None:
                pass

            def transcribe(self, **_kwargs: object) -> str:
                return "recovered words"

        class Clipboard:
            def __init__(self, *_args: object) -> None:
                pass

            def deliver(self, _text: str):
                from agentdictate.clipboard import PasteResult

                return PasteResult(copied=True, paste_triggered=True)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            audio_path = root / "speech.wav"
            audio_path.write_bytes(b"RIFF" + b"\0" * 128)
            storage = Storage(root / "agentdictate.sqlite")
            job_id = storage.ensure_dictation_job(
                audio_path,
                "2026-08-18T15:00:00+00:00",
                "gpt-transcribe",
            )
            storage.update_dictation_job(
                job_id,
                state="failed",
                stage="transcribing",
                duration_seconds=5.0,
                error_message="network unavailable",
            )
            finished = threading.Event()
            controller = AgentDictateController(
                settings=Settings(openai_api_key="sk-test", cleanup_enabled=False),
                storage=storage,
                audio_ducker=self.FakeDucker(),
                refresh_callback=finished.set,
            )

            with patch("agentdictate.controller.OpenAIClient", Client), patch(
                "agentdictate.controller.ClipboardPaste", Clipboard
            ):
                self.assertTrue(controller.retry_dictation(job_id))
                self.assertTrue(finished.wait(2))

            self.assertEqual(controller.list_recoverable_dictations(), [])
            history = controller.storage.list_history(limit=1)
            self.assertEqual(history[0]["final_text"], "recovered words")
            self.assertTrue(audio_path.exists())
            controller.close()

    def test_retry_worker_start_failure_does_not_wedge_the_controller(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            audio_path = root / "speech.wav"
            audio_path.write_bytes(b"RIFF" + b"\0" * 128)
            storage = Storage(root / "agentdictate.sqlite")
            job_id = storage.ensure_dictation_job(
                audio_path,
                "2026-08-18T15:00:00+00:00",
                "gpt-transcribe",
            )
            storage.update_dictation_job(
                job_id,
                state="failed",
                stage="transcribing",
                error_message="network unavailable",
            )
            controller = AgentDictateController(
                settings=Settings(cleanup_enabled=False),
                storage=storage,
                audio_ducker=self.FakeDucker(),
            )
            controller._start_processing_thread = Mock(  # type: ignore[method-assign]
                side_effect=RuntimeError("worker unavailable")
            )

            self.assertFalse(controller.retry_dictation(job_id))

            self.assertIsNone(controller.processing_session_id)
            self.assertEqual(controller.storage.get_dictation_job(job_id)["state"], "failed")
            self.assertTrue(audio_path.exists())
            controller.close()

    def test_delivery_retry_uses_saved_transcript_without_retranscribing(self) -> None:
        class MustNotTranscribe:
            def __init__(self, _api_key: str) -> None:
                raise AssertionError("delivery retry must not call the transcription API")

        class Clipboard:
            delivered: list[str] = []

            def __init__(self, *_args: object) -> None:
                pass

            def deliver(self, text: str):
                from agentdictate.clipboard import PasteResult

                Clipboard.delivered.append(text)
                return PasteResult(copied=True, paste_triggered=True)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            audio_path = root / "speech.wav"
            audio_path.write_bytes(b"RIFF" + b"\0" * 128)
            storage = Storage(root / "agentdictate.sqlite")
            job_id = storage.ensure_dictation_job(
                audio_path,
                "2026-08-18T15:00:00+00:00",
                "gpt-transcribe",
            )
            storage.update_dictation_job(
                job_id,
                state="delivery_failed",
                stage="delivering",
                duration_seconds=5.0,
                raw_transcript="saved words",
                final_text="saved words",
                error_message="clipboard unavailable",
            )
            finished = threading.Event()
            controller = AgentDictateController(
                settings=Settings(openai_api_key="sk-test", cleanup_enabled=False),
                storage=storage,
                audio_ducker=self.FakeDucker(),
                refresh_callback=finished.set,
            )

            with patch("agentdictate.controller.OpenAIClient", MustNotTranscribe), patch(
                "agentdictate.controller.ClipboardPaste", Clipboard
            ):
                self.assertTrue(controller.retry_dictation(job_id))
                self.assertTrue(finished.wait(2))

            self.assertEqual(Clipboard.delivered, ["saved words"])
            self.assertEqual(controller.list_recoverable_dictations(), [])
            controller.close()

    def test_delivery_retry_does_not_require_the_audio_file(self) -> None:
        class Clipboard:
            delivered: list[str] = []

            def __init__(self, *_args: object) -> None:
                pass

            def deliver(self, text: str):
                from agentdictate.clipboard import PasteResult

                Clipboard.delivered.append(text)
                return PasteResult(copied=True, paste_triggered=True)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            missing_audio = root / "missing.wav"
            storage = Storage(root / "agentdictate.sqlite")
            job_id = storage.ensure_dictation_job(
                missing_audio,
                "2026-08-18T15:00:00+00:00",
                "gpt-transcribe",
            )
            storage.update_dictation_job(
                job_id,
                state="delivery_failed",
                stage="delivering",
                final_text="already transcribed",
                error_message="clipboard unavailable",
            )
            finished = threading.Event()
            controller = AgentDictateController(
                settings=Settings(cleanup_enabled=False),
                storage=storage,
                audio_ducker=self.FakeDucker(),
                refresh_callback=finished.set,
            )

            with patch("agentdictate.controller.ClipboardPaste", Clipboard):
                self.assertTrue(controller.retry_dictation(job_id))
                self.assertTrue(finished.wait(2))

            self.assertEqual(Clipboard.delivered, ["already transcribed"])
            self.assertEqual(controller.list_recoverable_dictations(), [])
            controller.close()

    def test_saved_audio_is_deleted_only_by_an_explicit_recovery_action(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            audio_path = root / "speech.wav"
            audio_path.write_bytes(b"RIFF" + b"\0" * 128)
            storage = Storage(root / "agentdictate.sqlite")
            job_id = storage.ensure_dictation_job(
                audio_path,
                "2026-08-18T15:00:00+00:00",
                "gpt-transcribe",
            )
            storage.update_dictation_job(
                job_id,
                state="failed",
                stage="transcribing",
                error_message="network unavailable",
            )
            controller = AgentDictateController(
                settings=Settings(cleanup_enabled=False),
                storage=storage,
                audio_ducker=self.FakeDucker(),
            )

            self.assertTrue(controller.delete_recoverable_dictation(job_id))

            self.assertFalse(audio_path.exists())
            self.assertEqual(controller.list_recoverable_dictations(), [])
            self.assertEqual(controller.storage.get_dictation_job(job_id)["state"], "deleted")
            controller.close()

    def test_recording_is_registered_as_recoverable_before_it_stops(self) -> None:
        class Recorder:
            def __init__(self, recording: Recording) -> None:
                self.recording = recording

            def start(self) -> Recording:
                return self.recording

            def stop(self, _recording: Recording) -> float:
                return 1.0

            def delete_temp(self, _path: Path, preserve: bool = False) -> None:
                pass

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            audio_path = root / "speech.wav"
            audio_path.write_bytes(b"RIFF" + b"\0" * 128)
            recording = Recording(
                audio_path,
                time.monotonic(),
                Mock(),
                "test-recorder",
            )
            controller = AgentDictateController(
                settings=Settings(cleanup_enabled=False),
                storage=Storage(root / "agentdictate.sqlite"),
                audio_ducker=self.FakeDucker(),
            )
            controller.audio = Recorder(recording)  # type: ignore[assignment]

            controller.start_recording()

            recoverable = controller.list_recoverable_dictations()
            self.assertEqual(len(recoverable), 1)
            self.assertEqual(recoverable[0]["state"], "recording")
            self.assertEqual(Path(recoverable[0]["audio_path"]), audio_path)
            controller.close()

    def test_cancel_keeps_audio_as_an_explicit_recovery(self) -> None:
        class Recorder:
            def __init__(self, recording: Recording) -> None:
                self.recording = recording

            def start(self) -> Recording:
                return self.recording

            def stop(self, _recording: Recording) -> float:
                return 4.0

            def delete_temp(self, path: Path, preserve: bool = False) -> None:
                if not preserve:
                    path.unlink(missing_ok=True)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            audio_path = root / "speech.wav"
            audio_path.write_bytes(b"RIFF" + b"\0" * 128)
            controller = AgentDictateController(
                settings=Settings(cleanup_enabled=False),
                storage=Storage(root / "agentdictate.sqlite"),
                audio_ducker=self.FakeDucker(),
            )
            controller.audio = Recorder(
                Recording(audio_path, time.monotonic(), Mock(), "test-recorder")
            )  # type: ignore[assignment]
            controller.start_recording()

            controller.cancel_current_flow()

            self.assertTrue(audio_path.exists())
            recoverable = controller.list_recoverable_dictations()
            self.assertEqual(len(recoverable), 1)
            self.assertEqual(recoverable[0]["state"], "canceled")
            controller.close()

    def test_short_recording_is_transcribed_instead_of_deleted(self) -> None:
        class ShortRecorder:
            def __init__(self, recording: Recording) -> None:
                self.recording = recording

            def start(self) -> Recording:
                return self.recording

            def stop(self, _recording: Recording) -> float:
                return 0.1

            def delete_temp(self, path: Path, preserve: bool = False) -> None:
                if not preserve:
                    path.unlink(missing_ok=True)

        class Client:
            last_transcription_model = "gpt-transcribe"

            def __init__(self, _api_key: str) -> None:
                pass

            def transcribe(self, **_kwargs: object) -> str:
                return "yes"

        class Clipboard:
            def __init__(self, *_args: object) -> None:
                pass

            def deliver(self, _text: str):
                from agentdictate.clipboard import PasteResult

                return PasteResult(copied=True, paste_triggered=True)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            audio_path = root / "speech.wav"
            audio_path.write_bytes(b"RIFF" + b"\0" * 128)
            finished = threading.Event()
            controller = AgentDictateController(
                settings=Settings(openai_api_key="sk-test", cleanup_enabled=False),
                storage=Storage(root / "agentdictate.sqlite"),
                audio_ducker=self.FakeDucker(),
                refresh_callback=finished.set,
            )
            controller.audio = ShortRecorder(
                Recording(audio_path, time.monotonic(), Mock(), "test-recorder")
            )  # type: ignore[assignment]

            with patch("agentdictate.controller.OpenAIClient", Client), patch(
                "agentdictate.controller.ClipboardPaste", Clipboard
            ):
                controller.start_recording()
                controller.stop_recording()
                self.assertTrue(finished.wait(2))

            history = controller.storage.list_history(limit=1)
            self.assertEqual(len(history), 1)
            self.assertEqual(history[0]["final_text"], "yes")
            self.assertTrue(audio_path.exists())
            controller.close()

    def test_unexpected_recorder_exit_preserves_audio_and_ends_recording(self) -> None:
        class Recorder:
            def __init__(self, recording: Recording) -> None:
                self.recording = recording
                self.exited = threading.Event()

            def start(self) -> Recording:
                return self.recording

            def stop(self, _recording: Recording) -> float:
                return 1.0

            def wait_until_stopped(self, _recording: Recording) -> int:
                self.exited.wait(2)
                return 1

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            audio_path = root / "speech.wav"
            audio_path.write_bytes(b"RIFF" + b"\0" * 128)
            finished = threading.Event()
            controller = AgentDictateController(
                settings=Settings(cleanup_enabled=False),
                storage=Storage(root / "agentdictate.sqlite"),
                audio_ducker=self.FakeDucker(),
                refresh_callback=finished.set,
            )
            recorder = Recorder(
                Recording(audio_path, time.monotonic() - 1.0, Mock(), "test-recorder")
            )
            controller.audio = recorder  # type: ignore[assignment]
            controller.start_recording()

            recorder.exited.set()
            self.assertTrue(finished.wait(2))

            self.assertIsNone(controller.recording)
            self.assertEqual(controller.status, "Error")
            self.assertTrue(audio_path.exists())
            recoverable = controller.list_recoverable_dictations()
            self.assertEqual(recoverable[0]["state"], "interrupted")
            controller.close()

    def test_clipboard_failure_keeps_transcript_and_audio_recoverable(self) -> None:
        class Client:
            last_transcription_model = "gpt-transcribe"

            def __init__(self, _api_key: str) -> None:
                pass

            def transcribe(self, **_kwargs: object) -> str:
                return "durable transcript"

        class UnavailableClipboard:
            def __init__(self, *_args: object) -> None:
                pass

            def deliver(self, _text: str):
                from agentdictate.clipboard import PasteResult

                return PasteResult(
                    copied=False,
                    paste_triggered=False,
                    error="clipboard unavailable",
                )

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            audio_path = root / "speech.wav"
            audio_path.write_bytes(b"RIFF" + b"\0" * 128)
            finished = threading.Event()
            controller = AgentDictateController(
                settings=Settings(openai_api_key="sk-test", cleanup_enabled=False),
                storage=Storage(root / "agentdictate.sqlite"),
                audio_ducker=self.FakeDucker(),
                refresh_callback=finished.set,
            )
            process = Mock()
            process.poll.return_value = 0
            controller.recording = Recording(
                audio_path,
                time.monotonic() - 3.0,
                process,
                "test-recorder",
            )

            with patch("agentdictate.controller.OpenAIClient", Client), patch(
                "agentdictate.controller.ClipboardPaste", UnavailableClipboard
            ):
                controller.stop_recording()
                self.assertTrue(finished.wait(2))

            recoverable = controller.list_recoverable_dictations()
            self.assertEqual(len(recoverable), 1)
            self.assertEqual(recoverable[0]["state"], "delivery_failed")
            self.assertEqual(recoverable[0]["final_text"], "durable transcript")
            history = controller.storage.list_history(limit=1)
            self.assertEqual(history[0]["final_text"], "durable transcript")
            self.assertEqual(history[0]["success"], 0)
            self.assertTrue(audio_path.exists())
            controller.close()

    def test_shutdown_preserves_an_active_recording_as_interrupted(self) -> None:
        class Recorder:
            def __init__(self, recording: Recording) -> None:
                self.recording = recording

            def start(self) -> Recording:
                return self.recording

            def stop(self, _recording: Recording) -> float:
                return 8.0

            def delete_temp(self, path: Path, preserve: bool = False) -> None:
                if not preserve:
                    path.unlink(missing_ok=True)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            database_path = root / "agentdictate.sqlite"
            audio_path = root / "speech.wav"
            audio_path.write_bytes(b"RIFF" + b"\0" * 128)
            controller = AgentDictateController(
                settings=Settings(cleanup_enabled=False),
                storage=Storage(database_path),
                audio_ducker=self.FakeDucker(),
            )
            controller.audio = Recorder(
                Recording(audio_path, time.monotonic(), Mock(), "test-recorder")
            )  # type: ignore[assignment]
            controller.start_recording()

            controller.close()

            self.assertTrue(audio_path.exists())
            storage = Storage(database_path)
            recoverable = storage.list_recoverable_dictations()
            self.assertEqual(len(recoverable), 1)
            self.assertEqual(recoverable[0]["state"], "interrupted")
            storage.close()

    def test_shutdown_during_transcription_marks_job_interrupted(self) -> None:
        started = threading.Event()
        release = threading.Event()

        class BlockingClient:
            def __init__(self, _api_key: str) -> None:
                pass

            def transcribe(self, **_kwargs: object) -> str:
                started.set()
                release.wait(2)
                return "must not be pasted after shutdown"

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            database_path = root / "agentdictate.sqlite"
            audio_path = root / "speech.wav"
            audio_path.write_bytes(b"RIFF" + b"\0" * 128)
            storage = Storage(database_path)
            job_id = storage.ensure_dictation_job(
                audio_path,
                "2026-08-18T15:00:00+00:00",
                "gpt-transcribe",
            )
            storage.update_dictation_job(
                job_id,
                state="failed",
                stage="transcribing",
                duration_seconds=5.0,
                error_message="network unavailable",
            )
            controller = AgentDictateController(
                settings=Settings(openai_api_key="sk-test", cleanup_enabled=False),
                storage=storage,
                audio_ducker=self.FakeDucker(),
            )

            with patch("agentdictate.controller.OpenAIClient", BlockingClient):
                self.assertTrue(controller.retry_dictation(job_id))
                self.assertTrue(started.wait(2))
                controller.close()

                inspection = Storage(database_path)
                row = inspection.get_dictation_job(job_id)
                self.assertEqual(row["state"], "interrupted")
                self.assertTrue(audio_path.exists())
                inspection.close()
                release.set()

    def test_close_immediately_after_stop_marks_processing_job_interrupted(self) -> None:
        class Recorder:
            def __init__(self, recording: Recording) -> None:
                self.recording = recording

            def start(self) -> Recording:
                return self.recording

            def stop(self, _recording: Recording) -> float:
                return 2.0

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            database_path = root / "agentdictate.sqlite"
            audio_path = root / "speech.wav"
            audio_path.write_bytes(b"RIFF" + b"\0" * 128)
            controller = AgentDictateController(
                settings=Settings(cleanup_enabled=False),
                storage=Storage(database_path),
                audio_ducker=self.FakeDucker(),
            )
            controller.audio = Recorder(
                Recording(audio_path, time.monotonic(), Mock(), "test-recorder")
            )  # type: ignore[assignment]
            controller._start_processing_thread = Mock()  # type: ignore[method-assign]
            controller.start_recording()
            job_id = controller.recording_job_id

            controller.stop_recording()
            controller.close()

            inspection = Storage(database_path)
            self.assertEqual(
                inspection.get_dictation_job(job_id)["state"],
                "interrupted",
            )
            inspection.close()

    def test_processing_thread_start_failure_keeps_recording_recoverable(self) -> None:
        class Recorder:
            def __init__(self, recording: Recording) -> None:
                self.recording = recording

            def start(self) -> Recording:
                return self.recording

            def stop(self, _recording: Recording) -> float:
                return 2.0

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            audio_path = root / "speech.wav"
            audio_path.write_bytes(b"RIFF" + b"\0" * 128)
            controller = AgentDictateController(
                settings=Settings(cleanup_enabled=False),
                storage=Storage(root / "agentdictate.sqlite"),
                audio_ducker=self.FakeDucker(),
            )
            controller.audio = Recorder(
                Recording(audio_path, time.monotonic(), Mock(), "test-recorder")
            )  # type: ignore[assignment]
            controller.start_recording()
            controller._start_processing_thread = Mock(  # type: ignore[method-assign]
                side_effect=RuntimeError("could not start worker")
            )

            controller.stop_recording()

            self.assertEqual(controller.status, "Error")
            self.assertTrue(audio_path.exists())
            recoverable = controller.list_recoverable_dictations()
            self.assertEqual(recoverable[0]["state"], "interrupted")
            self.assertIn("worker", recoverable[0]["error_message"])
            controller.close()

    def test_toggle_hotkey_uses_controller_recording_state(self) -> None:
        controller = object.__new__(AgentDictateController)
        controller.settings = Settings(recording_mode="toggle")
        controller.lock = threading.RLock()
        controller._closed = False
        controller.recording = None
        controller.start_recording = Mock()
        controller.stop_recording = Mock()

        controller._handle_hotkey_event(HotkeyEvent(HotkeyEventKind.PRESSED))
        controller._handle_hotkey_event(HotkeyEvent(HotkeyEventKind.RELEASED))
        # An automatic stop leaves recording empty; the next press must start again.
        controller.recording = None
        controller._handle_hotkey_event(HotkeyEvent(HotkeyEventKind.PRESSED))

        self.assertEqual(controller.start_recording.call_count, 2)
        controller.stop_recording.assert_not_called()

    def test_hotkey_recovery_does_not_overwrite_recording_status(self) -> None:
        controller = object.__new__(AgentDictateController)
        controller.lock = threading.RLock()
        controller._closed = False
        controller.settings = Settings()
        controller.status = "Recording"
        controller.hotkey_available = False
        controller._hotkey_unavailable_reported = False
        controller.message = Mock()
        controller.refresh = Mock()

        controller._handle_hotkey_event(
            HotkeyEvent(HotkeyEventKind.UNAVAILABLE, "input unavailable")
        )
        controller._handle_hotkey_event(HotkeyEvent(HotkeyEventKind.AVAILABLE))

        self.assertEqual(controller.status, "Recording")
        self.assertTrue(controller.hotkey_available)
        self.assertFalse(controller._hotkey_unavailable_reported)
        self.assertEqual(controller.refresh.call_count, 2)

    def test_hold_hotkey_starts_on_press_and_stops_on_release(self) -> None:
        controller = object.__new__(AgentDictateController)
        controller.lock = threading.RLock()
        controller._closed = False
        controller.settings = Settings(recording_mode="hold")
        controller.start_recording = Mock()
        controller.stop_recording = Mock()

        controller._handle_hotkey_event(HotkeyEvent(HotkeyEventKind.PRESSED))
        controller._handle_hotkey_event(HotkeyEvent(HotkeyEventKind.RELEASED))

        controller.start_recording.assert_called_once_with()
        controller.stop_recording.assert_called_once_with()

    def test_hotkey_event_cannot_start_recording_after_close_begins(self) -> None:
        controller = object.__new__(AgentDictateController)
        controller.lock = threading.RLock()
        controller._closed = True
        controller.start_recording = Mock()

        controller._handle_hotkey_event(HotkeyEvent(HotkeyEventKind.PRESSED))

        controller.start_recording.assert_not_called()

    def test_start_recording_is_rejected_after_close_begins(self) -> None:
        controller = object.__new__(AgentDictateController)
        controller.lock = threading.RLock()
        controller._closed = True
        controller.recording = None
        controller.audio = Mock()

        controller.start_recording()

        controller.audio.start.assert_not_called()

    def test_start_recording_is_rejected_while_another_dictation_processes(self) -> None:
        controller = object.__new__(AgentDictateController)
        controller.lock = threading.RLock()
        controller._closed = False
        controller.recording = None
        controller.processing_session_id = 123
        controller.audio = Mock()
        controller.message = Mock()

        controller.start_recording()

        controller.audio.start.assert_not_called()
        controller.message.assert_called_once()

    def test_start_recording_restores_audio_when_recorder_fails(self) -> None:
        class FailingAudio:
            def start(self) -> Recording:
                raise AudioError("microphone unavailable")

        with tempfile.TemporaryDirectory() as directory:
            storage = Storage(Path(directory) / "agentdictate.sqlite")
            ducker = self.FakeDucker()
            controller = AgentDictateController(
                settings=Settings(),
                storage=storage,
                audio_ducker=ducker,
            )
            controller.audio = FailingAudio()  # type: ignore[assignment]

            controller.start_recording()

            self.assertEqual(ducker.events, ["duck", ("restore", False)])
            self.assertEqual(controller.status, "Error")
            storage.close()

    def test_stop_recording_restores_audio_when_recorder_stop_fails(self) -> None:
        class FailingAudio:
            def stop(self, _recording: Recording) -> float:
                raise AudioError("stop failed")

        with tempfile.TemporaryDirectory() as directory:
            storage = Storage(Path(directory) / "agentdictate.sqlite")
            ducker = self.FakeDucker()
            controller = AgentDictateController(
                settings=Settings(),
                storage=storage,
                audio_ducker=ducker,
            )
            controller.audio = FailingAudio()  # type: ignore[assignment]
            controller.recording = Recording(Path(directory) / "speech.wav", 0.0, Mock(), "fake")

            controller.stop_recording()

            self.assertEqual(ducker.events, [("restore", False)])
            self.assertEqual(controller.status, "Error")
            storage.close()

    def test_cancel_recording_restores_audio(self) -> None:
        class FakeAudio:
            def stop(self, _recording: Recording) -> float:
                return 1.0

            def delete_temp(self, _path: Path, preserve: bool = False) -> None:
                pass

        with tempfile.TemporaryDirectory() as directory:
            storage = Storage(Path(directory) / "agentdictate.sqlite")
            ducker = self.FakeDucker()
            controller = AgentDictateController(
                settings=Settings(),
                storage=storage,
                audio_ducker=ducker,
            )
            controller.audio = FakeAudio()  # type: ignore[assignment]
            controller.recording = Recording(Path(directory) / "speech.wav", 0.0, Mock(), "fake")

            controller.cancel_current_flow()

            self.assertEqual(ducker.events, [("restore", False)])
            self.assertEqual(controller.status, "Ready")
            storage.close()

    def test_close_restores_audio_and_waits_for_fade(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            ducker = self.FakeDucker()
            controller = AgentDictateController(
                settings=Settings(),
                storage=Storage(Path(directory) / "agentdictate.sqlite"),
                audio_ducker=ducker,
            )

            controller.close()

            self.assertEqual(ducker.events, [("restore", True)])

    def test_close_stops_and_preserves_an_active_recording(self) -> None:
        class FakeAudio:
            def __init__(self) -> None:
                self.stopped: list[Recording] = []
                self.deleted: list[tuple[Path, bool]] = []

            def stop(self, recording: Recording) -> float:
                self.stopped.append(recording)
                return 1.0

            def delete_temp(self, path: Path, preserve: bool = False) -> None:
                self.deleted.append((path, preserve))

        with tempfile.TemporaryDirectory() as directory:
            storage = Storage(Path(directory) / "agentdictate.sqlite")
            ducker = self.FakeDucker()
            controller = AgentDictateController(
                settings=Settings(),
                storage=storage,
                audio_ducker=ducker,
            )
            audio = FakeAudio()
            controller.audio = audio  # type: ignore[assignment]
            recording = Recording(
                Path(directory) / "speech.wav", 0.0, Mock(), "fake"
            )
            controller.recording = recording

            controller.close()

            self.assertIsNone(controller.recording)
            self.assertEqual(audio.stopped, [recording])
            self.assertEqual(audio.deleted, [])

    def test_close_preserves_active_recording_when_recorder_stop_fails(self) -> None:
        class FailingAudio:
            def __init__(self) -> None:
                self.deleted: list[tuple[Path, bool]] = []

            def stop(self, _recording: Recording) -> float:
                raise AudioError("recorder already stopped")

            def delete_temp(self, path: Path, preserve: bool = False) -> None:
                self.deleted.append((path, preserve))

        with tempfile.TemporaryDirectory() as directory:
            storage = Storage(Path(directory) / "agentdictate.sqlite")
            controller = AgentDictateController(
                settings=Settings(),
                storage=storage,
                audio_ducker=self.FakeDucker(),
            )
            audio = FailingAudio()
            controller.audio = audio  # type: ignore[assignment]
            recording = Recording(
                Path(directory) / "speech.wav", 0.0, Mock(), "fake"
            )
            controller.recording = recording

            controller.close()

            self.assertIsNone(controller.recording)
            self.assertEqual(audio.deleted, [])

    def test_cleanup_failure_pastes_raw_and_records_error(self) -> None:
        class FakeClient:
            def __init__(self, _api_key: str) -> None:
                pass

            def transcribe(self, **_kwargs: object) -> str:
                return "raw transcript"

            def cleanup(self, **_kwargs: object) -> str:
                raise OpenAIClientError("cleanup unavailable")

        class FakeClipboard:
            def __init__(
                self,
                _restore: bool = False,
                _shortcut_mode: str = "Automatic",
            ) -> None:
                pass

            def deliver(self, text: str):
                from agentdictate.clipboard import PasteResult

                self.text = text
                return PasteResult(copied=True, paste_triggered=True)

        with tempfile.TemporaryDirectory() as directory:
            db_path = Path(directory) / "agentdictate.sqlite"
            audio_path = Path(directory) / "speech.wav"
            audio_path.write_bytes(b"RIFFfake")
            settings = Settings(openai_api_key="sk-test", cleanup_enabled=True)
            storage = Storage(db_path)
            controller = AgentDictateController(settings=settings, storage=storage)
            with patch("agentdictate.controller.OpenAIClient", FakeClient), patch(
                "agentdictate.controller.ClipboardPaste", FakeClipboard
            ):
                controller._process_recording(audio_path, 2.0, "2026-05-13T16:00:00+00:00")
            rows = storage.list_history()
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0]["raw_transcript"], "raw transcript")
            self.assertIsNone(rows[0]["cleaned_transcript"])
            self.assertEqual(rows[0]["final_text"], "raw transcript")
            self.assertEqual(rows[0]["cleanup_error"], "cleanup unavailable")
            self.assertEqual(rows[0]["paste_triggered"], 1)
            storage.close()

    def test_cancel_during_processing_does_not_paste_or_save_history(self) -> None:
        class FakeClient:
            def __init__(self, _api_key: str) -> None:
                pass

            def transcribe(self, **_kwargs: object) -> str:
                controller.cancel_current_flow()
                return "raw transcript"

        class FakeClipboard:
            called = False

            def __init__(
                self,
                _restore: bool = False,
                _shortcut_mode: str = "Automatic",
            ) -> None:
                pass

            def deliver(self, _text: str):
                FakeClipboard.called = True

        with tempfile.TemporaryDirectory() as directory:
            db_path = Path(directory) / "agentdictate.sqlite"
            audio_path = Path(directory) / "speech.wav"
            audio_path.write_bytes(b"RIFFfake")
            settings = Settings(openai_api_key="sk-test", cleanup_enabled=False)
            storage = Storage(db_path)
            controller = AgentDictateController(settings=settings, storage=storage)
            with patch("agentdictate.controller.OpenAIClient", FakeClient), patch(
                "agentdictate.controller.ClipboardPaste", FakeClipboard
            ):
                controller._process_recording(
                    audio_path,
                    2.0,
                    "2026-05-13T16:00:00+00:00",
                    session_id=99,
                )
            self.assertFalse(FakeClipboard.called)
            self.assertEqual(storage.list_history(), [])
            self.assertEqual(controller.status, "Ready")
            self.assertTrue(
                audio_path.exists(),
                "canceling transcription must preserve the captured audio",
            )
            recoverable = controller.list_recoverable_dictations()
            self.assertEqual(len(recoverable), 1)
            self.assertEqual(recoverable[0]["state"], "canceled")
            storage.close()
