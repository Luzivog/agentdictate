#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "${PROJECT_DIR}/packaging/linker-runtime-fallback.sh"
VERSION="$(awk '/^\[workspace.package\]/{found=1; next} found && /^version = /{gsub(/[\" ]/, "", $3); print $3; exit}' "${PROJECT_DIR}/Cargo.toml")"
if [[ -z "${VERSION}" ]]; then
  echo "Could not read the workspace version from Cargo.toml" >&2
  exit 1
fi
ARCHITECTURE="$(dpkg --print-architecture)"
# Consume the full rustc output; exiting early can SIGPIPE the rustup shim
# under pipefail.
RUST_HOST="$(rustc -vV | awk '/^host: / { host = $2 } END { print host }')"
case "${RUST_HOST%%-*}" in
  x86_64) EXPECTED_ARCHITECTURE="amd64" ;;
  aarch64) EXPECTED_ARCHITECTURE="arm64" ;;
  armv7*) EXPECTED_ARCHITECTURE="armhf" ;;
  i?86) EXPECTED_ARCHITECTURE="i386" ;;
  riscv64) EXPECTED_ARCHITECTURE="riscv64" ;;
  *)
    echo "Unsupported Debian architecture: ${RUST_HOST}" >&2
    exit 1
    ;;
esac
if [[ "${ARCHITECTURE}" != "${EXPECTED_ARCHITECTURE}" ]]; then
  echo "Refusing to label ${RUST_HOST} binaries as ${ARCHITECTURE}" >&2
  exit 1
fi
BUILD_DIR="${PROJECT_DIR}/dist/deb/agentdictate_${VERSION}_${ARCHITECTURE}"
PKG_DIR="${BUILD_DIR}/DEBIAN"
BIN_DIR="${BUILD_DIR}/usr/bin"
APP_DIR="${BUILD_DIR}/usr/share/applications"
AUTOSTART_DIR="${BUILD_DIR}/etc/xdg/autostart"
ICON_DIR="${BUILD_DIR}/usr/share/icons/hicolor/scalable/apps"
METAINFO_DIR="${BUILD_DIR}/usr/share/metainfo"
DOC_DIR="${BUILD_DIR}/usr/share/doc/agentdictate"
UDEV_RULES_DIR="${BUILD_DIR}/usr/lib/udev/rules.d"
SYSTEMD_USER_DIR="${BUILD_DIR}/usr/lib/systemd/user"
DESKTOP_ID="local.agentdictate.AgentDictate"

cargo build --manifest-path "${PROJECT_DIR}/Cargo.toml" \
  --locked --release --features desktop -p agentdictate-app --bins

rm -rf "${BUILD_DIR}"
mkdir -p "${PKG_DIR}" "${BIN_DIR}" "${APP_DIR}" "${AUTOSTART_DIR}" "${ICON_DIR}" \
  "${METAINFO_DIR}" "${DOC_DIR}" "${UDEV_RULES_DIR}" "${SYSTEMD_USER_DIR}"
install -m 0755 "${PROJECT_DIR}/target/release/agentdictate" "${BIN_DIR}/agentdictate"
install -m 0755 "${PROJECT_DIR}/target/release/agentdictated" "${BIN_DIR}/agentdictated"
install -m 0644 "${PROJECT_DIR}/agentdictate.desktop" "${APP_DIR}/${DESKTOP_ID}.desktop"
install -m 0644 "${PROJECT_DIR}/packaging/agentdictate-autostart.desktop" \
  "${AUTOSTART_DIR}/${DESKTOP_ID}.desktop"
install -m 0644 "${PROJECT_DIR}/assets/agentdictate.svg" "${ICON_DIR}/agentdictate.svg"
install -m 0644 "${PROJECT_DIR}/packaging/${DESKTOP_ID}.metainfo.xml" \
  "${METAINFO_DIR}/${DESKTOP_ID}.metainfo.xml"
install -m 0644 "${PROJECT_DIR}/LICENSE" "${DOC_DIR}/copyright"
install -m 0644 "${PROJECT_DIR}/README.md" "${DOC_DIR}/README.md"
install -m 0644 "${PROJECT_DIR}/packaging/NATIVE_ACCESS.md" \
  "${DOC_DIR}/NATIVE_ACCESS.md"
install -m 0644 "${PROJECT_DIR}/packaging/70-agentdictate-input.rules" \
  "${UDEV_RULES_DIR}/70-agentdictate-input.rules"
install -m 0644 "${PROJECT_DIR}/packaging/agentdictate-ydotoold.service" \
  "${SYSTEMD_USER_DIR}/agentdictate-ydotoold.service"
printf '%s\n' \
  "/etc/xdg/autostart/${DESKTOP_ID}.desktop" > "${PKG_DIR}/conffiles"

# Apply the package-owned udev policy to existing devices without enabling or
# starting any per-user service. logind assigns uaccess ACLs to active seats.
cat > "${PKG_DIR}/postinst" <<'EOF'
#!/bin/sh
set -e
if command -v udevadm >/dev/null 2>&1; then
  udevadm control --reload-rules || true
  udevadm trigger --subsystem-match=input --action=change || true
  udevadm trigger --subsystem-match=misc --sysname-match=uinput --action=change || true
fi
exit 0
EOF
chmod 0755 "${PKG_DIR}/postinst"
cat > "${PKG_DIR}/postrm" <<'EOF'
#!/bin/sh
set -e
if command -v udevadm >/dev/null 2>&1; then
  udevadm control --reload-rules || true
  udevadm trigger --subsystem-match=input --action=change || true
  udevadm trigger --subsystem-match=misc --sysname-match=uinput --action=change || true
fi
exit 0
EOF
chmod 0755 "${PKG_DIR}/postrm"

SHLIBDEPS_ROOT="$(mktemp -d)"
trap 'rm -rf -- "${SHLIBDEPS_ROOT}"' EXIT
mkdir -p "${SHLIBDEPS_ROOT}/debian"
cat > "${SHLIBDEPS_ROOT}/debian/control" <<'EOF'
Source: agentdictate
Section: utils
Priority: optional
Maintainer: AgentDictate <local@agentdictate>
Standards-Version: 4.6.2

Package: agentdictate
Architecture: any
Description: Fast native Linux dictation
EOF
SHLIB_DEPENDS="$(
  cd "${SHLIBDEPS_ROOT}"
  dpkg-shlibdeps -O -S"${BUILD_DIR}" \
    -e"${BIN_DIR}/agentdictate" -e"${BIN_DIR}/agentdictated" \
    | sed -n 's/^shlibs:Depends=//p'
)"
if [[ -z "${SHLIB_DEPENDS}" ]]; then
  echo "Could not derive shared-library dependencies" >&2
  exit 1
fi

cat > "${PKG_DIR}/control" <<EOF
Package: agentdictate
Version: ${VERSION}
Section: utils
Priority: optional
Architecture: ${ARCHITECTURE}
Depends: ${SHLIB_DEPENDS}, ca-certificates, pipewire-bin, udev, systemd, wl-clipboard, xsel, ydotool, ydotoold, xdotool, x11-utils, libfontconfig1, libfreetype6, libwayland-client0, libx11-6, libvulkan1
Maintainer: AgentDictate <local@agentdictate>
Description: Fast native Linux dictation for AI coding prompts
 AgentDictate records speech, transcribes it with OpenAI, safely checkpoints
 recovery data, and pastes the result into the focused application.
EOF

mkdir -p "${PROJECT_DIR}/dist"
find "${BUILD_DIR}" -type d -exec chmod 0755 {} +
dpkg-deb --root-owner-group --build "${BUILD_DIR}" \
  "${PROJECT_DIR}/dist/agentdictate_${VERSION}_${ARCHITECTURE}.deb"
rm -rf -- "${SHLIBDEPS_ROOT}"
trap - EXIT
