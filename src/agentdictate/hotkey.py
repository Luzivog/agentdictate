from __future__ import annotations

import os
import re
import selectors
import struct
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable


EV_KEY = 0x01
KEY_LEFTCTRL = 29
KEY_RIGHTCTRL = 97
KEY_ESC = 1
KEY_LEFTALT = 56
KEY_RIGHTALT = 100
KEY_LEFTMETA = 125
KEY_RIGHTMETA = 126
KEY_LEFTSHIFT = 42
KEY_RIGHTSHIFT = 54
KEY_SPACE = 57
KEY_F8 = 66
KEY_F9 = 67

KEY_NAME_TO_CODES = {
    "ctrl": {KEY_LEFTCTRL, KEY_RIGHTCTRL},
    "control": {KEY_LEFTCTRL, KEY_RIGHTCTRL},
    "alt": {KEY_LEFTALT, KEY_RIGHTALT},
    "super": {KEY_LEFTMETA, KEY_RIGHTMETA},
    "meta": {KEY_LEFTMETA, KEY_RIGHTMETA},
    "shift": {KEY_LEFTSHIFT, KEY_RIGHTSHIFT},
    "space": {KEY_SPACE},
    "f8": {KEY_F8},
    "f9": {KEY_F9},
}

EVENT_STRUCT = struct.Struct("llHHI")


class HotkeyError(RuntimeError):
    pass


@dataclass
class HotkeySpec:
    display: str
    groups: list[set[int]]

    @property
    def all_codes(self) -> set[int]:
        result: set[int] = set()
        for group in self.groups:
            result.update(group)
        return result

    def matches(self, pressed: set[int]) -> bool:
        return all(group & pressed for group in self.groups)

    def includes_code(self, code: int) -> bool:
        return code in self.all_codes


def parse_hotkey(value: str) -> HotkeySpec:
    parts = [part.strip().lower() for part in re.split(r"[+ ]+", value) if part.strip()]
    groups: list[set[int]] = []
    for part in parts:
        if part not in KEY_NAME_TO_CODES:
            raise HotkeyError(f"Unsupported hotkey part: {part}")
        groups.append(KEY_NAME_TO_CODES[part])
    if not groups:
        raise HotkeyError("Hotkey is empty.")
    return HotkeySpec(display=value, groups=groups)


def keyboard_event_paths(devices_file: Path = Path("/proc/bus/input/devices")) -> list[Path]:
    if not devices_file.exists():
        return []
    content = devices_file.read_text(encoding="utf-8", errors="ignore")
    paths: list[Path] = []
    for block in content.split("\n\n"):
        handlers_match = re.search(r"H: Handlers=(.*)", block)
        if not handlers_match:
            continue
        handlers = handlers_match.group(1)
        if "kbd" not in handlers:
            continue
        for event_name in re.findall(r"\bevent\d+\b", handlers):
            path = Path("/dev/input") / event_name
            if path.exists():
                paths.append(path)
    return sorted(set(paths))


class InputHotkeyListener:
    def __init__(
        self,
        hotkey: str,
        recording_mode: str,
        on_start: Callable[[], None],
        on_stop: Callable[[], None],
        on_cancel: Callable[[], None],
        on_error: Callable[[str], None],
    ) -> None:
        self.spec = parse_hotkey(hotkey)
        self.recording_mode = recording_mode
        self.on_start = on_start
        self.on_stop = on_stop
        self.on_cancel = on_cancel
        self.on_error = on_error
        self._thread: threading.Thread | None = None
        self._stop_event = threading.Event()
        self._active = False
        self._last_toggle_at = 0.0

    def start(self) -> None:
        if self._thread and self._thread.is_alive():
            return
        self._stop_event.clear()
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def stop(self) -> None:
        self._stop_event.set()
        if self._thread and self._thread.is_alive():
            self._thread.join(timeout=1)

    def _run(self) -> None:
        paths = keyboard_event_paths()
        if not paths:
            self.on_error(
                "Could not register Ctrl+Space. Choose another hotkey or grant access to /dev/input keyboard events."
            )
            return
        selector = selectors.DefaultSelector()
        files: list[int] = []
        try:
            for path in paths:
                try:
                    fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
                except OSError:
                    continue
                files.append(fd)
                selector.register(fd, selectors.EVENT_READ)
            if not files:
                self.on_error(
                    "Could not register Ctrl+Space. Another app or your desktop environment may already use it."
                )
                return
            pressed: set[int] = set()
            while not self._stop_event.is_set():
                for key, _mask in selector.select(timeout=0.25):
                    try:
                        chunk = os.read(key.fd, EVENT_STRUCT.size * 32)
                    except BlockingIOError:
                        continue
                    except OSError:
                        continue
                    for event in self._events_from_chunk(chunk):
                        _sec, _usec, event_type, code, value = event
                        if event_type != EV_KEY:
                            continue
                        if value in (1, 2):
                            pressed.add(code)
                        elif value == 0:
                            pressed.discard(code)
                        if code == KEY_ESC and value == 1:
                            self._active = False
                            self.on_cancel()
                            continue
                        if not self.spec.includes_code(code):
                            continue
                        self._handle_key_event(code, value, pressed)
        finally:
            for fd in files:
                try:
                    selector.unregister(fd)
                except Exception:
                    pass
                try:
                    os.close(fd)
                except OSError:
                    pass

    def _events_from_chunk(self, chunk: bytes) -> list[tuple[int, int, int, int, int]]:
        events = []
        for offset in range(0, len(chunk) - (len(chunk) % EVENT_STRUCT.size), EVENT_STRUCT.size):
            events.append(EVENT_STRUCT.unpack_from(chunk, offset))
        return events

    def _handle_key_event(self, code: int, value: int, pressed: set[int]) -> None:
        if code == KEY_ESC and value == 1:
            self._active = False
            self.on_cancel()
            return
        matches = self.spec.matches(pressed)
        if self.recording_mode == "toggle":
            if value != 1 or not matches:
                return
            now = time.monotonic()
            if now - self._last_toggle_at < 0.4:
                return
            self._last_toggle_at = now
            if self._active:
                self._active = False
                self.on_stop()
            else:
                self._active = True
                self.on_start()
            return
        if matches and not self._active and value == 1:
            self._active = True
            self.on_start()
        elif self._active and not matches and value == 0:
            self._active = False
            self.on_stop()
