from __future__ import annotations

import cairo
import json
import math
import os
import subprocess
import sys
import threading
from typing import Any, Callable

import gi

gi.require_version("Gdk", "3.0")
gi.require_version("Gtk", "3.0")
from gi.repository import Gdk, Gio, GLib, Gtk  # noqa: E402

from .paths import APP_DESKTOP_ID, APP_NAME


class DictationOverlayCanvas(Gtk.DrawingArea):
    WIDTH = 143
    HEIGHT = 56
    BAR_COUNT = 20

    def __init__(
        self,
        waveform_provider: Callable[[], list[float]],
        elapsed_provider: Callable[[], float],
    ) -> None:
        super().__init__()
        self.status = "Ready"
        self.cleanup_enabled = True
        self.waveform_provider = waveform_provider
        self.elapsed_provider = elapsed_provider
        self._tick_id: int | None = None
        self._waveform = [0.0] * self.BAR_COUNT
        self.set_size_request(self.WIDTH, self.HEIGHT)
        self.connect("draw", self._draw)

    def set_overlay_state(self, status: str, cleanup_enabled: bool) -> None:
        if status != self.status:
            self._waveform = [0.0] * self.BAR_COUNT
        self.status = status
        self.cleanup_enabled = cleanup_enabled
        self.queue_draw()

    def start_animation(self) -> None:
        if self._tick_id is None:
            self._tick_id = GLib.timeout_add(33, self._animate)

    def stop_animation(self) -> None:
        if self._tick_id is not None:
            GLib.source_remove(self._tick_id)
            self._tick_id = None

    def _animate(self) -> bool:
        if not self.get_mapped():
            self._tick_id = None
            return False
        if self.status == "Recording":
            values = self.waveform_provider()
            values = self._fit_waveform(values, self.BAR_COUNT)
            next_waveform: list[float] = []
            for current, target in zip(self._waveform, values):
                gated = 0.0 if target < 0.005 else min(1.0, (target - 0.005) / 0.13)
                factor = 0.62 if gated > current else 0.34
                next_waveform.append((current * (1.0 - factor)) + (gated * factor))
            self._waveform = next_waveform
        self.queue_draw()
        return True

    @staticmethod
    def _fit_waveform(values: list[float], count: int) -> list[float]:
        if count <= 0:
            return []
        if len(values) == count:
            return values
        if len(values) < count:
            return (values + [0.0] * count)[:count]

        scale = len(values) / count
        fitted: list[float] = []
        for index in range(count):
            start = int(index * scale)
            end = min(len(values), max(start + 1, int((index + 1) * scale)))
            fitted.append(max(values[start:end] or [0.0]))
        return fitted

    @staticmethod
    def _rounded_rectangle(
        cr: Any, x: float, y: float, width: float, height: float, radius: float
    ) -> None:
        radius = min(radius, width / 2, height / 2)
        cr.new_sub_path()
        cr.arc(x + width - radius, y + radius, radius, -math.pi / 2, 0)
        cr.arc(x + width - radius, y + height - radius, radius, 0, math.pi / 2)
        cr.arc(x + radius, y + height - radius, radius, math.pi / 2, math.pi)
        cr.arc(x + radius, y + radius, radius, math.pi, 3 * math.pi / 2)
        cr.close_path()

    def _draw(self, widget: Gtk.Widget, cr: Any) -> bool:
        width = widget.get_allocated_width()
        height = widget.get_allocated_height()

        cr.save()
        cr.set_operator(cairo.OPERATOR_CLEAR)
        cr.paint()
        cr.restore()
        cr.set_operator(cairo.OPERATOR_OVER)
        cr.select_font_face("Sans")

        card_x = 6
        card_y = 6
        card_width = max(1, width - 16)
        card_height = max(1, height - 14)

        self._rounded_rectangle(cr, card_x, card_y + 2, card_width, card_height, 14)
        cr.set_source_rgba(0, 0, 0, 0.24)
        cr.fill()

        self._rounded_rectangle(cr, card_x, card_y, card_width, card_height, 14)
        cr.set_source_rgba(0.065, 0.065, 0.07, 0.95)
        cr.fill_preserve()
        cr.set_line_width(1)
        cr.set_source_rgba(1, 1, 1, 0.11)
        cr.stroke()

        if self.status == "Recording":
            self._draw_recording(cr, card_x, card_y, card_width, card_height)
        elif self.status == "Transcribing":
            self._draw_center_text(cr, card_x, card_y, card_width, card_height, "Transcribing")
        elif self.status == "Cleaning up":
            self._draw_center_text(cr, card_x, card_y, card_width, card_height, "Cleaning up...")
        return False

    def _draw_recording(
        self, cr: Any, x: float, y: float, width: float, height: float
    ) -> None:
        timer = self._format_elapsed(self.elapsed_provider())
        cr.select_font_face("Sans", cairo.FONT_SLANT_NORMAL, cairo.FONT_WEIGHT_BOLD)
        cr.set_font_size(13)
        timer_extents = cr.text_extents(timer)
        timer_x = x + width - timer_extents.width - 10
        timer_y = y + (height / 2) + (timer_extents.height / 2)
        cr.set_source_rgba(0.96, 0.96, 0.96, 0.94)
        cr.move_to(timer_x, timer_y)
        cr.show_text(timer)

        wave_x = x + 12
        wave_width = max(1.0, timer_x - wave_x - 8)
        center_y = y + height / 2
        bar_gap = 1.25
        bar_width = max(
            0.8,
            min(2.4, (wave_width - (self.BAR_COUNT - 1) * bar_gap) / self.BAR_COUNT),
        )
        cr.set_line_width(bar_width)
        cr.set_line_cap(cairo.LINE_CAP_ROUND)
        for index, level in enumerate(self._waveform):
            contour = 0.78 + 0.22 * math.sin((index / max(1, self.BAR_COUNT - 1)) * math.pi)
            bar_height = 2.5 + (level ** 0.55) * 26 * contour
            bar_x = wave_x + index * (bar_width + bar_gap)
            alpha = 0.24 + 0.68 * min(1.0, level + 0.10)
            cr.set_source_rgba(0.94, 0.29, 0.12, alpha)
            cr.move_to(bar_x, center_y - bar_height / 2)
            cr.line_to(bar_x, center_y + bar_height / 2)
            cr.stroke()

    def _draw_center_text(
        self, cr: Any, x: float, y: float, width: float, height: float, text: str
    ) -> None:
        cr.select_font_face("Sans", cairo.FONT_SLANT_NORMAL, cairo.FONT_WEIGHT_BOLD)
        cr.set_font_size(13)
        cr.set_source_rgba(0.96, 0.96, 0.96, 0.96)
        extents = cr.text_extents(text)
        text_x = x + (width - extents.width) / 2 - extents.x_bearing
        text_y = y + (height - extents.height) / 2 - extents.y_bearing
        cr.move_to(text_x, text_y)
        cr.show_text(text)

    @staticmethod
    def _format_elapsed(seconds: float) -> str:
        seconds = max(0, int(seconds))
        minutes, secs = divmod(seconds, 60)
        hours, minutes = divmod(minutes, 60)
        if hours:
            return f"{hours}:{minutes:02d}:{secs:02d}"
        return f"{minutes}:{secs:02d}"


class DictationOverlayWindow(Gtk.Window):
    ACTIVE_STATUSES = {"Recording", "Transcribing", "Cleaning up"}

    def __init__(
        self,
        application: Gtk.Application,
        level_provider: Callable[[], float],
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

        self.canvas = DictationOverlayCanvas(level_provider, elapsed_provider)
        self.add(self.canvas)
        self.connect("realize", lambda *_args: self._apply_window_hints())
        self.connect("size-allocate", lambda *_args: self._position())

    @staticmethod
    def _is_wayland_display(display: Gdk.Display | None = None) -> bool:
        display = display or Gdk.Display.get_default()
        return display is not None and "Wayland" in type(display).__name__

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
        area = self._primary_monitor_area()
        width, height = self.get_size()
        if width <= 1:
            width = DictationOverlayCanvas.WIDTH
        if height <= 1:
            height = DictationOverlayCanvas.HEIGHT
        x = area.x + max(0, (area.width - width) // 2)
        y = area.y + max(0, area.height - height - 72)
        self.move(x, y)

    def _primary_monitor_area(self) -> Gdk.Rectangle:
        display = Gdk.Display.get_default()
        monitor = None
        if display is not None:
            monitor = display.get_primary_monitor()
            if monitor is None and self._is_wayland_display(display):
                monitor = self._mutter_primary_monitor(display)
            if monitor is None and display.get_n_monitors() > 0:
                monitor = display.get_monitor(0)
        if monitor is not None:
            area = monitor.get_workarea()
            if display is not None and self._is_wayland_display(display):
                return self._constrain_to_x11_workarea(area)
            return area

        screen = self.get_screen()
        area = Gdk.Rectangle()
        area.x = 0
        area.y = 0
        area.width = screen.get_width()
        area.height = screen.get_height()
        return area

    def _constrain_to_x11_workarea(self, area: Gdk.Rectangle) -> Gdk.Rectangle:
        try:
            result = subprocess.run(
                ["xprop", "-root", "_NET_WORKAREA"],
                check=False,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                text=True,
                timeout=0.2,
            )
        except (OSError, subprocess.TimeoutExpired):
            return area

        values: list[int] = []
        for part in result.stdout.partition("=")[2].replace(",", " ").split():
            try:
                values.append(int(part))
            except ValueError:
                pass
        if len(values) < 4:
            return area

        work_x, work_y, work_width, work_height = values[:4]
        x1 = max(area.x, work_x)
        y1 = max(area.y, work_y)
        x2 = min(area.x + area.width, work_x + work_width)
        y2 = min(area.y + area.height, work_y + work_height)
        if x2 <= x1 or y2 <= y1:
            return area

        constrained = Gdk.Rectangle()
        constrained.x = x1
        constrained.y = y1
        constrained.width = x2 - x1
        constrained.height = y2 - y1
        return constrained

    def _mutter_primary_monitor(self, display: Gdk.Display) -> Gdk.Monitor | None:
        try:
            bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
            result = bus.call_sync(
                "org.gnome.Mutter.DisplayConfig",
                "/org/gnome/Mutter/DisplayConfig",
                "org.gnome.Mutter.DisplayConfig",
                "GetCurrentState",
                None,
                None,
                Gio.DBusCallFlags.NONE,
                500,
                None,
            )
            _serial, _monitors, logical_monitors, _properties = result.unpack()
        except Exception:
            return None

        for logical_monitor in logical_monitors:
            x, y, _scale, _transform, primary, _monitor_specs, _properties = logical_monitor
            if primary:
                return display.get_monitor_at_point(int(x) + 1, int(y) + 1)
        return None

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


class DictationOverlayHelperClient:
    def __init__(
        self,
        waveform_provider: Callable[[], list[float]],
        elapsed_provider: Callable[[], float],
    ) -> None:
        self.waveform_provider = waveform_provider
        self.elapsed_provider = elapsed_provider
        self.status = "Ready"
        self.cleanup_enabled = True
        self.process: subprocess.Popen[str] | None = None
        self._tick_id: int | None = None

    def set_status(self, status: str, cleanup_enabled: bool) -> None:
        self.status = status
        self.cleanup_enabled = cleanup_enabled
        if not self._ensure_process():
            return
        self._send_update()
        if status in DictationOverlayWindow.ACTIVE_STATUSES:
            if self._tick_id is None:
                self._tick_id = GLib.timeout_add(33, self._tick)
        elif self._tick_id is not None:
            GLib.source_remove(self._tick_id)
            self._tick_id = None

    def close(self) -> None:
        if self._tick_id is not None:
            GLib.source_remove(self._tick_id)
            self._tick_id = None
        process = self.process
        self.process = None
        if process is None:
            return
        try:
            if process.stdin is not None:
                process.stdin.close()
        except OSError:
            pass
        try:
            process.terminate()
        except OSError:
            pass

    def _tick(self) -> bool:
        if self.process is None or self.process.poll() is not None:
            self.process = None
            self._tick_id = None
            return False
        if self.status not in DictationOverlayWindow.ACTIVE_STATUSES:
            self._tick_id = None
            return False
        self._send_update()
        return True

    def _ensure_process(self) -> bool:
        if self.process is not None and self.process.poll() is None:
            return True

        command = self._command()
        env = os.environ.copy()
        env["GDK_BACKEND"] = "x11"
        try:
            self.process = subprocess.Popen(
                command,
                stdin=subprocess.PIPE,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                text=True,
                env=env,
            )
        except OSError:
            self.process = None
            return False
        return True

    def _command(self) -> list[str]:
        executable = os.environ.get("AGENTDICTATE_EXEC")
        if executable:
            return [executable, "--overlay-helper"]
        return [sys.executable, "-m", "agentdictate", "--overlay-helper"]

    def _send_update(self) -> None:
        process = self.process
        if process is None or process.stdin is None:
            return
        payload = {
            "status": self.status,
            "cleanup_enabled": self.cleanup_enabled,
            "elapsed": self.elapsed_provider(),
            "waveform": self.waveform_provider(),
        }
        try:
            process.stdin.write(json.dumps(payload, separators=(",", ":")) + "\n")
            process.stdin.flush()
        except (BrokenPipeError, OSError):
            self.process = None


class OverlayHelperState:
    def __init__(self) -> None:
        self.status = "Ready"
        self.cleanup_enabled = True
        self.elapsed = 0.0
        self.waveform = [0.0] * DictationOverlayCanvas.BAR_COUNT

    def waveform_values(self) -> list[float]:
        return self.waveform

    def elapsed_seconds(self) -> float:
        return self.elapsed


def run_overlay_helper() -> int:
    app = Gtk.Application(application_id=f"{APP_DESKTOP_ID}.Overlay")
    app.register(None)
    state = OverlayHelperState()
    window = DictationOverlayWindow(app, state.waveform_values, state.elapsed_seconds)

    def apply_update(payload: dict[str, Any]) -> bool:
        state.status = str(payload.get("status") or "Ready")
        state.cleanup_enabled = bool(payload.get("cleanup_enabled", True))
        try:
            state.elapsed = float(payload.get("elapsed") or 0.0)
        except (TypeError, ValueError):
            state.elapsed = 0.0
        waveform = payload.get("waveform")
        if isinstance(waveform, list):
            values: list[float] = []
            for value in waveform:
                try:
                    values.append(float(value))
                except (TypeError, ValueError):
                    values.append(0.0)
            state.waveform = values
        window.set_status(state.status, state.cleanup_enabled)
        return False

    def reader() -> None:
        for line in sys.stdin:
            try:
                payload = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(payload, dict):
                GLib.idle_add(apply_update, payload)
        GLib.idle_add(Gtk.main_quit)

    threading.Thread(target=reader, daemon=True).start()
    Gtk.main()
    window.destroy()
    return 0
