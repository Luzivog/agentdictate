from __future__ import annotations

import signal
from typing import Any

from agentdictate.config import load_settings
from agentdictate.controller import AgentDictateController
from agentdictate.overlay import DictationOverlayHelperClient, DictationOverlayWindow
from agentdictate.paths import APP_DESKTOP_ID
from agentdictate.widgets import UsageGraph

from .base_widgets import UiWidgetMixin
from .gtk import Gio, GLib, Gtk
from .history import HistoryMixin
from .replacements import ReplacementsMixin
from .settings_actions import SettingsActionsMixin
from .settings_form import SettingsFormMixin
from .stats import StatsMixin
from .status import StatusMixin
from .tabs_cleanup import CleanupTabMixin
from .tabs_data import DataTabsMixin
from .tabs_main import MainTabsMixin
from .tabs_stats_advanced import StatsAdvancedTabsMixin
from .tray import TrayMixin
from .window import WindowMixin


class AgentDictateGtkApp(
    Gtk.Application,
    TrayMixin,
    WindowMixin,
    MainTabsMixin,
    CleanupTabMixin,
    DataTabsMixin,
    StatsAdvancedTabsMixin,
    UiWidgetMixin,
    SettingsFormMixin,
    SettingsActionsMixin,
    ReplacementsMixin,
    HistoryMixin,
    StatsMixin,
    StatusMixin,
):
    def __init__(self, background: bool = False) -> None:
        super().__init__(
            application_id=APP_DESKTOP_ID,
            flags=Gio.ApplicationFlags.HANDLES_COMMAND_LINE,
        )
        Gtk.Window.set_default_icon_name("agentdictate")
        self.background = background
        self._held = False
        self._activated_once = False
        self.window: Gtk.ApplicationWindow | None = None
        self.notebook: Gtk.Notebook | None = None
        self.status_icon: Gtk.StatusIcon | None = None
        self.app_indicator: Any | None = None
        self.tray_menu: Gtk.Menu | None = None
        self.overlay: DictationOverlayWindow | None = None
        self.overlay_helper: DictationOverlayHelperClient | None = None
        self._settings_window_iconified = False
        self._last_window_page: int | None = None
        self.status_label: Gtk.Label | None = None
        self.message_label: Gtk.Label | None = None
        self.settings = load_settings()
        self.controller = AgentDictateController(
            status_callback=self._controller_status,
            message_callback=self._controller_message,
            refresh_callback=self._controller_refresh,
            settings=self.settings,
        )
        self._termination_sources = [
            GLib.unix_signal_add(
                GLib.PRIORITY_DEFAULT,
                signal_number,
                self._handle_termination,
            )
            for signal_number in (signal.SIGINT, signal.SIGTERM)
        ]
        self.graph = UsageGraph()
        self.history_rows: dict[int, Any] = {}
        self.selected_history_id: int | None = None
        self.recovery_rows: dict[int, Any] = {}
        self.selected_recovery_id: int | None = None
        self.selected_mapping_id: int | None = None
        self.cleanup_price_entries: dict[str, tuple[Gtk.Entry, Gtk.Entry]] = {}
        self.transcription_price_entries: dict[str, Gtk.Entry] = {}

    def _handle_termination(self) -> bool:
        self.quit()
        return GLib.SOURCE_REMOVE

    def do_command_line(self, command_line: Gio.ApplicationCommandLine) -> int:
        args = list(command_line.get_arguments()[1:])
        background = "--background" in args
        force_show = "--show" in args or not background
        if background:
            self.background = True
        self._activate_agentdictate(force_show=force_show)
        return 0

    def do_activate(self) -> None:
        self._activate_agentdictate(force_show=True)

    def _activate_agentdictate(self, force_show: bool) -> None:
        self._activated_once = True
        if not self._held:
            self.hold()
            self._held = True
        if self.overlay is None and self.overlay_helper is None:
            if DictationOverlayWindow._is_wayland_display():
                self.overlay_helper = DictationOverlayHelperClient(
                    self.controller.recording_waveform,
                    self.controller.recording_elapsed_seconds,
                )
            else:
                self.overlay = DictationOverlayWindow(
                    self,
                    self.controller.recording_waveform,
                    self.controller.recording_elapsed_seconds,
                )
        self._ensure_tray()
        self.controller.start_hotkey()
        if force_show or self.settings.launch_window_on_startup:
            self.show_window()
