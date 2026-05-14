#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "${PROJECT_DIR}/pyproject.toml")"
BUILD_DIR="${PROJECT_DIR}/dist/deb/agentdictate_${VERSION}_all"
PKG_DIR="${BUILD_DIR}/DEBIAN"
OPT_DIR="${BUILD_DIR}/opt/agentdictate"
BIN_DIR="${BUILD_DIR}/usr/bin"
APP_DIR="${BUILD_DIR}/usr/share/applications"
ICON_DIR="${BUILD_DIR}/usr/share/icons/hicolor/scalable/apps"
DESKTOP_ID="local.agentdictate.AgentDictate"

rm -rf "${BUILD_DIR}"
mkdir -p "${PKG_DIR}" "${OPT_DIR}" "${BIN_DIR}" "${APP_DIR}" "${ICON_DIR}"
cp -a "${PROJECT_DIR}/src" "${PROJECT_DIR}/README.md" "${PROJECT_DIR}/LICENSE" "${PROJECT_DIR}/pyproject.toml" "${OPT_DIR}/"
cp "${PROJECT_DIR}/agentdictate.desktop" "${APP_DIR}/${DESKTOP_ID}.desktop"
cp "${PROJECT_DIR}/assets/agentdictate.svg" "${ICON_DIR}/agentdictate.svg"

cat > "${BIN_DIR}/agentdictate" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
export PYTHONPATH="/opt/agentdictate/src${PYTHONPATH:+:${PYTHONPATH}}"
export AGENTDICTATE_EXEC="/usr/bin/agentdictate"
export GDK_BACKEND="${GDK_BACKEND:-wayland,x11}"
exec python3 -m agentdictate "$@"
EOF
chmod +x "${BIN_DIR}/agentdictate"

cat > "${PKG_DIR}/control" <<EOF
Package: agentdictate
Version: ${VERSION}
Section: utils
Priority: optional
Architecture: all
Depends: python3, python3-gi, gir1.2-gtk-3.0, gir1.2-ayatanaappindicator3-0.1 | gir1.2-appindicator3-0.1, python3-cairo, python3-requests, pipewire-bin | pulseaudio-utils | alsa-utils | ffmpeg, wl-clipboard | xclip | xsel, xdotool | ydotool
Maintainer: AgentDictate <local@agentdictate>
Description: Personal Linux speech-to-text app for AI coding prompts
 AgentDictate records from the default microphone, sends audio to OpenAI,
 optionally cleans transcripts, applies replacements, and pastes the final text.
EOF

mkdir -p "${PROJECT_DIR}/dist"
dpkg-deb --build "${BUILD_DIR}" "${PROJECT_DIR}/dist/agentdictate_${VERSION}_all.deb"
