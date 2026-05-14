from __future__ import annotations

import re
from typing import Any

TRANSCRIPTION_COMPLETENESS_PROMPT = (
    "Transcribe the entire recording from beginning to end. Include every "
    "spoken sentence and phrase. Do not summarize, omit later sentences, or "
    "stop after the first sentence."
)
FALLBACK_TRANSCRIPTION_MODEL = "whisper-1"


def extract_response_text(payload: dict[str, Any]) -> str:
    parts: list[str] = []
    for item in payload.get("output", []) or []:
        if item.get("type") != "message":
            continue
        for content in item.get("content", []) or []:
            if content.get("type") == "output_text":
                parts.append(str(content.get("text", "")))
    return "\n".join(part for part in parts if part)


def sentence_count(text: str) -> int:
    pieces = [piece.strip() for piece in re.split(r"[.!?]+(?:\s+|$)", text) if piece.strip()]
    return len(pieces)


def word_count(text: str) -> int:
    return len(re.findall(r"\b[\w'-]+\b", text))
