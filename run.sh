#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${PROJECT_DIR}/packaging/linker-runtime-fallback.sh"
cd "${PROJECT_DIR}"
export AGENTDICTATE_AUTOSTART_EXEC="${PROJECT_DIR}/run.sh"
export AGENTDICTATE_AUTOSTART_ARG="--background"
export AGENTDICTATE_SERVICE_EXEC="${PROJECT_DIR}/run.sh"
export AGENTDICTATE_SERVICE_ARG="--service"
export AGENTDICTATE_SERVICE_IDENTITY_FILE="${PROJECT_DIR}/target/debug/agentdictated"
# The daemon runs its recording overlay and tray "Open settings" through the
# sibling desktop binary. It is built separately because Cargo unifies features
# per invocation, and the daemon itself must stay free of GPUI.
build_desktop_binary() {
  cargo build --locked --features desktop -p agentdictate-app --bin agentdictate
}
case "${1:-}" in
  --background)
    build_desktop_binary
    exec cargo run --locked -p agentdictate-app --bin agentdictated -- --start-service
    ;;
  --service)
    build_desktop_binary
    exec cargo run --locked -p agentdictate-app --bin agentdictated -- --service
    ;;
  "")
    exec cargo run --locked --features desktop -p agentdictate-app --bin agentdictate
    ;;
  *)
    echo "Usage: ./run.sh [--background|--service]" >&2
    exit 64
    ;;
esac
