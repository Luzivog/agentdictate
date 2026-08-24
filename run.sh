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
case "${1:-}" in
  --background)
    exec cargo run --locked --features desktop -p agentdictate-app --bin agentdictated -- \
      --start-service
    ;;
  --service)
    exec cargo run --locked --features desktop -p agentdictate-app --bin agentdictated -- \
      --service
    ;;
  "")
    exec cargo run --locked --features desktop -p agentdictate-app --bin agentdictate
    ;;
  *)
    echo "Usage: ./run.sh [--background|--service]" >&2
    exit 64
    ;;
esac
