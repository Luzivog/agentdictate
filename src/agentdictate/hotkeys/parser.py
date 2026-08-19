from __future__ import annotations

import re
import struct
from dataclasses import dataclass
from pathlib import Path

from .constants import KEY_NAME_TO_CODES


class HotkeyError(RuntimeError):
    pass


@dataclass
class HotkeySpec:
    display: str
    groups: list[set[int]]

    def matches(self, pressed: set[int]) -> bool:
        return all(group & pressed for group in self.groups)


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


def keyboard_event_paths(
    devices_file: Path = Path("/proc/bus/input/devices"),
    *,
    hotkey: HotkeySpec | None = None,
    device_dir: Path = Path("/dev/input"),
    sysfs_input_dir: Path = Path("/sys/class/input"),
) -> list[Path]:
    if not devices_file.exists():
        return []
    content = devices_file.read_text(encoding="utf-8", errors="ignore")
    paths: list[Path] = []
    for block in content.split("\n\n"):
        name_match = re.search(r'N: Name="(.*)"', block)
        device_name = name_match.group(1).lower() if name_match else ""
        if "ydotoold virtual device" in device_name:
            continue
        handlers_match = re.search(r"H: Handlers=(.*)", block)
        if not handlers_match:
            continue
        handlers = handlers_match.group(1)
        if "kbd" not in handlers:
            continue
        for event_name in re.findall(r"\bevent\d+\b", handlers):
            path = device_dir / event_name
            if path.exists() and (
                hotkey is None
                or _device_supports_hotkey(sysfs_input_dir / event_name, hotkey)
            ):
                paths.append(path)
    return sorted(set(paths))


def _device_supports_hotkey(device_path: Path, hotkey: HotkeySpec) -> bool:
    capabilities_path = device_path / "device" / "capabilities" / "key"
    try:
        raw_capabilities = capabilities_path.read_text(encoding="ascii").strip()
    except OSError:
        return False
    try:
        mask = _parse_key_capabilities(raw_capabilities)
    except ValueError:
        return False
    return all(any(mask & (1 << code) for code in group) for group in hotkey.groups)


def _parse_key_capabilities(value: str) -> int:
    word_bits = struct.calcsize("L") * 8
    mask = 0
    for word in value.split():
        mask = (mask << word_bits) | int(word, 16)
    return mask
