from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path

from .constants import KEY_NAME_TO_CODES


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
