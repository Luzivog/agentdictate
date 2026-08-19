from __future__ import annotations

from typing import Any

from agentdictate.costs import format_cost, format_duration


class StatsMixin:
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
            cleanup = f"On, {self.settings.active_cleanup_model()}, {self.settings.cleanup_style}"
        hotkey = self.settings.hotkey
        if not self.controller.hotkey_available:
            hotkey += " (unavailable)"
        self.overview_status.set_text(self.controller.status)
        self.overview_hotkey.set_text(hotkey)
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
