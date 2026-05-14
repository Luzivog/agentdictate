from __future__ import annotations

import os
import re
import shutil
import subprocess
import time
from dataclasses import dataclass


class ClipboardError(RuntimeError):
    pass


@dataclass
class PasteResult:
    copied: bool
    paste_triggered: bool
    error: str = ""


DEFAULT_CTRL_KEY = 29
DEFAULT_SHIFT_KEY = 42
DEFAULT_V_KEY = 47
X_KEYCODE_OFFSET = 8
PASTE_KEY_DELAY_MS = "25"
PASTE_SHORTCUT = "ctrl+shift+v"
PASTE_SHORTCUT_LABEL = "Ctrl+Shift+V"


def detect_paste_keycode() -> int:
    override = os.environ.get("AGENTDICTATE_PASTE_KEYCODE", "").strip()
    if override:
        try:
            keycode = int(override)
        except ValueError:
            keycode = DEFAULT_V_KEY
        if keycode > 0:
            return keycode
    return _detect_v_keycode_from_xmodmap() or DEFAULT_V_KEY


def _detect_v_keycode_from_xmodmap() -> int | None:
    if not shutil.which("xmodmap"):
        return None
    result = subprocess.run(
        ["xmodmap", "-pke"],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        return None
    return _parse_xmodmap_v_keycode(result.stdout)


def _parse_xmodmap_v_keycode(output: str) -> int | None:
    fallback: int | None = None
    for line in output.splitlines():
        match = re.match(r"\s*keycode\s+(\d+)\s+=\s+(.*)", line)
        if not match:
            continue
        x_keycode = int(match.group(1))
        keysyms = match.group(2).split()
        if not keysyms:
            continue
        evdev_keycode = x_keycode - X_KEYCODE_OFFSET
        if evdev_keycode <= 0:
            continue
        primary = keysyms[:2]
        if "v" in primary or "V" in primary:
            return evdev_keycode
        if fallback is None and ("v" in keysyms or "V" in keysyms):
            fallback = evdev_keycode
    return fallback


class ClipboardPaste:
    def __init__(self, restore_previous: bool = False) -> None:
        self.restore_previous = restore_previous

    def copy_and_paste(self, text: str) -> PasteResult:
        previous = self.read_clipboard() if self.restore_previous else None
        copied = self.copy(text)
        if not copied:
            return PasteResult(False, False, "Could not copy transcript to clipboard.")
        time.sleep(0.05)
        paste_triggered = self.trigger_paste()
        if self.restore_previous and previous is not None:
            time.sleep(0.25)
            self.copy(previous)
        if not paste_triggered:
            return PasteResult(
                True,
                False,
                f"Transcript copied to clipboard. Press {PASTE_SHORTCUT_LABEL} to paste.",
            )
        return PasteResult(True, True)

    def copy(self, text: str) -> bool:
        commands = []
        if os.environ.get("WAYLAND_DISPLAY") and shutil.which("wl-copy"):
            commands.append(["wl-copy"])
        if shutil.which("xclip"):
            commands.append(["xclip", "-selection", "clipboard"])
        if shutil.which("xsel"):
            commands.append(["xsel", "--clipboard", "--input"])
        for command in commands:
            result = subprocess.run(
                command,
                input=text.encode("utf-8"),
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
            )
            if result.returncode == 0:
                return True
        return False

    def read_clipboard(self) -> str | None:
        commands = []
        if os.environ.get("WAYLAND_DISPLAY") and shutil.which("wl-paste"):
            commands.append(["wl-paste", "--no-newline"])
        if shutil.which("xclip"):
            commands.append(["xclip", "-selection", "clipboard", "-out"])
        if shutil.which("xsel"):
            commands.append(["xsel", "--clipboard", "--output"])
        for command in commands:
            result = subprocess.run(
                command,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                check=False,
            )
            if result.returncode == 0:
                return result.stdout.decode("utf-8", errors="replace")
        return None

    def trigger_paste(self) -> bool:
        if os.environ.get("WAYLAND_DISPLAY") and shutil.which("ydotool"):
            for command in self._ydotool_paste_commands():
                result = subprocess.run(
                    command,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    check=False,
                )
                if result.returncode == 0:
                    return True
        if shutil.which("xdotool"):
            result = subprocess.run(
                ["xdotool", "key", "--clearmodifiers", PASTE_SHORTCUT],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
            )
            return result.returncode == 0
        return False

    def _ydotool_paste_commands(self) -> list[list[str]]:
        paste_key = detect_paste_keycode()
        return [
            ["ydotool", "key", "--key-delay", PASTE_KEY_DELAY_MS, PASTE_SHORTCUT],
            [
                "ydotool",
                "key",
                "--key-delay",
                PASTE_KEY_DELAY_MS,
                f"{DEFAULT_CTRL_KEY}:1",
                f"{DEFAULT_SHIFT_KEY}:1",
                f"{paste_key}:1",
                f"{paste_key}:0",
                f"{DEFAULT_SHIFT_KEY}:0",
                f"{DEFAULT_CTRL_KEY}:0",
            ],
        ]
