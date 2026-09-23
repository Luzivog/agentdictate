#!/usr/bin/env bash
# Runs a development build as an isolated instance: its config, data, logs,
# cache and IPC socket live under AGENTDICTATE_HOME (target/dev-home), and its
# daemon runs directly instead of as agentdictated.service. The installed
# service and its data are never touched.
#
#   ./run.sh            the dev daemon in the background plus the settings
#                       window; closing the window stops that daemon
#   ./run.sh --service  only the dev daemon, in the foreground
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${PROJECT_DIR}/packaging/linker-runtime-fallback.sh"
cd "${PROJECT_DIR}"
export AGENTDICTATE_HOME="${AGENTDICTATE_HOME:-${PROJECT_DIR}/target/dev-home}"
BIN_DIR="${PROJECT_DIR}/target/debug"

case "${1:-}" in
  "" | --service) ;;
  *)
    echo "Usage: ./run.sh [--service]" >&2
    exit 64
    ;;
esac

# Two invocations, because Cargo unifies features per invocation and the
# daemon must not link GPUI; it runs its overlay through agentdictate.
cargo build --locked -p agentdictate-app --bin agentdictated
cargo build --locked --features desktop -p agentdictate-app --bin agentdictate
if [[ "${1:-}" == "--service" ]]; then
  exec "${BIN_DIR}/agentdictated" --service
fi

# A dev daemon that is already running keeps the socket; this one then exits
# with "already listening" and the window uses the running one.
"${BIN_DIR}/agentdictated" --service &
DAEMON_PID=$!
trap 'kill "${DAEMON_PID}" 2>/dev/null || true; wait "${DAEMON_PID}" 2>/dev/null || true' EXIT
"${BIN_DIR}/agentdictate"
