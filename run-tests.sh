#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${PROJECT_DIR}/packaging/linker-runtime-fallback.sh"
cd "${PROJECT_DIR}"
"${PROJECT_DIR}/scripts/test-dev.sh"
cargo test --locked --workspace --all-targets --all-features
"${PROJECT_DIR}/packaging/test-native-readiness.sh"
if command -v cargo-deny >/dev/null 2>&1; then
  cargo deny check
else
  echo "SKIPPED: cargo-deny not installed"
fi
