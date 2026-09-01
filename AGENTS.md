# Repository Guidelines

## Mainline Delivery Workflow

All work happens directly on `main`. Do not create feature branches, worktree
branches, or pull requests for this repository. When a change is complete and
its focused tests pass: commit on `main`, push to `origin/main`, rebuild and
reinstall the release binaries with `./install.sh`, and restart the running
daemon (`systemctl --user restart agentdictated`) so the change is actually
live. A change is not done until it is committed, pushed, and running.

## Project Structure & Module Organization

AgentDictate is a Rust workspace for a native Linux dictation app. The workspace
uses the Rust toolchain pinned in `rust-toolchain.toml` and is split by
responsibility:

- `crates/agentdictate-core`: domain types, settings, replacements, workflow,
  and protocol logic.
- `crates/agentdictate-runtime`: durable history, recovery, usage, pricing, and
  IPC persistence.
- `crates/agentdictate-linux`: Linux recording, hotkey, focus, clipboard, and
  paste integrations.
- `crates/agentdictate-ui`: GPUI presentation, view models, route surfaces, and
  the recording overlay. Native UI code is behind the `desktop` feature.
- `crates/agentdictate-app`: daemon and desktop composition plus the
  `agentdictate` and `agentdictated` binaries.

Rust unit tests live beside their modules. Integration-test harnesses live in
each crate's `tests/` directory; prefer a small number of coherent top-level
harnesses with module files rather than one linked executable per test topic.
Keep separate harnesses only when process or global-state isolation is part of
the behavior being tested.

Static desktop assets live in `assets/` and `agentdictate.desktop`. Packaging
scripts are under `packaging/`; `dist/` and `target/` contain generated
artifacts and must not be edited directly.

## Development Commands

Use the narrowest command that exercises the code being changed. Preserve
Cargo's native/default job parallelism; do not add a fixed `-j` or `jobs` cap.

- UI compilation: `cargo check --locked -p agentdictate-ui --features desktop`
- Focused library test: `cargo test --locked -p <package> --lib <test-filter>`
- One integration harness: `cargo test --locked -p <package> --test <harness> <test-filter>`
- Focused lint: `cargo clippy --locked -p <package> --lib -- -D warnings`
- Run the desktop app: `./run.sh`
- Run only the daemon/background app: `./run.sh --background`

A test-name filter by itself is not a narrow command: without `-p`, `--lib`, or
`--test`, Cargo can still compile every selected integration-test executable.
Use only the features required by the affected target. Do not repeatedly use
`--workspace`, `--all-targets`, or `--all-features` during implementation.

The workspace profiles intentionally retain incremental compilation for normal
iterative development while limiting debug information. Use
`--profile debugging` only when full debug information is actually needed.
`CARGO_INCREMENTAL=0` is appropriate only for a coordinated one-shot or
ephemeral full gate, not as a global configuration.

## Disk-Heavy Gate Coordination

Before any workspace-wide, all-target, all-feature, release, or LTO build,
check both available filesystem space and whether another `cargo`, `rustc`, or
linker workload is active. If another broad gate is running or free space is
unsafe, continue non-build work and report the wait instead of starting a
second bulk writer. Do not interrupt or stop the other task.

Cargo does not automatically garbage-collect a workspace's `target/`
directory. Never run `cargo clean`, delete target artifacts, redirect or change
a shared/global `CARGO_TARGET_DIR`, or install/configure `sccache` or another
linker without explicit user approval. A profile change creates a new artifact
variant, so avoid uncontrolled full rebuilds while an oversized target remains.

## Final Test Gate

`./run-tests.sh` is the one final comprehensive local gate. It runs the locked
Rust workspace with every target and feature, the native-readiness packaging
checks, and `cargo deny check` when `cargo-deny` is installed. The gate is
local-only by design; CI runs tag-gated packaging only. Run it exactly once
after focused checks pass and only after the disk-heavy gate coordination above
says it is safe. Do not use it as an inner loop command.

For ordinary changes, add focused Rust tests to the affected crate and mock
network, subprocess, clipboard, audio, desktop, and external-service
boundaries. Prefer headless GPUI tests and deterministic fixtures. Do not open
the application visibly, move the user's mouse, or interfere with their active
desktop during automated verification.

## Coding Style & Naming Conventions

Use idiomatic Rust 2024, explicit domain types, exhaustive matching, and small
interfaces between crates. Keep modules and functions in `snake_case`, types
and traits in `PascalCase`, and constants in `SCREAMING_SNAKE_CASE`. Run
`cargo fmt` on changed Rust files and keep comments concise and synchronized
with behavior. Avoid `unsafe` unless a Linux integration requires it and its
safety contract is documented next to the boundary.

## Packaging and Installation

- `./install.sh`: builds the release desktop binaries and installs them, the
  desktop entry, autostart entry, and icon into the user profile.
- `packaging/build-deb.sh`: builds the Debian package in `dist/`.
- `packaging/build-appimage.sh`: builds `dist/AppDir` and an AppImage when
  `appimagetool` is available.

These are release/LTO-style disk-heavy operations. Coordinate them using the
same free-space and active-workload rules, and do not run them for ordinary
source validation.

## Security & Runtime Data

Do not commit real OpenAI API keys, local configuration, SQLite history, logs,
temporary or retained audio, IPC sockets, diagnostics, or generated package
contents. Runtime data belongs under `~/.config/agentdictate/`,
`~/.local/share/agentdictate/`, `~/.local/state/agentdictate/`, and
`~/.cache/agentdictate/`.

## UI Placement Requirement

The recording status overlay must remain horizontally centered at the bottom
of the primary monitor, above the dock/taskbar. Do not move it to a corner,
attach it to the settings window, or let backend-specific window changes alter
this placement unless explicitly requested.
