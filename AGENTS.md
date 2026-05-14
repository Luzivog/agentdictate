# Repository Guidelines

## Project Structure & Module Organization

AgentDictate is a Python 3.11+ Linux desktop app. Application code lives in `src/agentdictate/`, with the console entry point in `src/agentdictate/__main__.py`. Core modules include recording (`audio.py`), hotkeys (`hotkey.py`), clipboard/paste behavior (`clipboard.py`), OpenAI API calls (`openai_client.py`), persistence (`storage.py`), settings (`config.py`), and GTK UI (`ui.py`). Tests are in `tests/`, currently concentrated in `tests/test_core.py`. Static desktop assets live in `assets/` and `agentdictate.desktop`. Packaging scripts are under `packaging/`; `dist/` contains generated build artifacts and should not be edited directly.

## Build, Test, and Development Commands

- `./run.sh`: runs the app from source with `PYTHONPATH=src`.
- `./run.sh --background`: starts the tray/background app without opening settings.
- `./run-tests.sh`: runs `python3 -m unittest discover -s tests -v`.
- `./install.sh`: installs the local wrapper and desktop entry into the user profile.
- `packaging/build-deb.sh`: builds `dist/agentdictate_<version>_all.deb`.
- `packaging/build-appimage.sh`: builds `dist/AppDir` and an AppImage when `appimagetool` is available.

## Coding Style & Naming Conventions

Use idiomatic Python with 4-space indentation, type hints, and `from __future__ import annotations` in new modules. Prefer `pathlib.Path`, dataclasses for structured settings/data records, and small functions with explicit return types. Keep module names lowercase with underscores, class names in `PascalCase`, and functions, variables, and tests in `snake_case`. No formatter or linter config is currently checked in, so match the surrounding style and keep imports grouped as standard library, third-party, then local relative imports.

## Testing Guidelines

The project uses the standard library `unittest` framework plus `unittest.mock`. Add tests under `tests/` using files named `test_*.py`, classes ending in `Tests`, and methods beginning with `test_`. Prefer isolated tests with `tempfile.TemporaryDirectory()` for config, database, and audio fixtures. Mock network calls, subprocesses, clipboard tools, and desktop environment dependencies. Run `./run-tests.sh` before submitting changes.

## Commit & Pull Request Guidelines

This checkout does not include Git history, so no repository-specific commit convention can be inferred. Use short imperative commit subjects such as `Fix Wayland paste fallback` or `Add pricing repair test`. Pull requests should include a focused summary, test results, linked issues if applicable, and screenshots or notes for visible UI changes.

## Security & Configuration Tips

Do not commit real OpenAI API keys, local config, SQLite history, logs, temporary audio, or generated package contents. Runtime data belongs under `~/.config/agentdictate/`, `~/.local/share/agentdictate/`, `~/.local/state/agentdictate/`, and `~/.cache/agentdictate/`.

## UI Placement Requirement

The recording status overlay must remain horizontally centered at the bottom of the primary monitor, above the dock/taskbar. Do not move it to a corner, attach it to the settings window, or let backend-specific window changes alter this placement unless explicitly requested.
