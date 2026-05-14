from __future__ import annotations

import re
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Iterable


@dataclass
class ReplacementMapping:
    id: int | None
    source_phrase: str
    replacement_phrase: str
    enabled: bool = True
    case_sensitive: bool = False
    whole_word_only: bool = True
    created_at: str = ""
    updated_at: str = ""

    @staticmethod
    def now_iso() -> str:
        return datetime.now(timezone.utc).replace(microsecond=0).isoformat()

    @classmethod
    def new(
        cls,
        source_phrase: str,
        replacement_phrase: str,
        enabled: bool = True,
        case_sensitive: bool = False,
        whole_word_only: bool = True,
    ) -> "ReplacementMapping":
        now = cls.now_iso()
        return cls(
            id=None,
            source_phrase=source_phrase,
            replacement_phrase=replacement_phrase,
            enabled=enabled,
            case_sensitive=case_sensitive,
            whole_word_only=whole_word_only,
            created_at=now,
            updated_at=now,
        )


def _pattern_for(mapping: ReplacementMapping) -> re.Pattern[str]:
    source = re.escape(mapping.source_phrase)
    if mapping.whole_word_only:
        source = rf"(?<!\w){source}(?!\w)"
    flags = 0 if mapping.case_sensitive else re.IGNORECASE
    return re.compile(source, flags)


def apply_replacements(
    text: str, mappings: Iterable[ReplacementMapping]
) -> tuple[str, list[dict[str, str | int]]]:
    result = text
    applied: list[dict[str, str | int]] = []
    for mapping in mappings:
        if not mapping.enabled or not mapping.source_phrase:
            continue
        pattern = _pattern_for(mapping)
        result, count = pattern.subn(mapping.replacement_phrase, result)
        if count:
            applied.append(
                {
                    "id": mapping.id or 0,
                    "source_phrase": mapping.source_phrase,
                    "replacement_phrase": mapping.replacement_phrase,
                    "count": count,
                }
            )
    return result, applied
