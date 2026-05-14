from __future__ import annotations

import subprocess

from .gtk import Gdk, Gio, Gtk


def is_wayland_display(display: Gdk.Display | None = None) -> bool:
    display = display or Gdk.Display.get_default()
    return display is not None and "Wayland" in type(display).__name__


def primary_monitor_area(window: Gtk.Window) -> Gdk.Rectangle:
    display = Gdk.Display.get_default()
    monitor = None
    if display is not None:
        monitor = display.get_primary_monitor()
        if monitor is None and is_wayland_display(display):
            monitor = mutter_primary_monitor(display)
        if monitor is None and display.get_n_monitors() > 0:
            monitor = display.get_monitor(0)
    if monitor is not None:
        area = monitor.get_workarea()
        if display is not None and is_wayland_display(display):
            return constrain_to_x11_workarea(area)
        return area

    screen = window.get_screen()
    area = Gdk.Rectangle()
    area.x = 0
    area.y = 0
    area.width = screen.get_width()
    area.height = screen.get_height()
    return area


def constrain_to_x11_workarea(area: Gdk.Rectangle) -> Gdk.Rectangle:
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


def mutter_primary_monitor(display: Gdk.Display) -> Gdk.Monitor | None:
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
