from __future__ import annotations

import json
import os
import subprocess
import sys
from typing import Callable

from .gtk import GLib
from .window import DictationOverlayWindow


class DictationOverlayHelperClient:
    def __init__(
        self,
        waveform_provider: Callable[[], list[float]],
        elapsed_provider: Callable[[], float],
    ) -> None:
        self.waveform_provider = waveform_provider
        self.elapsed_provider = elapsed_provider
        self.status = "Ready"
        self.cleanup_enabled = True
        self.process: subprocess.Popen[str] | None = None
        self._tick_id: int | None = None

    def set_status(self, status: str, cleanup_enabled: bool) -> None:
        self.status = status
        self.cleanup_enabled = cleanup_enabled
        if not self._ensure_process():
            return
        self._send_update()
        if status in DictationOverlayWindow.ACTIVE_STATUSES:
            if self._tick_id is None:
                self._tick_id = GLib.timeout_add(33, self._tick)
        elif self._tick_id is not None:
            GLib.source_remove(self._tick_id)
            self._tick_id = None

    def close(self) -> None:
        if self._tick_id is not None:
            GLib.source_remove(self._tick_id)
            self._tick_id = None
        process = self.process
        self.process = None
        if process is None:
            return
        try:
            if process.stdin is not None:
                process.stdin.close()
        except OSError:
            pass
        try:
            process.terminate()
        except OSError:
            pass

    def _tick(self) -> bool:
        if self.process is None or self.process.poll() is not None:
            self.process = None
            self._tick_id = None
            return False
        if self.status not in DictationOverlayWindow.ACTIVE_STATUSES:
            self._tick_id = None
            return False
        self._send_update()
        return True

    def _ensure_process(self) -> bool:
        if self.process is not None and self.process.poll() is None:
            return True
        env = os.environ.copy()
        env["GDK_BACKEND"] = "x11"
        try:
            self.process = subprocess.Popen(
                self._command(),
                stdin=subprocess.PIPE,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                text=True,
                env=env,
            )
        except OSError:
            self.process = None
            return False
        return True

    def _command(self) -> list[str]:
        executable = os.environ.get("AGENTDICTATE_EXEC")
        if executable:
            return [executable, "--overlay-helper"]
        return [sys.executable, "-m", "agentdictate", "--overlay-helper"]

    def _send_update(self) -> None:
        process = self.process
        if process is None or process.stdin is None:
            return
        payload = {
            "status": self.status,
            "cleanup_enabled": self.cleanup_enabled,
            "elapsed": self.elapsed_provider(),
            "waveform": self.waveform_provider(),
        }
        try:
            process.stdin.write(json.dumps(payload, separators=(",", ":")) + "\n")
            process.stdin.flush()
        except (BrokenPipeError, OSError):
            self.process = None
