# Develop and verify AgentDictate

## Check the checkout and host

Run `scripts/dev.sh doctor` before starting. It reports build tools, development
library metadata, optional tools, Git status, worktrees, disk space, and active
Rust or linker processes. A nonzero exit means a build prerequisite is missing.
See [installation requirements](INSTALL.md#requirements) for the package list.
The xkbcommon runtime-library fallback can allow local builds without development
metadata. A passing build proves that target links, not that packaging prerequisites
are complete. The doctor does not install packages or check native input access.

Work directly on `main`. Do not create branches, worktrees, or PRs. Coordinate
writers in this checkout and inspect `git diff` before staging. For independent
audit work, use read-only agents. Build artifacts share `target/`; do not run
concurrent broad gates, delete artifacts, or change `CARGO_TARGET_DIR`.

Before release builds, benchmarks, or the full gate, check the doctor's disk and
process output. Wait for competing builds to finish. Keep Cargo's default job
parallelism. Use the normal profiles for iterative work.

## Run focused checks with saved feedback

The runner accepts a crate suffix, `lib` or an integration harness, and an optional
test filter. It adds `--locked`, selects one target, and saves the command, Git
revision and status, compiler version, output, elapsed time, and exit status under
`$XDG_STATE_HOME/agentdictate/checks`, defaulting to
`~/.local/state/agentdictate/checks`.

```bash
scripts/dev.sh test core core replacements
scripts/dev.sh test core lib textfmt
scripts/dev.sh test runtime runtime history_usage
scripts/dev.sh test app app daemon_flow
scripts/dev.sh test ui contracts
scripts/dev.sh test ui desktop rendered_interactions
```

The desktop harness automatically enables `test-support`. Its GPUI tests drive
rendered controls in a headless test context without moving your mouse or opening
the app. An unknown harness, compilation failure, failing test, or filter with
zero passing tests returns nonzero. The runner does not enable ignored tests.
To rerun one failure, use its name as the filter and inspect the saved log.

For compiler or lint checks, use Cargo directly:

```bash
cargo check --locked -p agentdictate-ui --features desktop
cargo clippy --locked -p agentdictate-core --lib -- -D warnings
cargo clippy --locked -p agentdictate-runtime --lib -- -D warnings
```

Format only changed files with `rustfmt --edition 2024 <changed-files>`.
If a direct desktop build cannot find the xkbcommon linker names, use the same
fallback as the installer in a subshell:

```bash
(
  PROJECT_DIR="$PWD"
  source packaging/linker-runtime-fallback.sh
  cargo test --locked -p agentdictate-ui --test desktop --features test-support
)
```

## Measure replacement processing

After checking disk and competing builds, run `scripts/dev.sh bench` before and
after the implementation change on the same host. Avoid other heavy workloads
during measurement. The standalone benchmark uses the release profile and has
no additional dependencies. It covers no rules, disabled rules, a miss, a short
transcript, and a 30 KB transcript with 2,000 accepted replacements.

Each case doubles its iteration count until a batch takes at least 30 ms, then
reports the minimum, median, and maximum of nine batches in nanoseconds per call.
The timed section includes output allocation, destruction, and per-rule regex
compilation. Calibration warms the cached word-character matcher. Rust compilation
and fixture construction are excluded.
Compare medians, retain the complete logs, and report absolute savings as well as
ratios. These numbers measure replacement processing, not recording or network latency.

The benchmark exits without measuring when `cargo test --all-targets` invokes it.
Run correctness tests separately, including Unicode boundaries and ordered rules.

## Debug without changing the active desktop

Start with a failing test and `RUST_BACKTRACE=1 scripts/dev.sh test ...`. The daemon
writes daily files under `~/.local/state/agentdictate/logs` by default. Read only
the relevant error or timing lines; logs can contain private transcript data.
Inspect service state with:

```bash
systemctl --user show agentdictated -p ActiveState -p SubState -p MainPID -p ExecStart
```

For a native crash, use `coredumpctl info agentdictated` if the host collects
coredumps. If line tables are insufficient, build only the affected test target
with `--profile debugging --no-run` and run the printed executable under GDB.
This creates another artifact variant, so first check disk and competing builds.
Do not attach to or restart the active daemon just to debug a unit test.

## Complete the change

After focused checks pass, run `./run-tests.sh` exactly once. It checks the developer
runner, all Rust targets and features, native-readiness packaging fixtures, and
`cargo deny` when installed. Preserve its output and inspect skipped checks.

The automated layers prove domain behavior, SQLite and IPC contracts, mocked
provider responses, and headless GPUI interactions. They do not prove compositor
visibility, real microphone capture, transcription-provider access, or insertion
into another app. The ignored subscription test sends audio to an external service;
run it only when that separate check is intended and authorized. Do not describe
a headless run as a live desktop E2E pass.

Review the diff, commit on `main`, and push to `origin/main`. Check disk and build
activity again, then run `./install.sh` and
`systemctl --user restart agentdictated`. Verify that the service is active and
that `/proc/<MainPID>/exe` matches the installed `agentdictated` binary. Installation
exit status 2 means native access remains incomplete even though files were copied.
