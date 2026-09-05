# Install and develop AgentDictate

## Requirements

AgentDictate targets Linux desktop sessions on Wayland and X11. On Ubuntu 24.04,
install the runtime and build dependencies:

```bash
sudo apt install build-essential git pkg-config libxkbcommon-dev libxkbcommon-x11-dev \
  libfontconfig1-dev libfreetype6-dev libvulkan1 x11-utils \
  pipewire-bin xsel xdotool
```

Debian 13 uses the same package list. Paste injection is built into
AgentDictate (an in-process uinput virtual keyboard) and needs write access to
`/dev/uinput`, granted by the packaged udev rule.

Install [Rust with rustup](https://rustup.rs/). The repository selects Rust
1.95.0 through `rust-toolchain.toml`. Recording also requires a running
PipeWire session and a graphical session with a working systemd user manager.

For subscription STT, install the
[Codex CLI](https://learn.chatgpt.com/docs/codex/cli), run `codex login`, and
choose **Sign in with ChatGPT**. `codex login status` must report
`Logged in using ChatGPT`. An API-key login cannot authenticate this route.

## User-profile install

```bash
git clone https://github.com/luzivog/agentdictate.git
cd agentdictate
./install.sh
```

The installer builds the release binaries and copies the app, desktop entry,
graphical-session user service, login bootstrap, icon, and native-input support
files into your user profile. It does not use `sudo` or start services.

On a fresh install, the login bootstrap starts the daemon at the next desktop
login. That imports eligible ChatGPT desktop dictation transcripts and metadata
into local SQLite. Before starting the daemon manually or logging out, read
[Local data and network use](#local-data-and-network-use).

On a clean machine, installation can finish with exit status 2 after the files
have been copied. This means native input still needs setup. Follow
[`packaging/NATIVE_ACCESS.md`](../packaging/NATIVE_ACCESS.md). After any logout
and login, return to the cloned `agentdictate` directory. Then verify without
rebuilding:

```bash
./install.sh --check-native-access
```

Continue only when the final line is `Native input readiness: ready`.

Run `agentdictate` or `~/.local/bin/agentdictate`. In **Settings**, set
**Transcription source** to **ChatGPT subscription**. For the no-key path, turn
**Cleanup** off and click **Save changes**. Press `Ctrl+Space` once to start
recording. Before pressing it again, focus the destination app and keep it
focused until AgentDictate submits the paste shortcut. The shortcut targets the
app focused at paste time. AgentDictate confirms shortcut submission, not
insertion into that app.

Closing the settings window leaves the daemon, global shortcut, and transcript
import running. Stop them with **Quit AgentDictate** in the tray or
`systemctl --user stop agentdictated.service`.

**Start on login** controls the AgentDictate daemon only.

## Run from source

```bash
./run.sh                 # desktop app
./run.sh --background    # start the background user service
./run.sh --service       # run the daemon directly as the service process
```

## Focused development

See [Develop and verify AgentDictate](DEVELOPMENT.md) for the setup doctor,
saved test feedback, headless desktop tests, benchmarks, debugging, and delivery.

Use the narrowest command that covers the change:

```bash
cargo check --locked -p agentdictate-ui --features desktop
cargo test --locked -p agentdictate-runtime --lib <test-filter>
cargo test --locked -p agentdictate-ui --test contracts <test-filter>
cargo clippy --locked -p agentdictate-ui --lib --features desktop -- -D warnings
```

After focused checks pass, `./run-tests.sh` is the one comprehensive gate. It
tests every target and feature in the locked Rust workspace.

## Build packages

```bash
packaging/build-deb.sh
packaging/build-appimage.sh
```

The Debian builder writes a package under `dist/` and requires Debian packaging
tools such as `dpkg-dev`. The AppImage builder always creates `dist/AppDir` and
creates an AppImage when `appimagetool` is available.

Both formats require the native-input setup described in
[`packaging/NATIVE_ACCESS.md`](../packaging/NATIVE_ACCESS.md). AppImages cannot
install host udev policy. Build them on the oldest glibc version you intend to
support.

## Local data and network use

When **ChatGPT subscription** is selected, AgentDictate uses an undocumented
Codex App Server auth-status method to get an in-memory ChatGPT bearer token,
derives the account ID from its claims, then sends audio and any configured
language to an undocumented ChatGPT endpoint as `Codex Desktop`. The request
authenticates with the bearer token and account ID. AgentDictate does not write
the token to its own files. Codex manages its login cache separately and may
store credentials unencrypted in `$CODEX_HOME/auth.json`. See
[Codex credential storage](https://learn.chatgpt.com/docs/auth#credential-storage).

OpenAI says ordinary Codex use in a ChatGPT Enterprise workspace follows that
workspace's retention and residency settings. It does not document whether
those policies or training controls cover AgentDictate's direct call, whether
third-party clients may make it, account eligibility, allowance, or billing.
The route may not be enabled for every account and can stop working without
notice. It does not require an OpenAI Platform API key.

The OpenAI API transcription route sends the recording with the selected model
and any applicable language or prompt text. Cleanup sends the transcript,
cleanup instructions, selected model, and optional reasoning effort. Both
require a Platform API key and can incur Platform charges. A suspiciously short
API transcription can trigger one paid retry with `whisper-1`.

When a Platform API key is saved, daemon startup and model-catalog refreshes use
it for an authenticated `/v1/models` request. This happens even when
**ChatGPT subscription** is selected and **Cleanup** is off. AgentDictate caches
the returned model IDs and a key fingerprint in its XDG cache directory.

The Platform API key is stored unencrypted in the XDG config directory with
user-only `0600` permissions. The SQLite database and retained WAV files are
also unencrypted and protected by local Unix permissions. Once transcription
succeeds, raw and final text, plus cleaned text when available, remain in
durable SQLite job rows after successful shortcut submission or delivery
failure. Deleting an item from Recovery removes its recording and marks the job
deleted. It does not remove the transcript. **Save history** controls additional
History and usage rows, not the durable job rows. While it is off, those
additional rows are skipped. If it is later enabled, restarting the daemon
backfills them from submitted durable jobs. There is no in-app purge for the
durable job rows.

While the daemon runs, AgentDictate imports existing and new completed ChatGPT
desktop dictation records that contain a duration and a nonblank transcript.
Other records are skipped. The source directory is
`$CODEX_HOME/dictation-history`, defaulting to `~/.codex/dictation-history`.
AgentDictate stores each imported record in SQLite, including the transcript,
dictation ID, creation time, duration, derived end time, and import time. The
transcript, derived end time, and duration appear in History. The creation date,
word count, and duration contribute to usage totals. There is no in-app opt-out
or purge, and **Save history** does not disable the import. Deleting
AgentDictate's database does not delete the source metadata. The next daemon
start imports it again.

Recordings are created in the XDG data directory. Audio is normally deleted
after paste submission. Failed or interrupted recordings remain for recovery,
and **Preserve temporary audio** also keeps recordings after successful shortcut
submission.
The default local paths are:

- `~/.config/agentdictate/`
- `~/.local/share/agentdictate/`
- `~/.local/state/agentdictate/`
- `~/.cache/agentdictate/`

Set `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, or `XDG_CACHE_HOME` to
move the corresponding directory.

## Uninstall

Each user who ran AgentDictate must first stop the services and remove the
per-user startup files. Package removal does not own these generated files.

```bash
systemctl --user disable --now agentdictated.service

agentdictate_data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
agentdictate_config_home="${XDG_CONFIG_HOME:-$HOME/.config}"

rm -f -- \
  "$agentdictate_config_home/autostart/local.agentdictate.AgentDictate.desktop" \
  "$agentdictate_data_home/systemd/user/agentdictated.service"
systemctl --user daemon-reload
```

For a repository user install, remove the remaining user-profile files and the
host udev rule:

```bash
agentdictate_data_home="${XDG_DATA_HOME:-$HOME/.local/share}"

rm -f -- \
  "$HOME/.local/bin/agentdictate" \
  "$HOME/.local/bin/agentdictated" \
  "$agentdictate_data_home/applications/local.agentdictate.AgentDictate.desktop" \
  "$agentdictate_data_home/icons/hicolor/scalable/apps/agentdictate.svg" \
  "$agentdictate_data_home/agentdictate/native-access/70-agentdictate-input.rules" \
  "$agentdictate_data_home/agentdictate/native-access/NATIVE_ACCESS.md"

sudo rm -f -- /etc/udev/rules.d/70-agentdictate-input.rules
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=input --action=change
sudo udevadm trigger --subsystem-match=misc --sysname-match=uinput --action=change
```

For a Debian package, run the common per-user teardown above, then use the
package manager:

```bash
sudo apt purge agentdictate
```

For an AppImage, run the common teardown, remove the host udev rule with the
commands above, and delete the AppImage file. If you used the discouraged
`input`-group fallback, review other tools that need it before removing that
group membership.

These steps keep any saved unencrypted Platform API key, the SQLite database,
retained recordings, logs, and cache. Delete those data directories only as a
separate, explicit choice.

To permanently delete all AgentDictate data after uninstalling, review the
resolved targets, then remove only these application directories:

```bash
agentdictate_config_home="${XDG_CONFIG_HOME:-$HOME/.config}"
agentdictate_data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
agentdictate_state_home="${XDG_STATE_HOME:-$HOME/.local/state}"
agentdictate_cache_home="${XDG_CACHE_HOME:-$HOME/.cache}"

printf '%s\n' \
  "$agentdictate_config_home/agentdictate" \
  "$agentdictate_data_home/agentdictate" \
  "$agentdictate_state_home/agentdictate" \
  "$agentdictate_cache_home/agentdictate"

rm -rf -- \
  "$agentdictate_config_home/agentdictate" \
  "$agentdictate_data_home/agentdictate" \
  "$agentdictate_state_home/agentdictate" \
  "$agentdictate_cache_home/agentdictate"
```

This does not delete ChatGPT desktop metadata under `$CODEX_HOME`. Reinstalling
or restarting AgentDictate imports that source data again unless it is removed
separately.
