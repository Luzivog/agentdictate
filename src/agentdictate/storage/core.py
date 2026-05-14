from __future__ import annotations

import sqlite3
import threading
from pathlib import Path

from agentdictate.paths import database_path, ensure_app_dirs

from .daily import DailyStatsMixin
from .history import HistoryStoreMixin
from .mappings import MappingStoreMixin
from .pricing import PricingStoreMixin
from .schema import SCHEMA
from .stats import StatsStoreMixin


class Storage(
    PricingStoreMixin,
    HistoryStoreMixin,
    DailyStatsMixin,
    MappingStoreMixin,
    StatsStoreMixin,
):
    def __init__(self, path: Path | None = None) -> None:
        ensure_app_dirs()
        self.path = path or database_path()
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._lock = threading.RLock()
        self.conn = sqlite3.connect(self.path, check_same_thread=False)
        self.conn.row_factory = sqlite3.Row
        with self._lock:
            self.conn.executescript(SCHEMA)
            self.conn.execute("PRAGMA foreign_keys = ON")
            self.conn.commit()

    def close(self) -> None:
        with self._lock:
            self.conn.close()
