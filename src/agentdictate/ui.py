from __future__ import annotations

import ctypes
import ctypes.util
import subprocess
from pathlib import Path
from typing import Any, Callable

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

from .clipboard import ClipboardPaste
from .config import (
    CLEANUP_MODELS,
    CLEANUP_REASONING_EFFORTS,
    CLEANUP_STYLES,
    CUSTOM_LANGUAGE_VALUE,
    HISTORY_WARNING,
    PLAIN_KEY_WARNING,
    PRICING_DISCLAIMER,
    RECORDING_MODES,
    TRANSCRIPTION_LANGUAGES,
    TRANSCRIPTION_MODELS,
    Settings,
    load_settings,
    reset_pricing_defaults,
)
from .controller import AgentDictateController
from .costs import format_cost, format_duration
from .paths import APP_DESKTOP_ID, APP_NAME, cache_dir, config_path, database_path, logs_dir
from .overlay import DictationOverlayHelperClient, DictationOverlayWindow
from .widgets import UsageGraph
from .replacements import ReplacementMapping, apply_replacements


class CtypesAppIndicator:
    CATEGORY_APPLICATION_STATUS = 0
    STATUS_ACTIVE = 1

    def __init__(self, indicator_id: str, icon_name: str) -> None:
        library_path = (
            ctypes.util.find_library("ayatana-appindicator3")
            or ctypes.util.find_library("appindicator3")
            or "libappindicator3.so.1"
        )
        self._lib = ctypes.CDLL(library_path)
        self._lib.app_indicator_new.argtypes = [
            ctypes.c_char_p,
            ctypes.c_char_p,
            ctypes.c_int,
        ]
        self._lib.app_indicator_new.restype = ctypes.c_void_p
        self._lib.app_indicator_set_status.argtypes = [ctypes.c_void_p, ctypes.c_int]
        self._lib.app_indicator_set_menu.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
        if hasattr(self._lib, "app_indicator_set_icon_full"):
            self._lib.app_indicator_set_icon_full.argtypes = [
                ctypes.c_void_p,
                ctypes.c_char_p,
                ctypes.c_char_p,
            ]
        if hasattr(self._lib, "app_indicator_set_icon"):
            self._lib.app_indicator_set_icon.argtypes = [
                ctypes.c_void_p,
                ctypes.c_char_p,
            ]
        self._indicator = self._lib.app_indicator_new(
            indicator_id.encode("utf-8"),
            icon_name.encode("utf-8"),
            self.CATEGORY_APPLICATION_STATUS,
        )
        if not self._indicator:
            raise RuntimeError("Could not create AppIndicator")
        self._menu: Gtk.Menu | None = None

    def set_status_active(self) -> None:
        self._lib.app_indicator_set_status(self._indicator, self.STATUS_ACTIVE)

    def set_menu(self, menu: Gtk.Menu) -> None:
        self._menu = menu
        self._lib.app_indicator_set_menu(self._indicator, ctypes.c_void_p(hash(menu)))

    def set_icon_full(self, icon_name: str, description: str) -> None:
        if hasattr(self._lib, "app_indicator_set_icon_full"):
            self._lib.app_indicator_set_icon_full(
                self._indicator,
                icon_name.encode("utf-8"),
                description.encode("utf-8"),
            )
        else:
            self.set_icon(icon_name)

    def set_icon(self, icon_name: str) -> None:
        if hasattr(self._lib, "app_indicator_set_icon"):
            self._lib.app_indicator_set_icon(
                self._indicator,
                icon_name.encode("utf-8"),
            )


class AgentDictateGtkApp(Gtk.Application):
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
        self.graph = UsageGraph()
        self.history_rows: dict[int, Any] = {}
        self.selected_history_id: int | None = None
        self.selected_mapping_id: int | None = None
        self.cleanup_price_entries: dict[str, tuple[Gtk.Entry, Gtk.Entry]] = {}
        self.transcription_price_entries: dict[str, Gtk.Entry] = {}

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

    def _ensure_tray(self) -> None:
        if self.status_icon or self.app_indicator or not self.settings.show_tray_icon:
            return
        if AppIndicator is not None:
            indicator = AppIndicator.Indicator.new(
                APP_DESKTOP_ID,
                "agentdictate",
                AppIndicator.IndicatorCategory.APPLICATION_STATUS,
            )
            indicator.set_status(AppIndicator.IndicatorStatus.ACTIVE)
            if hasattr(indicator, "set_title"):
                indicator.set_title(APP_NAME)
            self.tray_menu = self._build_tray_menu()
            indicator.set_menu(self.tray_menu)
            self.app_indicator = indicator
            return
        try:
            indicator = CtypesAppIndicator(APP_DESKTOP_ID, "agentdictate")
            self.tray_menu = self._build_tray_menu()
            indicator.set_menu(self.tray_menu)
            indicator.set_status_active()
            self.app_indicator = indicator
            return
        except Exception:
            self.app_indicator = None
        icon = Gtk.StatusIcon.new_from_icon_name("agentdictate")
        icon.set_visible(True)
        icon.set_tooltip_text(f"{APP_NAME}: Ready")
        icon.connect("activate", lambda *_args: self.show_window())
        icon.connect("popup-menu", self._tray_popup)
        self.status_icon = icon

    def _build_tray_menu(self) -> Gtk.Menu:
        menu = Gtk.Menu()
        items = [
            ("Show AgentDictate", self.show_window),
            ("Start Recording", self.controller.start_recording),
            ("Stop Recording", self.controller.stop_recording),
            ("Enable/Disable Dictation", self._toggle_dictation),
            ("Copy Last Transcript", self._copy_last_transcript),
            ("Open History", lambda: self.show_window(tab=5)),
            ("Open Stats", lambda: self.show_window(tab=6)),
            ("Open Settings", self.show_window),
            ("View Logs", lambda: self._open_path(logs_dir())),
            ("Quit", self.quit),
        ]
        for label, callback in items:
            item = Gtk.MenuItem(label=label)
            item.connect("activate", lambda _item, cb=callback: self._run_menu_action(cb))
            menu.append(item)
        menu.show_all()
        return menu

    def _run_menu_action(self, callback: Callable[[], Any]) -> None:
        callback()

    def _tray_popup(self, _icon: Gtk.StatusIcon, button: int, activate_time: int) -> None:
        menu = self._build_tray_menu()
        menu.popup(None, None, None, None, button, activate_time)

    def _toggle_dictation(self) -> None:
        if self.controller.hotkey_listener:
            self.controller.stop_hotkey()
            self._set_status_ui("Disabled")
        else:
            self.controller.start_hotkey()
            self._set_status_ui("Ready")

    def _copy_last_transcript(self) -> None:
        rows = self.controller.storage.list_history(limit=1)
        if not rows:
            self._set_message("No transcript history yet.", "")
            return
        text = str(rows[0]["final_text"] or "")
        ClipboardPaste().copy(text)
        self._set_message("Last transcript copied.", "")

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
        if window is None:
            return False
        gdk_window = window.get_window()
        if gdk_window is None:
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

        header = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        outer.pack_start(header, False, False, 0)
        self.status_label = Gtk.Label(label="Status: Ready")
        self.status_label.set_xalign(0)
        header.pack_start(self.status_label, True, True, 0)
        self.message_label = Gtk.Label(label="")
        self.message_label.set_xalign(1)
        header.pack_start(self.message_label, True, True, 0)

        notebook = Gtk.Notebook()
        self.notebook = notebook
        outer.pack_start(notebook, True, True, 0)

        notebook.append_page(self._overview_tab(), Gtk.Label(label="Overview"))
        notebook.append_page(self._openai_tab(), Gtk.Label(label="OpenAI"))
        notebook.append_page(self._dictation_tab(), Gtk.Label(label="Dictation"))
        notebook.append_page(self._cleanup_tab(), Gtk.Label(label="Cleanup"))
        notebook.append_page(self._replacements_tab(), Gtk.Label(label="Replacements"))
        notebook.append_page(self._history_tab(), Gtk.Label(label="History"))
        notebook.append_page(self._stats_tab(), Gtk.Label(label="Stats"))
        notebook.append_page(self._advanced_tab(), Gtk.Label(label="Advanced"))

        self._sync_ui_from_settings()
        return window

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

    def _overview_tab(self) -> Gtk.Widget:
        box = self._tab_box()
        self.overview_status = self._value_label(box, "Status")
        self.overview_hotkey = self._value_label(box, "Hotkey")
        self.overview_transcription = self._value_label(box, "Transcription")
        self.overview_cleanup = self._value_label(box, "Cleanup")
        self.overview_last = self._value_label(box, "Last transcript")
        self.overview_today = self._value_label(box, "Today")
        actions = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        test_button = Gtk.Button(label="Test recording")
        test_button.connect("clicked", self._toggle_test_recording)
        actions.pack_start(test_button, False, False, 0)
        history_button = Gtk.Button(label="Open history")
        history_button.connect("clicked", lambda *_args: self.show_window(tab=5))
        actions.pack_start(history_button, False, False, 0)
        box.pack_start(actions, False, False, 0)
        return box

    def _openai_tab(self) -> Gtk.Widget:
        box = self._tab_box()
        box.pack_start(self._warning_label(PLAIN_KEY_WARNING), False, False, 0)
        grid = self._grid()
        box.pack_start(grid, False, False, 0)
        self.api_key_entry = Gtk.Entry()
        self.api_key_entry.set_visibility(False)
        self.api_key_entry.set_input_purpose(Gtk.InputPurpose.PASSWORD)
        self._grid_attach(grid, "API key", self.api_key_entry, 0)
        buttons = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        show_button = Gtk.ToggleButton(label="Show")
        show_button.connect(
            "toggled",
            lambda button: self.api_key_entry.set_visibility(button.get_active()),
        )
        save_button = Gtk.Button(label="Save key")
        save_button.connect("clicked", self._save_from_ui)
        clear_button = Gtk.Button(label="Clear key")
        clear_button.connect("clicked", self._clear_key)
        test_button = Gtk.Button(label="Test key")
        test_button.connect("clicked", self._test_key)
        for button in (show_button, save_button, clear_button, test_button):
            buttons.pack_start(button, False, False, 0)
        box.pack_start(buttons, False, False, 0)

        self.transcription_combo = self._combo(TRANSCRIPTION_MODELS)
        self._grid_attach(grid, "Transcription model", self.transcription_combo, 1)
        self.custom_transcription_entry = Gtk.Entry()
        self._grid_attach(grid, "Custom transcription model", self.custom_transcription_entry, 2)
        self.language_combo = self._combo([label for label, _code in TRANSCRIPTION_LANGUAGES])
        self.language_combo.connect("changed", lambda *_args: self._update_language_custom_enabled())
        self._grid_attach(grid, "Language", self.language_combo, 3)
        self.language_entry = Gtk.Entry()
        self.language_entry.set_placeholder_text("ISO-639-1 code, for example en")
        self._grid_attach(grid, "Custom language code", self.language_entry, 4)
        box.pack_start(Gtk.Label(label="Transcription prompt"), False, False, 0)
        self.transcription_prompt_view = self._text_view(height=72)
        box.pack_start(self.transcription_prompt_view, False, False, 0)
        self._save_button_row(box)
        return box

    def _dictation_tab(self) -> Gtk.Widget:
        box = self._tab_box()
        grid = self._grid()
        box.pack_start(grid, False, False, 0)
        self.hotkey_entry = Gtk.Entry()
        self._grid_attach(grid, "Current hotkey", self.hotkey_entry, 0)
        change_button = Gtk.Button(label="Change hotkey")
        change_button.connect("clicked", self._save_from_ui)
        reset_button = Gtk.Button(label="Reset to Ctrl+Space")
        reset_button.connect("clicked", self._reset_hotkey)
        button_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        button_row.pack_start(change_button, False, False, 0)
        button_row.pack_start(reset_button, False, False, 0)
        grid.attach(button_row, 1, 1, 1, 1)
        self.recording_mode_combo = self._combo(RECORDING_MODES)
        self._grid_attach(grid, "Recording mode", self.recording_mode_combo, 2)
        self.max_duration_spin = Gtk.SpinButton()
        self.max_duration_spin.set_range(10, 3600)
        self.max_duration_spin.set_increments(10, 60)
        self._grid_attach(grid, "Max recording seconds", self.max_duration_spin, 3)
        self.sound_switch = Gtk.Switch()
        self._grid_attach(grid, "Sound feedback", self.sound_switch, 4)
        self.start_sound_switch = Gtk.Switch()
        self._grid_attach(grid, "Start sound", self.start_sound_switch, 5)
        self.stop_sound_switch = Gtk.Switch()
        self._grid_attach(grid, "Stop sound", self.stop_sound_switch, 6)
        self.audio_ducking_switch = Gtk.Switch()
        self._grid_attach(grid, "Fade playback while recording", self.audio_ducking_switch, 7)
        self.audio_ducking_volume_spin = Gtk.SpinButton()
        self.audio_ducking_volume_spin.set_range(0, 100)
        self.audio_ducking_volume_spin.set_increments(1, 5)
        self._grid_attach(grid, "Ducked playback volume (%)", self.audio_ducking_volume_spin, 8)
        self.audio_ducking_fade_spin = Gtk.SpinButton()
        self.audio_ducking_fade_spin.set_range(0, 5000)
        self.audio_ducking_fade_spin.set_increments(100, 500)
        self._grid_attach(grid, "Fade duration (ms)", self.audio_ducking_fade_spin, 9)
        self._save_button_row(box)
        return box

    def _cleanup_tab(self) -> Gtk.Widget:
        box = self._tab_box()
        grid = self._grid()
        box.pack_start(grid, False, False, 0)
        self.cleanup_switch = Gtk.Switch()
        self.cleanup_switch.connect("notify::active", lambda *_args: self._update_cleanup_enabled())
        self._grid_attach(grid, "Cleanup mode", self.cleanup_switch, 0)
        self.cleanup_model_combo = self._combo(CLEANUP_MODELS)
        self._grid_attach(grid, "Cleanup model", self.cleanup_model_combo, 1)
        self.custom_cleanup_entry = Gtk.Entry()
        self._grid_attach(grid, "Custom cleanup model", self.custom_cleanup_entry, 2)
        self.cleanup_style_combo = self._combo(CLEANUP_STYLES)
        self._grid_attach(grid, "Cleanup style", self.cleanup_style_combo, 3)
        self.cleanup_reasoning_combo = self._combo(CLEANUP_REASONING_EFFORTS)
        self._grid_attach(grid, "Reasoning effort", self.cleanup_reasoning_combo, 4)
        self.cleanup_cost_preview = Gtk.Label(label="")
        self.cleanup_cost_preview.set_xalign(0)
        self._grid_attach(grid, "Estimated cleanup cost", self.cleanup_cost_preview, 5)
        box.pack_start(Gtk.Label(label="Cleanup prompt"), False, False, 0)
        self.cleanup_prompt_view = self._text_view(height=100)
        box.pack_start(self.cleanup_prompt_view, False, False, 0)
        expander = Gtk.Expander(label="Pricing settings")
        pricing_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        pricing_grid = self._grid()
        pricing_box.pack_start(pricing_grid, False, False, 0)
        row = 0
        pricing_grid.attach(Gtk.Label(label="Transcription model"), 0, row, 1, 1)
        pricing_grid.attach(Gtk.Label(label="Price per audio minute"), 1, row, 1, 1)
        row += 1
        for model in TRANSCRIPTION_MODELS:
            if model == "Custom":
                continue
            label = Gtk.Label(label=model)
            label.set_xalign(0)
            entry = Gtk.Entry()
            self.transcription_price_entries[model] = entry
            pricing_grid.attach(label, 0, row, 1, 1)
            pricing_grid.attach(entry, 1, row, 1, 1)
            row += 1
        pricing_grid.attach(Gtk.Label(label="Cleanup model"), 0, row, 1, 1)
        pricing_grid.attach(Gtk.Label(label="Input / 1M tokens"), 1, row, 1, 1)
        pricing_grid.attach(Gtk.Label(label="Output / 1M tokens"), 2, row, 1, 1)
        row += 1
        for model in CLEANUP_MODELS:
            if model == "Custom":
                continue
            label = Gtk.Label(label=model)
            label.set_xalign(0)
            input_entry = Gtk.Entry()
            output_entry = Gtk.Entry()
            self.cleanup_price_entries[model] = (input_entry, output_entry)
            pricing_grid.attach(label, 0, row, 1, 1)
            pricing_grid.attach(input_entry, 1, row, 1, 1)
            pricing_grid.attach(output_entry, 2, row, 1, 1)
            row += 1
        self.currency_entry = Gtk.Entry()
        self._grid_attach(pricing_grid, "Currency", self.currency_entry, row)
        reset_button = Gtk.Button(label="Reset pricing defaults")
        reset_button.connect("clicked", self._reset_pricing)
        pricing_box.pack_start(reset_button, False, False, 0)
        pricing_box.pack_start(self._warning_label(PRICING_DISCLAIMER), False, False, 0)
        expander.add(pricing_box)
        box.pack_start(expander, False, False, 0)
        self._save_button_row(box)
        return box

    def _replacements_tab(self) -> Gtk.Widget:
        box = self._tab_box()
        search_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        self.replacement_search = Gtk.SearchEntry()
        self.replacement_search.connect("search-changed", lambda *_args: self.refresh_replacements())
        search_row.pack_start(self.replacement_search, True, True, 0)
        add_button = Gtk.Button(label="Add mapping")
        add_button.connect("clicked", self._add_mapping)
        edit_button = Gtk.Button(label="Edit mapping")
        edit_button.connect("clicked", self._edit_mapping)
        delete_button = Gtk.Button(label="Delete mapping")
        delete_button.connect("clicked", self._delete_mapping)
        for button in (add_button, edit_button, delete_button):
            search_row.pack_start(button, False, False, 0)
        box.pack_start(search_row, False, False, 0)

        self.replacements_store = Gtk.ListStore(int, str, str, bool, bool, bool)
        tree = Gtk.TreeView(model=self.replacements_store)
        for index, title in enumerate(
            ["ID", "Source phrase", "Replacement phrase", "Enabled", "Case-sensitive", "Whole-word"]
        ):
            renderer = Gtk.CellRendererText()
            column = Gtk.TreeViewColumn(title, renderer, text=index)
            if index == 0:
                column.set_visible(False)
            tree.append_column(column)
        tree.get_selection().connect("changed", self._mapping_selection_changed)
        box.pack_start(self._scrolled(tree, height=180), True, True, 0)
        self.replacements_empty = Gtk.Label(
            label="No replacements yet. Add words or phrases that should be automatically corrected after transcription."
        )
        self.replacements_empty.set_xalign(0)
        box.pack_start(self.replacements_empty, False, False, 0)

        box.pack_start(Gtk.Label(label="Test replacement preview"), False, False, 0)
        self.preview_input = self._text_view(height=70)
        self.preview_output = self._text_view(height=70, editable=False)
        self._text_buffer(self.preview_input).connect(
            "changed", lambda *_args: self._update_replacement_preview()
        )
        box.pack_start(self.preview_input, False, False, 0)
        box.pack_start(self.preview_output, False, False, 0)
        return box

    def _history_tab(self) -> Gtk.Widget:
        box = self._tab_box()
        box.pack_start(self._warning_label(HISTORY_WARNING), False, False, 0)
        filters = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        self.history_search = Gtk.SearchEntry()
        self.history_search.connect("search-changed", lambda *_args: self.refresh_history())
        self.history_date = Gtk.Entry()
        self.history_date.set_placeholder_text("YYYY-MM-DD")
        self.history_date.connect("changed", lambda *_args: self.refresh_history())
        filters.pack_start(self.history_search, True, True, 0)
        filters.pack_start(self.history_date, False, False, 0)
        box.pack_start(filters, False, False, 0)
        self.history_store = Gtk.ListStore(int, str, str, int, str, str, str, str)
        tree = Gtk.TreeView(model=self.history_store)
        for index, title in enumerate(
            ["ID", "Date", "Final transcript", "Words", "Duration", "Model", "Cleanup", "Cost"]
        ):
            renderer = Gtk.CellRendererText()
            column = Gtk.TreeViewColumn(title, renderer, text=index)
            if index == 0:
                column.set_visible(False)
            if index == 2:
                column.set_expand(True)
            tree.append_column(column)
        tree.get_selection().connect("changed", self._history_selection_changed)
        box.pack_start(self._scrolled(tree, height=190), True, True, 0)
        details = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        copy_raw = Gtk.Button(label="Copy raw")
        copy_raw.connect("clicked", self._copy_selected_raw)
        copy_final = Gtk.Button(label="Copy final")
        copy_final.connect("clicked", self._copy_selected_final)
        delete_item = Gtk.Button(label="Delete item")
        delete_item.connect("clicked", self._delete_selected_history)
        clear_all = Gtk.Button(label="Clear all history")
        clear_all.connect("clicked", self._clear_history)
        for button in (copy_raw, copy_final, delete_item, clear_all):
            details.pack_start(button, False, False, 0)
        box.pack_start(details, False, False, 0)
        self.history_cost_label = Gtk.Label(label="")
        self.history_cost_label.set_xalign(0)
        box.pack_start(self.history_cost_label, False, False, 0)
        box.pack_start(Gtk.Label(label="Raw transcript"), False, False, 0)
        self.history_raw_view = self._text_view(height=70, editable=False)
        box.pack_start(self.history_raw_view, False, False, 0)
        box.pack_start(Gtk.Label(label="Cleaned transcript"), False, False, 0)
        self.history_cleaned_view = self._text_view(height=70, editable=False)
        box.pack_start(self.history_cleaned_view, False, False, 0)
        box.pack_start(Gtk.Label(label="Final transcript"), False, False, 0)
        self.history_final_view = self._text_view(height=70, editable=False)
        box.pack_start(self.history_final_view, False, False, 0)
        return box

    def _stats_tab(self) -> Gtk.Widget:
        box = self._tab_box()
        grid = self._grid()
        box.pack_start(grid, False, False, 0)
        self.stats_labels: dict[str, Gtk.Label] = {}
        labels = [
            ("total_words", "Total words"),
            ("total_audio", "Total audio time"),
            ("average_wpm", "Average WPM"),
            ("total_sessions", "Total sessions"),
            ("average_words", "Average words/session"),
            ("average_duration", "Average duration/session"),
            ("most_transcription", "Most used transcription model"),
            ("most_cleanup", "Most used cleanup model"),
            ("cleanup_usage", "Cleanup mode usage count"),
            ("cost_total", "Estimated total cost"),
            ("cost_transcription", "Estimated transcription cost"),
            ("cost_cleanup", "Estimated cleanup cost"),
            ("today", "Today"),
            ("week", "This week"),
            ("month", "This month"),
        ]
        for row, (key, label) in enumerate(labels):
            self.stats_labels[key] = self._value_label(grid, label, row=row)
        controls = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        self.graph_metric_combo = self._combo(
            ["words", "audio_minutes", "sessions", "estimated_cost", "average_wpm"]
        )
        self.graph_metric_combo.connect("changed", lambda *_args: self.refresh_stats())
        self.graph_range_combo = self._combo(["7", "30", "90", "365"])
        self.graph_range_combo.set_active(1)
        self.graph_range_combo.connect("changed", lambda *_args: self.refresh_stats())
        controls.pack_start(Gtk.Label(label="Graph metric"), False, False, 0)
        controls.pack_start(self.graph_metric_combo, False, False, 0)
        controls.pack_start(Gtk.Label(label="Range"), False, False, 0)
        controls.pack_start(self.graph_range_combo, False, False, 0)
        box.pack_start(controls, False, False, 0)
        box.pack_start(self.graph, False, False, 0)
        return box

    def _advanced_tab(self) -> Gtk.Widget:
        box = self._tab_box()
        grid = self._grid()
        box.pack_start(grid, False, False, 0)
        self.start_on_login_switch = Gtk.Switch()
        self._grid_attach(grid, "Start on login", self.start_on_login_switch, 0)
        self.show_tray_switch = Gtk.Switch()
        self._grid_attach(grid, "Show tray icon", self.show_tray_switch, 1)
        self.minimize_to_tray_switch = Gtk.Switch()
        self._grid_attach(grid, "Minimize to tray on close", self.minimize_to_tray_switch, 2)
        self.launch_window_switch = Gtk.Switch()
        self._grid_attach(grid, "Open window on startup", self.launch_window_switch, 3)
        self.restore_clipboard_switch = Gtk.Switch()
        self._grid_attach(grid, "Restore previous clipboard after paste", self.restore_clipboard_switch, 4)
        self.debug_switch = Gtk.Switch()
        self._grid_attach(grid, "Debug mode", self.debug_switch, 5)
        self.preserve_audio_switch = Gtk.Switch()
        self._grid_attach(grid, "Preserve temporary audio", self.preserve_audio_switch, 6)
        audio_warning = self._warning_label(
            "Temporary audio files may contain sensitive speech. Only enable this for debugging."
        )
        grid.attach(audio_warning, 1, 7, 1, 1)
        buttons = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        for label, path in (
            ("Open config file", config_path()),
            ("Open database folder", database_path().parent),
            ("Open logs", logs_dir()),
            ("Open cache", cache_dir()),
        ):
            button = Gtk.Button(label=label)
            button.connect("clicked", lambda _button, p=path: self._open_path(p))
            buttons.pack_start(button, False, False, 0)
        box.pack_start(buttons, False, False, 0)
        action_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        reset_button = Gtk.Button(label="Reset settings")
        reset_button.connect("clicked", self._reset_settings)
        quit_button = Gtk.Button(label="Quit app")
        quit_button.connect("clicked", lambda *_args: self.quit())
        action_row.pack_start(reset_button, False, False, 0)
        action_row.pack_start(quit_button, False, False, 0)
        box.pack_start(action_row, False, False, 0)
        self._save_button_row(box)
        return box

    def _tab_box(self) -> Gtk.Box:
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        box.set_border_width(8)
        return box

    def _grid(self) -> Gtk.Grid:
        grid = Gtk.Grid(row_spacing=8, column_spacing=10)
        grid.set_column_homogeneous(False)
        return grid

    def _grid_attach(self, grid: Gtk.Grid, label: str, widget: Gtk.Widget, row: int) -> None:
        label_widget = Gtk.Label(label=label)
        label_widget.set_xalign(0)
        label_widget.set_valign(Gtk.Align.CENTER)
        grid.attach(label_widget, 0, row, 1, 1)
        if isinstance(widget, Gtk.Switch):
            widget.set_halign(Gtk.Align.START)
            widget.set_valign(Gtk.Align.CENTER)
            widget.set_hexpand(False)
            widget.set_vexpand(False)
        grid.attach(widget, 1, row, 1, 1)

    def _value_label(
        self, container: Gtk.Container, label: str, row: int | None = None
    ) -> Gtk.Label:
        value = Gtk.Label(label="")
        value.set_xalign(0)
        label_widget = Gtk.Label(label=label)
        label_widget.set_xalign(0)
        if isinstance(container, Gtk.Grid):
            assert row is not None
            container.attach(label_widget, 0, row, 1, 1)
            container.attach(value, 1, row, 1, 1)
        else:
            row_box = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
            row_box.pack_start(label_widget, False, False, 0)
            row_box.pack_start(value, True, True, 0)
            container.pack_start(row_box, False, False, 0)
        return value

    def _combo(self, items: list[str]) -> Gtk.ComboBoxText:
        combo = Gtk.ComboBoxText()
        for item in items:
            combo.append_text(item)
        combo.set_active(0)
        return combo

    def _text_view(self, height: int, editable: bool = True) -> Gtk.ScrolledWindow:
        view = Gtk.TextView()
        view.set_wrap_mode(Gtk.WrapMode.WORD_CHAR)
        view.set_editable(editable)
        view.set_monospace(False)
        scrolled = Gtk.ScrolledWindow()
        scrolled.set_policy(Gtk.PolicyType.AUTOMATIC, Gtk.PolicyType.AUTOMATIC)
        scrolled.set_size_request(-1, height)
        scrolled.add(view)
        scrolled.text_view = view  # type: ignore[attr-defined]
        return scrolled

    def _text_buffer(self, scrolled: Gtk.ScrolledWindow) -> Gtk.TextBuffer:
        view = scrolled.text_view  # type: ignore[attr-defined]
        return view.get_buffer()

    def _set_text(self, scrolled: Gtk.ScrolledWindow, text: str) -> None:
        self._text_buffer(scrolled).set_text(text or "")

    def _get_text(self, scrolled: Gtk.ScrolledWindow) -> str:
        buffer = self._text_buffer(scrolled)
        start, end = buffer.get_bounds()
        return buffer.get_text(start, end, True)

    def _scrolled(self, child: Gtk.Widget, height: int) -> Gtk.ScrolledWindow:
        scrolled = Gtk.ScrolledWindow()
        scrolled.set_policy(Gtk.PolicyType.AUTOMATIC, Gtk.PolicyType.AUTOMATIC)
        scrolled.set_size_request(-1, height)
        scrolled.add(child)
        return scrolled

    def _warning_label(self, text: str) -> Gtk.Label:
        label = Gtk.Label(label=text)
        label.set_line_wrap(True)
        label.set_xalign(0)
        return label

    def _save_button_row(self, box: Gtk.Box) -> None:
        button = Gtk.Button(label="Save settings")
        button.connect("clicked", self._save_from_ui)
        box.pack_end(button, False, False, 0)

    def _sync_ui_from_settings(self) -> None:
        s = self.settings
        self.api_key_entry.set_text(s.openai_api_key)
        self._set_combo_value(self.transcription_combo, s.transcription_model)
        self.custom_transcription_entry.set_text(s.custom_transcription_model)
        self._set_language_selection(s.language)
        self._set_text(self.transcription_prompt_view, s.transcription_prompt)
        self.hotkey_entry.set_text(s.hotkey)
        self._set_combo_value(self.recording_mode_combo, s.recording_mode)
        self.max_duration_spin.set_value(s.max_recording_seconds)
        self.sound_switch.set_active(s.sound_feedback)
        self.start_sound_switch.set_active(s.start_sound)
        self.stop_sound_switch.set_active(s.stop_sound)
        self.audio_ducking_switch.set_active(s.audio_ducking_enabled)
        self.audio_ducking_volume_spin.set_value(s.audio_ducking_volume_percent)
        self.audio_ducking_fade_spin.set_value(s.audio_ducking_fade_ms)
        self.cleanup_switch.set_active(s.cleanup_enabled)
        self._set_combo_value(self.cleanup_model_combo, s.cleanup_model)
        self.custom_cleanup_entry.set_text(s.custom_cleanup_model)
        self._set_combo_value(self.cleanup_style_combo, s.cleanup_style)
        self._set_combo_value(self.cleanup_reasoning_combo, s.cleanup_reasoning_effort)
        self._set_text(self.cleanup_prompt_view, s.cleanup_prompt)
        self.currency_entry.set_text(s.currency)
        for model, entry in self.transcription_price_entries.items():
            entry.set_text(str(s.transcription_prices.get(model, {}).get("price_per_audio_minute", 0.0)))
        for model, (input_entry, output_entry) in self.cleanup_price_entries.items():
            price = s.cleanup_prices.get(model, {})
            input_entry.set_text(str(price.get("input_price_per_1m_tokens", 0.0)))
            output_entry.set_text(str(price.get("output_price_per_1m_tokens", 0.0)))
        self.start_on_login_switch.set_active(s.start_on_login)
        self.show_tray_switch.set_active(s.show_tray_icon)
        self.minimize_to_tray_switch.set_active(s.minimize_to_tray_on_close)
        self.launch_window_switch.set_active(s.launch_window_on_startup)
        self.restore_clipboard_switch.set_active(s.restore_clipboard_after_paste)
        self.debug_switch.set_active(s.debug_mode)
        self.preserve_audio_switch.set_active(s.preserve_temp_audio)
        self._update_cleanup_enabled()
        self.refresh_all()

    def _set_combo_value(self, combo: Gtk.ComboBoxText, value: str) -> None:
        model = combo.get_model()
        for index, row in enumerate(model):
            if row[0] == value:
                combo.set_active(index)
                return
        combo.set_active(0)

    def _combo_value(self, combo: Gtk.ComboBoxText) -> str:
        return combo.get_active_text() or ""

    def _language_code_from_label(self, label: str) -> str:
        for language_label, code in TRANSCRIPTION_LANGUAGES:
            if language_label == label:
                return code
        return ""

    def _set_language_selection(self, language: str) -> None:
        language = language.strip()
        for index, (label, code) in enumerate(TRANSCRIPTION_LANGUAGES):
            if code == language and code != CUSTOM_LANGUAGE_VALUE:
                self.language_combo.set_active(index)
                self.language_entry.set_text("")
                self._update_language_custom_enabled()
                return
        custom_index = next(
            (
                index
                for index, (_label, code) in enumerate(TRANSCRIPTION_LANGUAGES)
                if code == CUSTOM_LANGUAGE_VALUE
            ),
            0,
        )
        self.language_combo.set_active(custom_index if language else 0)
        self.language_entry.set_text(language)
        self._update_language_custom_enabled()

    def _language_from_ui(self) -> str:
        selected_code = self._language_code_from_label(self._combo_value(self.language_combo))
        if selected_code == CUSTOM_LANGUAGE_VALUE:
            return self.language_entry.get_text().strip()
        return selected_code

    def _update_language_custom_enabled(self) -> None:
        selected_code = self._language_code_from_label(self._combo_value(self.language_combo))
        self.language_entry.set_sensitive(selected_code == CUSTOM_LANGUAGE_VALUE)

    def _settings_from_ui(self) -> Settings:
        s = self.settings
        s.openai_api_key = self.api_key_entry.get_text()
        s.transcription_model = self._combo_value(self.transcription_combo)
        s.custom_transcription_model = self.custom_transcription_entry.get_text()
        s.language = self._language_from_ui()
        s.transcription_prompt = self._get_text(self.transcription_prompt_view)
        s.hotkey = self.hotkey_entry.get_text() or "Ctrl+Space"
        s.recording_mode = self._combo_value(self.recording_mode_combo) or "toggle"
        s.max_recording_seconds = int(self.max_duration_spin.get_value())
        s.sound_feedback = self.sound_switch.get_active()
        s.start_sound = self.start_sound_switch.get_active()
        s.stop_sound = self.stop_sound_switch.get_active()
        s.audio_ducking_enabled = self.audio_ducking_switch.get_active()
        s.audio_ducking_volume_percent = int(self.audio_ducking_volume_spin.get_value())
        s.audio_ducking_fade_ms = int(self.audio_ducking_fade_spin.get_value())
        s.cleanup_enabled = self.cleanup_switch.get_active()
        s.cleanup_model = self._combo_value(self.cleanup_model_combo)
        s.custom_cleanup_model = self.custom_cleanup_entry.get_text()
        s.cleanup_style = self._combo_value(self.cleanup_style_combo)
        s.cleanup_reasoning_effort = self._combo_value(self.cleanup_reasoning_combo) or "default"
        s.cleanup_prompt = self._get_text(self.cleanup_prompt_view)
        s.currency = self.currency_entry.get_text() or "USD"
        for model, entry in self.transcription_price_entries.items():
            s.transcription_prices.setdefault(model, {"model_name": model})
            s.transcription_prices[model]["price_per_audio_minute"] = self._float_entry(entry)
            s.transcription_prices[model]["currency"] = s.currency
        for model, (input_entry, output_entry) in self.cleanup_price_entries.items():
            s.cleanup_prices.setdefault(model, {"model_name": model})
            s.cleanup_prices[model]["input_price_per_1m_tokens"] = self._float_entry(input_entry)
            s.cleanup_prices[model]["output_price_per_1m_tokens"] = self._float_entry(output_entry)
            s.cleanup_prices[model]["currency"] = s.currency
        s.start_on_login = self.start_on_login_switch.get_active()
        s.show_tray_icon = self.show_tray_switch.get_active()
        s.minimize_to_tray_on_close = self.minimize_to_tray_switch.get_active()
        s.launch_window_on_startup = self.launch_window_switch.get_active()
        s.restore_clipboard_after_paste = self.restore_clipboard_switch.get_active()
        s.debug_mode = self.debug_switch.get_active()
        s.preserve_temp_audio = self.preserve_audio_switch.get_active()
        return s

    def _float_entry(self, entry: Gtk.Entry) -> float:
        try:
            return float(entry.get_text())
        except ValueError:
            return 0.0

    def _save_from_ui(self, *_args: Any) -> None:
        self.settings = self._settings_from_ui()
        self.controller.update_settings(self.settings)
        self._set_message("Settings saved.", "")
        self._update_cleanup_enabled()

    def _clear_key(self, *_args: Any) -> None:
        self.api_key_entry.set_text("")
        self._save_from_ui()

    def _test_key(self, *_args: Any) -> None:
        self._save_from_ui()
        ok, message = self.controller.test_api_key()
        self._dialog("API key", message, error=not ok)
        if ok and not self.start_on_login_switch.get_active():
            dialog = Gtk.MessageDialog(
                transient_for=self.window,
                flags=0,
                message_type=Gtk.MessageType.QUESTION,
                buttons=Gtk.ButtonsType.YES_NO,
                text="Start AgentDictate when you log in?",
            )
            response = dialog.run()
            dialog.destroy()
            if response == Gtk.ResponseType.YES:
                self.start_on_login_switch.set_active(True)
                self._save_from_ui()

    def _reset_hotkey(self, *_args: Any) -> None:
        self.hotkey_entry.set_text("Ctrl+Space")
        self._save_from_ui()

    def _reset_pricing(self, *_args: Any) -> None:
        reset_pricing_defaults(self.settings)
        self._sync_ui_from_settings()
        self._save_from_ui()

    def _reset_settings(self, *_args: Any) -> None:
        dialog = Gtk.MessageDialog(
            transient_for=self.window,
            flags=0,
            message_type=Gtk.MessageType.WARNING,
            buttons=Gtk.ButtonsType.OK_CANCEL,
            text="Reset settings?",
        )
        response = dialog.run()
        dialog.destroy()
        if response != Gtk.ResponseType.OK:
            return
        self.settings = Settings()
        self.controller.update_settings(self.settings)
        self._sync_ui_from_settings()

    def _toggle_test_recording(self, *_args: Any) -> None:
        if self.controller.status == "Recording":
            self.controller.stop_recording()
        else:
            self._save_from_ui()
            self.controller.start_recording()

    def _update_cleanup_enabled(self) -> None:
        active = self.cleanup_switch.get_active()
        for widget in (
            self.cleanup_model_combo,
            self.custom_cleanup_entry,
            self.cleanup_style_combo,
            self.cleanup_reasoning_combo,
            self.cleanup_prompt_view,
        ):
            widget.set_sensitive(active)
        self._update_cleanup_preview()

    def _update_cleanup_preview(self) -> None:
        raw = "Update the onboarding flow and add tests."
        cleaned = raw
        settings = self._settings_from_ui() if hasattr(self, "cleanup_switch") else self.settings
        price = settings.cleanup_price()
        from .costs import estimate_cleanup_cost

        if settings.cleanup_enabled:
            cost, _input_tokens, _output_tokens = estimate_cleanup_cost(
                raw, cleaned, price.input_price_per_1m_tokens, price.output_price_per_1m_tokens
            )
        else:
            cost = 0.0
        self.cleanup_cost_preview.set_text(
            f"Approximately {format_cost(cost, settings.currency)} for a short prompt preview"
        )

    def _add_mapping(self, *_args: Any) -> None:
        mapping = self._mapping_dialog(None)
        if mapping:
            self.controller.storage.add_mapping(mapping)
            self.refresh_replacements()

    def _edit_mapping(self, *_args: Any) -> None:
        if self.selected_mapping_id is None:
            return
        existing = next(
            (m for m in self.controller.storage.list_mappings() if m.id == self.selected_mapping_id),
            None,
        )
        if not existing:
            return
        mapping = self._mapping_dialog(existing)
        if mapping:
            mapping.id = existing.id
            self.controller.storage.update_mapping(mapping)
            self.refresh_replacements()

    def _delete_mapping(self, *_args: Any) -> None:
        if self.selected_mapping_id is None:
            return
        self.controller.storage.delete_mapping(self.selected_mapping_id)
        self.selected_mapping_id = None
        self.refresh_replacements()

    def _mapping_dialog(
        self, existing: ReplacementMapping | None
    ) -> ReplacementMapping | None:
        dialog = Gtk.Dialog(
            title="Replacement mapping",
            transient_for=self.window,
            flags=0,
            buttons=(Gtk.STOCK_CANCEL, Gtk.ResponseType.CANCEL, Gtk.STOCK_OK, Gtk.ResponseType.OK),
        )
        content = dialog.get_content_area()
        grid = self._grid()
        content.add(grid)
        source = Gtk.Entry()
        replacement = Gtk.Entry()
        enabled = Gtk.Switch()
        case_sensitive = Gtk.Switch()
        whole_word = Gtk.Switch()
        self._grid_attach(grid, "Source phrase", source, 0)
        self._grid_attach(grid, "Replacement phrase", replacement, 1)
        self._grid_attach(grid, "Enabled", enabled, 2)
        self._grid_attach(grid, "Case-sensitive", case_sensitive, 3)
        self._grid_attach(grid, "Whole-word-only", whole_word, 4)
        if existing:
            source.set_text(existing.source_phrase)
            replacement.set_text(existing.replacement_phrase)
            enabled.set_active(existing.enabled)
            case_sensitive.set_active(existing.case_sensitive)
            whole_word.set_active(existing.whole_word_only)
        else:
            enabled.set_active(True)
            whole_word.set_active(True)
        dialog.show_all()
        response = dialog.run()
        dialog.destroy()
        if response != Gtk.ResponseType.OK:
            return None
        return ReplacementMapping.new(
            source_phrase=source.get_text(),
            replacement_phrase=replacement.get_text(),
            enabled=enabled.get_active(),
            case_sensitive=case_sensitive.get_active(),
            whole_word_only=whole_word.get_active(),
        )

    def _mapping_selection_changed(self, selection: Gtk.TreeSelection) -> None:
        model, iterator = selection.get_selected()
        self.selected_mapping_id = int(model[iterator][0]) if iterator else None

    def refresh_replacements(self) -> None:
        if not hasattr(self, "replacements_store"):
            return
        self.replacements_store.clear()
        mappings = self.controller.storage.list_mappings(self.replacement_search.get_text())
        for mapping in mappings:
            self.replacements_store.append(
                [
                    mapping.id or 0,
                    mapping.source_phrase,
                    mapping.replacement_phrase,
                    mapping.enabled,
                    mapping.case_sensitive,
                    mapping.whole_word_only,
                ]
            )
        self.replacements_empty.set_visible(len(mappings) == 0)
        self._update_replacement_preview()

    def _update_replacement_preview(self) -> None:
        if not hasattr(self, "preview_input"):
            return
        text = self._get_text(self.preview_input)
        mappings = self.controller.storage.list_mappings()
        output, _applied = apply_replacements(text, mappings)
        self._set_text(self.preview_output, output)

    def refresh_history(self) -> None:
        if not hasattr(self, "history_store"):
            return
        search = self.history_search.get_text()
        day = self.history_date.get_text()
        rows = self.controller.storage.list_history(search=search, day=day, limit=500)
        self.history_store.clear()
        self.history_rows = {}
        for row in rows:
            history_id = int(row["id"])
            self.history_rows[history_id] = row
            preview = " ".join(str(row["final_text"] or "").split())
            if len(preview) > 80:
                preview = preview[:77] + "..."
            cleanup = "on" if row["cleanup_enabled"] else "off"
            if row["cleanup_error"]:
                cleanup = "failed"
            self.history_store.append(
                [
                    history_id,
                    str(row["created_at"])[:19].replace("T", " "),
                    preview,
                    int(row["final_word_count"] or 0),
                    format_duration(float(row["duration_seconds"] or 0)),
                    str(row["transcription_model"] or ""),
                    cleanup,
                    format_cost(float(row["estimated_total_cost"] or 0.0), self.settings.currency),
                ]
            )
        if self.selected_history_id not in self.history_rows:
            self.selected_history_id = None
            self._show_history_detail(None)

    def _history_selection_changed(self, selection: Gtk.TreeSelection) -> None:
        model, iterator = selection.get_selected()
        if not iterator:
            self.selected_history_id = None
            self._show_history_detail(None)
            return
        self.selected_history_id = int(model[iterator][0])
        self._show_history_detail(self.history_rows.get(self.selected_history_id))

    def _show_history_detail(self, row: Any | None) -> None:
        if row is None:
            self._set_text(self.history_raw_view, "")
            self._set_text(self.history_cleaned_view, "")
            self._set_text(self.history_final_view, "")
            self.history_cost_label.set_text("")
            return
        self._set_text(self.history_raw_view, str(row["raw_transcript"] or ""))
        self._set_text(self.history_cleaned_view, str(row["cleaned_transcript"] or ""))
        self._set_text(self.history_final_view, str(row["final_text"] or ""))
        self.history_cost_label.set_text(
            " · ".join(
                [
                    f"Transcription {format_cost(float(row['estimated_transcription_cost'] or 0.0), self.settings.currency)}",
                    f"Cleanup {format_cost(float(row['estimated_cleanup_cost'] or 0.0), self.settings.currency)}",
                    f"Total {format_cost(float(row['estimated_total_cost'] or 0.0), self.settings.currency)}",
                    f"{int(row['raw_word_count'] or 0)} raw words",
                    f"{int(row['final_word_count'] or 0)} final words",
                ]
            )
        )

    def _copy_selected_raw(self, *_args: Any) -> None:
        row = self.history_rows.get(self.selected_history_id or -1)
        if row:
            ClipboardPaste().copy(str(row["raw_transcript"] or ""))
            self._set_message("Raw transcript copied.", "")

    def _copy_selected_final(self, *_args: Any) -> None:
        row = self.history_rows.get(self.selected_history_id or -1)
        if row:
            ClipboardPaste().copy(str(row["final_text"] or ""))
            self._set_message("Final transcript copied.", "")

    def _delete_selected_history(self, *_args: Any) -> None:
        if self.selected_history_id is None:
            return
        self.controller.storage.delete_history(self.selected_history_id)
        self.selected_history_id = None
        self.refresh_all()

    def _clear_history(self, *_args: Any) -> None:
        dialog = Gtk.MessageDialog(
            transient_for=self.window,
            flags=0,
            message_type=Gtk.MessageType.WARNING,
            buttons=Gtk.ButtonsType.OK_CANCEL,
            text="Clear all transcript history? This cannot be undone.",
        )
        response = dialog.run()
        dialog.destroy()
        if response == Gtk.ResponseType.OK:
            self.controller.storage.clear_history()
            self.refresh_all()

    def refresh_stats(self) -> None:
        if not hasattr(self, "stats_labels"):
            return
        stats = self.controller.storage.stats_summary()
        currency = self.settings.currency
        self.stats_labels["total_words"].set_text(str(stats["total_words"]))
        self.stats_labels["total_audio"].set_text(format_duration(stats["total_audio_seconds"]))
        self.stats_labels["average_wpm"].set_text(f"{stats['average_wpm']:.1f}")
        self.stats_labels["total_sessions"].set_text(str(stats["total_sessions"]))
        self.stats_labels["average_words"].set_text(f"{stats['average_words_per_session']:.1f}")
        self.stats_labels["average_duration"].set_text(
            format_duration(stats["average_duration_per_session"])
        )
        self.stats_labels["most_transcription"].set_text(stats["most_used_transcription_model"])
        self.stats_labels["most_cleanup"].set_text(stats["most_used_cleanup_model"])
        self.stats_labels["cleanup_usage"].set_text(str(stats["cleanup_mode_usage_count"]))
        self.stats_labels["cost_total"].set_text(format_cost(stats["estimated_total_cost"], currency))
        self.stats_labels["cost_transcription"].set_text(
            format_cost(stats["estimated_transcription_cost"], currency)
        )
        self.stats_labels["cost_cleanup"].set_text(
            format_cost(stats["estimated_cleanup_cost"], currency)
        )
        self.stats_labels["today"].set_text(self._period_text(stats["today"]))
        self.stats_labels["week"].set_text(self._period_text(stats["week"]))
        self.stats_labels["month"].set_text(self._period_text(stats["month"]))
        metric = self._combo_value(self.graph_metric_combo)
        days = int(self._combo_value(self.graph_range_combo) or "30")
        self.graph.set_values(self.controller.storage.graph_days(days=days, metric=metric))

    def _period_text(self, data: dict[str, Any]) -> str:
        return (
            f"{data['total_words']} words · "
            f"{format_duration(data['total_audio_seconds'])} · "
            f"{format_cost(data['estimated_total_cost'], self.settings.currency)}"
        )

    def refresh_overview(self) -> None:
        if not hasattr(self, "overview_status"):
            return
        stats = self.controller.storage.stats_summary()
        last_rows = self.controller.storage.list_history(limit=1)
        last = ""
        if last_rows:
            last = " ".join(str(last_rows[0]["final_text"] or "").split())
            if len(last) > 110:
                last = last[:107] + "..."
        cleanup = "Off"
        if self.settings.cleanup_enabled:
            cleanup = (
                f"On, {self.settings.active_cleanup_model()}, {self.settings.cleanup_style}"
            )
        self.overview_status.set_text(self.controller.status)
        self.overview_hotkey.set_text(self.settings.hotkey)
        self.overview_transcription.set_text(self.settings.active_transcription_model())
        self.overview_cleanup.set_text(cleanup)
        self.overview_last.set_text(last)
        self.overview_today.set_text(self._period_text(stats["today"]))

    def refresh_all(self) -> bool:
        self.refresh_overview()
        self.refresh_replacements()
        self.refresh_history()
        self.refresh_stats()
        self._update_cleanup_preview()
        return False

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
