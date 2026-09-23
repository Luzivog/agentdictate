# AgentDictate Architecture

AgentDictate is a native Linux dictation app written in Rust. A daemon records
audio, transcribes it through the OpenAI speech-to-text API, and pastes the
transcript into the focused window. A GPUI desktop app provides settings and
history. The workspace is split by responsibility; dependencies flow downward.

## Crate Layering

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

Arrows show "depends on": app -> {runtime, linux, ui} -> core.
runtime, linux, and ui do not depend on each other.
```

## Crates

- **agentdictate-core**: Platform-independent domain types: protocol v3 wire
  messages and command enums, settings, the spoken-replacement engine,
  transcription cost estimation, and the dictation workflow state machine.
  Depends on nothing Linux-specific.
- **agentdictate-runtime**: Durable state on top of core: SQLite history with
  FTS5 search, model pricing tables, recovery of interrupted recordings, usage
  reporting, and the IPC server over a Unix domain socket.
- **agentdictate-linux**: Desktop integration: PipeWire recording, evdev
  hotkey listening with udev-driven recovery, clipboard publication, paced
  paste-chord injection, and focus observation.
- **agentdictate-ui**: Toolkit-free view models plus GPUI presentation behind
  the `desktop` feature, including route surfaces and the recording overlay.
  The view models stay testable without a display server.
- **agentdictate-app**: Composition root. Builds the `agentdictated` daemon
  binary and the `agentdictate` desktop binary, wires all crates together, and
  implements the OpenAI transcriber.

The app crate also implements the experimental ChatGPT subscription route in
`crates/agentdictate-app/src/codex_subscription.rs`. It uses the ChatGPT account
signed into Codex. `crates/agentdictate-app/src/chatgpt_dictation_import.rs`
imports completed ChatGPT desktop dictation records into local history. The
undocumented route can stop working without notice.

## Transcription Upload

The daemon captures 16 kHz mono s16 WAV. Before the OpenAI transcription
request, the app transport encodes the capture to Opus in WebM (ffmpeg,
32 kbps, speech mode) so upload time does not dominate stop-to-paste latency on
slow uplinks; if ffmpeg is unavailable or encoding fails, it falls back to
uploading the raw WAV. If OpenAI answers HTTP 400 about the uploaded file or
its format, the WAV is sent once more. A connection failure before OpenAI
returns any status is retried once on a fresh connection; nothing is retried
after a status arrives. The durable on-disk artifact stays WAV — recovery and
retry are unaffected. Each transcription and cleanup request logs its payload
size, encode time, and request time. Per dictation, the daemon logs the time
from the start command to the first audio (`capture_ready_ms`), the focus,
clipboard, and paste-chord stages of delivery, the overlay gate wait, and both
stop-to-paste and stop-to-flow-complete times. The overlay helper launches at
the start command, in parallel with the recorder, and its window-created and
first-frame lines carry the time since launch (`since_launch_ms`).

## Daemon And Settings App Communication

The settings app (`agentdictate`) talks to the daemon (`agentdictated`) over a
Unix domain socket at `$XDG_RUNTIME_DIR/agentdictate/agentdictate.sock`,
created with mode 0600 and guarded by a singleton lock file so only one daemon
listens.

The protocol is newline-delimited JSON, versioned by `protocol_version`
(currently 3) carried in every message. On connect the daemon pushes a full
snapshot before waiting for commands, so reconnects never depend on replayed
events; subsequent commands receive per-command responses. While connected,
the desktop app watches the SQLite database and the model-catalog cache file
with inotify and refreshes its workspace when they change, so writes made by
the daemon appear without polling or debounce delays.

## Text Delivery Pipeline

After transcription, the daemon delivers text to the focused application:

1. Observe the focused window: the daemon reads `_NET_ACTIVE_WINDOW`, its
   `WM_CLASS`, and `_NET_WM_STATE` from the X server (X11 or XWayland)
   in-process. On native Wayland, only an XWayland window that holds focus
   counts as the target.
2. Publish the transcript. Automatic mode on native Wayland publishes the
   same text to both the clipboard and the primary selection; other
   deliveries publish only to the clipboard. The daemon owns both X11
   selections itself: one long-lived thread keeps an unmapped window on the
   X server (X11 or XWayland) and answers `TARGETS`, `UTF8_STRING`, `TEXT`
   and Latin-1 `STRING` requests until the next delivery, or until another
   application takes the selection. The compositor's XWayland selection
   bridge carries both selections to Wayland-native applications.
   wl-clipboard is deliberately unused: without a data-control protocol on
   GNOME, every `wl-copy`/`wl-paste` call pops a transient toplevel that
   visibly re-layouts the taskbar at paste time. Publication is confirmed
   when the X server reports AgentDictate's window as each selection's owner.
3. Select the paste chord. On X11 or XWayland, Automatic mode uses
   `Ctrl+Shift+V` for detected terminals and `Ctrl+V` for regular or unknown
   targets. On native Wayland, Automatic mode uses `Shift+Insert`. Standard
   and Terminal modes bypass target detection and use their named shortcuts.
4. Inject one paced paste chord from an in-process uinput virtual keyboard
   (`evdev`). Press and release always run in-process and stay paired, and
   the kernel releases any held key if the daemon dies, so a chord can never
   leave a key stuck.
5. Watch for the acknowledgement. A request for the text that reaches the
   selection owner after the paste key press means the target took the
   paste: the daemon waits up to 150 ms after the press for it, logs
   "target requested the text", and records `consumed` in the delivery
   result and the `paste command submitted` log line. Earlier requests do
   not count, because clipboard managers fetch each new clipboard as soon as
   it is published (Mutter's own does so within milliseconds). A missing
   acknowledgement means unconfirmed rather than failed: a toolkit can answer
   a repeated paste of the same clipboard from its own cache.

Injection follows a single-injection-no-retry policy. A retry after a failed
or ambiguous paste risks duplicating already-inserted text, which is worse
than missing text the user can re-dictate. A successful injection command is
stored as `submitted` whether or not the target acknowledged it, and
submitted delivery is complete and non-retryable. The acknowledgement is only
logged for now.

## Runtime Data Locations

Runtime data lives under XDG directories, each created with mode 0700:

- `~/.config/agentdictate/config.json` — settings. The app writes the daemon
  unit to `~/.local/share/systemd/user/agentdictated.service` (only when its
  text changes); **Start on login** enables it for `graphical-session.target`.
- `~/.local/share/agentdictate/` — SQLite history database (`agentdictate.sqlite`)
  and retained audio under `recordings/`.
- `~/.local/state/agentdictate/logs/` — daily logs; the newest 14 files are
  kept for the daemon and for the settings window. Levels default to info, with the overlay's GPU crates at warn;
  `RUST_LOG` (for example `RUST_LOG=debug`) replaces those defaults.
- `~/.local/state/agentdictate/ducking.json` — present only while audio ducking
  has lowered an output: the sink, its original volume, and the volume
  AgentDictate set. If the daemon dies mid-recording, the next start restores
  the original volume unless the user has changed it since.
- `~/.cache/agentdictate/` — model catalog cache (`model-catalog.json`).
- `$XDG_RUNTIME_DIR/agentdictate/` — IPC socket and singleton lock; not durable.

The legacy Python implementation and parity suite were removed on 2026-08-24.
See [the parity exit record](parity-exit-strategy.md) for the removal decision
and accepted divergences.
