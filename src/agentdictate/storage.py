from __future__ import annotations

import json
import sqlite3
import threading
from collections import Counter
from dataclasses import dataclass
from datetime import date, datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from .config import Settings
from .costs import average_wpm, estimate_session_cost
from .paths import database_path, ensure_app_dirs
from .replacements import ReplacementMapping


SCHEMA = """
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS dictation_sessions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at TEXT NOT NULL,
    ended_at TEXT NOT NULL,
    duration_seconds REAL NOT NULL DEFAULT 0,
    transcription_model TEXT NOT NULL,
    cleanup_enabled INTEGER NOT NULL DEFAULT 0,
    cleanup_model TEXT,
    cleanup_style TEXT,
    raw_word_count INTEGER NOT NULL DEFAULT 0,
    final_word_count INTEGER NOT NULL DEFAULT 0,
    final_character_count INTEGER NOT NULL DEFAULT 0,
    estimated_transcription_cost REAL NOT NULL DEFAULT 0,
    estimated_cleanup_cost REAL NOT NULL DEFAULT 0,
    estimated_total_cost REAL NOT NULL DEFAULT 0,
    success INTEGER NOT NULL DEFAULT 1,
    error_message TEXT
);

CREATE TABLE IF NOT EXISTS transcript_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id INTEGER NOT NULL REFERENCES dictation_sessions(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    raw_transcript TEXT NOT NULL DEFAULT '',
    cleaned_transcript TEXT,
    final_text TEXT NOT NULL DEFAULT '',
    replacements_applied TEXT NOT NULL DEFAULT '[]',
    copied_to_clipboard INTEGER NOT NULL DEFAULT 0,
    paste_triggered INTEGER NOT NULL DEFAULT 0,
    cleanup_error TEXT
);

CREATE TABLE IF NOT EXISTS replacement_mappings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_phrase TEXT NOT NULL,
    replacement_phrase TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    case_sensitive INTEGER NOT NULL DEFAULT 0,
    whole_word_only INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS daily_stats (
    date TEXT PRIMARY KEY,
    total_sessions INTEGER NOT NULL DEFAULT 0,
    total_words INTEGER NOT NULL DEFAULT 0,
    total_audio_seconds REAL NOT NULL DEFAULT 0,
    average_wpm REAL NOT NULL DEFAULT 0,
    estimated_transcription_cost REAL NOT NULL DEFAULT 0,
    estimated_cleanup_cost REAL NOT NULL DEFAULT 0,
    estimated_total_cost REAL NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS pricing_settings (
    model_name TEXT NOT NULL,
    model_type TEXT NOT NULL,
    input_price_per_1m_tokens REAL NOT NULL DEFAULT 0,
    output_price_per_1m_tokens REAL NOT NULL DEFAULT 0,
    price_per_audio_minute REAL NOT NULL DEFAULT 0,
    currency TEXT NOT NULL DEFAULT 'USD',
    updated_at TEXT NOT NULL,
    PRIMARY KEY (model_name, model_type)
);

CREATE INDEX IF NOT EXISTS idx_sessions_started_at ON dictation_sessions(started_at);
CREATE INDEX IF NOT EXISTS idx_history_created_at ON transcript_history(created_at);
CREATE INDEX IF NOT EXISTS idx_history_session_id ON transcript_history(session_id);
"""


def utc_now() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat()


@dataclass
class HistoryRecord:
    started_at: str
    ended_at: str
    duration_seconds: float
    transcription_model: str
    cleanup_enabled: bool
    cleanup_model: str | None
    cleanup_style: str | None
    raw_transcript: str
    cleaned_transcript: str | None
    final_text: str
    replacements_applied: list[dict[str, Any]]
    copied_to_clipboard: bool
    paste_triggered: bool
    raw_word_count: int
    final_word_count: int
    final_character_count: int
    estimated_transcription_cost: float
    estimated_cleanup_cost: float
    estimated_total_cost: float
    success: bool
    error_message: str | None = None
    cleanup_error: str | None = None


class Storage:
    def __init__(self, path: Path | None = None) -> None:
        ensure_app_dirs()
        self.path = path or database_path()
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._lock = threading.RLock()
        self.conn = sqlite3.connect(self.path, check_same_thread=False)
        self.conn.row_factory = sqlite3.Row
        with self._lock:
            self.conn.executescript(SCHEMA)
            self.conn.execute("PRAGMA foreign_keys = ON")
            self.conn.commit()

    def close(self) -> None:
        with self._lock:
            self.conn.close()

    def seed_pricing(self, settings: Settings) -> None:
        now = utc_now()
        with self._lock:
            with self.conn:
                for model_name, price in settings.transcription_prices.items():
                    self.conn.execute(
                        """
                        INSERT INTO pricing_settings (
                            model_name, model_type, price_per_audio_minute, currency,
                            updated_at
                        )
                        VALUES (?, 'transcription', ?, ?, ?)
                        ON CONFLICT(model_name, model_type) DO UPDATE SET
                            price_per_audio_minute=excluded.price_per_audio_minute,
                            currency=excluded.currency,
                            updated_at=excluded.updated_at
                        """,
                        (
                            model_name,
                            float(price.get("price_per_audio_minute", 0.0) or 0.0),
                            str(price.get("currency", settings.currency) or settings.currency),
                            now,
                        ),
                    )
                for model_name, price in settings.cleanup_prices.items():
                    self.conn.execute(
                        """
                        INSERT INTO pricing_settings (
                            model_name, model_type, input_price_per_1m_tokens,
                            output_price_per_1m_tokens, currency, updated_at
                        )
                        VALUES (?, 'cleanup', ?, ?, ?, ?)
                        ON CONFLICT(model_name, model_type) DO UPDATE SET
                            input_price_per_1m_tokens=excluded.input_price_per_1m_tokens,
                            output_price_per_1m_tokens=excluded.output_price_per_1m_tokens,
                            currency=excluded.currency,
                            updated_at=excluded.updated_at
                        """,
                        (
                            model_name,
                            float(price.get("input_price_per_1m_tokens", 0.0) or 0.0),
                            float(price.get("output_price_per_1m_tokens", 0.0) or 0.0),
                            str(price.get("currency", settings.currency) or settings.currency),
                            now,
                        ),
                    )

    def list_pricing(self) -> list[sqlite3.Row]:
        with self._lock:
            return list(
                self.conn.execute(
                    "SELECT * FROM pricing_settings ORDER BY model_type, model_name"
                )
            )

    def reprice_history(self, settings: Settings) -> None:
        with self._lock:
            rows = list(
                self.conn.execute(
                    """
                    SELECT
                        s.id,
                        s.started_at,
                        s.duration_seconds,
                        s.transcription_model,
                        s.cleanup_enabled,
                        s.cleanup_model,
                        h.raw_transcript,
                        h.cleaned_transcript
                    FROM dictation_sessions s
                    LEFT JOIN transcript_history h ON h.session_id = s.id
                    """
                )
            )
            days: set[str] = set()
            with self.conn:
                for row in rows:
                    transcription_price = settings.transcription_price_per_minute(
                        str(row["transcription_model"] or "")
                    )
                    cleanup_price = settings.cleanup_price(str(row["cleanup_model"] or ""))
                    cleaned_transcript = row["cleaned_transcript"]
                    cleanup_enabled = bool(row["cleanup_enabled"]) and cleaned_transcript is not None
                    estimate = estimate_session_cost(
                        duration_seconds=float(row["duration_seconds"] or 0.0),
                        raw_transcript=str(row["raw_transcript"] or ""),
                        cleaned_transcript=(
                            str(cleaned_transcript) if cleaned_transcript is not None else None
                        ),
                        cleanup_enabled=cleanup_enabled,
                        transcription_price_per_minute=transcription_price,
                        cleanup_input_price_per_1m_tokens=cleanup_price.input_price_per_1m_tokens,
                        cleanup_output_price_per_1m_tokens=cleanup_price.output_price_per_1m_tokens,
                    )
                    self.conn.execute(
                        """
                        UPDATE dictation_sessions
                        SET estimated_transcription_cost = ?,
                            estimated_cleanup_cost = ?,
                            estimated_total_cost = ?
                        WHERE id = ?
                        """,
                        (
                            estimate.transcription_cost,
                            estimate.cleanup_cost,
                            estimate.total_cost,
                            int(row["id"]),
                        ),
                    )
                    day = str(row["started_at"] or "")[:10]
                    if day:
                        days.add(day)
            for day in days:
                self.recompute_daily_stats(day)

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

    def list_mappings(self, search: str = "") -> list[ReplacementMapping]:
        with self._lock:
            if search:
                rows = self.conn.execute(
                    """
                    SELECT * FROM replacement_mappings
                    WHERE source_phrase LIKE ? OR replacement_phrase LIKE ?
                    ORDER BY source_phrase COLLATE NOCASE
                    """,
                    (f"%{search}%", f"%{search}%"),
                )
            else:
                rows = self.conn.execute(
                    "SELECT * FROM replacement_mappings ORDER BY source_phrase COLLATE NOCASE"
                )
            return [
                ReplacementMapping(
                    id=int(row["id"]),
                    source_phrase=str(row["source_phrase"]),
                    replacement_phrase=str(row["replacement_phrase"]),
                    enabled=bool(row["enabled"]),
                    case_sensitive=bool(row["case_sensitive"]),
                    whole_word_only=bool(row["whole_word_only"]),
                    created_at=str(row["created_at"]),
                    updated_at=str(row["updated_at"]),
                )
                for row in rows
            ]

    def add_mapping(self, mapping: ReplacementMapping) -> int:
        now = ReplacementMapping.now_iso()
        created_at = mapping.created_at or now
        updated_at = mapping.updated_at or now
        with self._lock:
            with self.conn:
                cursor = self.conn.execute(
                    """
                    INSERT INTO replacement_mappings (
                        source_phrase, replacement_phrase, enabled, case_sensitive,
                        whole_word_only, created_at, updated_at
                    )
                    VALUES (?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        mapping.source_phrase,
                        mapping.replacement_phrase,
                        int(mapping.enabled),
                        int(mapping.case_sensitive),
                        int(mapping.whole_word_only),
                        created_at,
                        updated_at,
                    ),
                )
            return int(cursor.lastrowid)

    def update_mapping(self, mapping: ReplacementMapping) -> None:
        if mapping.id is None:
            raise ValueError("mapping.id is required for update")
        with self._lock:
            with self.conn:
                self.conn.execute(
                    """
                    UPDATE replacement_mappings
                    SET source_phrase = ?, replacement_phrase = ?, enabled = ?,
                        case_sensitive = ?, whole_word_only = ?, updated_at = ?
                    WHERE id = ?
                    """,
                    (
                        mapping.source_phrase,
                        mapping.replacement_phrase,
                        int(mapping.enabled),
                        int(mapping.case_sensitive),
                        int(mapping.whole_word_only),
                        ReplacementMapping.now_iso(),
                        mapping.id,
                    ),
                )

    def delete_mapping(self, mapping_id: int) -> None:
        with self._lock:
            with self.conn:
                self.conn.execute("DELETE FROM replacement_mappings WHERE id = ?", (mapping_id,))

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
            week_start = today - timedelta(days=today.weekday())
            month_start = today.replace(day=1)
            periods = {
                "today": today.isoformat(),
                "week": week_start.isoformat(),
                "month": month_start.isoformat(),
            }
            period_rows = {
                key: self._aggregate_since(start)
                for key, start in periods.items()
            }
            model_rows = self.conn.execute(
                "SELECT transcription_model, cleanup_model, cleanup_enabled FROM dictation_sessions"
            )
            transcription_counter: Counter[str] = Counter()
            cleanup_counter: Counter[str] = Counter()
            cleanup_enabled_count = 0
            for row in model_rows:
                transcription_counter[str(row["transcription_model"])] += 1
                if row["cleanup_enabled"]:
                    cleanup_enabled_count += 1
                    if row["cleanup_model"]:
                        cleanup_counter[str(row["cleanup_model"])] += 1
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
                "cleanup_mode_usage_count": cleanup_enabled_count,
            }

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
