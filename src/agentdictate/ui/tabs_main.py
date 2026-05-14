from __future__ import annotations

from agentdictate.config import (
    PLAIN_KEY_WARNING,
    RECORDING_MODES,
    TRANSCRIPTION_LANGUAGES,
    TRANSCRIPTION_MODELS,
)

from .gtk import Gtk


class MainTabsMixin:
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
        buttons.pack_start(show_button, False, False, 0)
        for label, callback in (
            ("Save key", self._save_from_ui),
            ("Clear key", self._clear_key),
            ("Test key", self._test_key),
        ):
            button = Gtk.Button(label=label)
            button.connect("clicked", callback)
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
        button_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        change_button = Gtk.Button(label="Change hotkey")
        change_button.connect("clicked", self._save_from_ui)
        reset_button = Gtk.Button(label="Reset to Ctrl+Space")
        reset_button.connect("clicked", self._reset_hotkey)
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
