from __future__ import annotations

import json
import logging
import re
from datetime import datetime, timezone
from logging.handlers import RotatingFileHandler
from pathlib import Path

from .paths import logs_dir


LOGGER_NAME = "agentdictate"
LOG_FILE_NAME = "agentdictate.log"
_PRIVATE_FIELD_PARTS = ("transcript", "text", "prompt", "api_key")
_SECRET_PATTERNS = (
    re.compile(r"\bsk-[A-Za-z0-9_-]{4,}"),
    re.compile(r"(?i)\bBearer\s+[A-Za-z0-9._-]+"),
)


def configure_logging(directory: Path | None = None) -> Path:
    target_dir = directory or logs_dir()
    target_dir.mkdir(parents=True, exist_ok=True)
    path = target_dir / LOG_FILE_NAME
    shutdown_logging()
    handler = RotatingFileHandler(
        path,
        maxBytes=1_000_000,
        backupCount=3,
        encoding="utf-8",
    )
    handler.setFormatter(logging.Formatter("%(message)s"))
    handler._agentdictate_diagnostics = True  # type: ignore[attr-defined]
    logger = logging.getLogger(LOGGER_NAME)
    logger.setLevel(logging.INFO)
    logger.addHandler(handler)
    logger.propagate = False
    return path


def shutdown_logging() -> None:
    logger = logging.getLogger(LOGGER_NAME)
    for handler in list(logger.handlers):
        if not getattr(handler, "_agentdictate_diagnostics", False):
            continue
        logger.removeHandler(handler)
        handler.close()


def log_event(event: str, **fields: object) -> None:
    safe_fields = {
        key: _sanitize(value)
        for key, value in fields.items()
        if not any(part in key.lower() for part in _PRIVATE_FIELD_PARTS)
    }
    payload = {
        "timestamp": datetime.now(timezone.utc).isoformat(timespec="milliseconds"),
        "event": event,
        **safe_fields,
    }
    logging.getLogger(LOGGER_NAME).info(
        json.dumps(payload, ensure_ascii=True, sort_keys=True, default=str)
    )


def _sanitize(value: object) -> object:
    if not isinstance(value, str):
        return value
    sanitized = value.replace("\r", " ").replace("\n", " ")
    for pattern in _SECRET_PATTERNS:
        sanitized = pattern.sub("[redacted]", sanitized)
    return sanitized
