#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export PYTHONPATH="${PROJECT_DIR}/src${PYTHONPATH:+:${PYTHONPATH}}"
export AGENTDICTATE_EXEC="${PROJECT_DIR}/run.sh"
export GDK_BACKEND="${GDK_BACKEND:-wayland,x11}"
exec python3 -m agentdictate "$@"
