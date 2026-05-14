from __future__ import annotations

from typing import Callable

StatusCallback = Callable[[str], None]
MessageCallback = Callable[[str, str], None]
RefreshCallback = Callable[[], None]
