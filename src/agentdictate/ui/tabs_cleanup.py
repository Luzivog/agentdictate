from __future__ import annotations

from agentdictate.config import (
    CLEANUP_MODELS,
    CLEANUP_REASONING_EFFORTS,
    CLEANUP_STYLES,
    PRICING_DISCLAIMER,
    TRANSCRIPTION_MODELS,
)

from .gtk import Gtk


class CleanupTabMixin:
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
        box.pack_start(self._pricing_expander(), False, False, 0)
        self._save_button_row(box)
        return box

    def _pricing_expander(self) -> Gtk.Expander:
        expander = Gtk.Expander(label="Pricing settings")
        pricing_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        pricing_grid = self._grid()
        pricing_box.pack_start(pricing_grid, False, False, 0)
        row = self._transcription_price_rows(pricing_grid)
        row = self._cleanup_price_rows(pricing_grid, row)
        self.currency_entry = Gtk.Entry()
        self._grid_attach(pricing_grid, "Currency", self.currency_entry, row)
        reset_button = Gtk.Button(label="Reset pricing defaults")
        reset_button.connect("clicked", self._reset_pricing)
        pricing_box.pack_start(reset_button, False, False, 0)
        pricing_box.pack_start(self._warning_label(PRICING_DISCLAIMER), False, False, 0)
        expander.add(pricing_box)
        return expander

    def _transcription_price_rows(self, grid: Gtk.Grid) -> int:
        row = 0
        grid.attach(Gtk.Label(label="Transcription model"), 0, row, 1, 1)
        grid.attach(Gtk.Label(label="Price per audio minute"), 1, row, 1, 1)
        row += 1
        for model in TRANSCRIPTION_MODELS:
            if model == "Custom":
                continue
            label = Gtk.Label(label=model)
            label.set_xalign(0)
            entry = Gtk.Entry()
            self.transcription_price_entries[model] = entry
            grid.attach(label, 0, row, 1, 1)
            grid.attach(entry, 1, row, 1, 1)
            row += 1
        return row

    def _cleanup_price_rows(self, grid: Gtk.Grid, row: int) -> int:
        grid.attach(Gtk.Label(label="Cleanup model"), 0, row, 1, 1)
        grid.attach(Gtk.Label(label="Input / 1M tokens"), 1, row, 1, 1)
        grid.attach(Gtk.Label(label="Output / 1M tokens"), 2, row, 1, 1)
        row += 1
        for model in CLEANUP_MODELS:
            if model == "Custom":
                continue
            label = Gtk.Label(label=model)
            label.set_xalign(0)
            input_entry = Gtk.Entry()
            output_entry = Gtk.Entry()
            self.cleanup_price_entries[model] = (input_entry, output_entry)
            grid.attach(label, 0, row, 1, 1)
            grid.attach(input_entry, 1, row, 1, 1)
            grid.attach(output_entry, 2, row, 1, 1)
            row += 1
        return row
