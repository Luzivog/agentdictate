#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "${PROJECT_DIR}/packaging/linker-runtime-fallback.sh"
APPDIR="${PROJECT_DIR}/dist/AppDir"
VERSION="$(awk '/^\[workspace.package\]/{found=1; next} found && /^version = /{gsub(/[\" ]/, "", $3); print $3; exit}' "${PROJECT_DIR}/Cargo.toml")"
if [[ -z "${VERSION}" ]]; then
  echo "Could not read the workspace version from Cargo.toml" >&2
  exit 1
fi
DESKTOP_ID="local.agentdictate.AgentDictate"
# Consume the full rustc output; exiting early can SIGPIPE the rustup shim
# under pipefail.
RUST_HOST="$(rustc -vV | awk '/^host: / { host = $2 } END { print host }')"
case "${RUST_HOST%%-*}" in
  x86_64) APPIMAGE_ARCH="x86_64" ;;
  aarch64) APPIMAGE_ARCH="aarch64" ;;
  armv7*) APPIMAGE_ARCH="armhf" ;;
  i?86) APPIMAGE_ARCH="i686" ;;
  riscv64) APPIMAGE_ARCH="riscv64" ;;
  *)
    echo "Unsupported AppImage architecture: ${RUST_HOST}" >&2
    exit 1
    ;;
esac

cargo build --manifest-path "${PROJECT_DIR}/Cargo.toml" \
  --locked --release --features desktop -p agentdictate-app --bins

rm -rf "${APPDIR}"
mkdir -p "${APPDIR}/usr/bin" "${APPDIR}/usr/share/applications" \
  "${APPDIR}/usr/share/icons/hicolor/scalable/apps" \
  "${APPDIR}/usr/share/metainfo" \
  "${APPDIR}/usr/share/doc/agentdictate/native-access"
install -m 0755 "${PROJECT_DIR}/target/release/agentdictate" "${APPDIR}/usr/bin/agentdictate"
install -m 0755 "${PROJECT_DIR}/target/release/agentdictated" "${APPDIR}/usr/bin/agentdictated"
install -m 0644 "${PROJECT_DIR}/agentdictate.desktop" \
  "${APPDIR}/usr/share/applications/${DESKTOP_ID}.desktop"
install -m 0644 "${PROJECT_DIR}/assets/agentdictate.svg" "${APPDIR}/agentdictate.svg"
install -m 0644 "${PROJECT_DIR}/assets/agentdictate.svg" \
  "${APPDIR}/usr/share/icons/hicolor/scalable/apps/agentdictate.svg"
install -m 0644 "${PROJECT_DIR}/packaging/${DESKTOP_ID}.metainfo.xml" \
  "${APPDIR}/usr/share/metainfo/${DESKTOP_ID}.appdata.xml"
install -m 0644 "${PROJECT_DIR}/LICENSE" \
  "${APPDIR}/usr/share/doc/agentdictate/copyright"
install -m 0644 "${PROJECT_DIR}/packaging/NATIVE_ACCESS.md" \
  "${APPDIR}/usr/share/doc/agentdictate/native-access/NATIVE_ACCESS.md"
install -m 0644 "${PROJECT_DIR}/packaging/70-agentdictate-input.rules" \
  "${APPDIR}/usr/share/doc/agentdictate/native-access/70-agentdictate-input.rules"
install -m 0644 "${PROJECT_DIR}/packaging/agentdictate-ydotoold.service" \
  "${APPDIR}/usr/share/doc/agentdictate/native-access/agentdictate-ydotoold.service"

# Keep glibc and graphics drivers host-provided, but bundle the ordinary ELF
# libraries that the Rust binaries link directly or transitively.
mkdir -p "${APPDIR}/usr/lib"
for BINARY in agentdictate agentdictated; do
  while IFS= read -r LIBRARY; do
    case "$(basename "${LIBRARY}")" in
      ld-linux*.so*|libc.so.*|libdl.so.*|libgcc_s.so.*|libm.so.*|libpthread.so.*|librt.so.*)
        continue
        ;;
    esac
    install -m 0644 "${LIBRARY}" "${APPDIR}/usr/lib/$(basename "${LIBRARY}")"
  done < <(ldd "${APPDIR}/usr/bin/${BINARY}" | awk '/=> \// { print $(NF - 1) }')
done

cat > "${APPDIR}/AppRun" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export LD_LIBRARY_PATH="${HERE}/usr/lib${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}"
if [[ "${1:-}" == "--background" ]]; then
  shift
  exec "${HERE}/usr/bin/agentdictated" --start-service "$@"
fi
if [[ "${1:-}" == "--service" ]]; then
  shift
  exec "${HERE}/usr/bin/agentdictated" --service "$@"
fi
exec "${HERE}/usr/bin/agentdictate" "$@"
EOF
chmod 0755 "${APPDIR}/AppRun"
ln -sf "usr/share/applications/${DESKTOP_ID}.desktop" "${APPDIR}/${DESKTOP_ID}.desktop"
find "${APPDIR}" -type d -exec chmod 0755 {} +

APPIMAGETOOL_PATH="${APPIMAGETOOL:-}"
if [[ -z "${APPIMAGETOOL_PATH}" ]] && command -v appimagetool >/dev/null 2>&1; then
  APPIMAGETOOL_PATH="$(command -v appimagetool)"
fi
if [[ -z "${APPIMAGETOOL_PATH}" && -x "${PROJECT_DIR}/dist/tools/appimagetool-${APPIMAGE_ARCH}.AppImage" ]]; then
  APPIMAGETOOL_PATH="${PROJECT_DIR}/dist/tools/appimagetool-${APPIMAGE_ARCH}.AppImage"
fi

if [[ -n "${APPIMAGETOOL_PATH}" ]]; then
  ARCH="${APPIMAGE_ARCH}" APPIMAGE_EXTRACT_AND_RUN="${APPIMAGE_EXTRACT_AND_RUN:-1}" \
    "${APPIMAGETOOL_PATH}" "${APPDIR}" \
    "${PROJECT_DIR}/dist/AgentDictate-${VERSION}-${APPIMAGE_ARCH}.AppImage"
else
  echo "AppDir created at ${APPDIR}"
  echo "Install appimagetool and rerun this script to produce an AppImage."
fi
