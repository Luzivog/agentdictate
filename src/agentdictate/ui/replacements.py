from __future__ import annotations

from typing import Any

from agentdictate.replacements import ReplacementMapping, apply_replacements

from .gtk import Gtk


class ReplacementsMixin:
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
        output, _applied = apply_replacements(text, self.controller.storage.list_mappings())
        self._set_text(self.preview_output, output)
