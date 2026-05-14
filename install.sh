#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_DIR="${HOME}/.local/bin"
APP_DIR="${HOME}/.local/share/applications"
ICON_DIR="${HOME}/.local/share/icons/hicolor/scalable/apps"
WRAPPER="${BIN_DIR}/agentdictate"
DESKTOP_ID="local.agentdictate.AgentDictate"
DESKTOP_FILE="${APP_DIR}/${DESKTOP_ID}.desktop"
LEGACY_DESKTOP_FILE="${APP_DIR}/agentdictate.desktop"
ICON_FILE="${ICON_DIR}/agentdictate.svg"

mkdir -p "${BIN_DIR}" "${APP_DIR}" "${ICON_DIR}"

cat > "${WRAPPER}" <<EOF
#!/usr/bin/env bash
set -euo pipefail
export PYTHONPATH="${PROJECT_DIR}/src\${PYTHONPATH:+:\${PYTHONPATH}}"
export AGENTDICTATE_EXEC="${WRAPPER}"
export GDK_BACKEND="\${GDK_BACKEND:-wayland,x11}"
exec python3 -m agentdictate "\$@"
EOF
chmod +x "${WRAPPER}"

cat > "${DESKTOP_FILE}" <<EOF
[Desktop Entry]
Type=Application
Name=AgentDictate
Comment=Personal Linux speech-to-text app for AI coding prompts
Exec=${WRAPPER}
Icon=${ICON_FILE}
Terminal=false
Categories=Utility;
StartupWMClass=${DESKTOP_ID}
EOF

rm -f "${LEGACY_DESKTOP_FILE}"

cp "${PROJECT_DIR}/assets/agentdictate.svg" "${ICON_FILE}"

if [[ ! -f "${HOME}/.local/share/icons/hicolor/index.theme" && -f /usr/share/icons/hicolor/index.theme ]]; then
  cp /usr/share/icons/hicolor/index.theme "${HOME}/.local/share/icons/hicolor/index.theme"
fi

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "${APP_DIR}" >/dev/null 2>&1 || true
fi

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -q -f "${HOME}/.local/share/icons/hicolor" >/dev/null 2>&1 || true
fi

echo "Installed AgentDictate:"
echo "  ${WRAPPER}"
echo "  ${DESKTOP_FILE}"
echo "  ${ICON_FILE}"
echo
echo "Run: agentdictate"
echo "Uninstall: rm -f '${WRAPPER}' '${DESKTOP_FILE}' '${LEGACY_DESKTOP_FILE}' '${ICON_FILE}' '${HOME}/.config/autostart/${DESKTOP_ID}.desktop' '${HOME}/.config/autostart/agentdictate.desktop'"
