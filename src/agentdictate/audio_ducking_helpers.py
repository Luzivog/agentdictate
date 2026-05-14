from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Sequence

PACTL_VOLUME = 65536


@dataclass(frozen=True)
class SinkInput:
    stream_id: str
    volumes: tuple[int, ...]
    corked: bool = False


def clamp_audio_ducking_volume(value: object) -> int:
    try:
        number = int(value)
    except (TypeError, ValueError):
        number = 15
    return max(0, min(100, number))


def clamp_audio_ducking_fade_ms(value: object) -> int:
    try:
        number = int(value)
    except (TypeError, ValueError):
        number = 1000
    return max(0, min(5000, number))


def parse_sink_inputs(output: str) -> list[SinkInput]:
    streams: list[SinkInput] = []
    stream_id: str | None = None
    volumes: tuple[int, ...] = ()
    corked = False

    def flush() -> None:
        nonlocal stream_id, volumes, corked
        if stream_id is not None and volumes:
            streams.append(SinkInput(stream_id=stream_id, volumes=volumes, corked=corked))
        stream_id = None
        volumes = ()
        corked = False

    for line in output.splitlines():
        match = re.match(r"^Sink Input #(\d+)", line)
        if match:
            flush()
            stream_id = match.group(1)
            continue

        stripped = line.strip()
        if stripped.startswith("Corked:"):
            corked = stripped.split(":", 1)[1].strip().lower() == "yes"
        elif stripped.startswith("Volume:") and not volumes:
            values = re.findall(r":\s*(\d+)\s*/", stripped)
            volumes = tuple(int(value) for value in values)

    flush()
    return streams


def fade_steps(fade_ms: int) -> int:
    if fade_ms <= 0:
        return 1
    return max(1, min(50, int(round(fade_ms / 50))))


def fit_volume_count(volumes: Sequence[int], count: int) -> tuple[int, ...]:
    if count <= 0:
        return ()
    if len(volumes) == count:
        return tuple(volumes)
    if not volumes:
        return tuple(PACTL_VOLUME for _index in range(count))
    if len(volumes) == 1:
        return tuple(volumes[0] for _index in range(count))
    if len(volumes) > count:
        return tuple(volumes[:count])
    return tuple(volumes) + tuple(volumes[-1] for _index in range(count - len(volumes)))
