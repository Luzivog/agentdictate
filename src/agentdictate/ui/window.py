from __future__ import annotations

from typing import Any

from agentdictate.paths import APP_DESKTOP_ID, APP_NAME

from .gtk import Gdk, GLib, Gtk


class WindowMixin:
    def show_window(self, tab: int | None = None) -> None:
        if self.window is not None and self._settings_window_needs_rebuild():
            self._destroy_settings_window()
        if self.window is None:
            self.window = self._build_window()
        target_page = tab if tab is not None else self._last_window_page
        if target_page is not None and self.notebook:
            self.notebook.set_current_page(target_page)
        self._settings_window_iconified = False
        self._present_window()
        GLib.idle_add(self._present_window)
        GLib.timeout_add(150, self._recover_iconified_window)
        GLib.timeout_add(500, self._recover_iconified_window)
        self.refresh_all()

    def _present_window(self) -> bool:
        window = self.window
        if window is None:
            return False
        window.show()
        window.show_all()
        window.deiconify()
        timestamp = Gtk.get_current_event_time() or Gdk.CURRENT_TIME
        gdk_window = window.get_window()
        if gdk_window is not None:
            try:
                gdk_window.show()
                gdk_window.deiconify()
                gdk_window.raise_()
                gdk_window.focus(timestamp)
            except Exception:
                pass
        window.present_with_time(timestamp)
        return False

    def _settings_window_needs_rebuild(self) -> bool:
        if self._settings_window_iconified:
            return True
        if self.window is None:
            return False
        gdk_window = self.window.get_window()
        if gdk_window is None:
            return False
        return bool(gdk_window.get_state() & Gdk.WindowState.ICONIFIED)

    def _recover_iconified_window(self) -> bool:
        window = self.window
        if window is None or window.get_window() is None:
            return False
        if not self._settings_window_needs_rebuild():
            return False
        current_page = self._current_window_page()
        self._destroy_settings_window()
        self.window = self._build_window()
        if self.notebook:
            self.notebook.set_current_page(current_page)
        self._settings_window_iconified = False
        self._present_window()
        return False

    def _current_window_page(self) -> int:
        if self.notebook is None:
            return self._last_window_page or 0
        page = self.notebook.get_current_page()
        return page if page >= 0 else 0

    def _destroy_settings_window(self) -> None:
        window = self.window
        if window is None:
            return
        self._last_window_page = self._current_window_page()
        window.destroy()

    def _on_window_state_event(self, window: Gtk.ApplicationWindow, event: Any) -> bool:
        if event.changed_mask & Gdk.WindowState.ICONIFIED:
            if event.new_window_state & Gdk.WindowState.ICONIFIED:
                self._settings_window_iconified = True
                self._last_window_page = self._current_window_page()
                GLib.idle_add(self._destroy_iconified_window, window)
            else:
                self._settings_window_iconified = False
        return False

    def _destroy_iconified_window(self, window: Gtk.ApplicationWindow) -> bool:
        if self.window is window:
            self._destroy_settings_window()
        return False

    def _build_window(self) -> Gtk.ApplicationWindow:
        window = Gtk.ApplicationWindow(application=self)
        window.set_title(APP_NAME)
        window.set_icon_name("agentdictate")
        try:
            window.set_wmclass("agentdictate", APP_DESKTOP_ID)
        except Exception:
            pass
        window.set_default_size(640, 720)
        window.set_resizable(True)
        window.connect("window-state-event", self._on_window_state_event)
        window.connect("delete-event", self._on_window_close)
        window.connect("destroy", self._on_window_destroy)
        outer = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        outer.set_border_width(10)
        window.add(outer)
        self._build_header(outer)
        notebook = Gtk.Notebook()
        self.notebook = notebook
        outer.pack_start(notebook, True, True, 0)
        for page, label in self._settings_pages():
            notebook.append_page(page, Gtk.Label(label=label))
        self._sync_ui_from_settings()
        return window

    def _build_header(self, outer: Gtk.Box) -> None:
        header = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        outer.pack_start(header, False, False, 0)
        self.status_label = Gtk.Label(label="Status: Ready")
        self.status_label.set_xalign(0)
        header.pack_start(self.status_label, True, True, 0)
        self.message_label = Gtk.Label(label="")
        self.message_label.set_xalign(1)
        header.pack_start(self.message_label, True, True, 0)

    def _settings_pages(self) -> list[tuple[Gtk.Widget, str]]:
        return [
            (self._overview_tab(), "Overview"),
            (self._openai_tab(), "OpenAI"),
            (self._dictation_tab(), "Dictation"),
            (self._cleanup_tab(), "Cleanup"),
            (self._replacements_tab(), "Replacements"),
            (self._history_tab(), "History"),
            (self._stats_tab(), "Stats"),
            (self._advanced_tab(), "Advanced"),
        ]

    def _on_window_close(self, window: Gtk.ApplicationWindow, _event: Any) -> bool:
        if self.settings.minimize_to_tray_on_close:
            self._last_window_page = self._current_window_page()
            window.hide()
            return True
        return False

    def _on_window_destroy(self, _window: Gtk.ApplicationWindow) -> None:
        self.window = None
        self.notebook = None
        self._settings_window_iconified = False
