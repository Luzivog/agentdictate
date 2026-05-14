from __future__ import annotations

from typing import Callable

from agentdictate.paths import APP_NAME

from .canvas import DictationOverlayCanvas
from .gtk import Gdk, GLib, Gtk
from .positioning import is_wayland_display, primary_monitor_area


class DictationOverlayWindow(Gtk.Window):
    ACTIVE_STATUSES = {"Recording", "Transcribing", "Cleaning up"}

    def __init__(
        self,
        application: Gtk.Application,
        waveform_provider: Callable[[], list[float]],
        elapsed_provider: Callable[[], float],
    ) -> None:
        super().__init__(type=self._window_type())
        self.set_application(application)
        self.set_title(f"{APP_NAME} status")
        self.set_decorated(False)
        self.set_resizable(False)
        self.set_keep_above(True)
        self.set_skip_taskbar_hint(True)
        self.set_skip_pager_hint(True)
        self.set_accept_focus(False)
        self.set_focus_on_map(False)
        self.set_type_hint(Gdk.WindowTypeHint.NOTIFICATION)
        self.set_app_paintable(True)
        self._hide_source: int | None = None

        screen = self.get_screen()
        visual = screen.get_rgba_visual()
        if visual is not None and screen.is_composited():
            self.set_visual(visual)

        self.canvas = DictationOverlayCanvas(waveform_provider, elapsed_provider)
        self.add(self.canvas)
        self.connect("realize", lambda *_args: self._apply_window_hints())
        self.connect("size-allocate", lambda *_args: self._position())

    @staticmethod
    def _is_wayland_display(display: Gdk.Display | None = None) -> bool:
        return is_wayland_display(display)

    @classmethod
    def _window_type(cls) -> Gtk.WindowType:
        if cls._is_wayland_display():
            return Gtk.WindowType.POPUP
        return Gtk.WindowType.TOPLEVEL

    def set_status(self, status: str, cleanup_enabled: bool) -> None:
        self.canvas.set_overlay_state(status, cleanup_enabled)
        if status in self.ACTIVE_STATUSES:
            self._cancel_hide()
            if not self.get_visible():
                self.show_all()
            self._apply_window_hints()
            self._position()
            self.canvas.start_animation()
            return
        if status in {"Ready", "Disabled"}:
            self._schedule_hide(450)
        elif status == "Pasting":
            self._schedule_hide(160)
        elif status == "Error":
            self._schedule_hide(900)

    def _apply_window_hints(self) -> None:
        gdk_window = self.get_window()
        if gdk_window is not None:
            gdk_window.set_accept_focus(False)
            gdk_window.set_focus_on_map(False)

    def _position(self) -> None:
        area = primary_monitor_area(self)
        width, height = self.get_size()
        if width <= 1:
            width = DictationOverlayCanvas.WIDTH
        if height <= 1:
            height = DictationOverlayCanvas.HEIGHT
        x = area.x + max(0, (area.width - width) // 2)
        y = area.y + max(0, area.height - height - 72)
        self.move(x, y)

    def _schedule_hide(self, delay_ms: int) -> None:
        self._cancel_hide()
        self._hide_source = GLib.timeout_add(delay_ms, self._hide_now)

    def _cancel_hide(self) -> None:
        if self._hide_source is not None:
            GLib.source_remove(self._hide_source)
            self._hide_source = None

    def _hide_now(self) -> bool:
        self._hide_source = None
        self.canvas.stop_animation()
        self.hide()
        return False
