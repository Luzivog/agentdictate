from __future__ import annotations

import os
from pathlib import Path


APP_ID = "agentdictate"
APP_DESKTOP_ID = "local.agentdictate.AgentDictate"
APP_NAME = "AgentDictate"


def _xdg_path(env_name: str, default: Path) -> Path:
    value = os.environ.get(env_name)
    return Path(value).expanduser() if value else default


def config_dir() -> Path:
    return _xdg_path("XDG_CONFIG_HOME", Path.home() / ".config") / APP_ID


def data_dir() -> Path:
    return _xdg_path("XDG_DATA_HOME", Path.home() / ".local" / "share") / APP_ID


def state_dir() -> Path:
    return _xdg_path("XDG_STATE_HOME", Path.home() / ".local" / "state") / APP_ID


def cache_dir() -> Path:
    return _xdg_path("XDG_CACHE_HOME", Path.home() / ".cache") / APP_ID


def logs_dir() -> Path:
    return state_dir() / "logs"


def config_path() -> Path:
    return config_dir() / "config.json"


def database_path() -> Path:
    return data_dir() / "agentdictate.sqlite"


def ensure_app_dirs() -> None:
    for path in (config_dir(), data_dir(), logs_dir(), cache_dir()):
        path.mkdir(parents=True, exist_ok=True)
