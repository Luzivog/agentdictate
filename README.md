# AgentDictate - Linux Push-to-Talk Dictation for AI Agents

Personal Linux push-to-talk dictation for AI coding prompts. Press
`Ctrl+Space`, speak, and AgentDictate transcribes with OpenAI, optionally cleans
the text, applies replacements, copies it to the clipboard, and pastes it.

- OpenAI speech-to-text transcription
- Prompt cleanup for coding agents
- Custom text replacements
- Wayland and X11 paste support
- Local transcript history and usage stats

<p>
  <img src="docs/images/agentdictate-overview.png" alt="Overview tab" width="32%">
  <img src="docs/images/agentdictate-dictation.png" alt="Dictation tab" width="32%">
  <img src="docs/images/agentdictate-replacements.png" alt="Replacements tab" width="32%">
</p>

## Install

Install system dependencies first:

```bash
sudo apt install python3 python3-gi gir1.2-gtk-3.0 python3-cairo python3-requests \
  pipewire-bin wl-clipboard ydotool xdotool
```

Then install AgentDictate into your user profile:

```bash
git clone https://github.com/Luzivog/agentdictate.git
cd agentdictate
./install.sh
agentdictate
```

Paste your OpenAI API key in the OpenAI tab, save, then use `Ctrl+Space` to
start and stop dictation.

## Run From Source

```bash
./run.sh
./run.sh --background
```

## Test

```bash
./run-tests.sh
```
