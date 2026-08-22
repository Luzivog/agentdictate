from __future__ import annotations

import subprocess
import time
import unittest
from types import SimpleNamespace
from unittest.mock import ANY, Mock, call, patch

from agentdictate.clipboard import (
    ClipboardPaste,
    ClipboardProtocol,
    ClipboardSelection,
    PasteTarget,
)
from agentdictate.settings.constants import (
    PASTE_SHORTCUT_STANDARD,
    PASTE_SHORTCUT_TERMINAL,
)


class ClipboardTests(unittest.TestCase):
    def setUp(self) -> None:
        ClipboardPaste._active_sources.clear()

    def test_deliver_uses_standard_shortcut_for_current_gui_target(self) -> None:
        paste = ClipboardPaste()
        target = PasteTarget(ClipboardProtocol.X11, "42", "chatgpt Chatgpt")
        source = self._source(ClipboardProtocol.X11)

        with patch.object(paste, "_active_target", return_value=target), patch.object(
            paste, "_publish_regular", return_value=source
        ) as publish, patch.object(paste, "_send_shortcut", return_value=True) as send:
            result = paste.deliver("hello")

        self.assertTrue(result.copied)
        self.assertTrue(result.paste_triggered)
        self.assertEqual(result.shortcut, "ctrl+v")
        publish.assert_called_once()
        send.assert_called_once_with("ctrl+v", target, ANY)

    @patch.dict(
        "agentdictate.clipboard.os.environ",
        {"WAYLAND_DISPLAY": "wayland-0"},
        clear=True,
    )
    @patch("agentdictate.clipboard.shutil.which")
    @patch("agentdictate.clipboard.subprocess.Popen")
    @patch("agentdictate.clipboard.subprocess.run")
    def test_deliver_uses_both_wayland_selections_for_unclassified_target(
        self,
        run: Mock,
        popen: Mock,
        which: Mock,
    ) -> None:
        paste = ClipboardPaste()
        target = PasteTarget(ClipboardProtocol.WAYLAND)
        clipboard_process = Mock(stdin=Mock())
        clipboard_process.poll.return_value = None
        primary_process = Mock(stdin=Mock())
        primary_process.poll.return_value = None
        popen.side_effect = [clipboard_process, primary_process]
        which.side_effect = lambda command: f"/usr/bin/{command}"

        def complete(command: list[str], **_kwargs):
            stdout = b"hello" if command[0] == "/usr/bin/wl-paste" else b""
            return subprocess.CompletedProcess(command, 0, stdout, b"")

        run.side_effect = complete

        with patch.object(paste, "_active_target", return_value=target):
            result = paste.deliver("hello")

        self.assertTrue(result.copied)
        self.assertTrue(result.paste_triggered)
        self.assertEqual(result.shortcut, "shift+insert")
        self.assertEqual(
            [item.args[0] for item in popen.call_args_list],
            [
                ["/usr/bin/wl-copy", "--foreground"],
                ["/usr/bin/wl-copy", "--foreground", "--primary"],
            ],
        )
        clipboard_process.stdin.write.assert_called_once_with(b"hello")
        primary_process.stdin.write.assert_called_once_with(b"hello")
        injection_commands = [
            item.args[0]
            for item in run.call_args_list
            if item.args[0][0] == "ydotool"
        ]
        self.assertEqual(
            injection_commands,
            [
                [
                    "ydotool",
                    "key",
                    "--delay",
                    "50",
                    "--key-delay",
                    "25",
                    "shift+insert",
                ]
            ],
        )

        ClipboardPaste.close_sources()
        clipboard_process.terminate.assert_called_once_with()
        primary_process.terminate.assert_called_once_with()

    def test_deliver_follows_new_focus_without_refocusing_old_window(self) -> None:
        paste = ClipboardPaste()
        chatgpt = PasteTarget(ClipboardProtocol.X11, "42", "chatgpt Chatgpt")
        kitty = PasteTarget(ClipboardProtocol.X11, "84", "kitty kitty")
        source = self._source(ClipboardProtocol.X11)

        with patch.object(
            paste,
            "_active_target",
            side_effect=[chatgpt, kitty, kitty, kitty],
        ), patch.object(
            paste, "_publish_regular", return_value=source
        ) as publish, patch.object(paste, "_send_shortcut", return_value=True) as send:
            result = paste.deliver("hello")

        self.assertEqual(result.shortcut, "ctrl+shift+v")
        self.assertEqual(publish.call_count, 1)
        send.assert_called_once_with("ctrl+shift+v", kitty, ANY)

    def test_deliver_republishes_when_current_focus_uses_another_protocol(self) -> None:
        paste = ClipboardPaste()
        x11 = PasteTarget(ClipboardProtocol.X11, "42", "chatgpt Chatgpt")
        wayland = PasteTarget(ClipboardProtocol.WAYLAND)
        x11_source = self._source(ClipboardProtocol.X11)
        wayland_source = self._source(ClipboardProtocol.WAYLAND)
        primary_source = self._source(
            ClipboardProtocol.WAYLAND,
            ClipboardSelection.PRIMARY,
        )

        with patch.object(
            paste,
            "_active_target",
            side_effect=[x11, wayland, wayland, wayland],
        ), patch.object(
            paste,
            "_publish_regular",
            side_effect=[x11_source, wayland_source],
        ) as publish, patch.object(
            paste, "_publish_primary", return_value=primary_source
        ), patch.object(paste, "_send_shortcut", return_value=True):
            result = paste.deliver("hello")

        self.assertTrue(result.paste_triggered)
        self.assertEqual(
            [item.args[1] for item in publish.call_args_list],
            [ClipboardProtocol.X11, ClipboardProtocol.WAYLAND],
        )

    def test_deliver_never_retries_an_ambiguous_injection_failure(self) -> None:
        paste = ClipboardPaste(shortcut_mode=PASTE_SHORTCUT_STANDARD)
        target = PasteTarget(ClipboardProtocol.X11, "42", "chatgpt Chatgpt")
        source = self._source(ClipboardProtocol.X11)

        with patch.object(paste, "_active_target", return_value=target), patch.object(
            paste, "_publish_regular", return_value=source
        ), patch.object(paste, "_send_shortcut", return_value=False) as send:
            result = paste.deliver("hello")

        self.assertTrue(result.copied)
        self.assertFalse(result.paste_triggered)
        self.assertEqual(send.call_count, 1)

    def test_known_clipboard_owner_exit_is_the_next_publish_readiness_signal(
        self,
    ) -> None:
        paste = ClipboardPaste()
        previous = self._source(ClipboardProtocol.WAYLAND)
        previous.process.poll.return_value = None
        current = self._source(ClipboardProtocol.WAYLAND)
        current.process.poll.return_value = None
        ClipboardPaste._active_sources[
            (ClipboardProtocol.WAYLAND, ClipboardSelection.CLIPBOARD)
        ] = previous

        with patch.object(
            paste, "_start_source", return_value=current
        ), patch.object(
            paste, "_wait_for_exit", return_value=True
        ) as wait, patch.object(
            paste, "_wait_for_clipboard"
        ) as readback:
            result = paste._publish_regular_bytes(
                b"hello", ClipboardProtocol.WAYLAND, time.monotonic() + 1
            )

        self.assertIs(result, current)
        wait.assert_called_once_with(previous.process, ANY)
        readback.assert_not_called()

    def test_close_sources_terminates_owned_clipboard_processes(self) -> None:
        wayland = self._source(ClipboardProtocol.WAYLAND)
        x11 = self._source(ClipboardProtocol.X11)
        ClipboardPaste._active_sources.update(
            {
                (ClipboardProtocol.WAYLAND, ClipboardSelection.CLIPBOARD): wayland,
                (ClipboardProtocol.X11, ClipboardSelection.CLIPBOARD): x11,
            }
        )

        with patch.object(ClipboardPaste, "_terminate_source") as terminate:
            ClipboardPaste.close_sources()

        self.assertEqual(
            terminate.call_args_list,
            [call(wayland), call(x11)],
        )
        self.assertEqual(ClipboardPaste._active_sources, {})

    @patch("agentdictate.clipboard.time.sleep")
    def test_deliver_has_no_correctness_sleep(self, sleep: Mock) -> None:
        paste = ClipboardPaste()
        target = PasteTarget(ClipboardProtocol.WAYLAND)
        source = self._source(ClipboardProtocol.WAYLAND)
        primary_source = self._source(
            ClipboardProtocol.WAYLAND,
            ClipboardSelection.PRIMARY,
        )

        with patch.object(paste, "_active_target", return_value=target), patch.object(
            paste, "_publish_regular", return_value=source
        ), patch.object(
            paste, "_publish_primary", return_value=primary_source
        ), patch.object(paste, "_send_shortcut", return_value=True):
            result = paste.deliver("hello")

        self.assertTrue(result.paste_triggered)
        sleep.assert_not_called()

    def test_wayland_restore_waits_for_consumption_before_restoring(self) -> None:
        paste = ClipboardPaste(restore_previous=True)
        target = PasteTarget(ClipboardProtocol.WAYLAND)
        regular = self._source(ClipboardProtocol.WAYLAND)
        primary = self._source(
            ClipboardProtocol.WAYLAND,
            ClipboardSelection.PRIMARY,
        )
        one_paste = self._source(ClipboardProtocol.WAYLAND)
        restored = self._source(ClipboardProtocol.WAYLAND)

        with patch.object(paste, "_active_target", return_value=target), patch.object(
            paste, "_read_clipboard", return_value=b"previous"
        ), patch.object(
            paste, "_publish_regular", return_value=regular
        ), patch.object(
            paste, "_publish_primary", return_value=primary
        ), patch.object(
            paste, "_replace_with_one_paste_source", return_value=one_paste
        ), patch.object(
            paste, "_send_shortcut", return_value=True
        ), patch.object(
            paste, "_wait_for_exit", return_value=True
        ) as wait, patch.object(
            paste, "_publish_regular_bytes", return_value=restored
        ) as restore:
            result = paste.deliver("hello")

        self.assertTrue(result.paste_triggered)
        wait.assert_called_once_with(one_paste.process, ANY)
        restore.assert_called_once_with(
            b"previous", ClipboardProtocol.WAYLAND, ANY
        )

    def test_x11_restore_setting_never_risks_the_paste(self) -> None:
        paste = ClipboardPaste(restore_previous=True)
        target = PasteTarget(ClipboardProtocol.X11, "42", "chatgpt Chatgpt")
        source = self._source(ClipboardProtocol.X11)

        with patch.object(paste, "_active_target", return_value=target), patch.object(
            paste, "_read_clipboard"
        ) as read, patch.object(
            paste, "_publish_regular", return_value=source
        ), patch.object(
            paste, "_replace_with_one_paste_source"
        ) as replace, patch.object(
            paste, "_send_shortcut", return_value=True
        ):
            result = paste.deliver("hello")

        self.assertTrue(result.paste_triggered)
        self.assertIn("not restored", result.error)
        read.assert_not_called()
        replace.assert_not_called()

    @patch("agentdictate.clipboard.shutil.which")
    def test_x11_uses_non_detaching_xsel_owner(self, which: Mock) -> None:
        which.side_effect = lambda command: f"/usr/bin/{command}"
        paste = ClipboardPaste()

        self.assertEqual(
            paste._source_command(ClipboardProtocol.X11, paste_once=False),
            [
                "/usr/bin/xsel",
                "--clipboard",
                "--input",
                "--nodetach",
            ],
        )
        self.assertEqual(
            paste._read_command(ClipboardProtocol.X11),
            ["/usr/bin/xsel", "--clipboard", "--output"],
        )

    @patch.dict(
        "agentdictate.clipboard.os.environ",
        {"WAYLAND_DISPLAY": "wayland-0"},
        clear=True,
    )
    @patch("agentdictate.clipboard.shutil.which")
    def test_ydotool_injection_has_zero_configured_delay(self, which: Mock) -> None:
        which.side_effect = lambda command: (
            "/usr/bin/ydotool" if command == "ydotool" else None
        )
        paste = ClipboardPaste()
        completed = subprocess.CompletedProcess(["ydotool"], 0, b"", b"")

        with patch.object(paste, "_run", return_value=completed) as run:
            sent = paste._send_shortcut(
                "ctrl+v",
                PasteTarget(ClipboardProtocol.WAYLAND),
                time.monotonic() + 1,
            )

        self.assertTrue(sent)
        run.assert_called_once_with(
            [
                "ydotool",
                "key",
                "--delay",
                "0",
                "--key-delay",
                "0",
                "ctrl+v",
            ],
            ANY,
        )

    @patch.dict(
        "agentdictate.clipboard.os.environ",
        {"DISPLAY": ":0", "WAYLAND_DISPLAY": "wayland-0"},
        clear=True,
    )
    @patch("agentdictate.clipboard.shutil.which")
    def test_active_xwayland_target_uses_supported_xprop_class_lookup(
        self, which: Mock
    ) -> None:
        which.side_effect = lambda command: f"/usr/bin/{command}"
        paste = ClipboardPaste()
        active = subprocess.CompletedProcess(
            ["xdotool", "getactivewindow"], 0, stdout="18874372\n", stderr=""
        )
        properties = subprocess.CompletedProcess(
            ["xprop"],
            0,
            stdout=(
                'WM_CLASS(STRING) = "chatgpt (/config/Codex)", "Chatgpt"\n'
                "_NET_WM_STATE(ATOM) = _NET_WM_STATE_FOCUSED\n"
            ),
            stderr="",
        )

        with patch.object(paste, "_run", side_effect=[active, properties]) as run:
            target = paste._active_target(time.monotonic() + 1)

        self.assertEqual(target.protocol, ClipboardProtocol.X11)
        self.assertEqual(target.window_id, "18874372")
        self.assertIn("Chatgpt", target.window_class)
        self.assertEqual(
            run.call_args_list[1],
            call(
                [
                    "xprop",
                    "-id",
                    "18874372",
                    "WM_CLASS",
                    "_NET_WM_STATE",
                ],
                ANY,
                text=True,
            ),
        )

    @patch.dict(
        "agentdictate.clipboard.os.environ",
        {"DISPLAY": ":0", "WAYLAND_DISPLAY": "wayland-0"},
        clear=True,
    )
    @patch("agentdictate.clipboard.shutil.which")
    def test_active_native_wayland_target_ignores_stale_xwayland_window(
        self, which: Mock
    ) -> None:
        which.side_effect = lambda command: f"/usr/bin/{command}"
        paste = ClipboardPaste()
        active = subprocess.CompletedProcess(
            ["xdotool", "getactivewindow"], 0, stdout="18874372\n", stderr=""
        )
        properties = subprocess.CompletedProcess(
            ["xprop"],
            0,
            stdout=(
                'WM_CLASS(STRING) = "chatgpt (/config/Codex)", "Chatgpt"\n'
                "_NET_WM_STATE(ATOM) = _NET_WM_STATE_MAXIMIZED_VERT\n"
            ),
            stderr="",
        )

        with patch.object(paste, "_run", side_effect=[active, properties]):
            target = paste._active_target(time.monotonic() + 1)

        self.assertEqual(target, PasteTarget(ClipboardProtocol.WAYLAND))

    def test_explicit_shortcut_modes_do_not_publish_the_primary_selection(
        self,
    ) -> None:
        target = PasteTarget(ClipboardProtocol.WAYLAND)
        cases = [
            (PASTE_SHORTCUT_STANDARD, "ctrl+v"),
            (PASTE_SHORTCUT_TERMINAL, "ctrl+shift+v"),
        ]

        for mode, expected_shortcut in cases:
            with self.subTest(mode=mode):
                paste = ClipboardPaste(shortcut_mode=mode)
                source = self._source(ClipboardProtocol.WAYLAND)
                with patch.object(
                    paste, "_active_target", return_value=target
                ), patch.object(
                    paste, "_publish_regular", return_value=source
                ), patch.object(
                    paste, "_publish_primary"
                ) as publish_primary, patch.object(
                    paste, "_send_shortcut", return_value=True
                ) as send:
                    result = paste.deliver("hello")

                self.assertEqual(result.shortcut, expected_shortcut)
                publish_primary.assert_not_called()
                send.assert_called_once_with(expected_shortcut, target, ANY)

    @staticmethod
    def _source(
        protocol: ClipboardProtocol,
        selection: ClipboardSelection = ClipboardSelection.CLIPBOARD,
    ):
        return SimpleNamespace(
            protocol=protocol,
            process=Mock(),
            selection=selection,
        )
