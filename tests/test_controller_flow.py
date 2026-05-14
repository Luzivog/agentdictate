from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from agentdictate.audio import AudioError, Recording
from agentdictate.config import Settings
from agentdictate.controller import AgentDictateController
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

    def test_cleanup_failure_pastes_raw_and_records_error(self) -> None:
        class FakeClient:
            def __init__(self, _api_key: str) -> None:
                pass

            def transcribe(self, **_kwargs: object) -> str:
                return "raw transcript"

            def cleanup(self, **_kwargs: object) -> str:
                raise OpenAIClientError("cleanup unavailable")

        class FakeClipboard:
            def __init__(self, _restore: bool = False) -> None:
                pass

            def copy_and_paste(self, text: str):
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

            def __init__(self, _restore: bool = False) -> None:
                pass

            def copy_and_paste(self, _text: str):
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
            storage.close()
