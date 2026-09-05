#!/usr/bin/env bash
# Focused checks with durable output and explicit failure when a filter matches nothing.
set -euo pipefail
PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${PROJECT_DIR}"

usage() {
  echo "Usage: scripts/dev.sh doctor | test <core|runtime|linux|ui|app> <lib|harness> [filter] | bench" >&2
  exit 64
}

case "${1:-}" in
  doctor)
    [[ $# == 1 ]] || usage
    missing=0
    for tool in cargo rustc cc pkg-config; do
      if command -v "$tool" >/dev/null; then
        printf 'FOUND %s: %s\n' "$tool" "$(command -v "$tool")"
      else
        echo "MISSING build tool: $tool"
        missing=1
      fi
    done
    if command -v rustc >/dev/null; then
      rustc --version || missing=1
    fi
    if command -v pkg-config >/dev/null; then
      for library in xkbcommon xkbcommon-x11 fontconfig freetype2; do
        if pkg-config --exists "$library"; then
          echo "FOUND development library: $library"
        else
          echo "MISSING development library: $library (see docs/INSTALL.md)"
          missing=1
        fi
      done
    fi
    for tool in cargo-deny shellcheck gdb ffmpeg pw-record xsel xdotool; do
      command -v "$tool" >/dev/null || echo "UNAVAILABLE optional check/debug/runtime tool: $tool"
    done
    git status --short --branch
    git worktree list
    df -h .
    echo 'Active Rust/linker workloads (coordinate before broad builds):'
    ps -eo pid,comm,args | awk '$2 ~ /^(cargo|rustc|rust-lld|ld|ld.lld)$/ { print }'
    exit "$missing"
    ;;
  test)
    [[ $# -ge 3 && $# -le 4 ]] || usage
    case "$2" in core|runtime|linux|ui|app) ;; *) usage ;; esac
    command_args=(cargo test --locked -p "agentdictate-$2")
    if [[ "$3" == lib ]]; then
      command_args+=(--lib)
    else
      command_args+=(--test "$3")
    fi
    if [[ "$2" == ui && "$3" == desktop ]]; then
      command_args+=(--features test-support)
    fi
    [[ $# == 3 ]] || command_args+=("$4")
    ;;
  bench)
    [[ $# == 1 ]] || usage
    command_args=(cargo bench --locked -p agentdictate-core --bench replacements)
    ;;
  *) usage ;;
esac

source "${PROJECT_DIR}/packaging/linker-runtime-fallback.sh"
log_dir="${XDG_STATE_HOME:-${HOME}/.local/state}/agentdictate/checks"
mkdir -p "$log_dir"
log_file="$(mktemp "$log_dir/$(date -u +%Y%m%dT%H%M%SZ)-$1.XXXXXX.log")"
echo "Check log: $log_file"
{
  printf 'revision: %s\n' "$(git rev-parse HEAD)"
  git status --short
  rustc --version
  printf 'command:'
  printf ' %q' "${command_args[@]}"
  printf '\n'
} | tee "$log_file"
SECONDS=0
set +e
CARGO_TERM_COLOR=never "${command_args[@]}" 2>&1 | tee -a "$log_file"
pipeline_status=("${PIPESTATUS[@]}")
set -e
status="${pipeline_status[0]}"
if [[ "$status" == 0 && "${pipeline_status[1]}" != 0 ]]; then
  status="${pipeline_status[1]}"
fi
if [[ "$1" == test && "$status" == 0 ]] && \
  ! grep -Eq 'test result: ok\. [1-9][0-9]* passed;' "$log_file"; then
  echo 'FAILED: no tests passed; check the harness, features, and filter.' | tee -a "$log_file"
  status=1
fi
printf 'exit_status: %s; elapsed_seconds: %s\n' "$status" "$SECONDS" | tee -a "$log_file"
exit "$status"
