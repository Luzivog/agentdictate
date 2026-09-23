#!/usr/bin/env bash
# Report whether this host can build AgentDictate and whether a broad build is
# safe to start now (disk space and running Cargo or linker processes).
set -euo pipefail
PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${PROJECT_DIR}"

if [[ "$*" != doctor ]]; then
  echo "Usage: scripts/dev.sh doctor" >&2
  exit 64
fi

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
for tool in cargo-deny shellcheck gdb ffmpeg pw-record; do
  command -v "$tool" >/dev/null || echo "UNAVAILABLE optional check/debug/runtime tool: $tool"
done
git status --short --branch
git worktree list
df -h .
echo 'Active Rust/linker workloads (coordinate before broad builds):'
ps -eo pid,comm,args | awk '$2 ~ /^(cargo|rustc|rust-lld|ld|ld.lld)$/ { print }'
exit "$missing"
