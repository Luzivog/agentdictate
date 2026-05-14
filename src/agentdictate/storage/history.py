from __future__ import annotations

import json
import sqlite3
from typing import Any

from .models import HistoryRecord


class HistoryStoreMixin:
    _lock: object
    conn: sqlite3.Connection

    def add_history_record(self, record: HistoryRecord) -> int:
        with self._lock:
            with self.conn:
                cursor = self.conn.execute(
                    """
                    INSERT INTO dictation_sessions (
                        started_at, ended_at, duration_seconds, transcription_model,
                        cleanup_enabled, cleanup_model, cleanup_style, raw_word_count,
                        final_word_count, final_character_count,
                        estimated_transcription_cost, estimated_cleanup_cost,
                        estimated_total_cost, success, error_message
                    )
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        record.started_at,
                        record.ended_at,
                        record.duration_seconds,
                        record.transcription_model,
                        int(record.cleanup_enabled),
                        record.cleanup_model,
                        record.cleanup_style,
                        record.raw_word_count,
                        record.final_word_count,
                        record.final_character_count,
                        record.estimated_transcription_cost,
                        record.estimated_cleanup_cost,
                        record.estimated_total_cost,
                        int(record.success),
                        record.error_message,
                    ),
                )
                session_id = int(cursor.lastrowid)
                self.conn.execute(
                    """
                    INSERT INTO transcript_history (
                        session_id, created_at, raw_transcript, cleaned_transcript,
                        final_text, replacements_applied, copied_to_clipboard,
                        paste_triggered, cleanup_error
                    )
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        session_id,
                        record.ended_at,
                        record.raw_transcript,
                        record.cleaned_transcript,
                        record.final_text,
                        json.dumps(record.replacements_applied),
                        int(record.copied_to_clipboard),
                        int(record.paste_triggered),
                        record.cleanup_error,
                    ),
                )
            self.recompute_daily_stats(record.started_at[:10])
            return session_id

    def list_history(
        self, search: str = "", day: str = "", limit: int = 250
    ) -> list[sqlite3.Row]:
        clauses: list[str] = []
        values: list[Any] = []
        if search:
            clauses.append(
                "(h.raw_transcript LIKE ? OR h.cleaned_transcript LIKE ? OR h.final_text LIKE ?)"
            )
            term = f"%{search}%"
            values.extend([term, term, term])
        if day:
            clauses.append("substr(h.created_at, 1, 10) = ?")
            values.append(day)
        where = f"WHERE {' AND '.join(clauses)}" if clauses else ""
        values.append(limit)
        with self._lock:
            return list(
                self.conn.execute(
                    f"""
                    SELECT
                        h.*,
                        s.started_at,
                        s.ended_at,
                        s.duration_seconds,
                        s.transcription_model,
                        s.cleanup_enabled,
                        s.cleanup_model,
                        s.cleanup_style,
                        s.raw_word_count,
                        s.final_word_count,
                        s.final_character_count,
                        s.estimated_transcription_cost,
                        s.estimated_cleanup_cost,
                        s.estimated_total_cost,
                        s.success,
                        s.error_message
                    FROM transcript_history h
                    JOIN dictation_sessions s ON s.id = h.session_id
                    {where}
                    ORDER BY h.created_at DESC
                    LIMIT ?
                    """,
                    values,
                )
            )

    def get_history(self, history_id: int) -> sqlite3.Row | None:
        rows = self.list_history(limit=5000)
        for row in rows:
            if int(row["id"]) == history_id:
                return row
        return None

    def delete_history(self, history_id: int) -> None:
        with self._lock:
            row = self.conn.execute(
                "SELECT session_id, created_at FROM transcript_history WHERE id = ?",
                (history_id,),
            ).fetchone()
            if row is None:
                return
            day = str(row["created_at"])[:10]
            with self.conn:
                self.conn.execute(
                    "DELETE FROM dictation_sessions WHERE id = ?", (int(row["session_id"]),)
                )
            self.recompute_daily_stats(day)

    def clear_history(self) -> None:
        with self._lock:
            with self.conn:
                self.conn.execute("DELETE FROM transcript_history")
                self.conn.execute("DELETE FROM dictation_sessions")
                self.conn.execute("DELETE FROM daily_stats")
