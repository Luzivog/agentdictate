from __future__ import annotations

import tempfile
import threading
import unittest
from pathlib import Path

from agentdictate.config import Settings
from agentdictate.replacements import ReplacementMapping
from agentdictate.storage import HistoryRecord, Storage


def history_record(**overrides: object) -> HistoryRecord:
    values = {
        "started_at": "2026-05-13T16:00:00+00:00",
        "ended_at": "2026-05-13T16:00:12+00:00",
        "duration_seconds": 12,
        "transcription_model": "gpt-4o-transcribe",
        "cleanup_enabled": True,
        "cleanup_model": "gpt-5.4-nano",
        "cleanup_style": "Light cleanup",
        "raw_transcript": "fix the versel deploy",
        "cleaned_transcript": "Fix the Vercel deploy.",
        "final_text": "Fix the Vercel deploy.",
        "replacements_applied": [],
        "copied_to_clipboard": True,
        "paste_triggered": True,
        "raw_word_count": 4,
        "final_word_count": 4,
        "final_character_count": 22,
        "estimated_transcription_cost": 0.0012,
        "estimated_cleanup_cost": 0.0001,
        "estimated_total_cost": 0.0013,
        "success": True,
    }
    values.update(overrides)
    return HistoryRecord(**values)  # type: ignore[arg-type]


class StorageTests(unittest.TestCase):
    def test_history_stats_mappings_and_pricing(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            storage = Storage(Path(directory) / "agentdictate.sqlite")
            settings = Settings()
            storage.seed_pricing(settings)
            self.assertGreaterEqual(len(storage.list_pricing()), 3)
            mapping_id = storage.add_mapping(ReplacementMapping.new("versel", "Vercel"))
            self.assertEqual(storage.list_mappings()[0].id, mapping_id)
            storage.add_history_record(history_record())
            history = storage.list_history(search="Vercel")
            self.assertEqual(len(history), 1)
            stats = storage.stats_summary()
            self.assertEqual(stats["total_words"], 4)
            self.assertGreater(stats["average_wpm"], 0)
            self.assertEqual(len(storage.graph_days(days=1)), 1)
            storage.close()

    def test_history_can_be_written_from_background_thread(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            storage = Storage(Path(directory) / "agentdictate.sqlite")
            errors: list[BaseException] = []

            def write_history() -> None:
                try:
                    storage.add_history_record(
                        history_record(
                            duration_seconds=5,
                            cleanup_enabled=False,
                            cleanup_model=None,
                            cleanup_style=None,
                            raw_transcript="threaded transcript",
                            cleaned_transcript=None,
                            final_text="threaded transcript",
                            raw_word_count=2,
                            final_word_count=2,
                            final_character_count=19,
                            estimated_transcription_cost=0.0005,
                            estimated_cleanup_cost=0.0,
                            estimated_total_cost=0.0005,
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
                history_record(
                    ended_at="2026-05-13T16:01:00+00:00",
                    duration_seconds=60,
                    cleanup_enabled=False,
                    cleanup_model=None,
                    cleanup_style=None,
                    raw_transcript="one minute transcript",
                    cleaned_transcript=None,
                    final_text="one minute transcript",
                    raw_word_count=3,
                    final_word_count=3,
                    final_character_count=21,
                    estimated_transcription_cost=0.0,
                    estimated_cleanup_cost=0.0,
                    estimated_total_cost=0.0,
                )
            )
            storage.reprice_history(Settings())
            row = storage.list_history()[0]
            self.assertAlmostEqual(float(row["estimated_transcription_cost"]), 0.006)
            self.assertAlmostEqual(float(row["estimated_total_cost"]), 0.006)
            self.assertAlmostEqual(storage.stats_summary()["estimated_total_cost"], 0.006)
            storage.close()
