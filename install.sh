#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${PROJECT_DIR}/packaging/native-readiness.sh"
BIN_DIR="${HOME}/.local/bin"
DATA_HOME="${XDG_DATA_HOME:-${HOME}/.local/share}"
CONFIG_HOME="${XDG_CONFIG_HOME:-${HOME}/.config}"
APP_DIR="${DATA_HOME}/applications"
AUTOSTART_DIR="${CONFIG_HOME}/autostart"
ICON_DIR="${DATA_HOME}/icons/hicolor/scalable/apps"
SYSTEMD_USER_DIR="${DATA_HOME}/systemd/user"
NATIVE_ACCESS_DIR="${DATA_HOME}/agentdictate/native-access"
DESKTOP_ID="local.agentdictate.AgentDictate"

case "${1:-}" in
  --check-native-access)
    agentdictate_check_native_readiness
    exit
    ;;
  "") ;;
  *)
    echo "Usage: ./install.sh [--check-native-access]" >&2
    exit 64
    ;;
esac

source "${PROJECT_DIR}/packaging/linker-runtime-fallback.sh"
cargo build --manifest-path "${PROJECT_DIR}/Cargo.toml" \
  --locked --release --features desktop -p agentdictate-app --bins

mkdir -p "${BIN_DIR}" "${APP_DIR}" "${AUTOSTART_DIR}" "${ICON_DIR}" \
  "${SYSTEMD_USER_DIR}" "${NATIVE_ACCESS_DIR}"
install -m 0755 "${PROJECT_DIR}/target/release/agentdictate" "${BIN_DIR}/agentdictate"
install -m 0755 "${PROJECT_DIR}/target/release/agentdictated" "${BIN_DIR}/agentdictated"

# Desktop launchers do not guarantee that ~/.local/bin is in PATH. Render
# absolute paths for both entry points so launch and autostart are reliable.
DESKTOP_TARGET="${APP_DIR}/${DESKTOP_ID}.desktop"
AUTOSTART_TARGET="${AUTOSTART_DIR}/${DESKTOP_ID}.desktop"
DESKTOP_TEMP="$(mktemp "${APP_DIR}/.${DESKTOP_ID}.XXXXXX")"
AUTOSTART_TEMP="$(mktemp "${AUTOSTART_DIR}/.${DESKTOP_ID}.XXXXXX")"
trap 'rm -f -- "${DESKTOP_TEMP}" "${AUTOSTART_TEMP}"' EXIT
while IFS= read -r line || [[ -n "${line}" ]]; do
  if [[ "${line}" == "Exec=agentdictate" ]]; then
    printf 'Exec="%s"\n' "${BIN_DIR}/agentdictate"
  else
    printf '%s\n' "${line}"
  fi
done < "${PROJECT_DIR}/agentdictate.desktop" > "${DESKTOP_TEMP}"
while IFS= read -r line || [[ -n "${line}" ]]; do
  if [[ "${line}" == "Exec=agentdictated" ]]; then
    printf 'Exec="%s"\n' "${BIN_DIR}/agentdictated"
  else
    printf '%s\n' "${line}"
  fi
done < "${PROJECT_DIR}/packaging/agentdictate-autostart.desktop" > "${AUTOSTART_TEMP}"
install -m 0644 "${DESKTOP_TEMP}" "${DESKTOP_TARGET}"
install -m 0644 "${AUTOSTART_TEMP}" "${AUTOSTART_TARGET}"
rm -f -- "${DESKTOP_TEMP}" "${AUTOSTART_TEMP}"
trap - EXIT

install -m 0644 "${PROJECT_DIR}/assets/agentdictate.svg" \
  "${ICON_DIR}/agentdictate.svg"
install -m 0644 "${PROJECT_DIR}/packaging/agentdictate-ydotoold.service" \
  "${SYSTEMD_USER_DIR}/agentdictate-ydotoold.service"
install -m 0644 "${PROJECT_DIR}/packaging/70-agentdictate-input.rules" \
  "${NATIVE_ACCESS_DIR}/70-agentdictate-input.rules"
install -m 0644 "${PROJECT_DIR}/packaging/NATIVE_ACCESS.md" \
  "${NATIVE_ACCESS_DIR}/NATIVE_ACCESS.md"

rm -f "${APP_DIR}/agentdictate.desktop" "${AUTOSTART_DIR}/agentdictate.desktop"
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "${APP_DIR}" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache --force --ignore-theme-index "${DATA_HOME}/icons/hicolor" \
    >/dev/null 2>&1 || true
fi

echo "Installed native AgentDictate:"
echo "  ${BIN_DIR}/agentdictate"
echo "  ${BIN_DIR}/agentdictated"
echo "  ${SYSTEMD_USER_DIR}/agentdictate-ydotoold.service"

if ! agentdictate_check_native_readiness; then
  cat >&2 <<EOF

AgentDictate was installed, but native input setup is incomplete.
No privileged changes or services were started automatically.
Follow: ${NATIVE_ACCESS_DIR}/NATIVE_ACCESS.md
Then rerun: ${PROJECT_DIR}/install.sh --check-native-access
EOF
  exit 2
fi

echo "Run: agentdictate"
