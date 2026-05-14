from __future__ import annotations

import math
from typing import Any, Callable

import cairo

from .gtk import GLib, Gtk


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
            values = self._fit_waveform(self.waveform_provider(), self.BAR_COUNT)
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
