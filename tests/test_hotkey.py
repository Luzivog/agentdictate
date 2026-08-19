from __future__ import annotations

import os
import struct
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

from agentdictate.hotkey import (
    EV_KEY,
    HotkeyEvent,
    HotkeyEventKind,
    InputHotkeyListener,
    KEY_ESC,
    KEY_LEFTCTRL,
    KEY_SPACE,
    keyboard_event_paths,
    parse_hotkey,
)
from agentdictate.hotkeys.constants import EVENT_STRUCT


class HotkeyTests(unittest.TestCase):
    def test_parse_ctrl_space(self) -> None:
        spec = parse_hotkey("Ctrl+Space")
        self.assertTrue(spec.matches({29, 57}))
        self.assertTrue(spec.matches({97, 57}))
        self.assertFalse(spec.matches({57}))

    def test_listener_emits_press_and_release_through_public_interface(self) -> None:
        read_fd, write_fd = os.pipe()
        events: list[HotkeyEvent] = []
        available = threading.Event()
        released = threading.Event()

        def receive(event: HotkeyEvent) -> None:
            events.append(event)
            if event.kind == HotkeyEventKind.AVAILABLE:
                available.set()
            if event.kind == HotkeyEventKind.RELEASED:
                released.set()

        listener = InputHotkeyListener("Ctrl+Space", receive)
        with patch(
            "agentdictate.hotkeys.listener.keyboard_event_paths",
            return_value=[Path("/dev/input/event99")],
        ), patch(
            "agentdictate.hotkeys.listener.os.open",
            side_effect=lambda *_args: os.dup(read_fd),
        ):
            listener.start()
            self.assertTrue(available.wait(2))
            os.write(
                write_fd,
                self._key_event(KEY_LEFTCTRL, 1)
                + self._key_event(KEY_SPACE, 1)
                + self._key_event(KEY_SPACE, 0)
                + self._key_event(KEY_LEFTCTRL, 0),
            )
            self.assertTrue(released.wait(2))
            listener.close()

        os.close(read_fd)
        os.close(write_fd)
        self.assertEqual(
            [event.kind for event in events],
            [
                HotkeyEventKind.AVAILABLE,
                HotkeyEventKind.PRESSED,
                HotkeyEventKind.RELEASED,
            ],
        )

    def test_listener_does_not_combine_keys_from_different_devices(self) -> None:
        first_read, first_write = os.pipe()
        second_read, second_write = os.pipe()
        events: list[HotkeyEventKind] = []
        available = threading.Event()
        released = threading.Event()
        cancelled = threading.Event()

        def receive(event: HotkeyEvent) -> None:
            events.append(event.kind)
            if event.kind == HotkeyEventKind.AVAILABLE:
                available.set()
            if event.kind == HotkeyEventKind.RELEASED:
                released.set()
            if event.kind == HotkeyEventKind.CANCELLED:
                cancelled.set()

        listener = InputHotkeyListener("Ctrl+Space", receive)
        source_fds = iter((first_read, second_read))
        with patch(
            "agentdictate.hotkeys.listener.keyboard_event_paths",
            return_value=[Path("/dev/input/event98"), Path("/dev/input/event99")],
        ), patch(
            "agentdictate.hotkeys.listener.os.open",
            side_effect=lambda *_args: os.dup(next(source_fds)),
        ):
            listener.start()
            self.assertTrue(available.wait(2))
            os.write(
                first_write,
                self._key_event(KEY_LEFTCTRL, 1)
                + self._key_event(KEY_SPACE, 1)
                + self._key_event(KEY_SPACE, 0),
            )
            self.assertTrue(released.wait(2))
            os.write(second_write, self._key_event(KEY_SPACE, 1))
            os.write(second_write, self._key_event(KEY_ESC, 1))
            self.assertTrue(cancelled.wait(2))
            listener.close()

        for fd in (first_read, first_write, second_read, second_write):
            os.close(fd)
        self.assertEqual(
            events,
            [
                HotkeyEventKind.AVAILABLE,
                HotkeyEventKind.PRESSED,
                HotkeyEventKind.RELEASED,
                HotkeyEventKind.CANCELLED,
            ],
        )

    def test_listener_recovers_when_keyboard_appears_after_startup(self) -> None:
        read_fd, write_fd = os.pipe()
        device_available = threading.Event()
        unavailable = threading.Event()
        available = threading.Event()
        events: list[HotkeyEventKind] = []

        def receive(event: HotkeyEvent) -> None:
            events.append(event.kind)
            if event.kind == HotkeyEventKind.UNAVAILABLE:
                unavailable.set()
            if event.kind == HotkeyEventKind.AVAILABLE:
                available.set()

        listener = InputHotkeyListener("Ctrl+Space", receive)
        with patch(
            "agentdictate.hotkeys.listener.keyboard_event_paths",
            side_effect=lambda **_kwargs: (
                [Path("/dev/input/event99")] if device_available.is_set() else []
            ),
        ), patch(
            "agentdictate.hotkeys.listener.os.open",
            side_effect=lambda *_args: os.dup(read_fd),
        ), patch(
            "agentdictate.hotkeys.listener.DEVICE_POLL_SECONDS",
            0.01,
        ), patch(
            "agentdictate.hotkeys.listener.DEVICE_ERROR_DELAY_SECONDS",
            0.0,
        ):
            listener.start()
            self.assertTrue(unavailable.wait(2))
            device_available.set()
            self.assertTrue(available.wait(2))
            listener.close()

        os.close(read_fd)
        os.close(write_fd)
        self.assertEqual(events, [HotkeyEventKind.UNAVAILABLE, HotkeyEventKind.AVAILABLE])

    def test_escape_cancels_active_chord_without_emitting_release(self) -> None:
        read_fd, write_fd = os.pipe()
        events: list[HotkeyEventKind] = []
        available = threading.Event()
        cancelled = threading.Event()

        def receive(event: HotkeyEvent) -> None:
            events.append(event.kind)
            if event.kind == HotkeyEventKind.AVAILABLE:
                available.set()
            if event.kind == HotkeyEventKind.CANCELLED:
                cancelled.set()

        listener = InputHotkeyListener("Ctrl+Space", receive)
        with patch(
            "agentdictate.hotkeys.listener.keyboard_event_paths",
            return_value=[Path("/dev/input/event99")],
        ), patch(
            "agentdictate.hotkeys.listener.os.open",
            side_effect=lambda *_args: os.dup(read_fd),
        ):
            listener.start()
            self.assertTrue(available.wait(2))
            os.write(
                write_fd,
                self._key_event(KEY_LEFTCTRL, 1)
                + self._key_event(KEY_SPACE, 1)
                + self._key_event(KEY_ESC, 1)
                + self._key_event(KEY_SPACE, 0)
                + self._key_event(KEY_LEFTCTRL, 0),
            )
            self.assertTrue(cancelled.wait(2))
            listener.close()

        os.close(read_fd)
        os.close(write_fd)
        self.assertEqual(
            events,
            [
                HotkeyEventKind.AVAILABLE,
                HotkeyEventKind.PRESSED,
                HotkeyEventKind.CANCELLED,
            ],
        )

    def test_device_discovery_ignores_devices_without_the_hotkey(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            devices_file = root / "devices"
            device_dir = root / "dev"
            sysfs_dir = root / "sys"
            device_dir.mkdir()
            devices_file.write_text(
                'N: Name="real keyboard"\nH: Handlers=sysrq kbd event1\n\n'
                'N: Name="power button"\nH: Handlers=kbd event2\n',
                encoding="utf-8",
            )
            for event_name, codes in (
                ("event1", {KEY_LEFTCTRL, KEY_SPACE}),
                ("event2", {KEY_SPACE}),
            ):
                (device_dir / event_name).touch()
                capability_dir = sysfs_dir / event_name / "device" / "capabilities"
                capability_dir.mkdir(parents=True)
                mask = sum(1 << code for code in codes)
                (capability_dir / "key").write_text(f"{mask:x}\n", encoding="utf-8")

            paths = keyboard_event_paths(
                devices_file,
                hotkey=parse_hotkey("Ctrl+Space"),
                device_dir=device_dir,
                sysfs_input_dir=sysfs_dir,
            )

        self.assertEqual(paths, [device_dir / "event1"])

    def test_device_discovery_ignores_ydotool_virtual_keyboard(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            devices_file = root / "devices"
            device_dir = root / "dev"
            sysfs_dir = root / "sys"
            device_dir.mkdir()
            devices_file.write_text(
                'N: Name="real keyboard"\nH: Handlers=kbd event1\n\n'
                'N: Name="ydotoold virtual device"\nH: Handlers=kbd event2\n',
                encoding="utf-8",
            )
            for event_name in ("event1", "event2"):
                (device_dir / event_name).touch()
                capability_dir = sysfs_dir / event_name / "device" / "capabilities"
                capability_dir.mkdir(parents=True)
                mask = (1 << KEY_LEFTCTRL) | (1 << KEY_SPACE)
                (capability_dir / "key").write_text(f"{mask:x}\n", encoding="utf-8")

            paths = keyboard_event_paths(
                devices_file,
                hotkey=parse_hotkey("Ctrl+Space"),
                device_dir=device_dir,
                sysfs_input_dir=sysfs_dir,
            )

        self.assertEqual(paths, [device_dir / "event1"])

    def test_device_discovery_ignores_malformed_capability_data(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            devices_file = root / "devices"
            device_dir = root / "dev"
            capability_dir = root / "sys" / "event1" / "device" / "capabilities"
            device_dir.mkdir()
            capability_dir.mkdir(parents=True)
            (device_dir / "event1").touch()
            devices_file.write_text("H: Handlers=kbd event1\n", encoding="utf-8")
            (capability_dir / "key").write_text("not-hex\n", encoding="ascii")

            paths = keyboard_event_paths(
                devices_file,
                hotkey=parse_hotkey("Ctrl+Space"),
                device_dir=device_dir,
                sysfs_input_dir=root / "sys",
            )

        self.assertEqual(paths, [])

    @staticmethod
    def _key_event(code: int, value: int) -> bytes:
        return EVENT_STRUCT.pack(0, 0, EV_KEY, code, value)
