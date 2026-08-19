from __future__ import annotations

import subprocess
import threading
import unittest

from agentdictate.audio_ducking import AudioDucker, parse_sink_inputs
from agentdictate.config import Settings


class FakePactl:
    def __init__(
        self,
        streams: dict[str, tuple[int, ...]] | None = None,
        corked: dict[str, bool] | None = None,
    ) -> None:
        self.streams = streams or {}
        self.corked = corked or {
            stream_id: False for stream_id in self.streams
        }
        self.set_calls: list[list[str]] = []
        self.list_calls = 0

    def __call__(self, command: list[str], **_kwargs: object):
        if command[:3] == ["pactl", "list", "sink-inputs"]:
            self.list_calls += 1
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
            corked = "yes" if self.corked.get(stream_id, False) else "no"
            parts.append(
                f"Sink Input #{stream_id}\n\tCorked: {corked}\n\tVolume: {volume_text}\n"
            )
        return "\n".join(parts)


class FakeSubscriberProcess:
    def __init__(self, lines: list[str]) -> None:
        self.stdout = iter(lines)
        self.returncode: int | None = None
        self.terminated = threading.Event()

    def poll(self) -> int | None:
        return self.returncode

    def terminate(self) -> None:
        self.returncode = -15
        self.terminated.set()


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
        fake = FakePactl(
            streams={"7": (40000, 20000), "8": (65536,)},
            corked={"7": False, "8": True},
        )
        ducker = AudioDucker(
            runner=fake,
            which=lambda _name: "/usr/bin/pactl",
            sleep=lambda _seconds: None,
            async_fades=False,
            monitor_streams=False,
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

    def test_audio_ducker_ducks_stream_created_after_recording_starts(self) -> None:
        fake = FakePactl()
        ducker = AudioDucker(
            runner=fake,
            which=lambda _name: "/usr/bin/pactl",
            sleep=lambda _seconds: None,
            async_fades=False,
            monitor_streams=False,
        )
        settings = Settings(
            audio_ducking_enabled=True,
            audio_ducking_volume_percent=25,
            audio_ducking_fade_ms=0,
        )

        ducker.duck(settings)
        fake.streams["9"] = (40000, 20000)
        fake.corked["9"] = False
        ducker._handle_subscription_event("Event 'new' on sink-input #9")

        self.assertEqual(fake.streams["9"], (10000, 5000))

        ducker.restore()

        self.assertEqual(fake.streams["9"], (40000, 20000))

    def test_audio_ducker_subscription_reconciles_new_stream(self) -> None:
        fake = FakePactl()
        process = FakeSubscriberProcess(["Event 'new' on sink-input #9\n"])

        def popen(_command: list[str], **_kwargs: object):
            fake.streams["9"] = (40000, 20000)
            fake.corked["9"] = False
            return process

        ducker = AudioDucker(
            runner=fake,
            popen=popen,  # type: ignore[arg-type]
            which=lambda _name: "/usr/bin/pactl",
            sleep=lambda _seconds: None,
            async_fades=False,
        )
        settings = Settings(
            audio_ducking_enabled=True,
            audio_ducking_volume_percent=25,
            audio_ducking_fade_ms=0,
        )

        ducker.duck(settings)

        self.assertTrue(process.terminated.wait(timeout=1))
        self.assertEqual(fake.streams["9"], (10000, 5000))

        ducker.restore()

        self.assertEqual(fake.streams["9"], (40000, 20000))

    def test_audio_ducker_ducks_corked_stream_when_it_becomes_active(self) -> None:
        fake = FakePactl(streams={"8": (65536,)}, corked={"8": True})
        ducker = AudioDucker(
            runner=fake,
            which=lambda _name: "/usr/bin/pactl",
            sleep=lambda _seconds: None,
            async_fades=False,
            monitor_streams=False,
        )
        settings = Settings(
            audio_ducking_enabled=True,
            audio_ducking_volume_percent=50,
            audio_ducking_fade_ms=0,
        )

        ducker.duck(settings)
        self.assertEqual(fake.streams["8"], (65536,))
        fake.corked["8"] = False
        ducker._handle_subscription_event("Event 'change' on sink-input #8")

        self.assertEqual(fake.streams["8"], (32768,))

        ducker.restore()

        self.assertEqual(fake.streams["8"], (65536,))

    def test_audio_ducker_reapplies_ducking_when_known_stream_resets_volume(self) -> None:
        fake = FakePactl(streams={"7": (40000, 20000)})
        ducker = AudioDucker(
            runner=fake,
            which=lambda _name: "/usr/bin/pactl",
            sleep=lambda _seconds: None,
            async_fades=False,
            monitor_streams=False,
        )
        settings = Settings(
            audio_ducking_enabled=True,
            audio_ducking_volume_percent=50,
            audio_ducking_fade_ms=0,
        )

        ducker.duck(settings)
        self.assertEqual(fake.streams["7"], (20000, 10000))
        fake.streams["7"] = (40000, 20000)
        ducker._handle_subscription_event("Event 'change' on sink-input #7")

        self.assertEqual(fake.streams["7"], (20000, 10000))

        ducker.restore()

        self.assertEqual(fake.streams["7"], (40000, 20000))

    def test_audio_ducker_ignores_unrelated_subscription_events(self) -> None:
        fake = FakePactl(streams={"7": (40000,)})
        ducker = AudioDucker(
            runner=fake,
            which=lambda _name: "/usr/bin/pactl",
            async_fades=False,
            monitor_streams=False,
        )
        ducker.duck(Settings(audio_ducking_fade_ms=0))
        list_calls = fake.list_calls

        ducker._handle_subscription_event("Event 'change' on source #2")
        ducker._handle_subscription_event("Event 'remove' on sink-input #7")

        self.assertEqual(fake.list_calls, list_calls)

    def test_audio_ducker_noops_when_disabled_or_unavailable(self) -> None:
        calls: list[list[str]] = []

        def runner(command: list[str], **_kwargs: object):
            calls.append(command)
            return subprocess.CompletedProcess(command, 0, stdout="")

        AudioDucker(
            runner=runner,
            which=lambda _name: None,
            async_fades=False,
            monitor_streams=False,
        ).duck(Settings(audio_ducking_enabled=True))
        AudioDucker(
            runner=runner,
            which=lambda _name: "/usr/bin/pactl",
            async_fades=False,
            monitor_streams=False,
        ).duck(Settings(audio_ducking_enabled=False))

        self.assertEqual(calls, [])
