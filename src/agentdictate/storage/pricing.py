from __future__ import annotations

import sqlite3

from agentdictate.config import Settings
from agentdictate.costs import estimate_session_cost

from .schema import utc_now


class PricingStoreMixin:
    _lock: object
    conn: sqlite3.Connection

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
                        s.id, s.started_at, s.duration_seconds,
                        s.transcription_model, s.cleanup_enabled, s.cleanup_model,
                        h.raw_transcript, h.cleaned_transcript
                    FROM dictation_sessions s
                    LEFT JOIN transcript_history h ON h.session_id = s.id
                    """
                )
            )
            days: set[str] = set()
            with self.conn:
                for row in rows:
                    estimate = self._priced_row_estimate(settings, row)
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

    def _priced_row_estimate(self, settings: Settings, row: sqlite3.Row):
        transcription_price = settings.transcription_price_per_minute(
            str(row["transcription_model"] or "")
        )
        cleanup_price = settings.cleanup_price(str(row["cleanup_model"] or ""))
        cleaned_transcript = row["cleaned_transcript"]
        cleanup_enabled = bool(row["cleanup_enabled"]) and cleaned_transcript is not None
        return estimate_session_cost(
            duration_seconds=float(row["duration_seconds"] or 0.0),
            raw_transcript=str(row["raw_transcript"] or ""),
            cleaned_transcript=str(cleaned_transcript) if cleaned_transcript is not None else None,
            cleanup_enabled=cleanup_enabled,
            transcription_price_per_minute=transcription_price,
            cleanup_input_price_per_1m_tokens=cleanup_price.input_price_per_1m_tokens,
            cleanup_output_price_per_1m_tokens=cleanup_price.output_price_per_1m_tokens,
        )
