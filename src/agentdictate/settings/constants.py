from __future__ import annotations

TRANSCRIPTION_MODELS = [
    "gpt-4o-transcribe",
    "gpt-4o-mini-transcribe",
    "whisper-1",
    "Custom",
]

CLEANUP_MODELS = [
    "gpt-5.4-nano",
    "gpt-5.4-mini",
    "gpt-5.5",
    "Custom",
]

CLEANUP_REASONING_EFFORTS = [
    "default",
    "low",
    "medium",
    "high",
    "xhigh",
]

CLEANUP_STYLES = [
    "Light cleanup",
    "Structured coding prompt",
]

CUSTOM_LANGUAGE_VALUE = "custom"

TRANSCRIPTION_LANGUAGES = [
    ("Auto-detect", ""),
    ("English (en)", "en"),
    ("French (fr)", "fr"),
    ("Spanish (es)", "es"),
    ("German (de)", "de"),
    ("Portuguese (pt)", "pt"),
    ("Italian (it)", "it"),
    ("Dutch (nl)", "nl"),
    ("Polish (pl)", "pl"),
    ("Arabic (ar)", "ar"),
    ("Chinese (zh)", "zh"),
    ("Japanese (ja)", "ja"),
    ("Korean (ko)", "ko"),
    ("Hindi (hi)", "hi"),
    ("Custom", CUSTOM_LANGUAGE_VALUE),
]

RECORDING_MODES = [
    "toggle",
    "hold",
]

DEFAULT_CLEANUP_PROMPT = (
    "Clean up this dictation into a clear prompt. Preserve intent and "
    "technical terms. Fix punctuation and obvious filler. Do not add new "
    "requirements."
)

PLAIN_KEY_WARNING = (
    "Your API key is stored locally in the AgentDictate config file for this "
    "MVP. Do not use this on a shared or untrusted machine."
)

HISTORY_WARNING = (
    "Transcript history is stored locally on your machine. Do not dictate "
    "secrets or sensitive information unless you are comfortable storing them "
    "locally and sending audio/transcripts to OpenAI."
)

PRICING_DISCLAIMER = (
    "Cost estimates are approximate. Actual OpenAI billing may differ. Check "
    "your OpenAI dashboard for exact usage and pricing."
)
