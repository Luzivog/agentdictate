from __future__ import annotations

import json
import struct
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from agentdictate.audio import AudioRecorder, Recording
from agentdictate.cleanup import build_cleanup_instruction
from agentdictate.clipboard import ClipboardPaste, _parse_xmodmap_v_keycode
from agentdictate.config import (
    Settings,
    load_settings,
    repair_zero_pricing_defaults,
    reset_pricing_defaults,
    save_settings,
)
from agentdictate.controller import AgentDictateController
from agentdictate.costs import (
    average_wpm,
    estimate_cleanup_cost,
    estimate_session_cost,
    estimate_tokens,
    estimate_transcription_cost,
    word_count,
)
from agentdictate.hotkey import (
    InputHotkeyListener,
    KEY_ESC,
    KEY_LEFTCTRL,
    KEY_SPACE,
    parse_hotkey,
)
from agentdictate.openai_client import OpenAIClient, OpenAIClientError, extract_response_text
from agentdictate.replacements import ReplacementMapping, apply_replacements
from agentdictate.startup import desktop_entry
from agentdictate.storage import HistoryRecord, Storage


class ReplacementTests(unittest.TestCase):
    def test_apply_whole_word_case_insensitive_replacement(self) -> None:
        mappings = [
            ReplacementMapping.new("shoe", "SHU"),
            ReplacementMapping.new("next js", "Next.js"),
        ]
        output, applied = apply_replacements(
            "Please update the shoe integration, not shoebox, in next js.",
            mappings,
        )
        self.assertEqual(
            output, "Please update the SHU integration, not shoebox, in Next.js."
        )
        self.assertEqual(len(applied), 2)

    def test_case_sensitive_mapping(self) -> None:
        mapping = ReplacementMapping.new(
            "API", "application programming interface", case_sensitive=True
        )
        output, applied = apply_replacements("api API", [mapping])
        self.assertEqual(output, "api application programming interface")
        self.assertEqual(applied[0]["count"], 1)


class CostTests(unittest.TestCase):
    def test_word_count_and_wpm(self) -> None:
        self.assertEqual(word_count("Fix next-js route handler tests."), 5)
        self.assertEqual(average_wpm(120, 60), 120)

    def test_transcription_and_cleanup_costs(self) -> None:
        self.assertAlmostEqual(estimate_transcription_cost(90, 0.006), 0.009)
        cleanup_cost, input_tokens, output_tokens = estimate_cleanup_cost(
            "abcd" * 10, "abcd" * 20, 1.0, 2.0
        )
        self.assertEqual(input_tokens, estimate_tokens("abcd" * 10))
        self.assertEqual(output_tokens, estimate_tokens("abcd" * 20))
        self.assertGreater(cleanup_cost, 0)

    def test_cleanup_disabled_cost_is_zero(self) -> None:
        estimate = estimate_session_cost(
            duration_seconds=30,
            raw_transcript="raw transcript",
            cleaned_transcript=None,
            cleanup_enabled=False,
            transcription_price_per_minute=0.006,
            cleanup_input_price_per_1m_tokens=10,
            cleanup_output_price_per_1m_tokens=10,
        )
        self.assertGreater(estimate.transcription_cost, 0)
        self.assertEqual(estimate.cleanup_cost, 0)
        self.assertEqual(estimate.total_cost, estimate.transcription_cost)


class ConfigTests(unittest.TestCase):
    def test_default_recording_mode_is_toggle(self) -> None:
        self.assertEqual(Settings().recording_mode, "toggle")

    def test_start_on_login_defaults_to_enabled(self) -> None:
        self.assertTrue(Settings().start_on_login)

    def test_config_round_trip_and_active_models(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "config.json"
            settings = Settings(
                transcription_model="Custom",
                custom_transcription_model="new-transcribe",
                cleanup_model="Custom",
                custom_cleanup_model="new-cleanup",
                cleanup_reasoning_effort="high",
            )
            save_settings(settings, path)
            loaded = load_settings(path)
            self.assertEqual(loaded.active_transcription_model(), "new-transcribe")
            self.assertEqual(loaded.active_cleanup_model(), "new-cleanup")
            self.assertEqual(loaded.active_cleanup_reasoning_effort(), "high")

    def test_default_cleanup_reasoning_effort_is_omitted(self) -> None:
        self.assertEqual(Settings().active_cleanup_reasoning_effort(), "")

    def test_reset_pricing_defaults(self) -> None:
        settings = Settings()
        settings.cleanup_prices = {}
        reset_pricing_defaults(settings)
        self.assertIn("gpt-5.4-nano", settings.cleanup_prices)

    def test_zero_pricing_defaults_are_repaired(self) -> None:
        settings = Settings()
        for price in settings.transcription_prices.values():
            price["price_per_audio_minute"] = 0.0
        for price in settings.cleanup_prices.values():
            price["input_price_per_1m_tokens"] = 0.0
            price["output_price_per_1m_tokens"] = 0.0
        self.assertTrue(repair_zero_pricing_defaults(settings))
        self.assertGreater(
            settings.transcription_prices["gpt-4o-transcribe"]["price_per_audio_minute"],
            0,
        )
        self.assertGreater(
            settings.cleanup_prices["gpt-5.4-nano"]["input_price_per_1m_tokens"],
            0,
        )

    def test_load_settings_repairs_zero_pricing_file(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "config.json"
            settings = Settings()
            for price in settings.transcription_prices.values():
                price["price_per_audio_minute"] = 0.0
            for price in settings.cleanup_prices.values():
                price["input_price_per_1m_tokens"] = 0.0
                price["output_price_per_1m_tokens"] = 0.0
            save_settings(settings, path)
            loaded = load_settings(path)
        self.assertGreater(
            loaded.transcription_prices["gpt-4o-transcribe"]["price_per_audio_minute"],
            0,
        )


class StorageTests(unittest.TestCase):
    def test_history_stats_mappings_and_pricing(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            storage = Storage(Path(directory) / "agentdictate.sqlite")
            settings = Settings()
            storage.seed_pricing(settings)
            self.assertGreaterEqual(len(storage.list_pricing()), 3)
            mapping_id = storage.add_mapping(
                ReplacementMapping.new("versel", "Vercel")
            )
            mappings = storage.list_mappings()
            self.assertEqual(mappings[0].id, mapping_id)
            storage.add_history_record(
                HistoryRecord(
                    started_at="2026-05-13T16:00:00+00:00",
                    ended_at="2026-05-13T16:00:12+00:00",
                    duration_seconds=12,
                    transcription_model="gpt-4o-transcribe",
                    cleanup_enabled=True,
                    cleanup_model="gpt-5.4-nano",
                    cleanup_style="Light cleanup",
                    raw_transcript="fix the versel deploy",
                    cleaned_transcript="Fix the Vercel deploy.",
                    final_text="Fix the Vercel deploy.",
                    replacements_applied=[],
                    copied_to_clipboard=True,
                    paste_triggered=True,
                    raw_word_count=4,
                    final_word_count=4,
                    final_character_count=22,
                    estimated_transcription_cost=0.0012,
                    estimated_cleanup_cost=0.0001,
                    estimated_total_cost=0.0013,
                    success=True,
                )
            )
            history = storage.list_history(search="Vercel")
            self.assertEqual(len(history), 1)
            stats = storage.stats_summary()
            self.assertEqual(stats["total_words"], 4)
            self.assertGreater(stats["average_wpm"], 0)
            graph = storage.graph_days(days=1)
            self.assertEqual(len(graph), 1)
            storage.close()

    def test_history_can_be_written_from_background_thread(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            storage = Storage(Path(directory) / "agentdictate.sqlite")
            errors: list[BaseException] = []

            def write_history() -> None:
                try:
                    storage.add_history_record(
                        HistoryRecord(
                            started_at="2026-05-13T16:00:00+00:00",
                            ended_at="2026-05-13T16:00:05+00:00",
                            duration_seconds=5,
                            transcription_model="gpt-4o-transcribe",
                            cleanup_enabled=False,
                            cleanup_model=None,
                            cleanup_style=None,
                            raw_transcript="threaded transcript",
                            cleaned_transcript=None,
                            final_text="threaded transcript",
                            replacements_applied=[],
                            copied_to_clipboard=True,
                            paste_triggered=True,
                            raw_word_count=2,
                            final_word_count=2,
                            final_character_count=19,
                            estimated_transcription_cost=0.0005,
                            estimated_cleanup_cost=0.0,
                            estimated_total_cost=0.0005,
                            success=True,
                        )
                    )
                except BaseException as exc:
                    errors.append(exc)

            thread = threading.Thread(target=write_history)
            thread.start()
            thread.join(timeout=5)
            self.assertFalse(thread.is_alive())
            self.assertEqual(errors, [])
            self.assertEqual(storage.list_history()[0]["final_text"], "threaded transcript")
            storage.close()

    def test_reprice_history_updates_existing_zero_cost_sessions(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            storage = Storage(Path(directory) / "agentdictate.sqlite")
            storage.add_history_record(
                HistoryRecord(
                    started_at="2026-05-13T16:00:00+00:00",
                    ended_at="2026-05-13T16:01:00+00:00",
                    duration_seconds=60,
                    transcription_model="gpt-4o-transcribe",
                    cleanup_enabled=False,
                    cleanup_model=None,
                    cleanup_style=None,
                    raw_transcript="one minute transcript",
                    cleaned_transcript=None,
                    final_text="one minute transcript",
                    replacements_applied=[],
                    copied_to_clipboard=True,
                    paste_triggered=True,
                    raw_word_count=3,
                    final_word_count=3,
                    final_character_count=21,
                    estimated_transcription_cost=0.0,
                    estimated_cleanup_cost=0.0,
                    estimated_total_cost=0.0,
                    success=True,
                )
            )
            storage.reprice_history(Settings())
            row = storage.list_history()[0]
            self.assertAlmostEqual(float(row["estimated_transcription_cost"]), 0.006)
            self.assertAlmostEqual(float(row["estimated_total_cost"]), 0.006)
            stats = storage.stats_summary()
            self.assertAlmostEqual(stats["estimated_total_cost"], 0.006)
            storage.close()


class OpenAITests(unittest.TestCase):
    def test_extract_response_text(self) -> None:
        payload = {
            "output": [
                {"type": "reasoning", "summary": []},
                {
                    "type": "message",
                    "content": [
                        {"type": "output_text", "text": "Cleaned prompt."},
                    ],
                },
            ]
        }
        self.assertEqual(extract_response_text(payload), "Cleaned prompt.")

    @patch("agentdictate.openai_client.requests.post")
    def test_transcription_request_shape(self, post: Mock) -> None:
        response = Mock()
        response.ok = True
        response.text = "raw transcript"
        post.return_value = response
        with tempfile.TemporaryDirectory() as directory:
            audio_path = Path(directory) / "speech.wav"
            audio_path.write_bytes(b"RIFFfake")
            client = OpenAIClient("sk-test")
            text = client.transcribe(
                audio_path, "gpt-4o-transcribe", language="en", prompt="coding terms"
            )
        self.assertEqual(text, "raw transcript")
        self.assertEqual(client.last_transcription_model, "gpt-4o-transcribe")
        self.assertEqual(post.call_args.args[0], "https://api.openai.com/v1/audio/transcriptions")
        data = post.call_args.kwargs["data"]
        self.assertEqual(data["model"], "gpt-4o-transcribe")
        self.assertEqual(data["response_format"], "text")
        self.assertEqual(data["language"], "en")
        self.assertIn("Transcribe the entire recording", data["prompt"])
        self.assertIn("coding terms", data["prompt"])

    @patch("agentdictate.openai_client.requests.post")
    def test_transcription_retries_single_sentence_long_recording_with_whisper(self, post: Mock) -> None:
        first_response = Mock()
        first_response.ok = True
        first_response.text = "This is only the first sentence."
        second_response = Mock()
        second_response.ok = True
        second_response.text = "This is only the first sentence. This is the second sentence too."
        post.side_effect = [first_response, second_response]
        with tempfile.TemporaryDirectory() as directory:
            audio_path = Path(directory) / "speech.wav"
            audio_path.write_bytes(b"RIFFfake")
            client = OpenAIClient("sk-test")
            text = client.transcribe(
                audio_path,
                "gpt-4o-transcribe",
                audio_duration_seconds=8.0,
            )
        self.assertEqual(text, "This is only the first sentence. This is the second sentence too.")
        self.assertEqual(client.last_transcription_model, "whisper-1")
        self.assertEqual(post.call_args_list[0].kwargs["data"]["model"], "gpt-4o-transcribe")
        self.assertEqual(post.call_args_list[1].kwargs["data"]["model"], "whisper-1")

    @patch("agentdictate.openai_client.requests.post")
    def test_cleanup_request_shape(self, post: Mock) -> None:
        response = Mock()
        response.ok = True
        response.json.return_value = {
            "output": [
                {
                    "type": "message",
                    "content": [{"type": "output_text", "text": "Cleaned prompt."}],
                }
            ]
        }
        post.return_value = response
        text = OpenAIClient("sk-test").cleanup("raw", "gpt-5.4-nano", "instruction")
        self.assertEqual(text, "Cleaned prompt.")
        self.assertEqual(post.call_args.args[0], "https://api.openai.com/v1/responses")
        payload = json.loads(post.call_args.kwargs["data"])
        self.assertEqual(payload["model"], "gpt-5.4-nano")
        self.assertNotIn("reasoning", payload)

    @patch("agentdictate.openai_client.requests.post")
    def test_cleanup_reasoning_effort_request_shape(self, post: Mock) -> None:
        response = Mock()
        response.ok = True
        response.json.return_value = {
            "output": [
                {
                    "type": "message",
                    "content": [{"type": "output_text", "text": "Cleaned prompt."}],
                }
            ]
        }
        post.return_value = response
        text = OpenAIClient("sk-test").cleanup(
            "raw", "gpt-5.4-nano", "instruction", reasoning_effort="high"
        )
        self.assertEqual(text, "Cleaned prompt.")
        payload = json.loads(post.call_args.kwargs["data"])
        self.assertEqual(payload["reasoning"]["effort"], "high")


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


class ClipboardTests(unittest.TestCase):
    @patch("agentdictate.clipboard.subprocess.run")
    @patch("agentdictate.clipboard.shutil.which")
    @patch.dict("agentdictate.clipboard.os.environ", {"WAYLAND_DISPLAY": "wayland-0"}, clear=True)
    def test_wayland_paste_triggers_ctrl_shift_v_not_enter(self, which: Mock, run: Mock) -> None:
        which.side_effect = lambda command: f"/usr/bin/{command}" if command == "ydotool" else None
        run.return_value = Mock(returncode=0)
        with patch("agentdictate.clipboard.detect_paste_keycode", return_value=47):
            self.assertTrue(ClipboardPaste().trigger_paste())
        command = run.call_args.args[0]
        self.assertEqual(command, ["ydotool", "key", "--key-delay", "25", "ctrl+shift+v"])
        self.assertNotIn("28:1", command)
        self.assertNotIn("type", command)

    @patch("agentdictate.clipboard.subprocess.run")
    @patch("agentdictate.clipboard.shutil.which")
    @patch.dict("agentdictate.clipboard.os.environ", {"WAYLAND_DISPLAY": "wayland-0"}, clear=True)
    def test_wayland_paste_raw_fallback_uses_detected_layout_key(self, which: Mock, run: Mock) -> None:
        which.side_effect = lambda command: f"/usr/bin/{command}" if command == "ydotool" else None
        run.side_effect = [Mock(returncode=1), Mock(returncode=0)]
        with patch("agentdictate.clipboard.detect_paste_keycode", return_value=39):
            self.assertTrue(ClipboardPaste().trigger_paste())
        self.assertEqual(
            run.call_args_list[0].args[0],
            ["ydotool", "key", "--key-delay", "25", "ctrl+shift+v"],
        )
        self.assertEqual(
            run.call_args_list[1].args[0],
            ["ydotool", "key", "--key-delay", "25", "29:1", "42:1", "39:1", "39:0", "42:0", "29:0"],
        )

    @patch("agentdictate.clipboard.subprocess.run")
    @patch("agentdictate.clipboard.shutil.which")
    @patch.dict("agentdictate.clipboard.os.environ", {}, clear=True)
    def test_xdotool_paste_clears_modifiers(self, which: Mock, run: Mock) -> None:
        which.side_effect = lambda command: f"/usr/bin/{command}" if command == "xdotool" else None
        run.return_value = Mock(returncode=0)
        self.assertTrue(ClipboardPaste().trigger_paste())
        self.assertEqual(run.call_args.args[0], ["xdotool", "key", "--clearmodifiers", "ctrl+shift+v"])

    def test_parse_xmodmap_detects_v_for_azerty_layout(self) -> None:
        output = """
        keycode  47 = m M semicolon colon mu ordmasculine
        keycode  55 = v V v V doublelowquotemark singlelowquotemark
        """
        self.assertEqual(_parse_xmodmap_v_keycode(output), 47)

    def test_parse_xmodmap_uses_non_default_v_key_when_layout_moves_v(self) -> None:
        output = """
        keycode  47 = m M m M
        keycode  48 = v V v V
        """
        self.assertEqual(_parse_xmodmap_v_keycode(output), 40)


class ControllerFlowTests(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main()
