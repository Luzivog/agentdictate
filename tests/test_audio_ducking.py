from __future__ import annotations

import subprocess
import unittest

from agentdictate.audio_ducking import AudioDucker, parse_sink_inputs
from agentdictate.config import Settings


class AudioDuckingTests(unittest.TestCase):
    def test_parse_sink_inputs_reads_active_and_corked_streams(self) -> None:
        output = """
Sink Input #7
\tCorked: no
\tVolume: front-left: 40000 /  61% / -12.88 dB,   front-right: 20000 /  31% / -31.00 dB
Sink Input #8
\tCorked: yes
\tVolume: mono: 65536 / 100% / 0.00 dB
"""
        streams = parse_sink_inputs(output)

        self.assertEqual(streams[0].stream_id, "7")
        self.assertEqual(streams[0].volumes, (40000, 20000))
        self.assertFalse(streams[0].corked)
        self.assertEqual(streams[1].stream_id, "8")
        self.assertEqual(streams[1].volumes, (65536,))
        self.assertTrue(streams[1].corked)

    def test_audio_ducker_fades_active_streams_and_restores_original_volume(self) -> None:
        class FakePactl:
            def __init__(self) -> None:
                self.streams = {"7": (40000, 20000), "8": (65536,)}
                self.corked = {"7": False, "8": True}
                self.set_calls: list[list[str]] = []

            def __call__(self, command: list[str], **_kwargs: object):
                if command[:3] == ["pactl", "list", "sink-inputs"]:
                    return subprocess.CompletedProcess(command, 0, stdout=self._output())
                if command[:2] == ["pactl", "set-sink-input-volume"]:
                    self.set_calls.append(command)
                    self.streams[command[2]] = tuple(int(value) for value in command[3:])
                    return subprocess.CompletedProcess(command, 0, stdout="")
                return subprocess.CompletedProcess(command, 1, stdout="")

            def _output(self) -> str:
                parts = []
                for stream_id, volumes in self.streams.items():
                    volume_text = ",   ".join(
                        f"front-{index}: {volume} /  50% / -18.00 dB"
                        for index, volume in enumerate(volumes)
                    )
                    corked = "yes" if self.corked[stream_id] else "no"
                    parts.append(
                        f"Sink Input #{stream_id}\n\tCorked: {corked}\n\tVolume: {volume_text}\n"
                    )
                return "\n".join(parts)

        fake = FakePactl()
        ducker = AudioDucker(
            runner=fake,
            which=lambda _name: "/usr/bin/pactl",
            sleep=lambda _seconds: None,
            async_fades=False,
        )
        settings = Settings(
            audio_ducking_enabled=True,
            audio_ducking_volume_percent=50,
            audio_ducking_fade_ms=100,
        )

        ducker.duck(settings)

        self.assertEqual(fake.streams["7"], (20000, 10000))
        self.assertEqual(fake.streams["8"], (65536,))
        self.assertTrue(all(call[2] == "7" for call in fake.set_calls))

        ducker.restore()

        self.assertEqual(fake.streams["7"], (40000, 20000))

    def test_audio_ducker_noops_when_disabled_or_unavailable(self) -> None:
        calls: list[list[str]] = []

        def runner(command: list[str], **_kwargs: object):
            calls.append(command)
            return subprocess.CompletedProcess(command, 0, stdout="")

        AudioDucker(
            runner=runner,
            which=lambda _name: None,
            async_fades=False,
        ).duck(Settings(audio_ducking_enabled=True))
        AudioDucker(
            runner=runner,
            which=lambda _name: "/usr/bin/pactl",
            async_fades=False,
        ).duck(Settings(audio_ducking_enabled=False))

        self.assertEqual(calls, [])
