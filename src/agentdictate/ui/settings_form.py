from __future__ import annotations

from typing import Any

from agentdictate.config import CUSTOM_LANGUAGE_VALUE, TRANSCRIPTION_LANGUAGES, Settings

from .gtk import Gtk


class SettingsFormMixin:
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
        self._set_combo_value(self.paste_shortcut_combo, s.paste_shortcut)
        self.cleanup_switch.set_active(s.cleanup_enabled)
        self._set_combo_value(self.cleanup_model_combo, s.cleanup_model)
        self.custom_cleanup_entry.set_text(s.custom_cleanup_model)
        self._set_combo_value(self.cleanup_style_combo, s.cleanup_style)
        self._set_combo_value(self.cleanup_reasoning_combo, s.cleanup_reasoning_effort)
        self._set_text(self.cleanup_prompt_view, s.cleanup_prompt)
        self.currency_entry.set_text(s.currency)
        self._sync_price_entries(s)
        self.start_on_login_switch.set_active(s.start_on_login)
        self.show_tray_switch.set_active(s.show_tray_icon)
        self.minimize_to_tray_switch.set_active(s.minimize_to_tray_on_close)
        self.launch_window_switch.set_active(s.launch_window_on_startup)
        self.restore_clipboard_switch.set_active(s.restore_clipboard_after_paste)
        self.debug_switch.set_active(s.debug_mode)
        self.preserve_audio_switch.set_active(s.preserve_temp_audio)
        self._update_cleanup_enabled()
        self.refresh_all()

    def _sync_price_entries(self, settings: Settings) -> None:
        for model, entry in self.transcription_price_entries.items():
            entry.set_text(
                str(settings.transcription_prices.get(model, {}).get("price_per_audio_minute", 0.0))
            )
        for model, (input_entry, output_entry) in self.cleanup_price_entries.items():
            price = settings.cleanup_prices.get(model, {})
            input_entry.set_text(str(price.get("input_price_per_1m_tokens", 0.0)))
            output_entry.set_text(str(price.get("output_price_per_1m_tokens", 0.0)))

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
        self._apply_openai_settings(s)
        self._apply_dictation_settings(s)
        self._apply_cleanup_settings(s)
        self._apply_advanced_settings(s)
        return s

    def _apply_openai_settings(self, settings: Settings) -> None:
        settings.openai_api_key = self.api_key_entry.get_text()
        settings.transcription_model = self._combo_value(self.transcription_combo)
        settings.custom_transcription_model = self.custom_transcription_entry.get_text()
        settings.language = self._language_from_ui()
        settings.transcription_prompt = self._get_text(self.transcription_prompt_view)

    def _apply_dictation_settings(self, settings: Settings) -> None:
        settings.hotkey = self.hotkey_entry.get_text() or "Ctrl+Space"
        settings.recording_mode = self._combo_value(self.recording_mode_combo) or "toggle"
        settings.max_recording_seconds = int(self.max_duration_spin.get_value())
        settings.sound_feedback = self.sound_switch.get_active()
        settings.start_sound = self.start_sound_switch.get_active()
        settings.stop_sound = self.stop_sound_switch.get_active()
        settings.audio_ducking_enabled = self.audio_ducking_switch.get_active()
        settings.audio_ducking_volume_percent = int(self.audio_ducking_volume_spin.get_value())
        settings.audio_ducking_fade_ms = int(self.audio_ducking_fade_spin.get_value())
        settings.paste_shortcut = self._combo_value(self.paste_shortcut_combo)

    def _apply_cleanup_settings(self, settings: Settings) -> None:
        settings.cleanup_enabled = self.cleanup_switch.get_active()
        settings.cleanup_model = self._combo_value(self.cleanup_model_combo)
        settings.custom_cleanup_model = self.custom_cleanup_entry.get_text()
        settings.cleanup_style = self._combo_value(self.cleanup_style_combo)
        settings.cleanup_reasoning_effort = self._combo_value(self.cleanup_reasoning_combo) or "default"
        settings.cleanup_prompt = self._get_text(self.cleanup_prompt_view)
        settings.currency = self.currency_entry.get_text() or "USD"
        for model, entry in self.transcription_price_entries.items():
            settings.transcription_prices.setdefault(model, {"model_name": model})
            settings.transcription_prices[model]["price_per_audio_minute"] = self._float_entry(entry)
            settings.transcription_prices[model]["currency"] = settings.currency
        for model, (input_entry, output_entry) in self.cleanup_price_entries.items():
            settings.cleanup_prices.setdefault(model, {"model_name": model})
            settings.cleanup_prices[model]["input_price_per_1m_tokens"] = self._float_entry(input_entry)
            settings.cleanup_prices[model]["output_price_per_1m_tokens"] = self._float_entry(output_entry)
            settings.cleanup_prices[model]["currency"] = settings.currency

    def _apply_advanced_settings(self, settings: Settings) -> None:
        settings.start_on_login = self.start_on_login_switch.get_active()
        settings.show_tray_icon = self.show_tray_switch.get_active()
        settings.minimize_to_tray_on_close = self.minimize_to_tray_switch.get_active()
        settings.launch_window_on_startup = self.launch_window_switch.get_active()
        settings.restore_clipboard_after_paste = self.restore_clipboard_switch.get_active()
        settings.debug_mode = self.debug_switch.get_active()
        settings.preserve_temp_audio = self.preserve_audio_switch.get_active()

    def _float_entry(self, entry: Gtk.Entry) -> float:
        try:
            return float(entry.get_text())
        except ValueError:
            return 0.0
