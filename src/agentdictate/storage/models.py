from __future__ import annotations

from dataclasses import dataclass
from typing import Any


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
