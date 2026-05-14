from __future__ import annotations

from typing import Any, Callable

from agentdictate.clipboard import ClipboardPaste
from agentdictate.paths import APP_DESKTOP_ID, APP_NAME, logs_dir

from .gtk import AppIndicator, Gtk
from .indicator import CtypesAppIndicator


class TrayMixin:
    def _ensure_tray(self) -> None:
        if self.status_icon or self.app_indicator or not self.settings.show_tray_icon:
            return
        if AppIndicator is not None:
            indicator = AppIndicator.Indicator.new(
                APP_DESKTOP_ID,
                "agentdictate",
                AppIndicator.IndicatorCategory.APPLICATION_STATUS,
            )
            indicator.set_status(AppIndicator.IndicatorStatus.ACTIVE)
            if hasattr(indicator, "set_title"):
                indicator.set_title(APP_NAME)
            self.tray_menu = self._build_tray_menu()
            indicator.set_menu(self.tray_menu)
            self.app_indicator = indicator
            return
        try:
            indicator = CtypesAppIndicator(APP_DESKTOP_ID, "agentdictate")
            self.tray_menu = self._build_tray_menu()
            indicator.set_menu(self.tray_menu)
            indicator.set_status_active()
            self.app_indicator = indicator
            return
        except Exception:
            self.app_indicator = None
        icon = Gtk.StatusIcon.new_from_icon_name("agentdictate")
        icon.set_visible(True)
        icon.set_tooltip_text(f"{APP_NAME}: Ready")
        icon.connect("activate", lambda *_args: self.show_window())
        icon.connect("popup-menu", self._tray_popup)
        self.status_icon = icon

    def _build_tray_menu(self) -> Gtk.Menu:
        menu = Gtk.Menu()
        items = [
            ("Show AgentDictate", self.show_window),
            ("Start Recording", self.controller.start_recording),
            ("Stop Recording", self.controller.stop_recording),
            ("Enable/Disable Dictation", self._toggle_dictation),
            ("Copy Last Transcript", self._copy_last_transcript),
            ("Open History", lambda: self.show_window(tab=5)),
            ("Open Stats", lambda: self.show_window(tab=6)),
            ("Open Settings", self.show_window),
            ("View Logs", lambda: self._open_path(logs_dir())),
            ("Quit", self.quit),
        ]
        for label, callback in items:
            item = Gtk.MenuItem(label=label)
            item.connect("activate", lambda _item, cb=callback: self._run_menu_action(cb))
            menu.append(item)
        menu.show_all()
        return menu

    def _run_menu_action(self, callback: Callable[[], Any]) -> None:
        callback()

    def _tray_popup(self, _icon: Gtk.StatusIcon, button: int, activate_time: int) -> None:
        menu = self._build_tray_menu()
        menu.popup(None, None, None, None, button, activate_time)

    def _toggle_dictation(self) -> None:
        if self.controller.hotkey_listener:
            self.controller.stop_hotkey()
            self._set_status_ui("Disabled")
        else:
            self.controller.start_hotkey()
            self._set_status_ui("Ready")

    def _copy_last_transcript(self) -> None:
        rows = self.controller.storage.list_history(limit=1)
        if not rows:
            self._set_message("No transcript history yet.", "")
            return
        text = str(rows[0]["final_text"] or "")
        ClipboardPaste().copy(text)
        self._set_message("Last transcript copied.", "")
