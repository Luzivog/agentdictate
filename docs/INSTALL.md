# Install AgentDictate

## Requirements

- A Linux desktop session on Wayland or X11. Wayland sessions need XWayland, which
  AgentDictate uses for its overlay, focus reading, and clipboard. GNOME on Ubuntu
  24.04 is the tested setup, and Debian 13 uses the same packages.
- A running PipeWire session and a working systemd user manager.
- An [OpenAI Platform](https://platform.openai.com/) API key. Transcription is billed
  to that account.
- Rust through [rustup](https://rustup.rs/), for building from source. The
  repository pins Rust 1.95.0 in `rust-toolchain.toml`.

On Ubuntu 24.04 or Debian 13, install the build and runtime packages:

```bash
sudo apt install build-essential git pkg-config libxkbcommon-dev libxkbcommon-x11-dev \
  libfontconfig1-dev libfreetype6-dev libvulkan1 libegl1 \
  pipewire-bin pulseaudio-utils ffmpeg
```

`pipewire-bin` provides `pw-record`, which records the microphone.
`pulseaudio-utils` provides `pactl`, which lowers other audio while you dictate.
`ffmpeg` compresses each recording before upload; without it, AgentDictate uploads
the raw WAV, about 8 times larger. The clipboard and paste injection are built into
AgentDictate and need no extra tools.

AgentDictate also has an experimental route that uses the ChatGPT account signed into
the [Codex CLI](https://learn.chatgpt.com/docs/codex/cli) instead of an API key. It
needs `codex login` with **Sign in with ChatGPT**.

## Install from source

```bash
git clone https://github.com/Luzivog/agentdictate.git
cd agentdictate
./install.sh
```

The installer builds the release binaries and installs, for your user only:

- `agentdictate` and `agentdictated` in `~/.local/bin`;
- the desktop entry and icon;
- the input-access rule and its guide in `~/.local/share/agentdictate/native-access`.

It does not use `sudo`, and it never enables or starts a service. When you reinstall
while the daemon is running, it restarts the daemon so the new build takes over. If
the xkbcommon development packages are missing, it links against the runtime
libraries through shims under `target/linker-shims`.

The installer ends by checking native input access and exits with its status:

| Exit status | Meaning | What to do |
| --- | --- | --- |
| 0 | Ready | Open AgentDictate. |
| 2 | AgentDictate cannot read the keyboard or use `/dev/uinput` yet | Run `./install.sh --setup-native-access`. It shows the one `sudo` command it needs and asks first. |
| 3 | Working, but another app's udev rule makes input devices world-accessible | AgentDictate works. The message names the rule to remove or narrow. |

After granting access, you may need to log out and back in. Return to the cloned
directory and check again without rebuilding:

```bash
./install.sh --check-native-access
```

[Native input access](../packaging/NATIVE_ACCESS.md) explains the rule and the
manual steps.

## Packages

Tagged releases publish a Debian package and an AppImage on the
[releases page](https://github.com/Luzivog/agentdictate/releases). The newest
release can be older than `main`.

- **Debian package.** `sudo apt install ./agentdictate_*.deb` installs the input
  rule and applies it. You may need to log out and back in once.
- **AppImage.** Run `./AgentDictate-*.AppImage setup-access` once to install the
  input rule, then start the AppImage.

To build them yourself:

```bash
packaging/build-deb.sh
packaging/build-appimage.sh
```

The Debian builder writes the package to `dist/` and needs `dpkg-dev`. The AppImage
builder always creates `dist/AppDir`, and creates the AppImage when `appimagetool` is
on `PATH`, set in `APPIMAGETOOL`, or present in `dist/tools`. Build AppImages on the
oldest glibc you want to support.

## Start and stop

Nothing runs until you open AgentDictate from the app menu or with `agentdictate`.
The first launch writes the `agentdictated.service` systemd user unit and starts the
daemon. While **Start on login** is on, the daemon enables the unit so it starts with
later desktop sessions. [The README](../README.md#first-use) covers first use.

Closing the settings window leaves the daemon and the global shortcut running. Stop
them with **Quit AgentDictate** in the tray, or with:

```bash
systemctl --user stop agentdictated.service
```

Turning **Start on login** off only affects future logins.

## Local data and network use

**What leaves your computer.** Each dictation's audio goes to OpenAI's
`/v1/audio/transcriptions` endpoint, with the language hint, the context text, and
your vocabulary spellings, unless you use Literal mode, which sends only the
language. With **Stream speech** on, audio is sent while you speak, and a failed
stream falls back to the normal upload, which can mean paying for both. A request
that fails before OpenAI answers is sent once more.

The experimental ChatGPT subscription route sends the audio and language hint to an
undocumented ChatGPT endpoint instead. It gets a short-lived ChatGPT token from the
Codex CLI, keeps it in memory, and never falls back to the paid API. The endpoint is
not a supported OpenAI API and can stop working without notice.

**What stays on your computer.**

- The OpenAI API key is stored in plain text in `~/.config/agentdictate/config.json`,
  readable only by you.
- While a dictation is in progress, its text and audio are kept so it can be
  recovered. After the paste, the text is kept only in History, and only when
  **Save history** is on. Usage numbers, such as duration, word count, model, and
  estimated cost, are always kept, without text.
- **Delete** on a History item removes its text and its usage numbers for good.
  **Delete all history…** under **Settings**, **Privacy** does that for every item.
  **Save history** and **Preserve temporary audio** are in the same section.
- A failed dictation keeps its text and recording in Recovery until you retry it or
  delete it. Deleting it removes both.
- Audio is deleted after the paste unless **Preserve temporary audio** is on. Each
  daemon start also deletes leftover recordings that no dictation needs.
- Logs can contain transcript text. The newest 14 daily files are kept.
- While the daemon runs, it imports the completed dictations that the ChatGPT desktop
  app saves under `$CODEX_HOME/dictation-history` (default
  `~/.codex/dictation-history`), including their transcripts, into History and the
  usage totals. There is no setting to turn this off, and **Save history** does
  not affect it. A deleted import stays deleted, but a new database imports every
  source file again.

The database, recordings, and logs are not encrypted; Unix permissions protect them.
They live in these directories, which `XDG_CONFIG_HOME`, `XDG_DATA_HOME`,
`XDG_STATE_HOME`, and `XDG_CACHE_HOME` can move:

- `~/.config/agentdictate/`
- `~/.local/share/agentdictate/`
- `~/.local/state/agentdictate/`
- `~/.cache/agentdictate/`

[The architecture overview](architecture.md#data-locations) lists every file.

## Uninstall

First, as each user who ran AgentDictate, stop the daemon and remove the per-user
service files. Package removal does not own these files.

```bash
systemctl --user disable --now agentdictated.service

agentdictate_data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
agentdictate_config_home="${XDG_CONFIG_HOME:-$HOME/.config}"

rm -f -- \
  "$agentdictate_data_home/systemd/user/agentdictated.service" \
  "$agentdictate_config_home/autostart/local.agentdictate.AgentDictate.desktop"
systemctl --user daemon-reload
```

The autostart entry exists only if an older version was installed.

**Repository install.** Remove the installed files and the host udev rule:

```bash
agentdictate_data_home="${XDG_DATA_HOME:-$HOME/.local/share}"

rm -f -- \
  "$HOME/.local/bin/agentdictate" \
  "$HOME/.local/bin/agentdictated" \
  "$agentdictate_data_home/applications/local.agentdictate.AgentDictate.desktop" \
  "$agentdictate_data_home/icons/hicolor/scalable/apps/agentdictate.svg"
rm -rf -- "$agentdictate_data_home/agentdictate/native-access"

sudo rm -f -- /etc/udev/rules.d/70-agentdictate-input.rules
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=input --action=change
sudo udevadm trigger --subsystem-match=misc --sysname-match=uinput --action=change
```

**Debian package.** `sudo apt purge agentdictate` removes the package and its rule.

**AppImage.** Remove the host udev rule with the `sudo` commands above, delete the
`native-access` directory as above, and delete the AppImage file.

These steps keep your settings and API key, the database, recordings, and logs. To
delete all AgentDictate data as well, review the printed directories, then remove
them:

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

This does not delete the ChatGPT desktop app's own files under `$CODEX_HOME`, so
a later install imports them again.
