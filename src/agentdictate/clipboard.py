from __future__ import annotations

import os
import re
import shutil
import subprocess
import threading
import time
from dataclasses import dataclass
from enum import Enum

from agentdictate.settings.constants import (
    PASTE_SHORTCUT_AUTO,
    PASTE_SHORTCUT_STANDARD,
    PASTE_SHORTCUT_TERMINAL,
)


class ClipboardProtocol(str, Enum):
    WAYLAND = "wayland"
    X11 = "x11"


class ClipboardSelection(str, Enum):
    CLIPBOARD = "clipboard"
    PRIMARY = "primary"


@dataclass(frozen=True)
class PasteTarget:
    protocol: ClipboardProtocol
    window_id: str = ""
    window_class: str = ""


@dataclass(frozen=True)
class PasteResult:
    copied: bool
    paste_triggered: bool
    error: str = ""
    shortcut: str = ""
    target_class: str = ""


@dataclass
class _ClipboardSource:
    protocol: ClipboardProtocol
    process: subprocess.Popen[bytes]
    selection: ClipboardSelection = ClipboardSelection.CLIPBOARD


DELIVERY_DEADLINE_SECONDS = 2.0
PASTE_DELAY_MS = "0"
PASTE_KEY_DELAY_MS = "0"
UNIVERSAL_PASTE_DELAY_MS = "50"
UNIVERSAL_PASTE_KEY_DELAY_MS = "25"
UNIVERSAL_PASTE_SHORTCUT = "shift+insert"
STANDARD_PASTE_SHORTCUT = "ctrl+v"
TERMINAL_PASTE_SHORTCUT = "ctrl+shift+v"
TERMINAL_WINDOW_CLASS = re.compile(
    r"(?:^|[.\s_-])(?:kitty|terminal|alacritty|wezterm|konsole|xterm|tilix|"
    r"terminator|foot|ghostty|rio|st)(?:$|[.\s_-])",
    re.IGNORECASE,
)
WM_CLASS_VALUE = re.compile(r'"([^"]*)"')


class ClipboardPaste:
    """Copy text and send one paste chord to the focus current at delivery time."""

    _source_lock = threading.RLock()
    _active_sources: dict[
        tuple[ClipboardProtocol, ClipboardSelection], _ClipboardSource
    ] = {}

    def __init__(
        self,
        restore_previous: bool = False,
        shortcut_mode: str = PASTE_SHORTCUT_AUTO,
    ) -> None:
        self.restore_previous = restore_previous
        self.shortcut_mode = shortcut_mode

    def deliver(self, text: str) -> PasteResult:
        deadline = time.monotonic() + DELIVERY_DEADLINE_SECONDS
        previous: bytes | None = None
        previous_protocol: ClipboardProtocol | None = None
        source: _ClipboardSource | None = None
        primary_source: _ClipboardSource | None = None
        target = self._active_target(deadline)

        while True:
            if (
                self.restore_previous
                and target.protocol == ClipboardProtocol.WAYLAND
                and previous_protocol != target.protocol
            ):
                previous = self._read_clipboard(target.protocol, deadline)
                previous_protocol = target.protocol

            if source is None or source.protocol != target.protocol:
                source = self._publish_regular(text, target.protocol, deadline)
                if source is None:
                    copied = self._copy_with_any_protocol(text, deadline)
                    return PasteResult(
                        copied=copied,
                        paste_triggered=False,
                        error=self._clipboard_unavailable_message(target.protocol),
                        target_class=target.window_class,
                    )
                primary_source = None
                if self._uses_universal_wayland_paste(target):
                    primary_source = self._publish_primary(text, deadline)
                    if primary_source is None:
                        return PasteResult(
                            copied=True,
                            paste_triggered=False,
                            error=(
                                "Could not prepare the Wayland primary selection. "
                                "Transcript remains copied; paste was not sent."
                            ),
                            target_class=target.window_class,
                        )

            current = self._active_target(deadline)
            if self._same_target(target, current):
                target = current
                break
            if time.monotonic() >= deadline:
                return PasteResult(
                    copied=True,
                    paste_triggered=False,
                    error="Focus kept changing. Transcript copied; paste was not sent.",
                    target_class=current.window_class,
                )
            target = current

        shortcut = self._shortcut_for(target)
        restore_after_paste = (
            self.restore_previous and target.protocol == ClipboardProtocol.WAYLAND
        )
        if restore_after_paste:
            one_paste_source = self._replace_with_one_paste_source(
                text, source, deadline
            )
            if one_paste_source is None:
                return PasteResult(
                    copied=True,
                    paste_triggered=False,
                    error=(
                        "Could not observe clipboard ownership. Transcript remains copied; "
                        "paste was not sent."
                    ),
                    shortcut=shortcut,
                    target_class=target.window_class,
                )
            source = one_paste_source

        if restore_after_paste:
            current = self._active_target(deadline)
            if not self._same_target(target, current):
                self._remember_source(source)
                return PasteResult(
                    copied=True,
                    paste_triggered=False,
                    error=(
                        "Focus changed before paste. Transcript copied; paste was not sent."
                    ),
                    shortcut=shortcut,
                    target_class=current.window_class,
                )

        sent = self._send_shortcut(shortcut, target, deadline)
        if not sent:
            self._remember_source(source)
            return PasteResult(
                copied=True,
                paste_triggered=False,
                error=(
                    "Transcript copied, but the paste shortcut could not be sent. "
                    f"Press {self._shortcut_label(shortcut)} to paste."
                ),
                shortcut=shortcut,
                target_class=target.window_class,
            )

        if restore_after_paste:
            if not self._wait_for_exit(source.process, deadline):
                self._remember_source(source)
                return PasteResult(
                    copied=True,
                    paste_triggered=True,
                    error=(
                        "Paste shortcut sent, but clipboard consumption was not observed. "
                        "The transcript remains in the clipboard."
                    ),
                    shortcut=shortcut,
                    target_class=target.window_class,
                )
            if previous is not None and previous_protocol is not None:
                restored = self._publish_regular_bytes(
                    previous, previous_protocol, deadline
                )
                if restored is None:
                    return PasteResult(
                        copied=True,
                        paste_triggered=True,
                        error="Paste shortcut sent, but the previous clipboard was not restored.",
                        shortcut=shortcut,
                        target_class=target.window_class,
                    )
        else:
            self._remember_source(source)
            if primary_source is not None:
                self._remember_source(primary_source)

        if self.restore_previous and not restore_after_paste:
            return PasteResult(
                copied=True,
                paste_triggered=True,
                error=(
                    "Paste shortcut sent. The previous clipboard was not restored because "
                    "X11 does not provide a reliable paste-consumption acknowledgement."
                ),
                shortcut=shortcut,
                target_class=target.window_class,
            )

        return PasteResult(
            copied=True,
            paste_triggered=True,
            shortcut=shortcut,
            target_class=target.window_class,
        )

    def copy(self, text: str) -> bool:
        deadline = time.monotonic() + DELIVERY_DEADLINE_SECONDS
        target = self._active_target(deadline)
        return self._publish_regular(text, target.protocol, deadline) is not None

    def read_clipboard(self) -> str | None:
        deadline = time.monotonic() + DELIVERY_DEADLINE_SECONDS
        data = self._read_clipboard(self._active_target(deadline).protocol, deadline)
        if data is None:
            return None
        return data.decode("utf-8", errors="replace")

    @classmethod
    def close_sources(cls) -> None:
        with cls._source_lock:
            sources = list(cls._active_sources.values())
            cls._active_sources.clear()
        for source in sources:
            cls._terminate_source(source)

    def _active_target(self, deadline: float) -> PasteTarget:
        if not shutil.which("xdotool") or not os.environ.get("DISPLAY"):
            return PasteTarget(self._default_protocol())
        result = self._run(
            ["xdotool", "getactivewindow"],
            deadline,
            text=True,
        )
        if result is None or result.returncode != 0:
            return PasteTarget(self._default_protocol())
        window_id = result.stdout.strip()
        if not window_id or not shutil.which("xprop"):
            return PasteTarget(self._default_protocol())
        properties = self._run(
            ["xprop", "-id", window_id, "WM_CLASS", "_NET_WM_STATE"],
            deadline,
            text=True,
        )
        if properties is None or properties.returncode != 0:
            return PasteTarget(self._default_protocol())
        window_class = self._parse_window_class(properties.stdout)
        focused = "_NET_WM_STATE_FOCUSED" in properties.stdout
        if not os.environ.get("WAYLAND_DISPLAY") or focused:
            return PasteTarget(ClipboardProtocol.X11, window_id, window_class)
        return PasteTarget(ClipboardProtocol.WAYLAND)

    def _publish_regular(
        self,
        text: str,
        protocol: ClipboardProtocol,
        deadline: float,
    ) -> _ClipboardSource | None:
        return self._publish_regular_bytes(text.encode("utf-8"), protocol, deadline)

    def _publish_primary(
        self,
        text: str,
        deadline: float,
    ) -> _ClipboardSource | None:
        return self._publish_bytes(
            text.encode("utf-8"),
            ClipboardProtocol.WAYLAND,
            ClipboardSelection.PRIMARY,
            deadline,
        )

    def _publish_regular_bytes(
        self,
        data: bytes,
        protocol: ClipboardProtocol,
        deadline: float,
    ) -> _ClipboardSource | None:
        return self._publish_bytes(
            data,
            protocol,
            ClipboardSelection.CLIPBOARD,
            deadline,
        )

    def _publish_bytes(
        self,
        data: bytes,
        protocol: ClipboardProtocol,
        selection: ClipboardSelection,
        deadline: float,
    ) -> _ClipboardSource | None:
        previous = self._known_source(protocol, selection)
        source = self._start_source(
            data,
            protocol,
            paste_once=False,
            selection=selection,
        )
        if source is None:
            return None
        ownership_observed = False
        if previous is not None and previous.process.poll() is None:
            ownership_observed = self._wait_for_exit(previous.process, deadline)
            ownership_observed = (
                ownership_observed and source.process.poll() is None
            )
        if not ownership_observed and not self._wait_for_selection(
            data, protocol, selection, deadline
        ):
            self._terminate_source(source)
            return None
        self._remember_source(source)
        return source

    def _replace_with_one_paste_source(
        self,
        text: str,
        current: _ClipboardSource,
        deadline: float,
    ) -> _ClipboardSource | None:
        replacement = self._start_source(
            text.encode("utf-8"),
            current.protocol,
            paste_once=True,
            selection=current.selection,
        )
        if replacement is None:
            return None
        if not self._wait_for_exit(current.process, deadline):
            self._terminate_source(replacement)
            return None
        return replacement

    def _start_source(
        self,
        data: bytes,
        protocol: ClipboardProtocol,
        paste_once: bool,
        selection: ClipboardSelection = ClipboardSelection.CLIPBOARD,
    ) -> _ClipboardSource | None:
        command = self._source_command(protocol, paste_once, selection)
        if command is None:
            return None
        try:
            process = subprocess.Popen(
                command,
                stdin=subprocess.PIPE,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            if process.stdin is None:
                process.terminate()
                return None
            process.stdin.write(data)
            process.stdin.close()
        except (BrokenPipeError, OSError):
            if "process" in locals() and process.poll() is None:
                process.terminate()
            return None
        return _ClipboardSource(protocol, process, selection)

    def _source_command(
        self,
        protocol: ClipboardProtocol,
        paste_once: bool,
        selection: ClipboardSelection = ClipboardSelection.CLIPBOARD,
    ) -> list[str] | None:
        if protocol == ClipboardProtocol.X11:
            if paste_once or selection != ClipboardSelection.CLIPBOARD:
                return None
            xsel = shutil.which("xsel")
            if not xsel:
                return None
            return [
                xsel,
                "--clipboard",
                "--input",
                "--nodetach",
            ]
        wl_copy = shutil.which("wl-copy")
        if not wl_copy:
            return None
        command = [wl_copy, "--foreground"]
        if selection == ClipboardSelection.PRIMARY:
            command.append("--primary")
        if paste_once:
            command.append("--paste-once")
        return command

    def _wait_for_clipboard(
        self,
        expected: bytes,
        protocol: ClipboardProtocol,
        deadline: float,
    ) -> bool:
        return self._wait_for_selection(
            expected,
            protocol,
            ClipboardSelection.CLIPBOARD,
            deadline,
        )

    def _wait_for_selection(
        self,
        expected: bytes,
        protocol: ClipboardProtocol,
        selection: ClipboardSelection,
        deadline: float,
    ) -> bool:
        while time.monotonic() < deadline:
            actual = self._read_selection(protocol, selection, deadline)
            if actual == expected:
                return True
        return False

    def _read_clipboard(
        self, protocol: ClipboardProtocol, deadline: float
    ) -> bytes | None:
        return self._read_selection(
            protocol,
            ClipboardSelection.CLIPBOARD,
            deadline,
        )

    def _read_selection(
        self,
        protocol: ClipboardProtocol,
        selection: ClipboardSelection,
        deadline: float,
    ) -> bytes | None:
        command = self._read_command(protocol, selection)
        if command is None:
            return None
        result = self._run(command, deadline)
        if result is None or result.returncode != 0:
            return None
        return result.stdout

    def _read_command(
        self,
        protocol: ClipboardProtocol,
        selection: ClipboardSelection = ClipboardSelection.CLIPBOARD,
    ) -> list[str] | None:
        if protocol == ClipboardProtocol.X11:
            if selection != ClipboardSelection.CLIPBOARD:
                return None
            xsel = shutil.which("xsel")
            if not xsel:
                return None
            return [xsel, "--clipboard", "--output"]
        wl_paste = shutil.which("wl-paste")
        if not wl_paste:
            return None
        command = [wl_paste, "--no-newline"]
        if selection == ClipboardSelection.PRIMARY:
            command.append("--primary")
        return command

    def _send_shortcut(
        self,
        shortcut: str,
        target: PasteTarget,
        deadline: float,
    ) -> bool:
        if os.environ.get("WAYLAND_DISPLAY") and shutil.which("ydotool"):
            paste_delay = (
                UNIVERSAL_PASTE_DELAY_MS
                if shortcut == UNIVERSAL_PASTE_SHORTCUT
                else PASTE_DELAY_MS
            )
            key_delay = (
                UNIVERSAL_PASTE_KEY_DELAY_MS
                if shortcut == UNIVERSAL_PASTE_SHORTCUT
                else PASTE_KEY_DELAY_MS
            )
            command = [
                "ydotool",
                "key",
                "--delay",
                paste_delay,
                "--key-delay",
                key_delay,
                shortcut,
            ]
        elif target.protocol == ClipboardProtocol.X11 and shutil.which("xdotool"):
            command = [
                "xdotool",
                "key",
                "--clearmodifiers",
                "--delay",
                "0",
                shortcut,
            ]
        else:
            return False
        result = self._run(command, deadline)
        return result is not None and result.returncode == 0

    def _copy_with_any_protocol(self, text: str, deadline: float) -> bool:
        for protocol in (ClipboardProtocol.WAYLAND, ClipboardProtocol.X11):
            if self._publish_regular(text, protocol, deadline) is not None:
                return True
        return False

    def _remember_source(self, source: _ClipboardSource) -> None:
        key = (source.protocol, source.selection)
        with self._source_lock:
            previous = self._active_sources.get(key)
            self._active_sources[key] = source
        if previous is not None and previous.process.poll() is not None:
            previous.process.wait()

    def _known_source(
        self,
        protocol: ClipboardProtocol,
        selection: ClipboardSelection,
    ) -> _ClipboardSource | None:
        with self._source_lock:
            return self._active_sources.get((protocol, selection))

    @staticmethod
    def _terminate_source(source: _ClipboardSource) -> None:
        if source.process.poll() is None:
            source.process.terminate()
        try:
            source.process.wait(timeout=0.5)
        except subprocess.TimeoutExpired:
            source.process.kill()
            source.process.wait()

    @staticmethod
    def _wait_for_exit(process: subprocess.Popen[bytes], deadline: float) -> bool:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return process.poll() is not None
        try:
            process.wait(timeout=remaining)
        except subprocess.TimeoutExpired:
            return False
        return True

    @staticmethod
    def _run(
        command: list[str],
        deadline: float,
        *,
        text: bool = False,
    ) -> subprocess.CompletedProcess[bytes] | subprocess.CompletedProcess[str] | None:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return None
        try:
            return subprocess.run(
                command,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                check=False,
                timeout=remaining,
                text=text,
            )
        except (OSError, subprocess.TimeoutExpired):
            return None

    def _shortcut_for(self, target: PasteTarget) -> str:
        if self.shortcut_mode == PASTE_SHORTCUT_STANDARD:
            return STANDARD_PASTE_SHORTCUT
        if self.shortcut_mode == PASTE_SHORTCUT_TERMINAL:
            return TERMINAL_PASTE_SHORTCUT
        if self._uses_universal_wayland_paste(target):
            return UNIVERSAL_PASTE_SHORTCUT
        if TERMINAL_WINDOW_CLASS.search(target.window_class):
            return TERMINAL_PASTE_SHORTCUT
        return STANDARD_PASTE_SHORTCUT

    def _uses_universal_wayland_paste(self, target: PasteTarget) -> bool:
        return (
            self.shortcut_mode == PASTE_SHORTCUT_AUTO
            and target.protocol == ClipboardProtocol.WAYLAND
        )

    @staticmethod
    def _same_target(first: PasteTarget, second: PasteTarget) -> bool:
        if first.protocol != second.protocol:
            return False
        if first.protocol == ClipboardProtocol.X11:
            return first.window_id == second.window_id
        return True

    @staticmethod
    def _parse_window_class(output: str) -> str:
        line = next(
            (line for line in output.splitlines() if line.startswith("WM_CLASS")),
            "",
        )
        return " ".join(WM_CLASS_VALUE.findall(line))

    @staticmethod
    def _default_protocol() -> ClipboardProtocol:
        if os.environ.get("WAYLAND_DISPLAY"):
            return ClipboardProtocol.WAYLAND
        return ClipboardProtocol.X11

    @staticmethod
    def _shortcut_label(shortcut: str) -> str:
        if shortcut == UNIVERSAL_PASTE_SHORTCUT:
            return "Shift+Insert"
        return "Ctrl+Shift+V" if shortcut == TERMINAL_PASTE_SHORTCUT else "Ctrl+V"

    @staticmethod
    def _clipboard_unavailable_message(protocol: ClipboardProtocol) -> str:
        tool = "xsel" if protocol == ClipboardProtocol.X11 else "wl-copy"
        return (
            f"Could not prepare the {protocol.value} clipboard with {tool}. "
            "Paste was not sent."
        )
