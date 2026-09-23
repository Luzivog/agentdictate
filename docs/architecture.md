# AgentDictate architecture

AgentDictate is a native Linux dictation app written in Rust. A background daemon
records speech, sends it to OpenAI for transcription, and pastes the text into the
focused window. A GPUI desktop app shows settings, history, and usage, and hosts the
recording overlay. This page describes the processes, the crates, the dictation
pipeline, where data lives, and the decisions that shape them.

## Processes

| Process | Started by | Owns |
| --- | --- | --- |
| `agentdictated --service` | The `agentdictated.service` user unit | Hotkey, recording, transcription, SQLite, delivery, audio ducking, tray, IPC server |
| `agentdictate` | The user, the desktop entry, or the tray | Settings window, plus the `start`, `stop`, `cancel`, and `setup-access` commands |
| `agentdictate --overlay-helper` | The daemon, once per recording | The recording overlay window |
| `agentdictate-evaluate` | A developer | Headless replay of dictation cases; not installed |

**Daemon.** The daemon runs as the `agentdictated.service` systemd user unit, which is
`PartOf=graphical-session.target` with `Restart=on-failure`. Nothing installs the
unit. When `agentdictate` finds no daemon on the socket, it writes the unit from the
running install, starts it, and waits for the socket. If the daemon answers with a
different protocol version, it is an older build, so the unit is restarted. At
startup, and whenever the setting changes, the daemon enables or disables the unit
for `graphical-session.target` to match **Start on login**. The daemon does not link
GPUI.

Older versions started the daemon from an XDG autostart entry that runs
`agentdictated --start-service`. That entry still works, and the daemon deletes it
once the unit's enablement has taken over.

**Desktop app.** `agentdictate` without arguments opens the settings window. With
arguments it runs one command and exits:

```bash
agentdictate start [--mode dictate|literal]
agentdictate stop
agentdictate cancel
agentdictate setup-access
```

`setup-access` runs a copy of `packaging/grant-access.sh` as root through `pkexec`
to install and apply the input-access rule. See
[native input access](../packaging/NATIVE_ACCESS.md).

**Overlay helper.** When a recording starts, the daemon spawns the sibling
`agentdictate` binary with `--overlay-helper`. It feeds the helper workflow and
waveform updates on stdin and reads status lines back. The helper logs into the
daemon's log file. It needs X11 or XWayland and refuses to open a Wayland toplevel.
A helper crash never affects the recording. The supervisor allows one automatic
restart per update. It records the overlay's health in `overlay-health` in the
runtime directory, which the settings window watches so it can show a notice when
the overlay cannot render.

**Tray.** The tray runs inside the daemon as a StatusNotifier item when
`show_tray_icon` is on. That is a `config.json` setting and it defaults to on. The
menu has **Open AgentDictate**, **Toggle dictation**, **Start literal dictation**,
and **Quit AgentDictate**. Opening settings launches the sibling `agentdictate`.

**Development instance.** With `AGENTDICTATE_HOME` set, as `./run.sh` does, every
data root moves under that directory and the daemon is unsupervised: nothing calls
`systemctl`, and **Start on login** does nothing. See
[Run a development build](DEVELOPMENT.md#run-a-development-build).

## Crates

```
                 +----------------------+
                 |   agentdictate-app   |
                 | agentdictated daemon |
                 | agentdictate desktop |
                 +----------+-----------+
                            |
        +-------------------+-------------------+
        |                   |                   |
        v                   v                   v
+---------------+ +-----------------+ +---------------+
|    runtime    | |      linux      | |       ui      |
+-------+-------+ +--------+--------+ +-------+-------+
        |                   |                  |
        +-------------------+------------------+
                            |
                            v
                  +-------------------+
                  |       core        |
                  +-------------------+

app depends on runtime, linux, and ui; each of those depends only on core.
```

- **agentdictate-core**: platform-independent types. Settings and their validation,
  the IPC protocol (`PROTOCOL_VERSION` in `crates/agentdictate-core/src/protocol.rs`),
  the workflow state machine and job stages, dictation options, vocabulary parsing and
  alias normalization, and the per-minute price table.
- **agentdictate-runtime**: durable state. The SQLite job table with its checkpoints,
  Recovery, History with full-text search, usage queries, startup cleanup, settings
  load and save, the IPC server and client, and the port traits (`Transcriber`,
  `Deliverer`, `DeliveryGate`, `Recorder`) that the app implements.
- **agentdictate-linux**: desktop integration. `pw-record` capture, the evdev hotkey
  listener, which watches `/dev/input` for new keyboards, the uinput paste keyboard,
  the in-process X11 selection owner, X11 focus reading, the paste delivery state
  machine, `pactl` audio ducking, overlay placement, and a subprocess runner with
  deadlines.
- **agentdictate-ui**: toolkit-free view models, plus the GPUI settings window and
  overlay view behind the `desktop` feature.
- **agentdictate-app**: composition. The daemon, the OpenAI speech transport,
  optional live streaming, the overlay supervisor and helper, the tray, the service
  unit and login startup, `setup-access`, the hotkey dispatch gate, logging, and the
  three binaries.

## Dictation pipeline

The daemon handles one dictation at a time. Each step that matters for recovery is a
checkpoint in the `dictation_jobs` table before the next step starts.

1. **Start.** The hotkey, the tray, or `agentdictate start` creates a `starting` job.
   The job stores a snapshot of its dictation options, so a later retry uses the
   same ones: mode, language, context, vocabulary, and streaming, never
   credentials. The daemon starts `pw-record` (16 kHz mono PCM16, 20 ms
   node latency) writing a WAV under `recordings/`, lowers other audio on a separate
   thread, and launches the overlay helper.
2. **Stream (optional).** With **Stream speech** on, a Realtime session tails the
   WAV, resamples it to 24 kHz, and sends it to `gpt-live-transcribe` while you
   speak.
3. **Stop.** A second press, a hold release, the maximum duration, the tray, or
   `agentdictate stop` finalizes the WAV and records the `captured` checkpoint. Esc
   discards the recording instead, and deletes its audio unless **Preserve temporary
   audio** is on.
4. **Transcribe.** A successful live result is used as is. Otherwise ffmpeg encodes
   the WAV to WebM/Opus at 32 kbps in speech mode, and the app posts it to
   `/v1/audio/transcriptions` with the model, `languages[]`, `keywords[]` (the
   vocabulary spellings), and `prompt` (the context). Without ffmpeg, the WAV is
   uploaded. A request that fails before OpenAI returns any status is sent once more,
   and an HTTP 400 about the file resends the WAV once. Nothing else is retried.
5. **Empty results.** An empty result from a near-silent WAV finishes quietly: the
   job is removed and nothing is pasted or kept in History. Any other empty result or
   error marks the job `failed` and keeps it in Recovery with its audio.
6. **Normalize.** The raw text is saved first, so a later failure never needs a second
   paid transcription. Vocabulary aliases then replace spoken forms with their
   spellings. The job is now
   `ready_to_deliver`.
7. **Gate.** The overlay is dismissed. If its helper confirmed an override-redirect
   window, the paste goes ahead while it fades. Otherwise the paste waits up to
   `OVERLAY_TEARDOWN_TIMEOUT` (2 s) for the helper to exit. A helper still running
   then is killed, nothing is pasted, and the text stays in Recovery.
8. **Deliver.** The job is marked `attempting`. The daemon reads the focused X11
   window, publishes the text, and reads the focus again. If the focus keeps
   changing, nothing is pasted. Otherwise it injects exactly one paste shortcut from
   its uinput keyboard, then waits up to 150 ms for an application to request the
   text. That request is logged as `consumed`, the target's acknowledgement. The
   delivery ends as `submitted`, `ambiguous` (the injection itself failed), or
   `not_sent` (nothing was injected).
9. **Complete.** One transaction records the usage session (numbers only), adds the
   History entry when **Save history** is on, and deletes the job row. The WAV is then
   deleted unless **Preserve temporary audio** is on.

At startup the daemon reconciles what a crash left behind. Jobs that were starting,
recording, or transcribing become `interrupted` and stay in Recovery with their audio.
A job whose paste had started becomes `ambiguous` and is never pasted again
automatically. Unless **Preserve temporary audio** is on, startup cleanup then deletes
the audio of finished jobs and any WAV file older than one hour that no job owns.

Recovery actions in the History page, **Transcribe again** and **Paste again**, copy
the text to the clipboard and never paste, because AgentDictate's own window has the
focus.

### Paste shortcut and selections

The daemon owns the X11 CLIPBOARD and PRIMARY selections itself. One long-lived
thread keeps an unmapped window on the X server (X11 or XWayland) and answers requests
for the text until the next delivery or until another application takes the
selection. Publication counts as done when the X server reports that window as the
owner. Mutter's XWayland bridge carries both selections to native Wayland apps.

| Paste shortcut setting | Selections published | Shortcut sent |
| --- | --- | --- |
| Automatic | Primary and clipboard | Shift+Insert |
| Standard | Clipboard | Ctrl+V |
| Terminal | Clipboard | Ctrl+Shift+V |

Automatic works for every target: terminals such as xterm, VTE, and kitty paste the
primary selection on Shift+Insert, and GTK, Qt, Tk, Chromium, and Electron apps paste
the clipboard. Copy-only actions publish only the clipboard.

A request that arrives after the key press acknowledges the paste. Earlier requests
do not count, because clipboard managers, Mutter's included, fetch each new clipboard
as soon as it is published. A missing acknowledgement means unconfirmed, not failed:
a toolkit can answer a repeated paste of the same clipboard from its own cache. The
acknowledgement is only logged for now.

## IPC

The desktop app and the CLI talk to the daemon over a Unix socket at
`$XDG_RUNTIME_DIR/agentdictate/agentdictate.sock` with mode 0600. A lock file next to
it guarantees one daemon. Messages are newline-delimited JSON, and every message
carries `protocol_version`, which must equal `PROTOCOL_VERSION` on both sides. Bump it
whenever the wire format changes. On connect the daemon sends a full snapshot first,
so a reconnect never depends on replayed events. The settings window uses short-lived
connections and watches the SQLite database and `overlay-health` with inotify, so
daemon writes appear without polling.

## Data locations

AgentDictate creates its own directories with mode 0700. `XDG_CONFIG_HOME`,
`XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME`, and `XDG_RUNTIME_DIR` move them,
and `AGENTDICTATE_HOME` moves all of them under one directory.

| Path | Contents |
| --- | --- |
| `~/.config/agentdictate/config.json` | Settings, including the OpenAI API key in plain text, mode 0600 |
| `~/.local/share/systemd/user/agentdictated.service` | The daemon's user unit, written by the app when its text changes |
| `~/.local/share/agentdictate/agentdictate.sqlite` | Jobs, History, usage, and the disabled rules of the retired Replacements feature |
| `~/.local/share/agentdictate/recordings/` | WAV files of in-flight, recoverable, and preserved dictations |
| `~/.local/share/agentdictate/native-access/` | The input-access rule and guide from `install.sh`, and the helper `setup-access` writes |
| `~/.local/state/agentdictate/logs/` | Daily logs, 14 files each: `agentdictated.log.*` for the daemon and overlay, `agentdictate.log.*` for the settings window |
| `~/.local/state/agentdictate/ducking.json` | Present only while ducking has lowered an output, so a crash can be undone at the next start |
| `~/.cache/agentdictate/` | Created at startup; currently unused |
| `$XDG_RUNTIME_DIR/agentdictate/` | IPC socket, singleton lock, `overlay-health` |

Logs default to `info`, with the overlay's GPU crates at `warn`. A valid `RUST_LOG`
replaces those defaults. Logs can contain transcript text.

## Decisions

- **One paste, never retried automatically.** A retry after a failed or unclear
  paste risks inserting the text twice, which is worse than missing text you can
  dictate again. `submitted` means the shortcut was sent, whether or not the target
  acknowledged it. Only a failure before any shortcut (`not_sent`) stays retryable.
- **In-process X11 selections, not wl-clipboard or `xsel`.** GNOME offers no
  data-control protocol, so every `wl-copy` or `wl-paste` maps a short-lived Wayland
  window, which visibly re-lays out the taskbar at paste time. Owning the X11
  selections needs no mapped window, Mutter bridges both to Wayland apps, and owning
  them in-process is what lets the daemon see the target take the paste.
- **The overlay is override-redirect, and the paste does not wait for its fade.** An
  override-redirect window is never managed by the window manager, so it cannot take
  the focus the paste targets. Each helper reads its window's `override_redirect`
  attribute from the X server once and reports it with `window_created`. Only a
  confirmed helper lets the paste run during the fade; otherwise the paste waits for
  its exit. A helper still alive 2 s after dismissal is killed either way.
- **The overlay can never cost audio.** Recording, the saved WAV, processing, and the
  delivery gate do not depend on the overlay rendering. The helper places itself on
  the RandR primary monitor inside the EWMH work area, horizontally centered and 72
  logical pixels above the bottom, and moves only when monitors or the work area
  change. AGENTS.md makes this placement a requirement.
- **The app owns its service unit.** Writing the unit from the running install, only
  when its text changes, keeps an ordinary window launch free of `systemctl` calls,
  and the installers never enable or start a user service.
- **No cleanup LLM.** `gpt-transcribe` output already needs little cleanup, and the
  cleanup request added about 2 s per dictation. The pipeline was removed, and a
  stored `organize` mode now reads as Dictate.
- **No local model yet.** An offline model (Parakeet) made about twice the errors,
  lacked working vocabulary hints, and needed 1.5 to 2 GB of RAM.
- **No noise suppression or gain control.** Enhancement front-ends tend to make
  modern speech recognition worse.
