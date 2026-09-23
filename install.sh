#!/usr/bin/env bash
# Installs AgentDictate into the user profile and restarts a running daemon.
# Exit status: 0 ready, 2 native input access is missing, 3 installed and
# working, but input devices are world-accessible through another app's rule.
#
#   ./install.sh --check-native-access  report readiness only (same codes)
#   ./install.sh --setup-native-access  show the sudo command that grants
#                                        access, ask, then run it
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${PROJECT_DIR}/packaging/common.sh"
source "${PROJECT_DIR}/packaging/native-readiness.sh"
BIN_DIR="${HOME}/.local/bin"
DATA_HOME="${XDG_DATA_HOME:-${HOME}/.local/share}"
CONFIG_HOME="${XDG_CONFIG_HOME:-${HOME}/.config}"
APP_DIR="${DATA_HOME}/applications"
AUTOSTART_DIR="${CONFIG_HOME}/autostart"
ICON_DIR="${DATA_HOME}/icons/hicolor/scalable/apps"
NATIVE_ACCESS_DIR="${DATA_HOME}/agentdictate/native-access"

case "${1:-}" in
  --check-native-access)
    agentdictate_check_native_readiness
    exit
    ;;
  --setup-native-access)
    agentdictate_setup_native_access "${PROJECT_DIR}/packaging/grant-access.sh"
    exit
    ;;
  "") ;;
  *)
    echo "Usage: ./install.sh [--check-native-access | --setup-native-access]" >&2
    exit 64
    ;;
esac

agentdictate_build_release_binaries

mkdir -p "${BIN_DIR}" "${APP_DIR}" "${ICON_DIR}" "${NATIVE_ACCESS_DIR}"
install -m 0755 "${PROJECT_DIR}/target/release/agentdictate" "${BIN_DIR}/agentdictate"
install -m 0755 "${PROJECT_DIR}/target/release/agentdictated" "${BIN_DIR}/agentdictated"

# Desktop launchers do not guarantee that ~/.local/bin is in PATH, so the
# entry uses an absolute path. The app writes its own systemd user unit
# (agentdictated.service) on first launch and enables it for login itself.
DESKTOP_TARGET="${APP_DIR}/${DESKTOP_ID}.desktop"
DESKTOP_TEMP="$(mktemp "${APP_DIR}/.${DESKTOP_ID}.XXXXXX")"
trap 'rm -f -- "${DESKTOP_TEMP}"' EXIT
while IFS= read -r line || [[ -n "${line}" ]]; do
  if [[ "${line}" == "Exec=agentdictate" ]]; then
    printf 'Exec="%s"\n' "${BIN_DIR}/agentdictate"
  else
    printf '%s\n' "${line}"
  fi
done < "${PROJECT_DIR}/agentdictate.desktop" > "${DESKTOP_TEMP}"
install -m 0644 "${DESKTOP_TEMP}" "${DESKTOP_TARGET}"
rm -f -- "${DESKTOP_TEMP}"
trap - EXIT

install -m 0644 "${PROJECT_DIR}/assets/agentdictate.svg" \
  "${ICON_DIR}/agentdictate.svg"
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
# A running daemon keeps executing the old binary until it restarts.
# try-restart never starts a stopped service.
if command -v systemctl >/dev/null 2>&1 && \
  systemctl --user --quiet is-active agentdictated.service 2>/dev/null; then
  if systemctl --user try-restart agentdictated.service; then
    echo "Restarted the running agentdictated.service."
  else
    echo "Warning: could not restart agentdictated.service; it still runs the old binaries." >&2
  fi
fi
if ! command -v ffmpeg >/dev/null 2>&1; then
  echo "Warning: ffmpeg is missing, so recordings upload uncompressed (about 8x larger); install it with: sudo apt install ffmpeg" >&2
fi

readiness=0
agentdictate_check_native_readiness || readiness=$?
case "${readiness}" in
  0) echo "Run: agentdictate" ;;
  2)
    cat >&2 <<EOF

AgentDictate was installed, but it cannot read the keyboard or use /dev/uinput
yet. Nothing privileged was changed. To grant access (it asks before using
sudo), run:
  ${PROJECT_DIR}/install.sh --setup-native-access
EOF
    ;;
  3) echo "AgentDictate was installed and works. Run: agentdictate" ;;
esac
exit "${readiness}"
