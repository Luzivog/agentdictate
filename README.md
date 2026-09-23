<p align="center">
  <img src="assets/agentdictate.svg" alt="AgentDictate microphone icon" width="72">
</p>

<h1 align="center">AgentDictate</h1>

<p align="center">
  <strong>Fast dictation for Linux, built for talking to AI coding agents.</strong><br>
  Press <kbd>Ctrl</kbd> + <kbd>Space</kbd>, speak, and press it again.
  AgentDictate transcribes your speech with OpenAI and pastes the text into the
  app you are using, on Wayland or X11.
</p>

AgentDictate runs in the background with a global shortcut. A small overlay at the
bottom of your screen shows that it is listening. When you stop, the audio goes to
OpenAI's `gpt-transcribe` model with your API key, and the text is pasted into the
focused window. If anything goes wrong, the recording is kept so you can try again.

## Requirements

- Linux with a Wayland or X11 desktop session; Wayland needs XWayland. Tested on
  GNOME with Ubuntu 24.04.
- PipeWire and a systemd user session, which current Ubuntu and Debian have.
- An [OpenAI Platform](https://platform.openai.com/) API key. You pay OpenAI per
  minute of audio.

The [install guide](docs/INSTALL.md#requirements) lists the packages to install.

## Install

From source, with [Rust](https://rustup.rs/) and the packages above installed:

```bash
git clone https://github.com/Luzivog/agentdictate.git
cd agentdictate
./install.sh
```

AgentDictate needs permission to read the keyboard for its shortcut and to type the
paste shortcut. If the installer says access is missing, run the command it prints:

```bash
./install.sh --setup-native-access
```

It shows the one `sudo` command it needs and asks before running it. You may need
to log out and back in afterwards.

A Debian package and an AppImage are also published on the
[releases page](https://github.com/Luzivog/agentdictate/releases), though they can
lag behind this repository. See [packages](docs/INSTALL.md#packages).

## First use

1. Open **AgentDictate** from your app menu, or run `agentdictate`. This starts the
   background service, which also starts with every later login while **Start
   AgentDictate when I log in** is on.
2. In **Settings**, paste your OpenAI API key and click **Save key**.
3. Click into any text field, press <kbd>Ctrl</kbd> + <kbd>Space</kbd>, speak, and
   press it again. Keep that field focused until the text appears.

Press <kbd>Esc</kbd> while recording to throw the recording away. If you recorded
for more than 5 seconds, it waits in **History**, under **Recovery**, for a day in
case you change your mind. You can close the window; dictation keeps working. To stop AgentDictate, choose **Quit AgentDictate**
in the tray menu.

## Everyday use

- **Shortcut.** Change it, or switch from press-to-toggle to hold-to-talk, under
  **Settings**, **Dictation shortcut**. Settings apply as you change them.
- **Spelling of names.** Add product and project names under **Words**. A
  **Sounds like** entry, such as `cube control` for `kubectl`, also fixes that
  spoken form every time. **Fix a word** on an expanded History transcript adds one
  for you. See [dictation output](docs/dictation-output.md).
- **Exact text.** **Start literal dictation** in the tray skips vocabulary and
  context for one recording.
- **Something failed.** Open **History**. Items under **Recovery** keep their audio
  for 7 days; **Transcribe again** or **Paste again** copies the text so you can paste
  it with <kbd>Ctrl</kbd> + <kbd>V</kbd>.

## Privacy

- Your audio is sent to OpenAI for each dictation, with your language, context, and
  vocabulary hints. Nothing is sent while you are not recording.
- Your API key is stored unencrypted in `~/.config/agentdictate/config.json`,
  readable only by you.
- After a successful paste, AgentDictate keeps the text only in History, for as long
  as **Keep transcripts** says: **Forever** (the default), **30 days**, or **Don't
  keep**. A shorter choice asks first, then deletes older saved text the next time
  you dictate or start AgentDictate. The audio is deleted after the paste, unless you
  turn on **Keep audio recordings** under **Settings**, **Show advanced settings**.
- **Delete** on a History item deletes it for good, and **Delete all history…** in
  **Settings** deletes every transcript. Deleted text is overwritten on disk, not
  just hidden.
- A failed dictation waits in **Recovery**, with its text and audio, for 7 days, then
  both are deleted. A recording you cancelled with <kbd>Esc</kbd> after more than 5
  seconds waits there for a day.
- Usage numbers, such as minutes and estimated cost, are kept without text.
- Logs can contain transcript text.

The [install guide](docs/INSTALL.md#local-data-and-network-use) has the details.

## Uninstall

Follow the [uninstall steps](docs/INSTALL.md#uninstall). They keep your settings,
API key, and history until you run the separate step that deletes all data.

## Documentation

- [Install guide](docs/INSTALL.md): requirements, packages, data, and uninstall.
- [Native input access](packaging/NATIVE_ACCESS.md): the keyboard and paste
  permission.
- [Dictation output](docs/dictation-output.md): vocabulary, Literal mode, streaming,
  failures, and evaluation.
- [Architecture](docs/architecture.md): processes, crates, pipeline, and decisions.
- [Development](docs/DEVELOPMENT.md): building, testing, and debugging.

Licensed under the [MIT License](LICENSE).
