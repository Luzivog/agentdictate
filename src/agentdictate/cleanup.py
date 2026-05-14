from __future__ import annotations

from .config import DEFAULT_CLEANUP_PROMPT


def build_cleanup_instruction(style: str, custom_prompt: str | None = None) -> str:
    base = (custom_prompt or DEFAULT_CLEANUP_PROMPT).strip()
    if style == "Structured coding prompt":
        return (
            f"{base}\n\n"
            "Cleanup style: Structured coding prompt. Use short bullets and "
            "sections only when helpful. Possible sections: Goal, "
            "Requirements, Constraints, Testing, Notes."
        )
    return (
        f"{base}\n\n"
        "Cleanup style: Light cleanup. Keep wording and structure close to "
        "the transcript. Do not invent details."
    )
