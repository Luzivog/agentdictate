from __future__ import annotations

import math
from typing import Any

import gi

gi.require_version("Gtk", "3.0")
from gi.repository import Gtk  # noqa: E402


class UsageGraph(Gtk.DrawingArea):
    def __init__(self) -> None:
        super().__init__()
        self.values: list[dict[str, Any]] = []
        self.set_size_request(560, 190)
        self.connect("draw", self._draw)

    def set_values(self, values: list[dict[str, Any]]) -> None:
        self.values = values
        self.queue_draw()

    @staticmethod
    def _rounded_rectangle(cr: Any, x: float, y: float, width: float, height: float, radius: float) -> None:
        radius = min(radius, width / 2, height / 2)
        cr.new_sub_path()
        cr.arc(x + width - radius, y + radius, radius, -math.pi / 2, 0)
        cr.arc(x + width - radius, y + height - radius, radius, 0, math.pi / 2)
        cr.arc(x + radius, y + height - radius, radius, math.pi / 2, math.pi)
        cr.arc(x + radius, y + radius, radius, math.pi, 3 * math.pi / 2)
        cr.close_path()

    @staticmethod
    def _nice_axis_max(value: float) -> float:
        if value <= 0:
            return 1.0
        magnitude = 10 ** math.floor(math.log10(value))
        normalized = value / magnitude
        if normalized <= 1:
            nice = 1
        elif normalized <= 2:
            nice = 2
        elif normalized <= 5:
            nice = 5
        else:
            nice = 10
        return nice * magnitude

    @staticmethod
    def _format_value(value: float) -> str:
        absolute = abs(value)
        if absolute == 0:
            return "0"
        if absolute >= 100:
            return f"{value:.0f}"
        if absolute >= 1:
            return f"{value:.1f}"
        if absolute >= 0.01:
            return f"{value:.2f}"
        return f"{value:.4f}"

    @staticmethod
    def _format_date(value: Any) -> str:
        text = str(value)
        parts = text.split("-")
        if len(parts) == 3 and parts[1].isdigit() and parts[2].isdigit():
            return f"{int(parts[1])}/{int(parts[2])}"
        return text

    def _draw(self, widget: Gtk.Widget, cr: Any) -> bool:
        width = widget.get_allocated_width()
        height = widget.get_allocated_height()
        if width <= 1 or height <= 1:
            return False

        cr.select_font_face("Sans")
        cr.set_font_size(10)
        self._rounded_rectangle(cr, 0.5, 0.5, width - 1, height - 1, 6)
        cr.set_source_rgb(0.105, 0.105, 0.105)
        cr.fill_preserve()
        cr.set_line_width(1)
        cr.set_source_rgb(0.235, 0.235, 0.235)
        cr.stroke()

        top = 26
        right = 18
        bottom = 30
        left = 54
        graph_width = max(1, width - left - right)
        graph_height = max(1, height - top - bottom)

        self._rounded_rectangle(cr, left, top, graph_width, graph_height, 4)
        cr.set_source_rgba(1, 1, 1, 0.025)
        cr.fill()

        numeric_values = [max(0.0, float(item["value"])) for item in self.values]
        max_value = max(numeric_values) if numeric_values else 0.0
        axis_max = self._nice_axis_max(max_value)

        for tick in range(5):
            ratio = tick / 4
            y = top + graph_height * ratio
            tick_value = axis_max * (1 - ratio)
            cr.set_line_width(1)
            cr.set_source_rgba(1, 1, 1, 0.13 if tick == 4 else 0.07)
            cr.move_to(left, y)
            cr.line_to(width - right, y)
            cr.stroke()

            label = self._format_value(tick_value)
            extents = cr.text_extents(label)
            cr.set_source_rgb(0.58, 0.58, 0.58)
            cr.move_to(max(6, left - extents.width - 9), y + 4)
            cr.show_text(label)

        if not self.values or max_value <= 0:
            message = "No usage yet"
            extents = cr.text_extents(message)
            cr.set_source_rgb(0.72, 0.72, 0.72)
            cr.move_to(left + graph_width / 2 - extents.width / 2, top + graph_height / 2)
            cr.show_text(message)
            return False

        max_label = f"Max {self._format_value(max_value)}"
        extents = cr.text_extents(max_label)
        cr.set_source_rgb(0.74, 0.74, 0.74)
        cr.move_to(width - right - extents.width, 16)
        cr.show_text(max_label)

        bar_slot = graph_width / len(self.values)
        bar_width = min(18, max(1.5, bar_slot * 0.62))
        for index, value in enumerate(numeric_values):
            if value <= 0:
                continue
            bar_height = max(2, (value / axis_max) * graph_height)
            x = left + index * bar_slot + (bar_slot - bar_width) / 2
            y = top + graph_height - bar_height
            self._rounded_rectangle(cr, x, y, bar_width, bar_height, min(4, bar_width / 2, bar_height / 2))
            cr.set_source_rgb(0.94, 0.29, 0.12)
            cr.fill()
            cr.rectangle(x, y, bar_width, min(3, bar_height))
            cr.set_source_rgba(1, 1, 1, 0.16)
            cr.fill()

        first_label = self._format_date(self.values[0].get("date", ""))
        last_label = self._format_date(self.values[-1].get("date", ""))
        cr.set_source_rgb(0.58, 0.58, 0.58)
        if first_label == last_label:
            extents = cr.text_extents(first_label)
            cr.move_to(left + graph_width / 2 - extents.width / 2, height - 10)
            cr.show_text(first_label)
        else:
            cr.move_to(left, height - 10)
            cr.show_text(first_label)
            extents = cr.text_extents(last_label)
            cr.move_to(width - right - extents.width, height - 10)
            cr.show_text(last_label)
        return False
