#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${PROJECT_DIR}/packaging/linker-runtime-fallback.sh"
cd "${PROJECT_DIR}"
cargo test --locked --workspace --all-targets --all-features
export PYTHONPATH="${PROJECT_DIR}/src${PYTHONPATH:+:${PYTHONPATH}}"
exec python3 -m unittest discover -s "${PROJECT_DIR}/tests" -v
