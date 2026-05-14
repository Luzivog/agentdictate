from __future__ import annotations

import subprocess
from pathlib import Path

from agentdictate.paths import APP_NAME

from .gtk import GLib, Gtk


class StatusMixin:
    def _controller_status(self, status: str) -> None:
        GLib.idle_add(self._set_status_ui, status)

    def _controller_message(self, title: str, body: str) -> None:
        GLib.idle_add(self._set_message, title, body)

    def _controller_refresh(self) -> None:
        GLib.idle_add(self.refresh_all)

    def _set_status_ui(self, status: str) -> bool:
        if self.status_label:
            self.status_label.set_text(f"Status: {status}")
        if self.status_icon:
            self.status_icon.set_tooltip_text(f"{APP_NAME}: {status}")
            self.status_icon.set_from_icon_name("agentdictate")
        if self.app_indicator is not None:
            if hasattr(self.app_indicator, "set_icon_full"):
                self.app_indicator.set_icon_full("agentdictate", f"{APP_NAME}: {status}")
            elif hasattr(self.app_indicator, "set_icon"):
                self.app_indicator.set_icon("agentdictate")
        if self.overlay:
            self.overlay.set_status(status, self.settings.cleanup_enabled)
        if self.overlay_helper:
            self.overlay_helper.set_status(status, self.settings.cleanup_enabled)
        self.refresh_overview()
        return False

    def _set_message(self, title: str, body: str) -> bool:
        if self.message_label:
            text = title if not body else f"{title} {body}"
            self.message_label.set_text(text)
        return False

    def _dialog(self, title: str, message: str, error: bool = False) -> None:
        dialog = Gtk.MessageDialog(
            transient_for=self.window,
            flags=0,
            message_type=Gtk.MessageType.ERROR if error else Gtk.MessageType.INFO,
            buttons=Gtk.ButtonsType.OK,
            text=title,
        )
        dialog.format_secondary_text(message)
        dialog.run()
        dialog.destroy()

    def _open_path(self, path: Path) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        if path.suffix and not path.exists():
            path.touch()
        subprocess.run(["xdg-open", str(path)], check=False)

    def quit(self) -> None:  # type: ignore[override]
        self.controller.close()
        if self.overlay:
            self.overlay.destroy()
            self.overlay = None
        if self.overlay_helper:
            self.overlay_helper.close()
            self.overlay_helper = None
        if self._held:
            self.release()
            self._held = False
        super().quit()
