from __future__ import annotations

import gi

gi.require_version("Gdk", "3.0")
gi.require_version("Gtk", "3.0")
from gi.repository import Gdk, Gio, GLib, Gtk  # noqa: E402

try:
    gi.require_version("AyatanaAppIndicator3", "0.1")
    from gi.repository import AyatanaAppIndicator3 as AppIndicator  # type: ignore[attr-defined]  # noqa: E402
except (ImportError, ValueError):
    try:
        gi.require_version("AppIndicator3", "0.1")
        from gi.repository import AppIndicator3 as AppIndicator  # type: ignore[attr-defined]  # noqa: E402
    except (ImportError, ValueError):
        AppIndicator = None  # type: ignore[assignment]

__all__ = ["AppIndicator", "Gdk", "Gio", "GLib", "Gtk"]
