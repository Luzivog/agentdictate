from __future__ import annotations

import os
import math
import shutil
import signal
import subprocess
import struct
import time
from dataclasses import dataclass
from pathlib import Path

from .paths import cache_dir, ensure_app_dirs


class AudioError(RuntimeError):
    pass


@dataclass
class Recording:
    path: Path
    started_at: float
    process: subprocess.Popen[bytes]
    command_name: str


class AudioRecorder:
    def __init__(self) -> None:
        ensure_app_dirs()

    def start(self) -> Recording:
        path = cache_dir() / f"recording-{int(time.time() * 1000)}.wav"
        command = self._record_command(path)
        try:
            process = subprocess.Popen(
                command,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                start_new_session=True,
            )
        except OSError as exc:
            raise AudioError(
                "Could not access the default microphone. Check your Linux audio settings."
            ) from exc
        time.sleep(0.05)
        if process.poll() is not None:
            raise AudioError(
                "Could not access the default microphone. Check your Linux audio settings."
            )
        return Recording(path=path, started_at=time.monotonic(), process=process, command_name=command[0])

    def stop(self, recording: Recording, wait_seconds: float = 5.0) -> float:
        duration = time.monotonic() - recording.started_at
        process = recording.process
        if process.poll() is None:
            try:
                os.killpg(process.pid, signal.SIGINT)
            except OSError:
                process.terminate()
            try:
                process.wait(timeout=wait_seconds)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=1)
        if not recording.path.exists() or recording.path.stat().st_size == 0:
            raise AudioError(
                "Could not access the default microphone. Check your Linux audio settings."
            )
        return duration

    def input_level(self, recording: Recording, sample_count: int = 2048) -> float:
        samples = self._recent_samples(recording, sample_count)
        if not samples:
            return 0.0
        rms = math.sqrt(sum(sample * sample for sample in samples) / len(samples))
        return min(1.0, rms / 32768.0)

    def input_waveform(self, recording: Recording, bin_count: int = 44) -> list[float]:
        samples = self._recent_samples(recording, max(2048, bin_count * 64))
        if not samples:
            return [0.0] * bin_count
        values: list[float] = []
        chunk_size = max(1, len(samples) // bin_count)
        for index in range(bin_count):
            start = index * chunk_size
            end = len(samples) if index == bin_count - 1 else min(len(samples), start + chunk_size)
            chunk = samples[start:end]
            if not chunk:
                values.append(0.0)
                continue
            peak = max(abs(sample) for sample in chunk)
            rms = math.sqrt(sum(sample * sample for sample in chunk) / len(chunk))
            values.append(min(1.0, ((peak * 0.65) + (rms * 0.35)) / 32768.0))
        return values

    def _recent_samples(self, recording: Recording, sample_count: int) -> tuple[int, ...]:
        try:
            size = recording.path.stat().st_size
        except OSError:
            return ()
        header_size = 44
        if size <= header_size:
            return ()

        byte_count = min(sample_count * 2, size - header_size)
        offset = max(header_size, size - byte_count)
        if offset % 2:
            offset += 1
        try:
            with recording.path.open("rb") as file:
                file.seek(offset)
                data = file.read(byte_count)
        except OSError:
            return ()

        if len(data) < 2:
            return ()
        if len(data) % 2:
            data = data[:-1]
        sample_total = len(data) // 2
        try:
            return struct.unpack(f"<{sample_total}h", data)
        except struct.error:
            return ()

    def delete_temp(self, path: Path, preserve: bool = False) -> None:
        if preserve:
            return
        try:
            path.unlink(missing_ok=True)
        except OSError:
            pass

    def _record_command(self, path: Path) -> list[str]:
        if shutil.which("pw-record"):
            return [
                "pw-record",
                "--media-category=Capture",
                "--rate=16000",
                "--channels=1",
                "--format=s16",
                str(path),
            ]
        if shutil.which("parec"):
            return [
                "parec",
                "--file-format=wav",
                "--rate=16000",
                "--channels=1",
                str(path),
            ]
        if shutil.which("arecord"):
            return ["arecord", "-f", "S16_LE", "-r", "16000", "-c", "1", str(path)]
        if shutil.which("ffmpeg"):
            return [
                "ffmpeg",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "pulse",
                "-i",
                "default",
                "-ac",
                "1",
                "-ar",
                "16000",
                str(path),
            ]
        raise AudioError(
            "Could not access the default microphone. Install PipeWire, PulseAudio, ALSA, or ffmpeg recording tools."
        )
