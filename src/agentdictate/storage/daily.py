from __future__ import annotations

import sqlite3

from agentdictate.costs import average_wpm


class DailyStatsMixin:
    _lock: object
    conn: sqlite3.Connection

    def recompute_daily_stats(self, day: str) -> None:
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
                WHERE substr(started_at, 1, 10) = ?
                """,
                (day,),
            ).fetchone()
            total_words = int(row["total_words"] or 0)
            total_audio_seconds = float(row["total_audio_seconds"] or 0.0)
            with self.conn:
                self.conn.execute(
                    """
                    INSERT INTO daily_stats (
                        date, total_sessions, total_words, total_audio_seconds,
                        average_wpm, estimated_transcription_cost,
                        estimated_cleanup_cost, estimated_total_cost
                    )
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                    ON CONFLICT(date) DO UPDATE SET
                        total_sessions=excluded.total_sessions,
                        total_words=excluded.total_words,
                        total_audio_seconds=excluded.total_audio_seconds,
                        average_wpm=excluded.average_wpm,
                        estimated_transcription_cost=excluded.estimated_transcription_cost,
                        estimated_cleanup_cost=excluded.estimated_cleanup_cost,
                        estimated_total_cost=excluded.estimated_total_cost
                    """,
                    (
                        day,
                        int(row["total_sessions"] or 0),
                        total_words,
                        total_audio_seconds,
                        average_wpm(total_words, total_audio_seconds),
                        float(row["transcription_cost"] or 0.0),
                        float(row["cleanup_cost"] or 0.0),
                        float(row["total_cost"] or 0.0),
                    ),
                )
