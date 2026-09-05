<p align="center">
  <img src="assets/agentdictate.svg" alt="AgentDictate microphone icon" width="72">
</p>

<h1 align="center">AgentDictate</h1>

<p align="center">
  <strong>Experimental STT using the ChatGPT account signed into Codex.</strong><br>
  By default, press <kbd>Ctrl</kbd> + <kbd>Space</kbd> once to start and again to
  stop. AgentDictate copies the transcript and submits one paste shortcut to
  the focused app on Wayland or X11. The undocumented route may not be enabled
  for every account.
</p>

## Install

These steps are tested on Ubuntu 24.04. The
[system requirements](docs/INSTALL.md#requirements)
cover both distributions. A graphical session with a working systemd user
manager, a running PipeWire session, and Rust 1.95.0 through
[rustup](https://rustup.rs/) are required.

1. Install the requirements for your distribution.
2. Install the [Codex CLI](https://learn.chatgpt.com/docs/codex/cli), run
   `codex login`, and choose **Sign in with ChatGPT**. Confirm the active login:

   ```bash
   codex login status
   ```

   It must report `Logged in using ChatGPT`. An API-key login cannot
   authenticate this route.

3. Build and install AgentDictate:

   ```bash
   git clone https://github.com/luzivog/agentdictate.git
   cd agentdictate
   ./install.sh
   ```

   The installer copies the app before it checks native input access. Exit
   status 2 means the app is installed, but native access still needs setup.
   It does not start the daemon immediately. On a fresh install, the next
   desktop login starts it and imports eligible ChatGPT desktop dictation
   transcripts and metadata into local SQLite. Read [Local storage](#local-storage)
   before logging out.
4. Native access can expose every keypress and synthesize arbitrary input for
   the active desktop session. Follow the
   [repository native-access steps](packaging/NATIVE_ACCESS.md#repository-user-install).
   After any logout and login, return to the cloned `agentdictate` directory.
   Then verify the result:

   ```bash
   ./install.sh --check-native-access
   ```

   Continue only when the final line is `Native input readiness: ready`.

## Data and billing

When **ChatGPT subscription** is selected, AgentDictate uses an undocumented
Codex App Server auth-status method to get an in-memory ChatGPT bearer token,
derives the account ID from its claims, then sends the recording and any
configured language to an undocumented ChatGPT endpoint as `Codex Desktop`.
The request authenticates with the bearer token and account ID. AgentDictate
does not write the token to its own files. Codex manages its login cache
separately and may store credentials unencrypted in `$CODEX_HOME/auth.json`. See
[Codex credential storage](https://learn.chatgpt.com/docs/auth#credential-storage).

OpenAI says ordinary Codex use in a ChatGPT Enterprise workspace follows that
workspace's retention and residency settings. It does not document whether
those policies or training controls cover AgentDictate's direct call, whether
third-party clients may make it, which accounts can access it, or how requests
are limited or billed. The route can stop working without notice.

OpenAI API transcription uploads the recording with the selected model and any
applicable language, context, or vocabulary hints. Optional streaming sends audio
while recording to `gpt-live-transcribe`. Both require an OpenAI Platform API
key and can incur Platform charges outside a ChatGPT subscription. New dictations
use direct transcription without a cleanup call. Historical recovery jobs and
explicit cleanup evaluations can still upload the transcript and saved instructions
to the paid OpenAI API. A failed live stream can use the selected file model,
which can add a second transcription charge.
Subscription failures never fall back to the paid API route.

When a Platform API key is saved, daemon startup and model-catalog refreshes use
it for an authenticated `/v1/models` request. This happens even when
**ChatGPT subscription** is selected. AgentDictate caches
the returned model IDs and a key fingerprint in its XDG cache directory.

## Local storage

The Platform API key is stored unencrypted in
`$XDG_CONFIG_HOME/agentdictate/config.json` (default
`~/.config/agentdictate/config.json`) with user-only `0600` permissions.
The SQLite database and retained WAV files are also unencrypted. Local Unix
permissions restrict their access.

Once transcription succeeds, raw and final text, plus cleaned text when
available, remain in durable job rows inside
`$XDG_DATA_HOME/agentdictate/agentdictate.sqlite` (default
`~/.local/share/agentdictate/agentdictate.sqlite`). The rows remain after
successful shortcut submission or delivery failure. Deleting an item from
Recovery removes its recording and marks the job deleted. It does not remove
the transcript from SQLite. **Save history** controls additional History and
usage rows, not the durable job rows. While it is off, those additional rows are
skipped. If it is later enabled, restarting the daemon backfills them from
submitted durable jobs. There is no in-app purge for the durable job rows.

While the daemon runs, AgentDictate imports existing and new completed ChatGPT
desktop dictation records that contain a duration and a nonblank transcript.
Other records are skipped. The source directory is
`$CODEX_HOME/dictation-history` (default `~/.codex/dictation-history`).
AgentDictate stores each imported record in SQLite, including the transcript,
dictation ID, creation time, duration, derived end time, and import time. The
transcript, derived end time, and duration appear in History. The creation date,
word count, and duration contribute to usage totals. There is no in-app opt-out
or purge, and **Save history** does not disable the import. Deleting
AgentDictate's database does not delete the source metadata. The next daemon
start imports it again.

Recordings are created under `$XDG_DATA_HOME/agentdictate/recordings` (default
`~/.local/share/agentdictate/recordings`). Audio is normally deleted after paste
submission. Failed or interrupted recordings remain for recovery, and
**Preserve temporary audio** also keeps recordings after successful shortcut
submission.

Development, packaging, and local data paths are in the
[installation and development guide](docs/INSTALL.md).

## Start and use

1. Start AgentDictate. This also starts the automatic ChatGPT desktop history
   import described above.

   ```bash
   ~/.local/bin/agentdictate
   ```

2. Subscription STT does not require an OpenAI Platform API key. For the
   no-key path, select **ChatGPT subscription**, turn **Cleanup** off, and click
   **Save changes**.
3. Press `Ctrl+Space` once to start recording. Before pressing it again, focus
   the destination app. Keep that app focused until AgentDictate copies the
   transcript and submits the paste shortcut. The shortcut targets the app
   focused at paste time. AgentDictate confirms the clipboard write and shortcut
   submission, not insertion into the target app.

Closing the settings window leaves the daemon, global shortcut, and history
import running. Stop them with **Quit AgentDictate** in the tray or
`systemctl --user stop agentdictated.service`.

The fresh-install behavior above comes from an XDG autostart entry. Turn off
**Start on login** to prevent the AgentDictate daemon from starting at future
logins. This does not stop a running daemon. Reinstalls preserve the AgentDictate
setting.

## Architecture

AgentDictate ships two binaries. `agentdictated` is the background daemon and
owns the hotkey, recording, STT, history, and paste workflow. `agentdictate` is
the GPUI desktop client. It sends commands to the daemon through a private Unix
socket. The daemon runs as a user service for the current graphical session.

| Crate | Owns |
| --- | --- |
| `agentdictate-core` | Settings, protocol types, and workflow state |
| `agentdictate-runtime` | SQLite, recovery, usage, and IPC |
| `agentdictate-linux` | Recording, hotkeys, focus, clipboard, and paste |
| `agentdictate-ui` | GPUI windows and view models |
| `agentdictate-app` | Process composition and both binaries |

See the [full architecture overview](docs/architecture.md).

See [dictation output and evaluation](docs/dictation-output.md) for vocabulary,
Literal mode, streaming, empty-capture handling, and audio replay.

For repository work, use the [development and verification workflow](docs/DEVELOPMENT.md).

## Uninstall

Stop and remove the app before removing the host udev rule that grants the
active local session keyboard-event and uinput access. Follow the
[uninstall steps](docs/INSTALL.md#uninstall). They keep any saved unencrypted
Platform API key, transcripts, retained recordings, logs, and cache until you
run the separate delete-all-data step.

Licensed under the [MIT License](LICENSE).
