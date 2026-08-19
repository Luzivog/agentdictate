from __future__ import annotations

from pathlib import Path
from typing import Any

from agentdictate.clipboard import ClipboardPaste
from agentdictate.costs import format_cost, format_duration

from .gtk import Gtk


class HistoryMixin:
    def refresh_history(self) -> None:
        if not hasattr(self, "history_store"):
            return
        self._refresh_recoveries()
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

    def _refresh_recoveries(self) -> None:
        rows = self.controller.list_recoverable_dictations()
        self.recovery_store.clear()
        self.recovery_rows = {}
        for row in rows:
            job_id = int(row["id"])
            self.recovery_rows[job_id] = row
            saved_content = str(row["final_text"] or row["raw_transcript"] or "")
            if saved_content:
                saved_content = " ".join(saved_content.split())
                if len(saved_content) > 70:
                    saved_content = saved_content[:67] + "..."
            else:
                saved_content = f"Audio: {Path(row['audio_path']).name}"
            error = " ".join(str(row["error_message"] or "").split())
            if len(error) > 70:
                error = error[:67] + "..."
            self.recovery_store.append(
                [
                    job_id,
                    str(row["started_at"])[:19].replace("T", " "),
                    str(row["state"]).replace("_", " "),
                    saved_content,
                    error,
                ]
            )
        self.recovery_empty.set_visible(not rows)
        if self.selected_recovery_id not in self.recovery_rows:
            self.selected_recovery_id = None

    def _recovery_selection_changed(self, selection: Gtk.TreeSelection) -> None:
        model, iterator = selection.get_selected()
        self.selected_recovery_id = int(model[iterator][0]) if iterator else None

    def _retry_selected_recovery(self, *_args: Any) -> None:
        if self.selected_recovery_id is None:
            return
        if self.controller.retry_dictation(self.selected_recovery_id):
            self._set_message("Retrying saved dictation...", "")

    def _copy_selected_recovery(self, *_args: Any) -> None:
        row = self.recovery_rows.get(self.selected_recovery_id or -1)
        if row is None:
            return
        saved_text = str(row["final_text"] or row["raw_transcript"] or "")
        if not saved_text:
            self._set_message("No transcript has been recovered from this audio yet.", "")
            return
        ClipboardPaste().copy(saved_text)
        self._set_message("Saved transcript copied.", "")

    def _open_selected_recovery(self, *_args: Any) -> None:
        row = self.recovery_rows.get(self.selected_recovery_id or -1)
        if row is not None:
            self._open_path(Path(row["audio_path"]).parent)

    def _delete_selected_recovery(self, *_args: Any) -> None:
        if self.selected_recovery_id is None:
            return
        dialog = Gtk.MessageDialog(
            transient_for=self.window,
            flags=0,
            message_type=Gtk.MessageType.WARNING,
            buttons=Gtk.ButtonsType.OK_CANCEL,
            text="Permanently delete this saved recording?",
        )
        dialog.format_secondary_text(
            "The audio cannot be recovered after deletion. Transcript history is not affected."
        )
        response = dialog.run()
        dialog.destroy()
        if response == Gtk.ResponseType.OK:
            self.controller.delete_recoverable_dictation(self.selected_recovery_id)
            self.selected_recovery_id = None
            self.refresh_all()

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
            self._set_history_transcript_detail_mode(cleanup_enabled=True)
            self._set_text(self.history_raw_view, "")
            self._set_text(self.history_cleaned_view, "")
            self._set_text(self.history_final_view, "")
            self.history_cost_label.set_text("")
            return
        cleanup_enabled = bool(row["cleanup_enabled"])
        self._set_history_transcript_detail_mode(cleanup_enabled=cleanup_enabled)
        if cleanup_enabled:
            self._set_text(self.history_raw_view, str(row["raw_transcript"] or ""))
            self._set_text(self.history_cleaned_view, str(row["cleaned_transcript"] or ""))
            self._set_text(self.history_final_view, str(row["final_text"] or ""))
        else:
            self._set_text(
                self.history_raw_view,
                str(row["final_text"] or row["raw_transcript"] or ""),
            )
            self._set_text(self.history_cleaned_view, "")
            self._set_text(self.history_final_view, "")
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

    def _set_history_transcript_detail_mode(self, cleanup_enabled: bool) -> None:
        cleanup_widgets = (
            self.history_cleaned_label,
            self.history_cleaned_view,
            self.history_final_label,
            self.history_final_view,
        )
        if cleanup_enabled:
            self.history_raw_label.set_text("Raw transcript")
            for widget in cleanup_widgets:
                widget.set_no_show_all(False)
                widget.show()
            self.history_raw_view.set_size_request(-1, 70)
            return
        self.history_raw_label.set_text("Transcript")
        for widget in cleanup_widgets:
            widget.set_no_show_all(True)
            widget.hide()
        self.history_raw_view.set_size_request(-1, 220)

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
