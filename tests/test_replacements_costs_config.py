from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from agentdictate.config import (
    Settings,
    load_settings,
    repair_zero_pricing_defaults,
    reset_pricing_defaults,
    save_settings,
)
from agentdictate.costs import (
    average_wpm,
    estimate_cleanup_cost,
    estimate_session_cost,
    estimate_tokens,
    estimate_transcription_cost,
    word_count,
)
from agentdictate.replacements import ReplacementMapping, apply_replacements


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
    def test_default_transcription_model_is_gpt_transcribe(self) -> None:
        self.assertEqual(Settings().transcription_model, "gpt-transcribe")

    def test_default_recording_mode_is_toggle(self) -> None:
        self.assertEqual(Settings().recording_mode, "toggle")

    def test_default_paste_shortcut_is_automatic(self) -> None:
        self.assertEqual(Settings().paste_shortcut, "Automatic")

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
                audio_ducking_enabled=False,
                audio_ducking_volume_percent=25,
                audio_ducking_fade_ms=1500,
                paste_shortcut="Terminal (Ctrl+Shift+V)",
            )
            save_settings(settings, path)
            loaded = load_settings(path)
            self.assertEqual(loaded.active_transcription_model(), "new-transcribe")
            self.assertEqual(loaded.active_cleanup_model(), "new-cleanup")
            self.assertEqual(loaded.active_cleanup_reasoning_effort(), "high")
            self.assertFalse(loaded.audio_ducking_enabled)
            self.assertEqual(loaded.audio_ducking_volume_percent, 25)
            self.assertEqual(loaded.audio_ducking_fade_ms, 1500)
            self.assertEqual(loaded.paste_shortcut, "Terminal (Ctrl+Shift+V)")

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

    def test_load_settings_adds_new_pricing_without_overwriting_custom_prices(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "config.json"
            settings = Settings(transcription_model="gpt-4o-transcribe")
            settings.transcription_prices.pop("gpt-transcribe")
            settings.transcription_prices["gpt-4o-transcribe"][
                "price_per_audio_minute"
            ] = 0.007
            save_settings(settings, path)

            loaded = load_settings(path)

        self.assertEqual(
            loaded.transcription_prices["gpt-transcribe"]["price_per_audio_minute"],
            0.0045,
        )
        self.assertEqual(
            loaded.transcription_prices["gpt-4o-transcribe"][
                "price_per_audio_minute"
            ],
            0.007,
        )
        self.assertEqual(loaded.transcription_model, "gpt-4o-transcribe")
