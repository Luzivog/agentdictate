from __future__ import annotations

import shutil
import subprocess


def play_feedback(kind: str, enabled: bool = True) -> None:
    if not enabled:
        return
    if shutil.which("canberra-gtk-play"):
        sound_id = "message-new-instant" if kind == "start" else "complete"
        subprocess.run(
            ["canberra-gtk-play", "-i", sound_id],
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        return
    if shutil.which("paplay"):
        candidate = "/usr/share/sounds/freedesktop/stereo/message.oga"
        subprocess.run(
            ["paplay", candidate],
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
