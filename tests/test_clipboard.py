from __future__ import annotations

import unittest
from unittest.mock import Mock, patch

from agentdictate.clipboard import ClipboardPaste, _parse_xmodmap_v_keycode


class ClipboardTests(unittest.TestCase):
    @patch("agentdictate.clipboard.subprocess.run")
    @patch("agentdictate.clipboard.shutil.which")
    @patch.dict("agentdictate.clipboard.os.environ", {"WAYLAND_DISPLAY": "wayland-0"}, clear=True)
    def test_wayland_paste_triggers_ctrl_shift_v_not_enter(self, which: Mock, run: Mock) -> None:
        which.side_effect = lambda command: f"/usr/bin/{command}" if command == "ydotool" else None
        run.return_value = Mock(returncode=0)
        with patch("agentdictate.clipboard.detect_paste_keycode", return_value=47):
            self.assertTrue(ClipboardPaste().trigger_paste())
        command = run.call_args.args[0]
        self.assertEqual(command, ["ydotool", "key", "--key-delay", "25", "ctrl+shift+v"])
        self.assertNotIn("28:1", command)
        self.assertNotIn("type", command)

    @patch("agentdictate.clipboard.subprocess.run")
    @patch("agentdictate.clipboard.shutil.which")
    @patch.dict("agentdictate.clipboard.os.environ", {"WAYLAND_DISPLAY": "wayland-0"}, clear=True)
    def test_wayland_paste_raw_fallback_uses_detected_layout_key(
        self, which: Mock, run: Mock
    ) -> None:
        which.side_effect = lambda command: f"/usr/bin/{command}" if command == "ydotool" else None
        run.side_effect = [Mock(returncode=1), Mock(returncode=0)]
        with patch("agentdictate.clipboard.detect_paste_keycode", return_value=39):
            self.assertTrue(ClipboardPaste().trigger_paste())
        self.assertEqual(
            run.call_args_list[0].args[0],
            ["ydotool", "key", "--key-delay", "25", "ctrl+shift+v"],
        )
        self.assertEqual(
            run.call_args_list[1].args[0],
            [
                "ydotool",
                "key",
                "--key-delay",
                "25",
                "29:1",
                "42:1",
                "39:1",
                "39:0",
                "42:0",
                "29:0",
            ],
        )

    @patch("agentdictate.clipboard.subprocess.run")
    @patch("agentdictate.clipboard.shutil.which")
    @patch.dict("agentdictate.clipboard.os.environ", {}, clear=True)
    def test_xdotool_paste_clears_modifiers(self, which: Mock, run: Mock) -> None:
        which.side_effect = lambda command: f"/usr/bin/{command}" if command == "xdotool" else None
        run.return_value = Mock(returncode=0)
        self.assertTrue(ClipboardPaste().trigger_paste())
        self.assertEqual(run.call_args.args[0], ["xdotool", "key", "--clearmodifiers", "ctrl+shift+v"])

    def test_parse_xmodmap_detects_v_for_azerty_layout(self) -> None:
        output = """
        keycode  47 = m M semicolon colon mu ordmasculine
        keycode  55 = v V v V doublelowquotemark singlelowquotemark
        """
        self.assertEqual(_parse_xmodmap_v_keycode(output), 47)

    def test_parse_xmodmap_uses_non_default_v_key_when_layout_moves_v(self) -> None:
        output = """
        keycode  47 = m M m M
        keycode  48 = v V v V
        """
        self.assertEqual(_parse_xmodmap_v_keycode(output), 40)
