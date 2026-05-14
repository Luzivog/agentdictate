from __future__ import annotations

from typing import Any

from agentdictate.clipboard import ClipboardPaste
from agentdictate.costs import format_cost, format_duration

from .gtk import Gtk


class HistoryMixin:
    def refresh_history(self) -> None:
        if not hasattr(self, "history_store"):
            return
        rows = self.controller.storage.list_history(
            search=self.history_search.get_text(),
            day=self.history_date.get_text(),
            limit=500,
        )
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
