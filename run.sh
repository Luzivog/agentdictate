#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${PROJECT_DIR}/packaging/linker-runtime-fallback.sh"
cd "${PROJECT_DIR}"
export AGENTDICTATE_AUTOSTART_EXEC="${PROJECT_DIR}/run.sh"
export AGENTDICTATE_AUTOSTART_ARG="--background"
if [[ "${1:-}" == "--background" ]]; then
  exec cargo run --locked --features desktop -p agentdictate-app --bin agentdictated
fi
exec cargo run --locked --features desktop -p agentdictate-app --bin agentdictate
