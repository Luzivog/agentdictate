from __future__ import annotations

import struct
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock

from agentdictate.audio import AudioRecorder, Recording
from agentdictate.cleanup import build_cleanup_instruction
from agentdictate.hotkey import InputHotkeyListener, KEY_ESC, KEY_LEFTCTRL, KEY_SPACE, parse_hotkey
from agentdictate.startup import desktop_entry


class CleanupPromptTests(unittest.TestCase):
    def test_cleanup_prompt_styles(self) -> None:
        light = build_cleanup_instruction("Light cleanup", "Base")
        structured = build_cleanup_instruction("Structured coding prompt", "Base")
        self.assertIn("Light cleanup", light)
        self.assertIn("Goal", structured)


class HotkeyTests(unittest.TestCase):
    def test_parse_ctrl_space(self) -> None:
        spec = parse_hotkey("Ctrl+Space")
        self.assertTrue(spec.matches({29, 57}))
        self.assertTrue(spec.matches({97, 57}))
        self.assertFalse(spec.matches({57}))

    def test_escape_cancels_active_hotkey_without_stopping(self) -> None:
        events: list[str] = []
        listener = InputHotkeyListener(
            hotkey="Ctrl+Space",
            recording_mode="toggle",
            on_start=lambda: events.append("start"),
            on_stop=lambda: events.append("stop"),
            on_cancel=lambda: events.append("cancel"),
            on_error=lambda _message: events.append("error"),
        )
        listener._handle_key_event(KEY_LEFTCTRL, 1, {KEY_LEFTCTRL})
        listener._handle_key_event(KEY_SPACE, 1, {KEY_LEFTCTRL, KEY_SPACE})
        listener._handle_key_event(KEY_ESC, 1, {KEY_LEFTCTRL, KEY_SPACE, KEY_ESC})
        listener._handle_key_event(KEY_SPACE, 0, {KEY_LEFTCTRL})
        self.assertEqual(events, ["start", "cancel"])


class StartupTests(unittest.TestCase):
    def test_desktop_entry_uses_explicit_executable_and_background(self) -> None:
        entry = desktop_entry(exec_path="/tmp/agentdictate", launch_hidden=True)
        self.assertIn("Exec=/tmp/agentdictate --background", entry)
        self.assertIn("StartupWMClass=local.agentdictate.AgentDictate", entry)
        self.assertIn("X-GNOME-Autostart-enabled=true", entry)


class AudioTests(unittest.TestCase):
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
