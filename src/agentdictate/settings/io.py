from __future__ import annotations

import json
import os
from dataclasses import asdict, fields
from pathlib import Path
from typing import Any

from agentdictate.paths import config_path, ensure_app_dirs

from .models import Settings, default_cleanup_prices, default_transcription_prices


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
