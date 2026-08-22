from __future__ import annotations

from dataclasses import asdict, dataclass, field
from typing import Any

from .constants import (
    CLEANUP_REASONING_EFFORTS,
    DEFAULT_CLEANUP_PROMPT,
    PASTE_SHORTCUT_AUTO,
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
        "gpt-transcribe": asdict(TranscriptionPrice("gpt-transcribe", 0.0045)),
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
    transcription_provider: str = "openai_api"
    transcription_model: str = "gpt-transcribe"
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
    audio_ducking_enabled: bool = True
    audio_ducking_volume_percent: int = 15
    audio_ducking_fade_ms: int = 1000
    start_on_login: bool = True
    show_tray_icon: bool = True
    minimize_to_tray_on_close: bool = True
    launch_window_on_startup: bool = False
    restore_clipboard_after_paste: bool = False
    debug_mode: bool = False
    preserve_temp_audio: bool = False
    save_history: bool = True
    paste_shortcut: str = PASTE_SHORTCUT_AUTO
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
