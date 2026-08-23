<p align="center">
  <img src="assets/agentdictate.svg" alt="AgentDictate microphone icon" width="72">
</p>

<h1 align="center">AgentDictate</h1>

<p align="center"><strong>STT through your Codex subscription.</strong></p>
<p align="center">Press <kbd>Ctrl</kbd> + <kbd>Space</kbd>, speak, and paste the transcript into the focused app on Wayland or X11. AgentDictate uses your existing Codex sign-in.</p>
<p align="center"><code>Ctrl+Space → Speak → STT → Paste</code></p>
<p align="center"><sub>Codex subscription · No API key for STT · Rust + GPUI</sub></p>

<p align="center"><a href="#install"><strong>Install AgentDictate →</strong></a></p>

## Architecture

AgentDictate runs as two local processes. `agentdictated` owns the hotkey,
recording, STT, history, and paste workflow. `agentdictate` is the GPUI desktop
client. They communicate through a private Unix socket.

| Crate | Responsibility |
| --- | --- |
| `agentdictate-core` | Settings, protocol types, and workflow state |
| `agentdictate-runtime` | SQLite history, recovery, usage, and IPC |
| `agentdictate-linux` | Audio capture, hotkeys, focus, clipboard, and paste |
| `agentdictate-ui` | GPUI windows and view models |
| `agentdictate-app` | The two binaries and their composition |

## Install

The source installer currently targets Ubuntu/Debian. Install the [system prerequisites](docs/INSTALL.md#requirements), then:

```bash
git clone https://github.com/Luzivog/agentdictate.git
cd agentdictate
./install.sh
agentdictate
```

Open AgentDictate, set **Transcription source** to **ChatGPT subscription**, then press `Ctrl+Space` to start or stop recording. AgentDictate uses your Codex sign-in for STT, so this route does not require an API key. If native input needs attention, follow the installer’s guide or [configure it manually](packaging/NATIVE_ACCESS.md).

An OpenAI API key is only needed for optional transcript cleanup or the alternate OpenAI API transcription route. Standard API charges apply to those features. Saved history, recovery data, and settings remain on your machine.

<details>
<summary>Build, test, and package from source</summary>

See the complete [installation and development guide](docs/INSTALL.md).

</details>

[MIT](LICENSE)
