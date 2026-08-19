from __future__ import annotations

import logging
import os
import selectors
import threading
import time
from dataclasses import dataclass
from enum import Enum
from pathlib import Path
from typing import Callable

from agentdictate.diagnostics import log_event

from .constants import EVENT_STRUCT, EV_KEY, KEY_ESC
from .parser import HotkeyError, keyboard_event_paths, parse_hotkey

DEVICE_POLL_SECONDS = 1.0
DEVICE_ERROR_DELAY_SECONDS = 5.0
LOGGER = logging.getLogger(__name__)


class HotkeyEventKind(str, Enum):
    PRESSED = "pressed"
    RELEASED = "released"
    CANCELLED = "cancelled"
    AVAILABLE = "available"
    UNAVAILABLE = "unavailable"


@dataclass(frozen=True)
class HotkeyEvent:
    kind: HotkeyEventKind
    message: str = ""


class InputHotkeyListener:
    def __init__(
        self,
        hotkey: str,
        on_event: Callable[[HotkeyEvent], None],
    ) -> None:
        self.spec = parse_hotkey(hotkey)
        self.on_event = on_event
        self._thread: threading.Thread | None = None
        self._stop_event = threading.Event()
        self._matched = False
        self._cancelled_until_release = False

    def start(self) -> None:
        if self._thread and self._thread.is_alive():
            return
        self._stop_event.clear()
        self._thread = threading.Thread(
            target=self._run,
            name="agentdictate-hotkey",
            daemon=True,
        )
        self._thread.start()

    def close(self) -> None:
        self._stop_event.set()
        thread = self._thread
        if thread and thread is not threading.current_thread():
            thread.join()
        if self._thread is thread and (thread is None or not thread.is_alive()):
            self._thread = None

    def _run(self) -> None:
        selector = selectors.DefaultSelector()
        files: dict[Path, int] = {}
        pressed_by_fd: dict[int, set[int]] = {}
        unavailable_since: float | None = None
        unavailable_reported = False
        availability_notified = False
        try:
            while not self._stop_event.is_set():
                self._reconcile_devices(selector, files, pressed_by_fd)
                if self._stop_event.is_set():
                    break
                self._update_match_state(pressed_by_fd)
                if files:
                    if not availability_notified:
                        self._emit(HotkeyEvent(HotkeyEventKind.AVAILABLE))
                        availability_notified = True
                    unavailable_since = None
                    unavailable_reported = False
                else:
                    availability_notified = False
                    now = time.monotonic()
                    if unavailable_since is None:
                        unavailable_since = now
                    if (
                        not unavailable_reported
                        and now - unavailable_since >= DEVICE_ERROR_DELAY_SECONDS
                    ):
                        self._emit(
                            HotkeyEvent(
                                HotkeyEventKind.UNAVAILABLE,
                                f"Could not register {self.spec.display}. Grant access to "
                                "/dev/input keyboard events. AgentDictate will keep retrying.",
                            )
                        )
                        unavailable_reported = True

                for key, _mask in selector.select(timeout=DEVICE_POLL_SECONDS):
                    if self._stop_event.is_set():
                        break
                    fd = key.fd
                    try:
                        chunk = os.read(fd, EVENT_STRUCT.size * 32)
                    except BlockingIOError:
                        continue
                    except OSError:
                        self._remove_device(selector, files, pressed_by_fd, fd)
                        self._update_match_state(pressed_by_fd)
                        continue
                    if not chunk:
                        self._remove_device(selector, files, pressed_by_fd, fd)
                        self._update_match_state(pressed_by_fd)
                        continue
                    device_pressed = pressed_by_fd.setdefault(fd, set())
                    for event in self._events_from_chunk(chunk):
                        if self._stop_event.is_set():
                            break
                        _sec, _usec, event_type, code, value = event
                        if event_type != EV_KEY:
                            continue
                        if value in (1, 2):
                            device_pressed.add(code)
                        elif value == 0:
                            device_pressed.discard(code)
                        if code == KEY_ESC and value == 1:
                            self._matched = False
                            self._cancelled_until_release = True
                            self._emit(HotkeyEvent(HotkeyEventKind.CANCELLED))
                            continue
                        self._update_match_state(pressed_by_fd)
        finally:
            for fd in list(files.values()):
                try:
                    selector.unregister(fd)
                except Exception:
                    pass
                try:
                    os.close(fd)
                except OSError:
                    pass
            try:
                selector.close()
            finally:
                if self._thread is threading.current_thread():
                    self._thread = None

    def _reconcile_devices(
        self,
        selector: selectors.BaseSelector,
        files: dict[Path, int],
        pressed_by_fd: dict[int, set[int]],
    ) -> None:
        try:
            desired_paths = set(keyboard_event_paths(hotkey=self.spec))
        except OSError:
            desired_paths = set()

        for path in set(files) - desired_paths:
            self._remove_device(selector, files, pressed_by_fd, files[path])

        for path in sorted(desired_paths - set(files)):
            fd: int | None = None
            try:
                fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
                selector.register(fd, selectors.EVENT_READ)
            except (OSError, ValueError, KeyError):
                if fd is not None:
                    try:
                        os.close(fd)
                    except OSError:
                        pass
                continue
            files[path] = fd
            pressed_by_fd[fd] = set()
            log_event("hotkey_device_opened", device=path.name)

    def _remove_device(
        self,
        selector: selectors.BaseSelector,
        files: dict[Path, int],
        pressed_by_fd: dict[int, set[int]],
        fd: int,
    ) -> None:
        path = next((path for path, candidate in files.items() if candidate == fd), None)
        if path is not None:
            files.pop(path, None)
            log_event("hotkey_device_removed", device=path.name)
        pressed_by_fd.pop(fd, None)
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

    def _update_match_state(self, pressed_by_fd: dict[int, set[int]]) -> None:
        # A chord must originate from one physical input device. Combining state
        # across keyboards lets stale modifiers or virtual devices manufacture a
        # second Ctrl+Space press that the user never made.
        matches = any(self.spec.matches(pressed) for pressed in pressed_by_fd.values())
        if self._cancelled_until_release:
            if not matches:
                self._cancelled_until_release = False
            return
        if matches and not self._matched:
            self._matched = True
            self._emit(HotkeyEvent(HotkeyEventKind.PRESSED))
        elif not matches and self._matched:
            self._matched = False
            self._emit(HotkeyEvent(HotkeyEventKind.RELEASED))

    def _emit(self, event: HotkeyEvent) -> None:
        if self._stop_event.is_set():
            return
        try:
            log_event("hotkey_event", kind=event.kind.value, detail=event.message)
            self.on_event(event)
        except Exception:
            LOGGER.exception("Hotkey event callback failed for %s", event.kind.value)
