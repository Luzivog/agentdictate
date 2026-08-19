from __future__ import annotations

import struct
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from agentdictate.audio import AudioRecorder, Recording
from agentdictate.cleanup import build_cleanup_instruction
from agentdictate.overlay import OverlayHelperState
from agentdictate.startup import desktop_entry
from agentdictate.ui.settings_actions import SettingsActionsMixin


class CleanupPromptTests(unittest.TestCase):
    def test_cleanup_prompt_styles(self) -> None:
        light = build_cleanup_instruction("Light cleanup", "Base")
        structured = build_cleanup_instruction("Structured coding prompt", "Base")
        self.assertIn("Light cleanup", light)
        self.assertIn("Goal", structured)


class OverlayStateTests(unittest.TestCase):
    def test_waveform_frames_do_not_repeat_window_status_transitions(self) -> None:
        state = OverlayHelperState()
        recording = {
            "status": "Recording",
            "cleanup_enabled": False,
            "elapsed": 1.5,
            "waveform": [0.1, 0.2],
        }

        self.assertTrue(state.apply(recording))
        self.assertFalse(state.apply({**recording, "elapsed": 1.6}))
        self.assertEqual(state.elapsed, 1.6)
        self.assertEqual(state.waveform, [0.1, 0.2])


class StartupTests(unittest.TestCase):
    def test_desktop_entry_uses_explicit_executable_and_background(self) -> None:
        entry = desktop_entry(exec_path="/tmp/agentdictate", launch_hidden=True)
        self.assertIn("Exec=/tmp/agentdictate --background", entry)
        self.assertIn("StartupWMClass=local.agentdictate.AgentDictate", entry)
        self.assertIn("X-GNOME-Autostart-enabled=true", entry)

    def test_background_refresh_skips_unbuilt_cleanup_preview(self) -> None:
        partial_ui = object.__new__(SettingsActionsMixin)

        partial_ui._update_cleanup_preview()


class AudioTests(unittest.TestCase):
    def test_start_uses_recording_readiness_instead_of_sleep(self) -> None:
        process = Mock(pid=1234)
        process.poll.return_value = None
        with tempfile.TemporaryDirectory() as directory:
            recorder = AudioRecorder()
            with patch("agentdictate.audio.recordings_dir", return_value=Path(directory)), patch.object(
                recorder, "_record_command", return_value=["pw-record", "test.wav"]
            ), patch("agentdictate.audio.subprocess.Popen", return_value=process), patch.object(
                recorder, "_wait_for_recording_ready", return_value=True
            ) as ready, patch("agentdictate.audio.time.sleep") as sleep:
                recording = recorder.start()

        ready.assert_called_once_with(process, recording.path)
        sleep.assert_not_called()

    def test_failed_start_reaps_the_recorder_process(self) -> None:
        process = Mock(pid=1234)
        process.poll.return_value = None
        with tempfile.TemporaryDirectory() as directory:
            recorder = AudioRecorder()
            with patch(
                "agentdictate.audio.recordings_dir", return_value=Path(directory)
            ), patch.object(
                recorder, "_record_command", return_value=["pw-record", "test.wav"]
            ), patch(
                "agentdictate.audio.subprocess.Popen", return_value=process
            ), patch.object(
                recorder, "_wait_for_recording_ready", return_value=False
            ):
                with self.assertRaisesRegex(RuntimeError, "default microphone"):
                    recorder.start()

        process.terminate.assert_called_once_with()
        process.wait.assert_called_once_with(timeout=1.0)

    def test_input_level_reflects_recent_wav_samples(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            audio_path = Path(directory) / "speech.wav"
            samples = [0] * 512 + [12000, -12000] * 512
            audio_path.write_bytes(b"0" * 44 + struct.pack(f"<{len(samples)}h", *samples))
            recording = Recording(
                path=audio_path,
                started_at=0.0,
                process=Mock(),
                command_name="test",
            )
            level = AudioRecorder().input_level(recording)
        self.assertGreater(level, 0.20)
        self.assertLess(level, 0.40)

    def test_input_waveform_uses_fixed_recent_sample_bins(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            audio_path = Path(directory) / "speech.wav"
            samples = [0] * 256 + [2000, -2000] * 256 + [14000, -14000] * 256
            audio_path.write_bytes(b"0" * 44 + struct.pack(f"<{len(samples)}h", *samples))
            recording = Recording(
                path=audio_path,
                started_at=0.0,
                process=Mock(),
                command_name="test",
            )
            waveform = AudioRecorder().input_waveform(recording, bin_count=8)
        self.assertEqual(len(waveform), 8)
        self.assertLess(waveform[0], waveform[-1])
        self.assertGreater(waveform[-1], 0.30)
