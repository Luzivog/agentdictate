# Develop AgentDictate

[AGENTS.md](../AGENTS.md) holds the repository rules: work on `main`, the narrow Cargo
commands, disk and build coordination, the final `./run-tests.sh` gate, and delivery.
This guide covers the practical side: setting up a host, running a development
build, testing, checking the overlay on a real compositor, and debugging.
[The architecture overview](architecture.md) explains how the pieces fit.

## Set up a host

Install the packages from [the install guide](INSTALL.md#requirements) and Rust
through rustup, then run:

```bash
scripts/dev.sh doctor
```

It reports build tools, development library metadata, optional tools, Git status,
worktrees, disk space, and running Cargo or linker processes. A nonzero exit means a
build prerequisite is missing. It does not install packages or check native input
access.

### Hosts without the xkbcommon development packages

GPUI links `libxkbcommon.so` and `libxkbcommon-x11.so` by their development names,
which only `libxkbcommon-dev` and `libxkbcommon-x11-dev` provide.
`packaging/linker-runtime-fallback.sh` works around a missing package: it points
symlinks in `target/linker-shims` at the installed runtime libraries and adds that
directory to `LIBRARY_PATH`. `install.sh`, `run.sh`, and `run-tests.sh` source
it for you.

Direct Cargo commands that link GPUI need it too. That means anything built with
the `desktop` or `test-support` features of `agentdictate-ui`, including the
`agentdictate` binary. Once the shims exist, export the path in the same shell:

```bash
export LIBRARY_PATH="$PWD/target/linker-shims"
cargo test --locked -p agentdictate-ui --test desktop --features test-support
```

Or create the shims and run one command in a subshell:

```bash
(
  PROJECT_DIR="$PWD"
  source packaging/linker-runtime-fallback.sh
  cargo check --locked -p agentdictate-ui --features desktop
)
```

A build that links this way proves that the target links, not that a clean host
has every packaging prerequisite.

## Run a development build

`./run.sh` builds both binaries and runs them as an isolated instance, so it
never replaces, restarts, or reconfigures your installed AgentDictate:

- `AGENTDICTATE_HOME` defaults to `target/dev-home`. The instance keeps its
  settings, database, logs, cache, and IPC socket in `config`, `data`,
  `state`, `cache`, and `runtime` there. Export it yourself to use another
  directory.
- Its daemon runs directly, not as `agentdictated.service`, and never calls
  `systemctl`. **Start on login** has no effect on it.
- `./run.sh` starts that daemon in the background and opens the settings
  window. Closing the window stops the daemon. `./run.sh --service` runs only
  the daemon, in the foreground; stop it with Ctrl+C.

A new instance starts with default settings and no API key, so enter one in its
window. It shows its own tray icon. Both daemons read the keyboard, so a
shortcut they share triggers both: give the dev instance another shortcut, or
stop the installed one with `systemctl --user stop agentdictated.service` while
you test and start it again afterwards.

## Test

Each crate has its unit tests beside the code and one or two integration harnesses
in `tests/`: `core`, `runtime`, `linux`, `app`, and, for the UI, `contracts`
(view models) and `desktop` (headless GPUI, needs `--features test-support`). The
desktop tests drive rendered controls in a headless GPUI context; they never open a
window or move your mouse.

Run one harness at a time with the narrow commands from
[AGENTS.md](../AGENTS.md#development-commands), adding a test-name filter as needed:

```bash
cargo test --locked -p agentdictate-core --lib textfmt
cargo test --locked -p agentdictate-runtime --test runtime history_usage
cargo test --locked -p agentdictate-app --test app daemon_flow
cargo test --locked -p agentdictate-ui --test desktop --features test-support rendered_interactions
```

A filter that matches nothing still passes, with `0 passed`, so check the count.

Tests that need `/dev/uinput` create and grab their own virtual keyboard, so their
key presses never reach your desktop. Without access they print `SKIPPED` and pass,
so a passing run on a host without access proves less.

To measure how a transcription change affects output, use `agentdictate-evaluate`
as described in [dictation output](dictation-output.md#evaluate-a-change).

## Check the overlay and paste on a real compositor

The automated tests cannot prove that the overlay is visible, where it appears, or
that a paste reaches another app. `scripts/test-overlay-desktop.py` checks those
on a private, headless GNOME Shell. It runs the production overlay helper with
synthetic audio and workflow updates, and the production clipboard owner through
the `selection_probe` example. It uses a private session bus and temporary XDG
directories, and sends its paste only to the private compositor's own virtual
keyboard, so your session is untouched.

It needs GNOME Shell 46 or later, XWayland, `gsettings`, `xrandr`, `xprop`,
`xwininfo`, GTK 3, Tesseract, and Python GI and Pillow, which is why it runs with
the system `/usr/bin/python3`. Build the desktop binary and the probe, then run it
for each target:

```bash
cargo build --locked -p agentdictate-app --features desktop --bin agentdictate
cargo build --locked -p agentdictate-linux --example selection_probe
/usr/bin/python3 scripts/test-overlay-desktop.py target/debug/agentdictate --target x11
/usr/bin/python3 scripts/test-overlay-desktop.py target/debug/agentdictate --scale 2 --target wayland
/usr/bin/python3 scripts/test-overlay-desktop.py target/debug/agentdictate \
  --monitor 1920x1080 --scale 1.25 --target x11
```

The script finds the probe in `target/debug/examples` next to the binary, or takes
`--selection-probe <path>`. `--monitor` can repeat; the default is three monitors of
different sizes. It prints a JSON report and fails on the first broken check:

- composited waveform pixels, the Transcribing label, and transparent corners;
- placement on the primary monitor, including after monitor and work-area changes;
- an unmanaged window that never takes focus from a real GTK target or adds an app
  entry;
- dismissal through a hidden update and through stdin EOF;
- both selections published, a paste into the target that the owner sees as
  acknowledged while Mutter's own clipboard fetch does not count, and CLIPBOARD and
  PRIMARY retrieval by Wayland and X11 targets.

It does not check a panel extension such as dash-to-panel. Run it after changing the
overlay, its placement, the clipboard, or the paste path.

## Debug

Start from a failing test, with `RUST_BACKTRACE=1` if needed. Logs are daily files in
`~/.local/state/agentdictate/logs`, or `target/dev-home/state/agentdictate/logs` for
a development instance. `RUST_LOG=debug` raises the level. Read only the lines you
need, because logs can contain transcript text.

To see what the installed service runs:

```bash
systemctl --user show agentdictated -p ActiveState -p SubState -p MainPID -p ExecStart
readlink /proc/<MainPID>/exe
```

After `./install.sh`, the executable should be `~/.local/bin/agentdictated`. For a
native crash, use `coredumpctl info agentdictated` if the host collects core dumps.
If line tables are not enough, build only the affected test target with
`--profile debugging --no-run` and run the printed executable under GDB. That
profile creates another artifact variant, so check disk space first. Do not attach
to or restart the installed daemon to debug a unit test.
