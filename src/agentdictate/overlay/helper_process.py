from __future__ import annotations

import json
import sys
import threading
from typing import Any

from agentdictate.paths import APP_DESKTOP_ID

from .canvas import DictationOverlayCanvas
from .gtk import GLib, Gtk
from .window import DictationOverlayWindow


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

    def apply(self, payload: dict[str, Any]) -> bool:
        next_status = str(payload.get("status") or "Ready")
        status_changed = next_status != self.status
        self.status = next_status
        self.cleanup_enabled = bool(payload.get("cleanup_enabled", True))
        try:
            self.elapsed = float(payload.get("elapsed") or 0.0)
        except (TypeError, ValueError):
            self.elapsed = 0.0
        waveform = payload.get("waveform")
        if isinstance(waveform, list):
            values: list[float] = []
            for value in waveform:
                try:
                    values.append(float(value))
                except (TypeError, ValueError):
                    values.append(0.0)
            self.waveform = values
        return status_changed


def run_overlay_helper() -> int:
    app = Gtk.Application(application_id=f"{APP_DESKTOP_ID}.Overlay")
    app.register(None)
    state = OverlayHelperState()
    window = DictationOverlayWindow(app, state.waveform_values, state.elapsed_seconds)

    def apply_update(payload: dict[str, Any]) -> bool:
        if state.apply(payload):
            window.set_status(state.status, state.cleanup_enabled)
        else:
            window.update_frame(state.status, state.cleanup_enabled)
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
