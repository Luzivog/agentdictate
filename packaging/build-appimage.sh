#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APPDIR="${PROJECT_DIR}/dist/AppDir"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "${PROJECT_DIR}/pyproject.toml")"
DESKTOP_ID="local.agentdictate.AgentDictate"

rm -rf "${APPDIR}"
mkdir -p "${APPDIR}/usr/bin" "${APPDIR}/usr/share/agentdictate" "${APPDIR}/usr/share/applications" "${APPDIR}/usr/share/icons/hicolor/scalable/apps"
cp -a "${PROJECT_DIR}/src" "${PROJECT_DIR}/README.md" "${PROJECT_DIR}/pyproject.toml" "${APPDIR}/usr/share/agentdictate/"
cp "${PROJECT_DIR}/agentdictate.desktop" "${APPDIR}/usr/share/applications/${DESKTOP_ID}.desktop"
cp "${PROJECT_DIR}/assets/agentdictate.svg" "${APPDIR}/agentdictate.svg"
cp "${PROJECT_DIR}/assets/agentdictate.svg" "${APPDIR}/usr/share/icons/hicolor/scalable/apps/agentdictate.svg"

cat > "${APPDIR}/AppRun" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export PYTHONPATH="${HERE}/usr/share/agentdictate/src${PYTHONPATH:+:${PYTHONPATH}}"
export AGENTDICTATE_EXEC="${HERE}/AppRun"
export GDK_BACKEND="${GDK_BACKEND:-wayland,x11}"
exec python3 -m agentdictate "$@"
EOF
chmod +x "${APPDIR}/AppRun"
ln -sf "usr/share/applications/${DESKTOP_ID}.desktop" "${APPDIR}/${DESKTOP_ID}.desktop"

APPIMAGETOOL_PATH="${APPIMAGETOOL:-}"
if [[ -z "${APPIMAGETOOL_PATH}" ]] && command -v appimagetool >/dev/null 2>&1; then
  APPIMAGETOOL_PATH="$(command -v appimagetool)"
fi
if [[ -z "${APPIMAGETOOL_PATH}" && -x "${PROJECT_DIR}/dist/tools/appimagetool-x86_64.AppImage" ]]; then
  APPIMAGETOOL_PATH="${PROJECT_DIR}/dist/tools/appimagetool-x86_64.AppImage"
fi

if [[ -n "${APPIMAGETOOL_PATH}" ]]; then
  ARCH="${ARCH:-x86_64}" APPIMAGE_EXTRACT_AND_RUN="${APPIMAGE_EXTRACT_AND_RUN:-1}" \
    "${APPIMAGETOOL_PATH}" "${APPDIR}" "${PROJECT_DIR}/dist/AgentDictate-${VERSION}.AppImage"
else
  echo "AppDir created at ${APPDIR}"
  echo "Install appimagetool and rerun this script to produce an AppImage."
fi
