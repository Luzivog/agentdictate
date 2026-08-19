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
PopenFactory = Callable[..., subprocess.Popen[str]]
Which = Callable[[str], str | None]
Sleep = Callable[[float], None]


class AudioDucker:
    def __init__(
        self,
        runner: Runner = subprocess.run,
        popen: PopenFactory = subprocess.Popen,
        which: Which = shutil.which,
        sleep: Sleep = time.sleep,
        async_fades: bool = True,
        monitor_streams: bool = True,
    ) -> None:
        self._runner = runner
        self._popen = popen
        self._which = which
        self._sleep = sleep
        self._async_fades = async_fades
        self._monitor_streams_enabled = monitor_streams
        self._lock = threading.RLock()
        self._originals: dict[str, tuple[int, ...]] = {}
        self._targets: dict[str, tuple[int, ...]] = {}
        self._fading_streams: set[str] = set()
        self._ducking = False
        self._ratio = 0.15
        self._generation = 0
        self._worker: threading.Thread | None = None
        self._fade_ms = 1000
        self._monitor_thread: threading.Thread | None = None
        self._monitor_stop: threading.Event | None = None
        self._subscriber_process: subprocess.Popen[str] | None = None

    def is_available(self) -> bool:
        return self._which("pactl") is not None

    def duck(self, settings: Settings) -> None:
        if not settings.audio_ducking_enabled or not self.is_available():
            return
        volume_percent = clamp_audio_ducking_volume(settings.audio_ducking_volume_percent)
        fade_ms = clamp_audio_ducking_fade_ms(settings.audio_ducking_fade_ms)
        with self._lock:
            self._ducking = True
            self._ratio = volume_percent / 100
            self._fade_ms = fade_ms
            self._generation += 1
            self._fading_streams.clear()
        self._start_monitor()
        self._reconcile_active_streams()

    def restore(self, wait: bool = False) -> None:
        with self._lock:
            self._ducking = False
            self._generation += 1
            generation = self._generation
            targets = dict(self._originals)
            fade_ms = self._fade_ms
            self._fading_streams.clear()
        self._stop_monitor()
        if not self.is_available() or not targets:
            with self._lock:
                if generation == self._generation:
                    self._originals.clear()
                    self._targets.clear()
            return
        self._start_fade(
            targets,
            fade_ms,
            generation,
            clear_on_finish=True,
            wait=wait,
        )

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

    def _start_monitor(self) -> None:
        if not self._monitor_streams_enabled:
            return
        with self._lock:
            if not self._ducking:
                return
            if self._monitor_thread and self._monitor_thread.is_alive():
                return
            stop_event = threading.Event()
            generation = self._generation
            worker = threading.Thread(
                target=self._monitor_sink_inputs,
                args=(stop_event, generation),
                daemon=True,
            )
            self._monitor_stop = stop_event
            self._monitor_thread = worker
        worker.start()

    def _stop_monitor(self) -> None:
        with self._lock:
            stop_event = self._monitor_stop
            process = self._subscriber_process
            worker = self._monitor_thread
        if stop_event is not None:
            stop_event.set()
        if process is not None and process.poll() is None:
            try:
                process.terminate()
            except OSError:
                pass
        if worker is not None and worker is not threading.current_thread():
            worker.join(timeout=1)
        with self._lock:
            if self._monitor_thread is worker and (
                worker is None or not worker.is_alive()
            ):
                self._monitor_thread = None
                self._monitor_stop = None
            if self._subscriber_process is process and (
                process is None or process.poll() is not None
            ):
                self._subscriber_process = None

    def _monitor_sink_inputs(
        self, stop_event: threading.Event, generation: int
    ) -> None:
        process: subprocess.Popen[str] | None = None
        try:
            process = self._popen(
                ["pactl", "subscribe"],
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                text=True,
                bufsize=1,
            )
            with self._lock:
                if (
                    stop_event.is_set()
                    or not self._ducking
                    or generation != self._generation
                ):
                    should_stop = True
                else:
                    self._subscriber_process = process
                    should_stop = False
            if should_stop:
                try:
                    process.terminate()
                except OSError:
                    pass
                return

            self._reconcile_active_streams()
            if process.stdout is None:
                return
            for line in process.stdout:
                if stop_event.is_set():
                    break
                self._handle_subscription_event(line)
        except OSError:
            return
        finally:
            if process is not None and process.poll() is None:
                try:
                    process.terminate()
                except OSError:
                    pass
            with self._lock:
                if self._subscriber_process is process:
                    self._subscriber_process = None
                if self._monitor_thread is threading.current_thread():
                    self._monitor_thread = None
                    self._monitor_stop = None

    def _handle_subscription_event(self, line: str) -> None:
        if "sink-input" not in line:
            return
        if "'new'" not in line and "'change'" not in line:
            return
        self._reconcile_active_streams()

    def _reconcile_active_streams(self) -> None:
        try:
            active_streams = self._list_sink_inputs(active_only=True)
        except Exception:
            return

        pending_targets: dict[str, tuple[int, ...]] = {}
        with self._lock:
            if not self._ducking:
                return
            generation = self._generation
            fade_ms = self._fade_ms
            for stream in active_streams:
                stream_id = stream.stream_id
                if stream_id not in self._originals:
                    self._originals[stream_id] = stream.volumes
                    self._targets[stream_id] = tuple(
                        max(0, int(volume * self._ratio))
                        for volume in stream.volumes
                    )
                target = self._targets[stream_id]
                if stream_id in self._fading_streams:
                    continue
                current = fit_volume_count(stream.volumes, len(target))
                fitted_target = fit_volume_count(target, len(current))
                if not any(
                    current_value > target_value
                    for current_value, target_value in zip(current, fitted_target)
                ):
                    continue
                self._fading_streams.add(stream_id)
                pending_targets[stream_id] = target

        if pending_targets:
            self._start_fade(
                pending_targets,
                fade_ms,
                generation,
                clear_on_finish=False,
                wait=False,
                tracked_stream_ids=tuple(pending_targets),
            )

    def _start_fade(
        self,
        targets: dict[str, tuple[int, ...]],
        fade_ms: int,
        generation: int,
        clear_on_finish: bool,
        wait: bool,
        tracked_stream_ids: tuple[str, ...] = (),
    ) -> None:
        worker = threading.Thread(
            target=self._fade,
            args=(
                targets,
                fade_ms,
                generation,
                clear_on_finish,
                tracked_stream_ids,
            ),
            daemon=True,
        )
        with self._lock:
            self._worker = worker
        if self._async_fades:
            worker.start()
            if wait:
                worker.join(timeout=(fade_ms / 1000) + 1)
        else:
            self._fade(
                targets,
                fade_ms,
                generation,
                clear_on_finish,
                tracked_stream_ids,
            )

    def _fade(
        self,
        targets: dict[str, tuple[int, ...]],
        fade_ms: int,
        generation: int,
        clear_on_finish: bool,
        tracked_stream_ids: tuple[str, ...] = (),
    ) -> None:
        try:
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
        finally:
            with self._lock:
                if generation == self._generation:
                    self._fading_streams.difference_update(tracked_stream_ids)
                    if clear_on_finish:
                        self._ducking = False
                        self._originals.clear()
                        self._targets.clear()
                        self._fading_streams.clear()

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
