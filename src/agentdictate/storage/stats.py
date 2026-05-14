from __future__ import annotations

import sqlite3
from collections import Counter
from datetime import date, timedelta
from typing import Any

from agentdictate.costs import average_wpm


class StatsStoreMixin:
    _lock: object
    conn: sqlite3.Connection

    def stats_summary(self) -> dict[str, Any]:
        with self._lock:
            all_time = self.conn.execute(
                """
                SELECT
                    COUNT(*) AS total_sessions,
                    COALESCE(SUM(final_word_count), 0) AS total_words,
                    COALESCE(SUM(duration_seconds), 0) AS total_audio_seconds,
                    COALESCE(SUM(estimated_transcription_cost), 0) AS transcription_cost,
                    COALESCE(SUM(estimated_cleanup_cost), 0) AS cleanup_cost,
                    COALESCE(SUM(estimated_total_cost), 0) AS total_cost
                FROM dictation_sessions
                """
            ).fetchone()
            today = date.today()
            periods = {
                "today": today.isoformat(),
                "week": (today - timedelta(days=today.weekday())).isoformat(),
                "month": today.replace(day=1).isoformat(),
            }
            period_rows = {key: self._aggregate_since(start) for key, start in periods.items()}
            transcription_counter, cleanup_counter, cleanup_count = self._model_usage()
            total_words = int(all_time["total_words"] or 0)
            total_audio_seconds = float(all_time["total_audio_seconds"] or 0.0)
            total_sessions = int(all_time["total_sessions"] or 0)
            return {
                "total_sessions": total_sessions,
                "total_words": total_words,
                "total_audio_seconds": total_audio_seconds,
                "total_audio_hours": total_audio_seconds / 3600.0,
                "average_wpm": average_wpm(total_words, total_audio_seconds),
                "average_words_per_session": (
                    total_words / total_sessions if total_sessions else 0.0
                ),
                "average_duration_per_session": (
                    total_audio_seconds / total_sessions if total_sessions else 0.0
                ),
                "estimated_transcription_cost": float(all_time["transcription_cost"] or 0.0),
                "estimated_cleanup_cost": float(all_time["cleanup_cost"] or 0.0),
                "estimated_total_cost": float(all_time["total_cost"] or 0.0),
                "today": period_rows["today"],
                "week": period_rows["week"],
                "month": period_rows["month"],
                "most_used_transcription_model": (
                    transcription_counter.most_common(1)[0][0]
                    if transcription_counter
                    else ""
                ),
                "most_used_cleanup_model": (
                    cleanup_counter.most_common(1)[0][0] if cleanup_counter else ""
                ),
                "cleanup_mode_usage_count": cleanup_count,
            }

    def _model_usage(self) -> tuple[Counter[str], Counter[str], int]:
        rows = self.conn.execute(
            "SELECT transcription_model, cleanup_model, cleanup_enabled FROM dictation_sessions"
        )
        transcription_counter: Counter[str] = Counter()
        cleanup_counter: Counter[str] = Counter()
        cleanup_enabled_count = 0
        for row in rows:
            transcription_counter[str(row["transcription_model"])] += 1
            if row["cleanup_enabled"]:
                cleanup_enabled_count += 1
                if row["cleanup_model"]:
                    cleanup_counter[str(row["cleanup_model"])] += 1
        return transcription_counter, cleanup_counter, cleanup_enabled_count

    def _aggregate_since(self, start_day: str) -> dict[str, float | int]:
        with self._lock:
            row = self.conn.execute(
                """
                SELECT
                    COUNT(*) AS total_sessions,
                    COALESCE(SUM(final_word_count), 0) AS total_words,
                    COALESCE(SUM(duration_seconds), 0) AS total_audio_seconds,
                    COALESCE(SUM(estimated_transcription_cost), 0) AS transcription_cost,
                    COALESCE(SUM(estimated_cleanup_cost), 0) AS cleanup_cost,
                    COALESCE(SUM(estimated_total_cost), 0) AS total_cost
                FROM dictation_sessions
                WHERE substr(started_at, 1, 10) >= ?
                """,
                (start_day,),
            ).fetchone()
            return {
                "total_sessions": int(row["total_sessions"] or 0),
                "total_words": int(row["total_words"] or 0),
                "total_audio_seconds": float(row["total_audio_seconds"] or 0.0),
                "estimated_transcription_cost": float(row["transcription_cost"] or 0.0),
                "estimated_cleanup_cost": float(row["cleanup_cost"] or 0.0),
                "estimated_total_cost": float(row["total_cost"] or 0.0),
            }

    def graph_days(self, days: int = 30, metric: str = "words") -> list[dict[str, Any]]:
        metric_map = {
            "words": "total_words",
            "audio_minutes": "total_audio_seconds",
            "sessions": "total_sessions",
            "estimated_cost": "estimated_total_cost",
            "average_wpm": "average_wpm",
        }
        column = metric_map.get(metric, "total_words")
        end = date.today()
        start = end - timedelta(days=days - 1)
        with self._lock:
            rows = {
                str(row["date"]): row
                for row in self.conn.execute(
                    "SELECT * FROM daily_stats WHERE date >= ? ORDER BY date ASC",
                    (start.isoformat(),),
                )
            }
            result = []
            for offset in range(days):
                day = (start + timedelta(days=offset)).isoformat()
                value = float(rows[day][column]) if day in rows else 0.0
                if metric == "audio_minutes":
                    value = value / 60.0
                result.append({"date": day, "value": value})
            return result
