from __future__ import annotations

import re
from dataclasses import dataclass


WORD_RE = re.compile(r"[A-Za-z0-9_]+(?:[-'][A-Za-z0-9_]+)?")


@dataclass(frozen=True)
class CostEstimate:
    transcription_cost: float
    cleanup_cost: float
    total_cost: float
    cleanup_input_tokens: int
    cleanup_output_tokens: int


def word_count(text: str) -> int:
    return len(WORD_RE.findall(text or ""))


def estimate_tokens(text: str) -> int:
    if not text:
        return 0
    return max(1, round(len(text) / 4))


def estimate_transcription_cost(
    duration_seconds: float, price_per_audio_minute: float
) -> float:
    audio_minutes = max(duration_seconds, 0.0) / 60.0
    return audio_minutes * max(price_per_audio_minute, 0.0)


def estimate_cleanup_cost(
    raw_transcript: str,
    cleaned_transcript: str,
    input_price_per_1m_tokens: float,
    output_price_per_1m_tokens: float,
) -> tuple[float, int, int]:
    input_tokens = estimate_tokens(raw_transcript)
    output_tokens = estimate_tokens(cleaned_transcript)
    cost = (
        (input_tokens / 1_000_000.0) * max(input_price_per_1m_tokens, 0.0)
        + (output_tokens / 1_000_000.0) * max(output_price_per_1m_tokens, 0.0)
    )
    return cost, input_tokens, output_tokens


def estimate_session_cost(
    duration_seconds: float,
    raw_transcript: str,
    cleaned_transcript: str | None,
    cleanup_enabled: bool,
    transcription_price_per_minute: float,
    cleanup_input_price_per_1m_tokens: float,
    cleanup_output_price_per_1m_tokens: float,
) -> CostEstimate:
    transcription_cost = estimate_transcription_cost(
        duration_seconds, transcription_price_per_minute
    )
    if cleanup_enabled and cleaned_transcript is not None:
        cleanup_cost, input_tokens, output_tokens = estimate_cleanup_cost(
            raw_transcript,
            cleaned_transcript,
            cleanup_input_price_per_1m_tokens,
            cleanup_output_price_per_1m_tokens,
        )
    else:
        cleanup_cost = 0.0
        input_tokens = 0
        output_tokens = 0
    return CostEstimate(
        transcription_cost=transcription_cost,
        cleanup_cost=cleanup_cost,
        total_cost=transcription_cost + cleanup_cost,
        cleanup_input_tokens=input_tokens,
        cleanup_output_tokens=output_tokens,
    )


def average_wpm(total_words: int, total_audio_seconds: float) -> float:
    if total_audio_seconds <= 0:
        return 0.0
    return total_words / (total_audio_seconds / 60.0)


def format_cost(value: float, currency: str = "USD") -> str:
    symbol = "$" if currency.upper() == "USD" else f"{currency.upper()} "
    return f"{symbol}{value:.4f}"


def format_duration(seconds: float) -> str:
    seconds = max(0, int(round(seconds)))
    if seconds < 60:
        return f"{seconds}s"
    minutes = seconds // 60
    remainder = seconds % 60
    if minutes < 60:
        return f"{minutes}m {remainder}s" if remainder else f"{minutes}m"
    hours = seconds / 3600.0
    return f"{hours:.1f}h"
