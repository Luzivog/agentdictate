# Install and develop AgentDictate

## Requirements

AgentDictate targets Linux desktop sessions on Wayland and X11. On
Ubuntu/Debian, install the runtime and build dependencies:

```bash
sudo apt install build-essential pkg-config libxkbcommon-dev libxkbcommon-x11-dev \
  libfontconfig1-dev libfreetype6-dev libvulkan1 x11-utils \
  pipewire-bin wl-clipboard xsel ydotool ydotoold xdotool
```

Install [Rust](https://rustup.rs/) if `cargo` is unavailable. The repository
selects the supported toolchain through `rust-toolchain.toml`.

## User-profile install

```bash
git clone https://github.com/Luzivog/agentdictate.git
cd agentdictate
./install.sh
```

The installer builds the release binaries and copies the app, desktop entry,
autostart entry, icon, and native-input support files into your user profile.
It does not use `sudo` or start services.

On a clean machine, installation can finish with exit status 2 after the files
have been copied. This means native input still needs setup. Follow
[`packaging/NATIVE_ACCESS.md`](../packaging/NATIVE_ACCESS.md), then verify it
without rebuilding:

```bash
./install.sh --check-native-access
```

Run `agentdictate` (or `~/.local/bin/agentdictate`), add your OpenAI API key in
**Settings**, and press `Ctrl+Space` to start or stop recording.

## Run from source

```bash
./run.sh                 # desktop app
./run.sh --background    # daemon only
```

## Focused development

Use the narrowest command that covers the change:

```bash
cargo check --locked -p agentdictate-ui --features desktop
cargo test --locked -p agentdictate-runtime --lib <test-filter>
cargo test --locked -p agentdictate-ui --test contracts <test-filter>
cargo clippy --locked -p agentdictate-ui --lib --features desktop -- -D warnings
```

After focused checks pass, `./run-tests.sh` is the one comprehensive gate. It
tests the Rust workspace and the legacy Python migration-parity suite; the
installed application does not use Python.

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
install host udev policy; build them on the oldest glibc version you intend to
support.

## Local data and network use

Audio is sent to OpenAI for transcription. When cleanup is enabled, transcript
text is also sent to OpenAI. Settings, saved history, diagnostics, and recovery
files are stored locally under:

- `~/.config/agentdictate/`
- `~/.local/share/agentdictate/`
- `~/.local/state/agentdictate/`
- `~/.cache/agentdictate/`
