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
| `agentdictate` | The user, the desktop entry, or the tray | Settings window, plus the `start`, `stop`, `cancel`, `paste-last`, and `setup-access` commands |
| `agentdictate --overlay-helper` | The daemon, once per recording | The recording overlay window |
| `agentdictate-evaluate` | A developer | Headless replay of dictation cases; not installed |

**Daemon.** The daemon runs as the `agentdictated.service` systemd user unit, which is
`PartOf=graphical-session.target` with `Restart=on-failure`. Nothing installs the
unit. When `agentdictate` finds no daemon on the socket, it writes the unit from the
running install, starts it, and waits for the socket. If the daemon answers with a
different protocol version, it is an older build, so the unit is restarted. At
startup, and whenever the setting changes, the daemon enables or disables the unit
for `graphical-session.target` to match **Start AgentDictate when I log in**. The
daemon does not link GPUI.

Older versions started the daemon from an XDG autostart entry that runs
`agentdictated --start-service`. That entry still works, and the daemon deletes it
once the unit's enablement has taken over.

**Desktop app.** `agentdictate` without arguments opens the settings window. With
arguments it runs one command and exits:

```bash
agentdictate start [--mode dictate|literal]
agentdictate stop
agentdictate cancel
agentdictate paste-last
agentdictate setup-access
```

`paste-last` pastes the last dictation again, through the same gate and single
paste chord as a dictation, once the shortcut's modifier keys are released. The
daemon keeps that text in memory only, forgets it when you delete it, and refuses
while a dictation is in flight. The tray's **Paste last dictation** does the same.
A notification's **Paste again** pastes the dictation that notification is about:
the last one, or one still waiting in Recovery. Otherwise it pastes nothing and says
so.

`setup-access` runs a copy of `packaging/grant-access.sh` as root through `pkexec`
to install and apply the input-access rule. See
[native input access](../packaging/NATIVE_ACCESS.md).

**Setup screen.** The window opens on **Set up AgentDictate** while dictation
can't work: no API key, or no access to read the shortcut or paste. Home's fix
card opens it too. Its four steps are optional. **Check key** asks the daemon to
check a pasted or saved key with OpenAI, and saves a pasted key only once OpenAI
accepts it. **Grant access** asks first, then runs the same grant as
`setup-access` from the window and re-reads the daemon's readiness; it says "Log
out and back in to finish" only if access is still missing. **Say something**
runs the daemon's microphone test with a live level meter. **Try it** is a text
box to dictate into. Automatic paste presses Shift+Insert, which the window binds
to paste in its text boxes.

**Overlay helper.** When a recording starts, the daemon spawns the sibling
`agentdictate` binary with `--overlay-helper`. It feeds the helper workflow and
waveform updates on stdin and reads status lines back. The helper logs into the
daemon's log file. It needs X11 or XWayland and refuses to open a Wayland toplevel.
A helper crash never affects the recording. The supervisor allows one automatic
restart per update. It records the overlay's health in `overlay-health` in the
runtime directory, which the settings window watches so it can show a notice when
the overlay cannot render.

**Notices.** A dictation that ends without a paste is announced twice. The overlay
turns into a notice card for 2.5 s, such as "Couldn't reach OpenAI · Saved to
Recovery", "Didn't hear anything · Check your microphone", or "Copied — press
Ctrl+V". It has no buttons, and its window's input region is empty, so it never
takes a click. The daemon also shows a desktop notification through
`org.freedesktop.Notifications` on the session bus, which never takes the focus.
Clicking a failure's notification opens the window. A failure that transcribing
again can fix offers **Try again**, which transcribes the Recovery item and copies its
text; copied text, and a paste that wasn't confirmed, offer **Paste again**. Each
notification replaces the previous one. A retry from the settings window, and a
shutdown, announce nothing.

**Tray.** The tray runs inside the daemon as a StatusNotifier item when
`show_tray_icon` is on. That is a `config.json` setting and it defaults to on. The
menu has **Open AgentDictate**, **Start dictation** (**Stop dictation** while
recording), **Start literal dictation**, **Paste last dictation**, **Cancel
dictation**, and **Quit AgentDictate**. Opening settings launches the
sibling `agentdictate`. Only one settings window runs at a time: it holds
`window.lock` in the runtime directory, and a later launch writes `window.raise`,
which that window watches to come to the front, and exits.

**Development instance.** With `AGENTDICTATE_HOME` set, as `./run.sh` does, every
data root moves under that directory and the daemon is unsupervised: nothing calls
`systemctl`, and **Start AgentDictate when I log in** does nothing. See
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
  alias normalization, and the per-minute price.
- **agentdictate-runtime**: durable state. The SQLite schema and its numbered
  migrations (`PRAGMA user_version`), the job table with its checkpoints, Recovery,
  the `dictations` table behind History search and usage, the settings window's
  read-only `DatabaseObserver`, startup cleanup, settings load and save, the IPC
  server and client, and the port traits (`Deliverer`, `DeliveryGate`, `Recorder`)
  that the app implements. It writes checkpoints and never calls the network.
- **agentdictate-linux**: desktop integration. `pw-record` capture, the evdev hotkey
  listener, which watches `/dev/input` for new keyboards, the uinput paste keyboard,
  the in-process X11 selection owner, X11 focus reading, the paste delivery state
  machine, `pactl` audio ducking, overlay placement, and a subprocess runner with
  deadlines.
- **agentdictate-ui**: toolkit-free view models, built from core's snapshots with
  times on the local clock, plus the GPUI settings window and overlay view behind the
  `desktop` feature.
- **agentdictate-app**: composition. The daemon and the `DaemonHandle` that shares
  it between threads, the `Transcriber` and its per-job processing tickets, the
  OpenAI speech transport, the ffmpeg encoder that runs beside each recording, the
  overlay supervisor and helper, the tray, the service unit and login startup,
  `setup-access`, the hotkey dispatch gate, logging, and the three binaries.

## Dictation pipeline

The daemon handles one dictation at a time. Each step that matters for recovery is a
checkpoint in the `dictation_jobs` table before the next step starts.

1. **Start.** The hotkey, the tray, or `agentdictate start` creates a `starting` job.
   The job stores a snapshot of its dictation options, so a later retry uses the
   same ones: mode, language, context, and vocabulary, never
   credentials. The daemon starts `pw-record` (16 kHz mono PCM16, 20 ms
   node latency) writing a WAV under `recordings/`, lowers other audio on a separate
   thread, and launches the overlay helper. Once audio flows, the recorder owner
   thread starts one `ffmpeg` that encodes the growing WAV to WebM/Opus (32 kbps,
   speech mode) while you speak; every 50 ms, a feeder thread hands it what
   `pw-record` appended.
2. **Stop.** A second press, a hold release, the tray, or `agentdictate stop`
   finalizes the WAV, hands ffmpeg the rest of its audio, and records the `captured`
   checkpoint, then `transcribing`. The recorder owner thread ends a recording at
   **Stop recording after**, whatever started it. Esc discards the recording
   instead and kills its ffmpeg. A recording longer than 5 s waits in Recovery as
   `cancelled`, with its audio, for 24 hours; a shorter one is deleted with its audio
   unless **Keep audio recordings** is on. If the recorder exits by itself, or the
   microphone delivers no audio for 3 s, the recording is kept in Recovery and not
   transcribed.
3. **Transcribe.** Transcription runs on its own thread, outside the daemon lock, so
   settings, the tray, and the settings window stay responsive meanwhile. Presses
   while a dictation transcribes are ignored. **Cancel dictation** in the tray or
   `agentdictate cancel` stops waiting: a new dictation can start at once, and the
   late result waits in Recovery as "Cancelled before paste". Esc does not cancel a
   transcription. The upload waits for ffmpeg to finish the WebM, usually tens of
   milliseconds after the stop. If that encode failed, ffmpeg encodes the saved WAV
   now, as it does for every Recovery retry; without ffmpeg, the WAV itself is
   uploaded. The app posts the audio to `/v1/audio/transcriptions` with the model,
   `languages[]`, `keywords[]` (the vocabulary spellings), and `prompt` (the
   context). A request that fails before it reaches OpenAI is sent once more, and an
   HTTP 400 about the file resends the WAV once. Nothing else is retried, not even a
   request that got no answer within the 180 s limit.
4. **Empty results.** An empty result from a near-silent WAV ends with the "Didn't
   hear anything" notice: the job is removed and nothing is pasted or kept in
   History. Any other empty result or
   error marks the job `failed` and keeps it in Recovery with its audio.
   Every failure is stored with a typed reason, `FailureKind` (offline, API key
   missing or refused, rate limited, service error, nothing heard, microphone,
   paste not confirmed, or unexpected). The window words the reason; the raw error
   only goes to the log.
5. **Normalize.** The raw text is saved first, so a later failure never needs a second
   paid transcription. Vocabulary aliases then replace spoken forms with their
   spellings. The job is now
   `ready_to_deliver`.
6. **Gate.** A result is copied to the clipboard instead of pasted when it arrives
   more than 8 s after the stop (plus 30 ms per second of audio), or when the focused
   window observably changed since the stop: another X11 window, or a switch between
   an X11 and a native Wayland window. Either way you may be somewhere else by now,
   so the overlay and a notification say "Copied — press Ctrl+V". For a
   paste, the overlay is dismissed. If its helper confirmed an override-redirect
   window, the paste goes ahead while it fades. Otherwise the paste waits up to
   `OVERLAY_TEARDOWN_TIMEOUT` (2 s) for the helper to exit. A helper still running
   then is killed, nothing is pasted, and the text stays in Recovery.
7. **Deliver.** The job is marked `attempting`. The daemon reads the focused X11
   window, publishes the text, and reads the focus again. If the focus keeps
   changing, nothing is pasted. Otherwise it injects exactly one paste shortcut from
   its uinput keyboard, then waits up to 150 ms for an application to request the
   text. That request is logged as `consumed`, the target's acknowledgement. Without
   it the paste may not have landed: it is never sent again, and the overlay and a
   notification say "Copied — press Ctrl+V". The delivery ends as `submitted`,
   `ambiguous` (the injection itself failed), or `not_sent` (nothing was injected).
8. **Complete.** One transaction records the dictation, with its usage numbers always
   and its text unless **Keep transcripts** is **Don't keep**, and deletes the job row.
   Then text older than **Keep transcripts** allows, Recovery items unchanged for 7
   days, and cancelled recordings older than 24 hours are deleted. The WAV is then
   deleted, and so is an expired Recovery item's, unless **Keep audio recordings** is
   on.

At startup the daemon reconciles what a crash left behind. A database SQLite cannot
read is renamed to `agentdictate.sqlite.corrupt-<unix time>` and a fresh one
started; the settings window shows where the old file went. Jobs that were starting,
recording, or transcribing become `interrupted` and stay in Recovery with their audio.
A job whose paste had started becomes `ambiguous` and is never pasted again
automatically. Unless **Keep audio recordings** is on, startup cleanup then deletes
the audio of finished jobs and any WAV file older than one hour that no job owns. It
also applies the same retention as step 8. Writers set `secure_delete`, and deleting
or expiring text truncates the write-ahead log, so removed text leaves the disk.

Recovery actions in the History page, **Transcribe again** (**Transcribe** on a
cancelled recording) and **Paste again**, copy the text to the clipboard and never
paste, because AgentDictate's own window has the focus.

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

## Concurrency

One mutex guards the daemon. `DaemonHandle` shares it between the IPC sessions, the
hotkey action worker, the tray worker, the signal handler, and the recorder-event
thread. Its rules, also in `crates/agentdictate-app/src/handle.rs`:

- Work under the lock is bounded: starting and finalizing the recorder (10 s
  deadlines), checkpoints, delivery (5 s), and settings changes. Network requests
  never run under it. A stopped recording hands a processing ticket, with the job
  and a clone of the transcriber, to a thread that transcribes and then takes the
  lock once to store and deliver the result. Settings saved meanwhile never change a
  job in flight.
- Only the job the daemon is processing is delivered, and deliveries run under the
  lock, one at a time. A cancelled job's late result is only stored.
- The hotkey dispatch loop and the recorder owner thread never wait for the lock,
  because the lock holder waits for them when it reconfigures the hotkey or stops a
  recording. They read a lock-free `DaemonStatus` and report through channels.
- A hotkey or tray action reads the workflow phase and acts on it under one lock,
  through one table (`lifecycle_action`).
- Quit preserves a recording for Recovery and gives a transcription in progress 3 s
  to be delivered. A job still transcribing is recovered at the next start.
- A panic while holding the lock ends the daemon with status 70, so systemd restarts
  it.

## IPC

The desktop app and the CLI talk to the daemon over a Unix socket at
`$XDG_RUNTIME_DIR/agentdictate/agentdictate.sock` with mode 0600. A lock file next to
it guarantees one daemon. Messages are newline-delimited JSON, and every message
carries `protocol_version`, which must equal `PROTOCOL_VERSION` on both sides. Bump it
whenever the wire format changes: `crates/agentdictate-core/tests/core/protocol.json`
holds a sample of every message for the current version, and a change under the same
version fails that test. Each reply answers the command just sent on the
same connection. Every session runs on its own thread and ends after 60 s without a
command. On connect the daemon sends its status snapshot first, so a reconnect never
depends on replayed events. IPC carries commands and that snapshot only. The
settings window reads History, usage, and Recovery straight from the database, with
a read-only connection, so a dictation in progress never delays them. It watches
the database, `overlay-health`, a `status` file, and the socket with inotify and
collects events for 30 ms after the first. Then a commit, detected with `PRAGMA
data_version`, re-reads the database. Any other change asks the daemon for its
status: the daemon writes `status` when it starts and whenever its readiness,
recording state, or settings change. The status carries the daemon's `Readiness`:
the shortcut listener, the API key, whether `/dev/uinput` is writable,
world-accessible input devices and the udev rule behind them, and a missing
`pw-record`, `ffmpeg` or `pactl`. Home shows one line when everything is in place,
such as "Ready — press Ctrl+Space anywhere to dictate", or one card with the most
important fix. A daemon that does not answer makes the window say "Reconnecting to
AgentDictate…" until it answers again. A daemon on another protocol version, or a
database with a newer schema, makes the window say "AgentDictate was updated —
reopen this window" and stop reading. Its changes, such as Delete or Transcribe
again, are commands, after which it reads the database again. Settings changes are
per setting: each control sends one `change_setting` command, and the daemon
applies it to the settings it holds, so two clients never overwrite each other's
changes. `agentdictate stop` returns once the
recording is stopped; the paste follows. A Recovery retry's reply waits for the
copied text, a shortcut capture's reply for the key press, and an API key check's
reply for OpenAI's answer, all without holding the daemon lock. The check sends a
transcription request without audio: OpenAI checks the key and its permission to
transcribe first, so a usable key gets 400 for the missing audio, and nothing is
billed. A microphone test listens for 3 s and sends the microphone's level every
50 ms as interim messages before its reply. It records through `pw-record` into
a file in the runtime directory whose name is removed as soon as recording
starts, or when it fails to, and at the next daemon start after a crash. Nothing
it hears is kept, uploaded, or added to Recovery.

## Data locations

AgentDictate creates its own directories with mode 0700. `XDG_CONFIG_HOME`,
`XDG_DATA_HOME`, `XDG_STATE_HOME`, and `XDG_RUNTIME_DIR` move them, and
`AGENTDICTATE_HOME` moves all of them under one directory.

| Path | Contents |
| --- | --- |
| `~/.config/agentdictate/config.json` | Settings, including the OpenAI API key in plain text, mode 0600 |
| `~/.local/share/systemd/user/agentdictated.service` | The daemon's user unit, written by the app when its text changes |
| `~/.local/share/agentdictate/agentdictate.sqlite` | In-flight and Recovery jobs, and completed dictations: their usage numbers, and their text for History |
| `~/.local/share/agentdictate/agentdictate.sqlite.pre-v<N>` | A copy of the database from before it migrated to schema version N, kept until the next daemon start or until any text is deleted |
| `~/.local/share/agentdictate/recordings/` | WAV files of in-flight, recoverable, and preserved dictations |
| `~/.local/share/agentdictate/native-access/` | The input-access rule and guide from `install.sh`, and the helper `setup-access` writes |
| `~/.local/state/agentdictate/logs/` | Daily logs, 14 files each: `agentdictated.log.*` for the daemon and overlay, `agentdictate.log.*` for the settings window |
| `~/.local/state/agentdictate/ducking.json` | Present only while ducking has lowered an output, so a crash can be undone at the next start |
| `$XDG_RUNTIME_DIR/agentdictate/` | IPC socket, singleton lock, `overlay-health`, `status`, settings window lock and raise file |

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
- **One upload per dictation, not Realtime streaming.** Streaming to a Realtime
  `gpt-transcribe` session was faster only for long dictations, and the model
  finishes each committed chunk as a sentence: most chunk boundaries gained a
  spurious sentence break. The whole recording goes up in one request instead, and
  ffmpeg encodes it while you speak, so the upload starts milliseconds after the
  stop.
- **No cleanup LLM.** `gpt-transcribe` output already needs little cleanup, and the
  cleanup request added about 2 s per dictation. The pipeline was removed, and a
  stored `organize` mode now reads as Dictate.
- **No local model yet.** An offline model (Parakeet) made about twice the errors,
  lacked working vocabulary hints, and needed 1.5 to 2 GB of RAM.
- **No noise suppression or gain control.** Enhancement front-ends tend to make
  modern speech recognition worse.
