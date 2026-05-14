from __future__ import annotations

import os
import shlex
import shutil
import sys
from pathlib import Path

from .paths import APP_DESKTOP_ID, APP_ID, APP_NAME


def autostart_dir() -> Path:
    return Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")) / "autostart"


def desktop_entry_path() -> Path:
    return autostart_dir() / f"{APP_DESKTOP_ID}.desktop"


def legacy_desktop_entry_path() -> Path:
    return autostart_dir() / f"{APP_ID}.desktop"


def desktop_entry(exec_path: str | None = None, launch_hidden: bool = True) -> str:
    executable = (
        exec_path
        or os.environ.get("AGENTDICTATE_EXEC")
        or shutil.which("agentdictate")
        or sys.argv[0]
    )
    command = shlex.quote(executable)
    hidden_arg = " --background" if launch_hidden else ""
    return (
        "[Desktop Entry]\n"
        "Type=Application\n"
        f"Name={APP_NAME}\n"
        f"Comment=Personal Linux speech-to-text app for AI coding prompts\n"
        f"Exec={command}{hidden_arg}\n"
        "Icon=agentdictate\n"
        "Terminal=false\n"
        "Categories=Utility;\n"
        f"StartupWMClass={APP_DESKTOP_ID}\n"
        "X-GNOME-Autostart-enabled=true\n"
    )


def set_start_on_login(enabled: bool, exec_path: str | None = None) -> None:
    path = desktop_entry_path()
    if enabled:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(desktop_entry(exec_path=exec_path), encoding="utf-8")
        legacy_desktop_entry_path().unlink(missing_ok=True)
    else:
        path.unlink(missing_ok=True)
        legacy_desktop_entry_path().unlink(missing_ok=True)


def is_start_on_login_enabled() -> bool:
    return desktop_entry_path().exists()
