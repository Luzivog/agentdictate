from __future__ import annotations

import sqlite3
from pathlib import Path
from typing import Any

from .schema import utc_now


class DictationJobStoreMixin:
    _lock: object
    conn: sqlite3.Connection

    def ensure_dictation_job(
        self,
        audio_path: Path,
        started_at: str,
        transcription_model: str,
    ) -> int:
        path = str(audio_path)
        with self._lock:
            row = self.conn.execute(
                "SELECT id FROM dictation_jobs WHERE audio_path = ?",
                (path,),
            ).fetchone()
            if row is not None:
                return int(row["id"])
            now = utc_now()
            with self.conn:
                cursor = self.conn.execute(
                    """
                    INSERT INTO dictation_jobs (
                        started_at, updated_at, state, stage, audio_path,
                        transcription_model
                    ) VALUES (?, ?, 'captured', 'captured', ?, ?)
                    """,
                    (started_at, now, path, transcription_model),
                )
            return int(cursor.lastrowid)

    def update_dictation_job(
        self,
        job_id: int,
        *,
        state: str,
        stage: str,
        duration_seconds: float | None = None,
        raw_transcript: str | None = None,
        final_text: str | None = None,
        copied_to_clipboard: bool | None = None,
        paste_triggered: bool | None = None,
        error_message: str | None = None,
    ) -> None:
        values: dict[str, Any] = {
            "state": state,
            "stage": stage,
            "updated_at": utc_now(),
            "error_message": error_message,
        }
        optional = {
            "duration_seconds": duration_seconds,
            "raw_transcript": raw_transcript,
            "final_text": final_text,
            "copied_to_clipboard": (
                int(copied_to_clipboard)
                if copied_to_clipboard is not None
                else None
            ),
            "paste_triggered": (
                int(paste_triggered) if paste_triggered is not None else None
            ),
        }
        values.update({key: value for key, value in optional.items() if value is not None})
        assignments = ", ".join(f"{key} = ?" for key in values)
        with self._lock:
            with self.conn:
                self.conn.execute(
                    f"UPDATE dictation_jobs SET {assignments} WHERE id = ?",
                    (*values.values(), job_id),
                )

    def list_recoverable_dictations(self) -> list[sqlite3.Row]:
        with self._lock:
            return list(
                self.conn.execute(
                    """
                    SELECT * FROM dictation_jobs
                    WHERE state NOT IN ('delivered', 'deleted')
                    ORDER BY updated_at DESC, id DESC
                    """
                )
            )

    def list_dictation_jobs(self) -> list[sqlite3.Row]:
        with self._lock:
            return list(
                self.conn.execute(
                    "SELECT * FROM dictation_jobs ORDER BY updated_at DESC, id DESC"
                )
            )

    def get_dictation_job(self, job_id: int) -> sqlite3.Row | None:
        with self._lock:
            return self.conn.execute(
                "SELECT * FROM dictation_jobs WHERE id = ?",
                (job_id,),
            ).fetchone()
