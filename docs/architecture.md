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

1. Observe the focused window.
2. Publish the transcript. Automatic mode on native Wayland publishes the
   same text to both the clipboard and the primary selection with live
   `wl-copy` owners. Other deliveries publish only to the clipboard, using
   `wl-copy` on Wayland or a live non-detaching `xsel` owner on X11. Read the
   published selections back to verify that the text landed.
3. Select the paste chord. On X11 or XWayland, Automatic mode uses
   `Ctrl+Shift+V` for detected terminals and `Ctrl+V` for regular or unknown
   targets. On native Wayland, Automatic mode uses `Shift+Insert`. Standard
   and Terminal modes bypass target detection and use their named shortcuts.
4. Inject one paced paste chord with `ydotool` via uinput on Wayland or
   `xdotool` on X11.

Injection follows a single-injection-no-retry policy. A retry after a failed
or ambiguous paste risks duplicating already-inserted text, which is worse
than missing text the user can re-dictate. A successful injection command is
stored as `submitted`: the backend accepted the command, but the target
application did not acknowledge consuming it. Submitted delivery is therefore
complete and non-retryable rather than falsely reported as confirmed.

## Runtime Data Locations

Runtime data lives under XDG directories, each created with mode 0700:

- `~/.config/agentdictate/config.json` — settings; login bootstrap at
  `~/.config/autostart/local.agentdictate.AgentDictate.desktop` and daemon unit
  at `~/.local/share/systemd/user/agentdictated.service`.
- `~/.local/share/agentdictate/` — SQLite history database (`agentdictate.sqlite`)
  and retained audio under `recordings/`.
- `~/.local/state/agentdictate/logs/` — logs.
- `~/.cache/agentdictate/` — model catalog cache (`model-catalog.json`).
- `$XDG_RUNTIME_DIR/agentdictate/` — IPC socket and singleton lock; not durable.

A legacy Python implementation under `src/agentdictate/` remains only as a
migration-parity suite; installed binaries are Rust-only. See
[docs/parity-exit-strategy.md](parity-exit-strategy.md) for its exit strategy.
