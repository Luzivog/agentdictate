# AgentDictate Refactor Plan

## Goals

- Keep recording, transcription, cleanup, paste, storage, and UI concerns in clear modules.
- Preserve existing runtime behavior while reducing the size and coupling of `ui.py`.
- Prefer small, mechanical moves first so behavior changes remain easy to review.

## Current Boundaries

- `controller.py` coordinates the dictation workflow and owns cross-cutting session state.
- `ui.py` owns the GTK application shell, settings tabs, tray menu, status propagation, overlay, history views, stats views, and custom drawing.
- `storage.py` owns persistence and reporting queries.
- Audio capture, playback feedback, ducking, hotkeys, clipboard, OpenAI calls, startup, and replacements already live in separate modules.

## Phase 1: UI Infrastructure Extraction

Status: implemented.

- Move reusable GTK drawing widgets to `widgets.py`.
- Move the recording status overlay, overlay helper process, and helper IPC to `overlay.py`.
- Keep overlay placement logic unchanged: centered near the bottom of the primary monitor and constrained above the work area.
- Keep `AgentDictateGtkApp` responsible for application lifecycle, tray integration, settings pages, history, and stats wiring.

## Phase 2: Settings UI Decomposition

Status: planned.

- Extract settings tab builders into a `settings_ui.py` module or package.
- Keep persistence of `Settings` in `config.py`; UI modules should only read from and write to a `Settings` instance.
- Preserve `AgentDictateGtkApp` as the owner of save/apply actions until the settings surface is split enough to introduce a presenter cleanly.

## Phase 3: History And Stats Views

Status: planned.

- Extract history list/detail rendering and actions into a dedicated view module.
- Extract stats labels and graph refresh logic into a stats view module.
- Keep storage queries in `storage.py`; view modules should receive prepared rows or call through app-owned storage adapters.

## Phase 4: Controller Workflow Boundaries

Status: planned.

- Split long workflow branches in `controller.py` into session-oriented helpers only if duplication or test complexity justifies it.
- Keep external side effects injectable where practical: audio recorder, OpenAI client, clipboard paste, feedback, storage, and audio ducking.
- Add focused unit tests around each extracted workflow seam before changing behavior.

## Review Rules

- Each phase should pass `./run-tests.sh`.
- Refactor-only commits should not change public defaults, config schema meaning, overlay placement, or runtime data locations.
- Behavior changes should be separate commits from mechanical moves unless the behavior change is required to make the move safe.
