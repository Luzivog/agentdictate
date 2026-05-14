from __future__ import annotations

import json
import os
from dataclasses import asdict, dataclass, field, fields
from pathlib import Path
from typing import Any

from .paths import config_path, ensure_app_dirs


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


@dataclass
class CleanupPrice:
    model_name: str
    input_price_per_1m_tokens: float
    output_price_per_1m_tokens: float
    currency: str = "USD"


@dataclass
class TranscriptionPrice:
    model_name: str
    price_per_audio_minute: float
    currency: str = "USD"


def default_transcription_prices() -> dict[str, dict[str, Any]]:
    return {
        "gpt-4o-transcribe": asdict(TranscriptionPrice("gpt-4o-transcribe", 0.006)),
        "gpt-4o-mini-transcribe": asdict(
            TranscriptionPrice("gpt-4o-mini-transcribe", 0.003)
        ),
        "whisper-1": asdict(TranscriptionPrice("whisper-1", 0.006)),
    }


def default_cleanup_prices() -> dict[str, dict[str, Any]]:
    return {
        "gpt-5.4-nano": asdict(CleanupPrice("gpt-5.4-nano", 0.05, 0.40)),
        "gpt-5.4-mini": asdict(CleanupPrice("gpt-5.4-mini", 0.25, 2.00)),
        "gpt-5.5": asdict(CleanupPrice("gpt-5.5", 1.25, 10.00)),
    }


@dataclass
class Settings:
    openai_api_key: str = ""
    transcription_model: str = "gpt-4o-transcribe"
    custom_transcription_model: str = ""
    language: str = ""
    transcription_prompt: str = ""
    cleanup_enabled: bool = True
    cleanup_model: str = "gpt-5.4-nano"
    custom_cleanup_model: str = ""
    cleanup_reasoning_effort: str = "default"
    cleanup_style: str = "Light cleanup"
    cleanup_prompt: str = DEFAULT_CLEANUP_PROMPT
    hotkey: str = "Ctrl+Space"
    recording_mode: str = "toggle"
    max_recording_seconds: int = 300
    sound_feedback: bool = False
    start_sound: bool = False
    stop_sound: bool = False
    start_on_login: bool = True
    show_tray_icon: bool = True
    minimize_to_tray_on_close: bool = True
    launch_window_on_startup: bool = False
    restore_clipboard_after_paste: bool = False
    debug_mode: bool = False
    preserve_temp_audio: bool = False
    save_history: bool = True
    paste_method: str = "clipboard"
    currency: str = "USD"
    transcription_prices: dict[str, dict[str, Any]] = field(
        default_factory=default_transcription_prices
    )
    cleanup_prices: dict[str, dict[str, Any]] = field(
        default_factory=default_cleanup_prices
    )

    def active_transcription_model(self) -> str:
        if self.transcription_model == "Custom":
            return self.custom_transcription_model.strip()
        return self.transcription_model

    def active_cleanup_model(self) -> str:
        if self.cleanup_model == "Custom":
            return self.custom_cleanup_model.strip()
        return self.cleanup_model

    def active_cleanup_reasoning_effort(self) -> str:
        effort = (self.cleanup_reasoning_effort or "default").strip().lower()
        if effort not in CLEANUP_REASONING_EFFORTS or effort == "default":
            return ""
        return effort

    def transcription_price_per_minute(self, model: str | None = None) -> float:
        model_name = model or self.active_transcription_model()
        price = self.transcription_prices.get(model_name, {})
        return float(price.get("price_per_audio_minute", 0.0) or 0.0)

    def cleanup_price(self, model: str | None = None) -> CleanupPrice:
        model_name = model or self.active_cleanup_model()
        price = self.cleanup_prices.get(model_name, {})
        return CleanupPrice(
            model_name=model_name,
            input_price_per_1m_tokens=float(
                price.get("input_price_per_1m_tokens", 0.0) or 0.0
            ),
            output_price_per_1m_tokens=float(
                price.get("output_price_per_1m_tokens", 0.0) or 0.0
            ),
            currency=str(price.get("currency", self.currency) or self.currency),
        )


def _coerce_settings(raw: dict[str, Any]) -> Settings:
    defaults = Settings()
    allowed = {field.name for field in fields(Settings)}
    merged = asdict(defaults)
    for key, value in raw.items():
        if key in allowed:
            merged[key] = value
    return Settings(**merged)


def _float_value(value: Any) -> float:
    try:
        return float(value or 0.0)
    except (TypeError, ValueError):
        return 0.0


def repair_zero_pricing_defaults(settings: Settings) -> bool:
    changed = False
    transcription_defaults = default_transcription_prices()
    transcription_values = [
        _float_value(settings.transcription_prices.get(model, {}).get("price_per_audio_minute"))
        for model in transcription_defaults
    ]
    if transcription_values and all(value <= 0 for value in transcription_values):
        settings.transcription_prices = transcription_defaults
        changed = True

    cleanup_defaults = default_cleanup_prices()
    cleanup_values: list[float] = []
    for model in cleanup_defaults:
        price = settings.cleanup_prices.get(model, {})
        cleanup_values.append(_float_value(price.get("input_price_per_1m_tokens")))
        cleanup_values.append(_float_value(price.get("output_price_per_1m_tokens")))
    if cleanup_values and all(value <= 0 for value in cleanup_values):
        settings.cleanup_prices = cleanup_defaults
        changed = True
    return changed


def load_settings(path: Path | None = None) -> Settings:
    ensure_app_dirs()
    target = path or config_path()
    if not target.exists():
        settings = Settings(openai_api_key=os.environ.get("OPENAI_API_KEY", ""))
        save_settings(settings, target)
        return settings
    with target.open("r", encoding="utf-8") as handle:
        raw = json.load(handle)
    settings = _coerce_settings(raw)
    if repair_zero_pricing_defaults(settings):
        save_settings(settings, target)
    return settings


def save_settings(settings: Settings, path: Path | None = None) -> None:
    ensure_app_dirs()
    target = path or config_path()
    target.parent.mkdir(parents=True, exist_ok=True)
    tmp = target.with_suffix(".json.tmp")
    with tmp.open("w", encoding="utf-8") as handle:
        json.dump(asdict(settings), handle, indent=2, sort_keys=True)
        handle.write("\n")
    os.replace(tmp, target)


def reset_pricing_defaults(settings: Settings) -> Settings:
    settings.transcription_prices = default_transcription_prices()
    settings.cleanup_prices = default_cleanup_prices()
    return settings
