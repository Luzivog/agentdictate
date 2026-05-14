from __future__ import annotations

from agentdictate.paths import cache_dir, config_path, database_path, logs_dir

from .gtk import Gtk


class StatsAdvancedTabsMixin:
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
        switches = [
            ("start_on_login_switch", "Start on login"),
            ("show_tray_switch", "Show tray icon"),
            ("minimize_to_tray_switch", "Minimize to tray on close"),
            ("launch_window_switch", "Open window on startup"),
            ("restore_clipboard_switch", "Restore previous clipboard after paste"),
            ("debug_switch", "Debug mode"),
            ("preserve_audio_switch", "Preserve temporary audio"),
        ]
        for row, (attr, label) in enumerate(switches):
            switch = Gtk.Switch()
            setattr(self, attr, switch)
            self._grid_attach(grid, label, switch, row)
        audio_warning = self._warning_label(
            "Temporary audio files may contain sensitive speech. Only enable this for debugging."
        )
        grid.attach(audio_warning, 1, 7, 1, 1)
        box.pack_start(self._advanced_path_buttons(), False, False, 0)
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

    def _advanced_path_buttons(self) -> Gtk.Box:
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
        return buttons
