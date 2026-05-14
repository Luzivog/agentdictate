from __future__ import annotations

from agentdictate.config import HISTORY_WARNING

from .gtk import Gtk


class DataTabsMixin:
    def _replacements_tab(self) -> Gtk.Widget:
        box = self._tab_box()
        search_row = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        self.replacement_search = Gtk.SearchEntry()
        self.replacement_search.connect("search-changed", lambda *_args: self.refresh_replacements())
        search_row.pack_start(self.replacement_search, True, True, 0)
        for label, callback in (
            ("Add mapping", self._add_mapping),
            ("Edit mapping", self._edit_mapping),
            ("Delete mapping", self._delete_mapping),
        ):
            button = Gtk.Button(label=label)
            button.connect("clicked", callback)
            search_row.pack_start(button, False, False, 0)
        box.pack_start(search_row, False, False, 0)

        self.replacements_store = Gtk.ListStore(int, str, str, bool, bool, bool)
        tree = Gtk.TreeView(model=self.replacements_store)
        titles = [
            "ID",
            "Source phrase",
            "Replacement phrase",
            "Enabled",
            "Case-sensitive",
            "Whole-word",
        ]
        for index, title in enumerate(titles):
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
        titles = ["ID", "Date", "Final transcript", "Words", "Duration", "Model", "Cleanup", "Cost"]
        for index, title in enumerate(titles):
            renderer = Gtk.CellRendererText()
            column = Gtk.TreeViewColumn(title, renderer, text=index)
            if index == 0:
                column.set_visible(False)
            if index == 2:
                column.set_expand(True)
            tree.append_column(column)
        tree.get_selection().connect("changed", self._history_selection_changed)
        box.pack_start(self._scrolled(tree, height=190), True, True, 0)
        box.pack_start(self._history_buttons(), False, False, 0)
        self.history_cost_label = Gtk.Label(label="")
        self.history_cost_label.set_xalign(0)
        box.pack_start(self.history_cost_label, False, False, 0)
        for label, attr in (
            ("Raw transcript", "history_raw_view"),
            ("Cleaned transcript", "history_cleaned_view"),
            ("Final transcript", "history_final_view"),
        ):
            box.pack_start(Gtk.Label(label=label), False, False, 0)
            view = self._text_view(height=70, editable=False)
            setattr(self, attr, view)
            box.pack_start(view, False, False, 0)
        return box

    def _history_buttons(self) -> Gtk.Box:
        details = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=8)
        for label, callback in (
            ("Copy raw", self._copy_selected_raw),
            ("Copy final", self._copy_selected_final),
            ("Delete item", self._delete_selected_history),
            ("Clear all history", self._clear_history),
        ):
            button = Gtk.Button(label=label)
            button.connect("clicked", callback)
            details.pack_start(button, False, False, 0)
        return details
