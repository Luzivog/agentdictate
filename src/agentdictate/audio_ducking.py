from __future__ import annotations

import shutil
import subprocess
import threading
import time
from typing import Callable, Sequence

from .audio_ducking_helpers import (
    PACTL_VOLUME,
    SinkInput,
    clamp_audio_ducking_fade_ms,
    clamp_audio_ducking_volume,
    fade_steps,
    fit_volume_count,
    parse_sink_inputs,
)
from .config import Settings

Runner = Callable[..., subprocess.CompletedProcess[str]]
Which = Callable[[str], str | None]
Sleep = Callable[[float], None]


class AudioDucker:
    def __init__(
        self,
        runner: Runner = subprocess.run,
        which: Which = shutil.which,
        sleep: Sleep = time.sleep,
        async_fades: bool = True,
    ) -> None:
        self._runner = runner
        self._which = which
        self._sleep = sleep
        self._async_fades = async_fades
        self._lock = threading.RLock()
        self._originals: dict[str, tuple[int, ...]] = {}
        self._generation = 0
        self._worker: threading.Thread | None = None
        self._fade_ms = 1000

    def is_available(self) -> bool:
        return self._which("pactl") is not None

    def duck(self, settings: Settings) -> None:
        if not settings.audio_ducking_enabled or not self.is_available():
            return
        volume_percent = clamp_audio_ducking_volume(settings.audio_ducking_volume_percent)
        fade_ms = clamp_audio_ducking_fade_ms(settings.audio_ducking_fade_ms)
        ratio = volume_percent / 100
        try:
            active_streams = self._list_sink_inputs(active_only=True)
        except Exception:
            return
        with self._lock:
            if not self._originals:
                self._originals = {
                    stream.stream_id: stream.volumes for stream in active_streams
                }
            if not self._originals:
                return
            self._fade_ms = fade_ms
            self._generation += 1
            generation = self._generation
            originals = dict(self._originals)
        targets = {
            stream_id: tuple(max(0, int(volume * ratio)) for volume in volumes)
            for stream_id, volumes in originals.items()
        }
        self._start_fade(targets, fade_ms, generation, clear_on_finish=False, wait=False)

    def restore(self, wait: bool = False) -> None:
        if not self.is_available():
            with self._lock:
                self._originals.clear()
            return
        with self._lock:
            if not self._originals:
                return
            self._generation += 1
            generation = self._generation
            targets = dict(self._originals)
            fade_ms = self._fade_ms
        self._start_fade(targets, fade_ms, generation, clear_on_finish=True, wait=wait)

    def _list_sink_inputs(self, active_only: bool = False) -> list[SinkInput]:
        result = self._runner(
            ["pactl", "list", "sink-inputs"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        if result.returncode != 0:
            return []
        streams = parse_sink_inputs(result.stdout or "")
        if active_only:
            streams = [stream for stream in streams if not stream.corked]
        return streams

    def _start_fade(
        self,
        targets: dict[str, tuple[int, ...]],
        fade_ms: int,
        generation: int,
        clear_on_finish: bool,
        wait: bool,
    ) -> None:
        worker = threading.Thread(
            target=self._fade,
            args=(targets, fade_ms, generation, clear_on_finish),
            daemon=True,
        )
        with self._lock:
            self._worker = worker
        if self._async_fades:
            worker.start()
            if wait:
                worker.join(timeout=(fade_ms / 1000) + 1)
        else:
            self._fade(targets, fade_ms, generation, clear_on_finish)

    def _fade(
        self,
        targets: dict[str, tuple[int, ...]],
        fade_ms: int,
        generation: int,
        clear_on_finish: bool,
    ) -> None:
        try:
            current = {
                stream.stream_id: stream.volumes
                for stream in self._list_sink_inputs(active_only=False)
            }
        except Exception:
            current = {}
        starts: dict[str, tuple[int, ...]] = {}
        for stream_id, target in targets.items():
            start = current.get(stream_id)
            if start is not None:
                starts[stream_id] = fit_volume_count(start, len(target))
        if not starts:
            if clear_on_finish:
                with self._lock:
                    if generation == self._generation:
                        self._originals.clear()
            return
        steps = fade_steps(fade_ms)
        delay = (fade_ms / 1000 / steps) if fade_ms > 0 else 0
        for step in range(1, steps + 1):
            with self._lock:
                if generation != self._generation:
                    return
            position = step / steps
            self._set_step_volumes(targets, starts, position)
            if delay > 0 and step < steps:
                self._sleep(delay)
        if clear_on_finish:
            with self._lock:
                if generation == self._generation:
                    self._originals.clear()

    def _set_step_volumes(
        self,
        targets: dict[str, tuple[int, ...]],
        starts: dict[str, tuple[int, ...]],
        position: float,
    ) -> None:
        for stream_id, target in targets.items():
            start = starts.get(stream_id)
            if start is None:
                continue
            fitted_target = fit_volume_count(target, len(start))
            values = tuple(
                int(round(start_value + (target_value - start_value) * position))
                for start_value, target_value in zip(start, fitted_target)
            )
            self._set_volume(stream_id, values)

    def _set_volume(self, stream_id: str, volumes: Sequence[int]) -> None:
        try:
            self._runner(
                [
                    "pactl",
                    "set-sink-input-volume",
                    stream_id,
                    *[str(volume) for volume in volumes],
                ],
                check=False,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                text=True,
            )
        except Exception:
            return


_fade_steps = fade_steps
_fit_volume_count = fit_volume_count
