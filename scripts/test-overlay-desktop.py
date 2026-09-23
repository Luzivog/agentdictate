#!/usr/bin/env python3
"""Exercise the real helper on a private, headless GNOME/Mutter desktop.

Requires GNOME Shell 46+, XWayland, gsettings, xrandr, xprop, xwininfo,
GTK 3, Tesseract, Python GI and Pillow, and the production clipboard owner's
test client (`cargo build -p agentdictate-linux --example selection_probe`),
found beside the binary unless --selection-probe names it. No user-session
windows, audio, configuration, or bus services are used, and the paste chord
goes only to the private compositor's own virtual keyboard. The
session-service stub only acknowledges GNOME's startup target; rendering uses
the real compositor.
"""

import argparse
import ctypes
import json
import math
import os
from pathlib import Path
import re
import select
import signal
import subprocess
import sys
import tempfile
import time
import wave

from gi.repository import Gio, GLib
from PIL import Image


def session_services():
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    xml = """<node><interface name="org.freedesktop.systemd1.Manager">
    <method name="GetUnit"><arg type="s" direction="in"/><arg type="o" direction="out"/></method>
    <method name="StartUnit"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="o" direction="out"/></method>
    <method name="StopUnit"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="o" direction="out"/></method>
    <signal name="JobRemoved"><arg type="u"/><arg type="o"/><arg type="s"/><arg type="s"/></signal>
    </interface></node>"""

    def call(conn, sender, path, interface, method, params, invocation):
        if method == "GetUnit":
            invocation.return_value(GLib.Variant("(o)", ("/org/freedesktop/systemd1/unit/test",)))
            return
        job = "/org/freedesktop/systemd1/job/1"
        invocation.return_value(GLib.Variant("(o)", (job,)))

        def complete():
            conn.emit_signal(None, path, interface, "JobRemoved",
                             GLib.Variant("(uoss)", (1, job, params.unpack()[0], "done")))
            return False

        GLib.idle_add(complete)

    bus.register_object("/org/freedesktop/systemd1", Gio.DBusNodeInfo.new_for_xml(xml).interfaces[0], call, None, None)
    Gio.bus_own_name_on_connection(bus, "org.freedesktop.systemd1", Gio.BusNameOwnerFlags.NONE, None, None)
    GLib.MainLoop().run()


def typing_target(backend, root):
    import gi
    gi.require_version("Gtk", "3.0")
    gi.require_version("Gdk", "3.0")
    from gi.repository import Gdk, Gtk
    window = Gtk.Window(title=f"Overlay test target ({backend})")
    window.set_default_size(320, 120)
    entry = Gtk.Entry(text="Synthetic typing target")
    window.add(entry)
    window.connect("destroy", Gtk.main_quit)
    window.show_all()
    entry.grab_focus()
    request = root / "clipboard-request"
    result = root / "clipboard-result"
    def check_clipboard():
        if request.exists():
            request.unlink()
            values = [Gtk.Clipboard.get(selection).wait_for_text()
                      for selection in [Gdk.SELECTION_CLIPBOARD, Gdk.SELECTION_PRIMARY]]
            result.write_text(json.dumps({"selections": values, "entry": entry.get_text()}))
        return True
    GLib.timeout_add(30, check_clipboard)
    Gtk.main()


def target_state(root):
    """The typing target's CLIPBOARD and PRIMARY text and its entry text."""
    result = root / "clipboard-result"
    result.unlink(missing_ok=True)
    (root / "clipboard-request").touch()
    return json.loads(wait_until(lambda: result.read_text() if result.exists() else None,
                                 "typing target state"))


# Linux evdev key codes, as the daemon's uinput paste keyboard sends them.
KEYS = {"shift": 42, "ctrl": 29, "insert": 110, "v": 47}


class SelectionProbe:
    """The production clipboard owner, driven line by line (see its source)."""

    def __init__(self, program, env):
        self.process = subprocess.Popen([str(program)], env=env, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, text=True)

    def ask(self, line):
        self.process.stdin.write(line + "\n")
        self.process.stdin.flush()
        return self.process.stdout.readline().strip()

    def close(self):
        self.process.stdin.close()
        self.process.wait(timeout=3)


def wait_until(check, description, timeout=8):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(0.02)
    raise AssertionError(f"Timed out: {description}")


class Desktop:
    def __init__(self, root, monitors):
        self.root = root
        self.processes = []
        self.bus_pid = None
        self.env = {key: value for key, value in os.environ.items()
                    if key not in {"DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY", "SESSION_MANAGER",
                                   "GNOME_SHELL_JS", "AGENTDICTATE_OVERLAY_WORK_AREA"}}
        runtime = root / "runtime"
        runtime.mkdir(mode=0o700)
        for key, folder in [("XDG_RUNTIME_DIR", "runtime"), ("XDG_CONFIG_HOME", "config"),
                            ("XDG_CACHE_HOME", "cache"), ("XDG_DATA_HOME", "data"),
                            ("XDG_STATE_HOME", "state")]:
            self.env[key] = str(root / folder)
        self.env.update(GSETTINGS_BACKEND="keyfile", GNOME_SHELL_SESSION_MODE="user")
        config = root / "bus.conf"
        config.write_text(f'<busconfig><type>session</type><listen>unix:tmpdir={root}</listen>'
                          '<policy context="default"><allow send_destination="*"/>'
                          '<allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
        bus = subprocess.check_output(["dbus-daemon", f"--config-file={config}", "--fork",
                                       "--print-address=1", "--print-pid=1"], text=True).splitlines()
        self.bus_pid = int(bus[1])
        self.env["DBUS_SESSION_BUS_ADDRESS"] = bus[0]
        self.bus = Gio.DBusConnection.new_for_address_sync(
            bus[0], Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
            None, None)
        self.spawn([sys.executable, str(Path(__file__).resolve()), "--session-services"], "services.log")
        wait_until(lambda: self.dbus("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                                   "NameHasOwner", GLib.Variant("(s)", ("org.freedesktop.systemd1",)))[0],
                   "isolated session services")
        extension = root / "data/gnome-shell/extensions/overlay-probe@local"
        extension.mkdir(parents=True)
        version = re.search(r"(\d+)\.", subprocess.check_output(["gnome-shell", "--version"], text=True))[1]
        (extension / "metadata.json").write_text(json.dumps({"uuid": "overlay-probe@local", "name": "Overlay test",
            "description": "Inspection inside a private test session", "shell-version": [version]}))
        (extension / "extension.js").write_text(
            'import {Extension} from "resource:///org/gnome/shell/extensions/extension.js";\n'
            'import * as Main from "resource:///org/gnome/shell/ui/main.js";\n'
            'export default class Probe extends Extension {\n'
            'enable() { global.context.unsafe_mode = true; Main.overview.hide(); }\n'
            'disable() {} }\n')
        self.run(["gsettings", "set", "org.gnome.shell", "enabled-extensions", "['overlay-probe@local']"])
        self.run(["gsettings", "set", "org.gnome.shell", "disable-user-extensions", "false"])
        # A solid mid-tone desktop makes transparent overlay corners easy to
        # tell apart from the card's near-black fill.
        for key, value in [("picture-options", "none"), ("color-shading-type", "solid"),
                           ("primary-color", "#3c5a82")]:
            self.run(["gsettings", "set", "org.gnome.desktop.background", key, value])
        args = ["gnome-shell", "--wayland", "--headless", "--sm-disable"]
        for monitor in monitors:
            args += ["--virtual-monitor", monitor]
        self.spawn(args, "shell.log")

        def initialized():
            try:
                return self.evaluate("global.context.unsafe_mode")
            except GLib.Error:
                return False

        wait_until(initialized, "GNOME private inspection interface", 12)
        # Created now because the compositor adds a new input device
        # asynchronously and drops its first events.
        seat = "imports.gi.Clutter.get_default_backend().get_default_seat()"
        assert self.evaluate(f"Boolean(global.pasteKeyboard = {seat}.create_virtual_device(1))")
        text = (root / "shell.log").read_text()
        self.env["DISPLAY"] = re.search(r"public X11 display (:\d+)", text)[1]
        self.env["XAUTHORITY"] = str(next(runtime.glob(".mutter-Xwaylandauth.*")))
        self.env["XDG_SESSION_TYPE"] = "x11"
        self.run(["xrandr", "--listmonitors"])

    def spawn(self, args, log_name, env=None):
        with (self.root / log_name).open("w") as log:
            p = subprocess.Popen(args, env=env or self.env, stdout=log, stderr=log, start_new_session=True)
        self.processes.append(p)
        return p

    def run(self, args):
        return subprocess.check_output(args, env=self.env, text=True, stderr=subprocess.STDOUT, timeout=8)

    def dbus(self, dest, path, interface, method, params=None):
        return self.bus.call_sync(dest, path, interface, method, params, None,
                                  Gio.DBusCallFlags.NONE, 2000, None).unpack()

    def evaluate(self, code):
        success, result = self.dbus("org.gnome.Shell", "/org/gnome/Shell", "org.gnome.Shell", "Eval",
                                    GLib.Variant("(s)", (code,)))
        return json.loads(result) if success and result not in {"", "undefined"} else None

    def press_chord(self, *keys):
        """Presses `keys` in order and releases them in reverse, paced like the
        daemon's chord, on the private compositor's virtual keyboard."""
        events = [(KEYS[key], 1) for key in keys] + [(KEYS[key], 0) for key in reversed(keys)]
        for index, (code, state) in enumerate(events):
            if index:
                time.sleep(.025)
            assert self.evaluate("Boolean(global.pasteKeyboard.notify_key("
                                 f"imports.gi.GLib.get_monotonic_time(), {code}, {state}) ?? true)")

    def screenshot(self, name):
        path = self.root / name
        success, _ = self.dbus("org.gnome.Shell.Screenshot", "/org/gnome/Shell/Screenshot",
            "org.gnome.Shell.Screenshot", "Screenshot", GLib.Variant("(bbs)", (False, False, str(path))))
        assert success, "composited screenshot unavailable"
        return Image.open(path).convert("RGB")

    def set_primary(self, connector):
        destination = "org.gnome.Mutter.DisplayConfig"
        path = "/org/gnome/Mutter/DisplayConfig"
        serial, monitors, logical, _ = self.dbus(destination, path, destination, "GetCurrentState")
        modes = {spec[0]: next(mode[0] for mode in modes if mode[-1].get("is-current"))
                 for spec, modes, _ in monitors}
        configuration = [(x, y, scale, transform, any(spec[0] == connector for spec in group),
                          [(spec[0], modes[spec[0]], {}) for spec in group])
                         for x, y, scale, transform, _, group, _ in logical]
        # Temporary configuration on the private compositor; no user settings.
        self.dbus(destination, path, destination, "ApplyMonitorsConfig",
                  GLib.Variant("(uua(iiduba(ssa{sv}))a{sv})", (serial, 1, configuration, {})))

    def close(self):
        for process in reversed(self.processes):
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
        if self.bus_pid:
            os.kill(self.bus_pid, signal.SIGTERM)


def geometry(desktop, window):
    info = desktop.run(["xwininfo", "-id", window])
    def value(label):
        return int(re.search(label + r":\s+(-?\d+)", info)[1])
    return [value("Absolute upper-left X"), value("Absolute upper-left Y"), value("Width"), value("Height")]


def input_rectangles(desktop, window):
    """How many rectangles make up the window's X input region; zero means
    every click passes through to whatever is below."""
    x11 = ctypes.CDLL("libX11.so.6")
    xext = ctypes.CDLL("libXext.so.6")
    x11.XOpenDisplay.restype = ctypes.c_void_p
    x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
    xext.XShapeGetRectangles.restype = ctypes.c_void_p
    xext.XShapeGetRectangles.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int,
                                         ctypes.POINTER(ctypes.c_int), ctypes.POINTER(ctypes.c_int)]
    os.environ["XAUTHORITY"] = desktop.env["XAUTHORITY"]
    display = x11.XOpenDisplay(desktop.env["DISPLAY"].encode())
    assert display, "could not open the private X display"
    count, ordering = ctypes.c_int(), ctypes.c_int()
    shape_input = 2
    xext.XShapeGetRectangles(display, int(window, 16), shape_input, ctypes.byref(count), ctypes.byref(ordering))
    x11.XCloseDisplay(display)
    return count.value


def helper_window(desktop):
    tree = desktop.run(["xwininfo", "-root", "-tree"])
    match = re.search(r'(0x[0-9a-f]+).*\("local.agentdictate.AgentDictate"', tree)
    return match[1] if match else None


def expected_frame(desktop, scale, primary=None):
    monitors = desktop.run(["xrandr", "--listmonitors"]).splitlines()[1:]
    selected = next((line for line in monitors if "*" in line), monitors[0]) if primary is None else monitors[primary]
    width, height, x, y = map(int, re.search(r"(\d+)/\d+x(\d+)/\d+([+-]\d+)([+-]\d+)", selected).groups())
    area = desktop.run(["xprop", "-root", "_NET_WORKAREA"])
    if "=" in area:
        ax, ay, aw, ah = map(int, area.split("=", 1)[1].split(",")[:4])
        right, bottom = min(x + width, ax + aw), min(y + height, ay + ah)
        x, y = max(x, ax), max(y, ay)
        width, height = right - x, bottom - y
    w, h, gap = [round(n * scale) for n in [220, 56, 72]]
    return [x + (width - w) // 2, y + height - gap - h, w, h]


def assert_transparent_corners(desktop, window, baseline, scale):
    """The window corners outside the rounded card must show the desktop
    unchanged, and the card body must be dark, so the overlay really composites
    as a transparent surface rather than an opaque rectangle."""
    image = desktop.screenshot("transparency.png")
    x, y, w, h = geometry(desktop, window)
    corners = {}
    for cx, cy in [(1, 1), (w - 2, 1), (1, h - 2), (w - 2, h - 2)]:
        actual, expected = image.getpixel((x + cx, y + cy)), baseline.getpixel((x + cx, y + cy))
        assert all(abs(a - e) <= 6 for a, e in zip(actual, expected)), \
            f"overlay corner ({cx}, {cy}) covers the desktop: {actual}, desktop {expected}"
        corners[f"{cx},{cy}"] = actual
    # Card-local (4, 21): inside the 127-wide recording card, centered in
    # the 220-wide window, left of the waveform.
    card = image.getpixel((x + round(50 * scale), y + round(27 * scale)))
    assert max(card) < 0x40, f"overlay card body is not dark: {card}"
    return {"desktop": baseline.getpixel((x + 1, y + 1)), "corners": corners, "card": card}


def exercise(desktop, binary, probe_program, scale, monitors, backend):
    import struct
    audio = desktop.root / "fixture.wav"
    with wave.open(str(audio), "wb") as writer:
        writer.setparams((1, 2, 16000, 0, "NONE", "not compressed"))
        writer.writeframes(b"".join(struct.pack("<h", round(16000 * math.sin(i * .07))) for i in range(8000)))
    # RUST_LOG=info keeps the renderer's adapter choice in the helper log.
    env = {**desktop.env, "GPUI_X11_SCALE_FACTOR": str(scale), "RUST_LOG": "info"}
    target_env = {**desktop.env, "GDK_BACKEND": backend, "WAYLAND_DISPLAY": next(p.name for p in (desktop.root / "runtime").glob("wayland-*") if not p.name.endswith(".lock"))}
    target = desktop.spawn([sys.executable, str(Path(__file__).resolve()), "--typing-target", backend,
                            str(desktop.root)], f"target-{backend}.log", target_env)
    title = f"Overlay test target ({backend})"
    lookup = f"global.get_window_actors().find(w => w.meta_window.get_title() === {json.dumps(title)})?.meta_window"
    try:
        wait_until(lambda: desktop.evaluate(f"Boolean({lookup})"), "typing target mapped")
    except AssertionError:
        raise AssertionError((desktop.root / f"target-{backend}.log").read_text())
    desktop.evaluate(f"({lookup}).activate(global.get_current_time())")
    wait_until(lambda: desktop.evaluate("global.display.focus_window?.get_title() ?? null") == title,
               "typing target focused")
    focus_before = desktop.evaluate("global.display.focus_window?.get_title() ?? null")
    clients_before = desktop.run(["xprop", "-root", "_NET_CLIENT_LIST"])
    probe = SelectionProbe(probe_program, desktop.env)
    baseline = desktop.screenshot("baseline.png")
    spawned = time.monotonic()
    helper = subprocess.Popen([str(binary), "--overlay-helper"], env=env, stdin=subprocess.PIPE,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    statuses = []
    status_ms = {}
    def send(phase):
        job_id = "00000000-0000-4000-8000-000000000001"
        if phase == "notice":
            # A dictation that failed: the card turns into its notice.
            update = {"workflow": {"phase": {"phase": "needs_attention", "job_id": job_id, "at": "failed",
                                             "failure": "offline"}},
                      "active_recording": None, "notice": {"notice": "failed", "failure": "offline"}}
        else:
            state = {"phase": phase if phase in {"starting", "recording"} else "processing", "job_id": job_id}
            if state["phase"] == "processing":
                state["stage"] = phase
            recording = None if phase == "starting" else {
                "audio_path": str(audio), "started_at_unix_millis": round(time.time() * 1000) - 2000}
            update = {"workflow": {"phase": state}, "active_recording": recording}
        helper.stdin.write(json.dumps(update) + "\n")
        helper.stdin.flush()
    try:
        # The daemon launches the helper at the start request, before audio exists.
        send("starting")
        deadline = time.monotonic() + 8
        # Unbuffered pipe reads avoid hiding a second status line in TextIO buffering.
        pending = b""
        while time.monotonic() < deadline:
            if not select.select([helper.stdout], [], [], .1)[0]:
                continue
            chunk = os.read(helper.stdout.fileno(), 4096)
            if not chunk:
                break
            pending += chunk
            while b"\n" in pending:
                line, pending = pending.split(b"\n", 1)
                statuses.append(json.loads(line))
                status_ms.setdefault(statuses[-1]["status"], round((time.monotonic() - spawned) * 1000))
            if any(s["status"] in {"frame_submitted", "ready", "error"} for s in statuses):
                break
        window = wait_until(lambda: helper_window(desktop), "overlay window")
        expected = expected_frame(desktop, scale)
        actual = geometry(desktop, window)
        assert actual == expected, f"wrong primary-monitor placement: actual={actual}, expected={expected}, statuses={statuses}"
        assert any(s["status"] == "frame_submitted" for s in statuses), statuses
        # The daemon pastes during the fade only on this per-launch confirmation.
        assert {"status": "window_created", "override_redirect": True} in statuses, statuses
        info = desktop.run(["xwininfo", "-id", window])
        assert "Override Redirect State: yes" in info and "Map State: IsViewable" in info
        # The transparent margin around the card must never take a click.
        assert input_rectangles(desktop, window) == 0, "the overlay takes clicks"
        assert window not in desktop.run(["xprop", "-root", "_NET_CLIENT_LIST"])
        recognized = {}
        for phase in ["recording", "transcribing", "notice"]:
            send(phase)
            def visible():
                im = desktop.screenshot(f"{phase}.png")
                x, y, w, h = geometry(desktop, window)
                crop = im.crop((x, y, x + w, y + h))
                if sum(max(pixel) > 150 for pixel in crop.getdata()) <= 30 * scale:
                    return False
                if phase == "recording":
                    return sum(r > 150 and r > 2 * g and r > 2 * b for r, g, b in crop.getdata()) > 20 * scale
                path = desktop.root / "label.png"
                crop.resize((w * 4, h * 4)).save(path)
                layout = "7" if phase == "transcribing" else "6"
                text = subprocess.check_output(["tesseract", str(path), "stdout", "--psm", layout],
                                                stderr=subprocess.DEVNULL, text=True).lower()
                recognized[phase] = text.strip()
                return "transcribing" in text if phase == "transcribing" else "saved to recovery" in text
            wait_until(visible, f"composited {phase} pixels")
            if phase == "recording":
                transparency = assert_transparent_corners(desktop, window, baseline, scale)
            assert desktop.evaluate("global.display.focus_window?.get_title() ?? null") == focus_before
        # Copying without pasting publishes the clipboard alone. Mutter offers
        # a new X selection to Wayland clients a moment after its owner changes.
        assert probe.ask("publish clipboard copied fixture") == "published"
        wait_until(lambda: target_state(desktop.root)["selections"][0] == "copied fixture",
                   "copied text on the target's clipboard")
        fixture = "overlay clipboard fixture déjà vu"
        assert probe.ask("pressed") == "marked"
        assert probe.ask(f"publish both {fixture}") == "published"
        # Mutter saves each new clipboard as soon as it is published. That
        # fetch comes before the paste key, so it never acknowledges a paste.
        eager_save = probe.ask("requested 500")
        assert eager_save.startswith("requested "), eager_save
        assert probe.ask("pressed") == "marked"
        assert probe.ask("requested 200") == "not-requested"
        # The paste comes first: toolkits may answer a later paste of the
        # same offer from the copy an earlier read cached.
        assert probe.ask("pressed") == "marked"
        # The daemon's Automatic chord for every target.
        chord = ("shift", "insert")
        desktop.press_chord(*chord)
        acknowledgement = probe.ask("requested 1000")
        assert acknowledgement.startswith("requested "), acknowledgement
        entry = wait_until(lambda: (lambda text: text if fixture in text else None)(
            target_state(desktop.root)["entry"]), "pasted text in the typing target")
        values = target_state(desktop.root)["selections"]
        assert values == [fixture, fixture], values
        assert desktop.evaluate("global.display.focus_window?.get_title() ?? null") == focus_before
        assert desktop.run(["xprop", "-root", "_NET_CLIENT_LIST"]) == clients_before
        if len(monitors) > 1:
            desktop.set_primary("Meta-1")
            try:
                wait_until(lambda: geometry(desktop, window) == expected_frame(desktop, scale), "primary change placement")
            except AssertionError:
                trace = "\n".join(line for path in (desktop.root / "state").rglob("*.log.*")
                                  for line in path.read_text().splitlines() if "overlay" in line)[-4000:]
                raise AssertionError(f"primary change: actual={geometry(desktop, window)}, expected={expected_frame(desktop, scale)}\n"
                                     + desktop.run(["xrandr", "--listmonitors"]) + trace)
        desktop.run(["xprop", "-root", "-f", "_NET_WORKAREA", "32c", "-set", "_NET_WORKAREA", "0, 32, 5440, 760"])
        wait_until(lambda: geometry(desktop, window) == expected_frame(desktop, scale), "work-area change placement")
        start = time.monotonic()
        if backend == "wayland":
            helper.stdin.write(json.dumps({"workflow": {"phase": {"phase": "ready"}}, "active_recording": None}) + "\n")
            helper.stdin.flush()
        else:
            helper.stdin.close()
        # The paste lands while the card fades, so focus must hold during the fade too.
        assert desktop.evaluate("global.display.focus_window?.get_title() ?? null") == focus_before
        helper.wait(timeout=2)
        assert helper.returncode == 0, helper.stderr.read()
        assert not helper_window(desktop), "helper window survived dismissal"
        # The helper writes into the daemon's log after every dictation, so
        # its teardown must not log a closed window as an error.
        errors = [line for path in (desktop.root / "state").rglob("agentdictated.log*")
                  for line in path.read_text().splitlines() if "window not found" in line]
        assert not errors, errors
        assert desktop.evaluate("global.display.focus_window?.get_title() ?? null") == focus_before
        assert desktop.run(["xprop", "-root", "_NET_CLIENT_LIST"]) == clients_before
        adapters = [line[line.index("Selected GPU"):]
                    for path in (desktop.root / "state").rglob("agentdictated.log*")
                    for line in path.read_text().splitlines() if "Selected GPU" in line]
        return {"binary": str(binary), "scale": scale, "statuses": statuses, "initial_frame": actual,
                "target": backend, "recognized_labels": recognized, "transparency": transparency,
                "gpu_adapter": adapters, "status_ms_since_launch": status_ms,
                "clipboard": {"target_selections": values, "paste_chord": "+".join(chord), "entry": entry,
                              "eager_save_ms_after_publish_start": int(eager_save.split()[1]),
                              "request_ms_after_chord_start": int(acknowledgement.split()[1])},
                "dismissal_ms": round((time.monotonic() - start) * 1000)}
    finally:
        if helper.poll() is None:
            helper.kill()
            helper.wait()
        probe.close()
        target.terminate()
        target.wait(timeout=3)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--scale", type=float, default=1)
    parser.add_argument("--monitor", action="append")
    parser.add_argument("--target", choices=["wayland", "x11"], default="wayland")
    parser.add_argument("--selection-probe", type=Path)
    args = parser.parse_args()
    probe = (args.selection_probe or args.binary.resolve().parent / "examples" / "selection_probe").resolve()
    if not probe.is_file():
        parser.error(f"{probe} is missing; build it with "
                     "`cargo build -p agentdictate-linux --example selection_probe` or pass --selection-probe")
    monitors = args.monitor or ["1920x1080", "1600x900", "1920x1200"]
    with tempfile.TemporaryDirectory(prefix="agentdictate-overlay-desktop-") as directory:
        desktop = Desktop.__new__(Desktop)
        try:
            desktop.__init__(Path(directory), monitors)
            print(json.dumps(exercise(desktop, args.binary.resolve(), probe, args.scale, monitors, args.target),
                             indent=2, ensure_ascii=False))
        finally:
            desktop.close()


if __name__ == "__main__":
    if sys.argv[1:] == ["--session-services"]:
        session_services()
    elif sys.argv[1:2] == ["--typing-target"]:
        typing_target(sys.argv[2], Path(sys.argv[3]))
    else:
        main()
