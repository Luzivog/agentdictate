#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "${PROJECT_DIR}/packaging/common.sh"
APPDIR="${PROJECT_DIR}/dist/AppDir"
VERSION="$(agentdictate_workspace_version)"
RUST_HOST="$(agentdictate_rust_host)"
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

agentdictate_build_release_binaries

rm -rf "${APPDIR}"
mkdir -p "${APPDIR}/usr/bin" "${APPDIR}/usr/share/applications" \
  "${APPDIR}/usr/share/icons/hicolor/scalable/apps" \
  "${APPDIR}/usr/share/metainfo" \
  "${APPDIR}/usr/share/doc/agentdictate/native-access"
install -m 0755 "${PROJECT_DIR}/target/release/agentdictate" "${APPDIR}/usr/bin/agentdictate"
install -m 0755 "${PROJECT_DIR}/target/release/agentdictated" "${APPDIR}/usr/bin/agentdictated"
agentdictate_install_shared_assets "${APPDIR}"
install -m 0644 "${PROJECT_DIR}/assets/agentdictate.svg" "${APPDIR}/agentdictate.svg"
install -m 0644 "${PROJECT_DIR}/packaging/NATIVE_ACCESS.md" \
  "${APPDIR}/usr/share/doc/agentdictate/native-access/NATIVE_ACCESS.md"
install -m 0644 "${PROJECT_DIR}/packaging/70-agentdictate-input.rules" \
  "${APPDIR}/usr/share/doc/agentdictate/native-access/70-agentdictate-input.rules"

# Bundle the ordinary ELF libraries the binaries link, directly or
# transitively, but leave the ones every desktop host provides to the host:
# the glibc family and the X11/graphics stack must match the running system.
# This is the relevant subset of the AppImage excludelist:
# https://github.com/AppImageCommunity/pkg2appimage/blob/master/excludelist
mkdir -p "${APPDIR}/usr/lib"
for BINARY in agentdictate agentdictated; do
  while IFS= read -r LIBRARY; do
    case "$(basename "${LIBRARY}")" in
      ld-linux*.so*|libc.so.*|libdl.so.*|libm.so.*|libmvec.so.*|libpthread.so.*|\
      librt.so.*|libresolv.so.*|libutil.so.*|libanl.so.*|libnss_*.so.*|\
      libgcc_s.so.*|libstdc++.so.*|\
      libxcb.so.1|libX11.so.6|libX11-xcb.so.1|libwayland-client.so.0|\
      libGL.so.1|libEGL.so.1|libGLX.so.0|libGLdispatch.so.0|libdrm.so.2|libgbm.so.1|\
      libfontconfig.so.1|libfreetype.so.6|libharfbuzz.so.0|libexpat.so.1|libz.so.1)
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
  # appimagetool downloads its newest runtime unless given one; the release
  # workflow passes a pinned, checksummed runtime in APPIMAGE_RUNTIME.
  RUNTIME_ARGUMENTS=()
  if [[ -n "${APPIMAGE_RUNTIME:-}" ]]; then
    RUNTIME_ARGUMENTS=(--runtime-file "$(realpath -- "${APPIMAGE_RUNTIME}")")
  fi
  ARCH="${APPIMAGE_ARCH}" APPIMAGE_EXTRACT_AND_RUN="${APPIMAGE_EXTRACT_AND_RUN:-1}" \
    "${APPIMAGETOOL_PATH}" "${RUNTIME_ARGUMENTS[@]}" "${APPDIR}" \
    "${PROJECT_DIR}/dist/AgentDictate-${VERSION}-${APPIMAGE_ARCH}.AppImage"
else
  echo "AppDir created at ${APPDIR}"
  echo "Install appimagetool and rerun this script to produce an AppImage."
fi
