from __future__ import annotations

from typing import Any

from agentdictate.config import Settings, reset_pricing_defaults
from agentdictate.costs import estimate_cleanup_cost, format_cost

from .gtk import Gtk


class SettingsActionsMixin:
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
        if not hasattr(self, "cleanup_cost_preview"):
            return
        raw = "Update the onboarding flow and add tests."
        cleaned = raw
        settings = self._settings_from_ui() if hasattr(self, "cleanup_switch") else self.settings
        price = settings.cleanup_price()
        if settings.cleanup_enabled:
            cost, _input_tokens, _output_tokens = estimate_cleanup_cost(
                raw, cleaned, price.input_price_per_1m_tokens, price.output_price_per_1m_tokens
            )
        else:
            cost = 0.0
        self.cleanup_cost_preview.set_text(
            f"Approximately {format_cost(cost, settings.currency)} for a short prompt preview"
        )
