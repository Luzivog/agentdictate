#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "${PROJECT_DIR}/packaging/native-readiness.sh"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

assert_contains() {
  local haystack="$1"
  local needle="$2"
  [[ "${haystack}" == *"${needle}"* ]] || fail "expected output to contain: ${needle}"
}

assert_file_contains() {
  local path="$1"
  local needle="$2"
  grep -Fq -- "${needle}" "${path}" || fail "${path} does not contain: ${needle}"
}

fixture_root="$(mktemp -d)"
trap 'rm -rf -- "${fixture_root}"' EXIT
mkdir -p "${fixture_root}/dev/input" "${fixture_root}/bin"
cat > "${fixture_root}/proc-input-devices" <<'EOF'
N: Name="USB Keyboard"
H: Handlers=sysrq kbd event4 leds

N: Name="Mouse"
H: Handlers=mouse0 event7
EOF
touch "${fixture_root}/dev/input/event4" "${fixture_root}/dev/uinput"
chmod 0660 "${fixture_root}/dev/input/event4" "${fixture_root}/dev/uinput"

cat > "${fixture_root}/bin/ydotool" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
cat > "${fixture_root}/bin/ydotoold" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
cat > "${fixture_root}/bin/systemctl" <<'EOF'
#!/usr/bin/env bash
[[ "$*" == *"is-active"* ]]
EOF
chmod 0755 "${fixture_root}/bin/ydotool" "${fixture_root}/bin/ydotoold" \
  "${fixture_root}/bin/systemctl"

if ! ready_output="$(
  PATH="${fixture_root}/bin:/usr/bin:/bin" \
  AGENTDICTATE_PROC_INPUT_DEVICES="${fixture_root}/proc-input-devices" \
  AGENTDICTATE_DEV_INPUT_DIR="${fixture_root}/dev/input" \
  AGENTDICTATE_UINPUT_PATH="${fixture_root}/dev/uinput" \
  agentdictate_check_native_readiness
)"; then
  fail "secure native-access fixture should be ready"
fi
assert_contains "${ready_output}" "Native input readiness: ready"

chmod 0666 "${fixture_root}/dev/input/event4" "${fixture_root}/dev/uinput"
if insecure_output="$(
  PATH="${fixture_root}/bin:/usr/bin:/bin" \
  AGENTDICTATE_PROC_INPUT_DEVICES="${fixture_root}/proc-input-devices" \
  AGENTDICTATE_DEV_INPUT_DIR="${fixture_root}/dev/input" \
  AGENTDICTATE_UINPUT_PATH="${fixture_root}/dev/uinput" \
  agentdictate_check_native_readiness 2>&1
)"; then
  fail "world-accessible native devices must not be reported as secure"
fi
assert_contains "${insecure_output}" "world-accessible"
assert_contains "${insecure_output}" "70-agentdictate-input.rules"

rm -f "${fixture_root}/dev/input/event4" "${fixture_root}/dev/uinput"
if missing_output="$(
  PATH="${fixture_root}/bin:/usr/bin:/bin" \
  AGENTDICTATE_PROC_INPUT_DEVICES="${fixture_root}/proc-input-devices" \
  AGENTDICTATE_DEV_INPUT_DIR="${fixture_root}/dev/input" \
  AGENTDICTATE_UINPUT_PATH="${fixture_root}/dev/uinput" \
  agentdictate_check_native_readiness 2>&1
)"; then
  fail "missing input devices must fail readiness"
fi
assert_contains "${missing_output}" "No readable keyboard event device"
assert_contains "${missing_output}" "Cannot write"

touch "${fixture_root}/dev/input/event4" "${fixture_root}/dev/uinput"
chmod 0660 "${fixture_root}/dev/input/event4" "${fixture_root}/dev/uinput"
if ! installer_check_output="$(
  PATH="${fixture_root}/bin:/usr/bin:/bin" \
  AGENTDICTATE_PROC_INPUT_DEVICES="${fixture_root}/proc-input-devices" \
  AGENTDICTATE_DEV_INPUT_DIR="${fixture_root}/dev/input" \
  AGENTDICTATE_UINPUT_PATH="${fixture_root}/dev/uinput" \
  "${PROJECT_DIR}/install.sh" --check-native-access
)"; then
  fail "install.sh readiness mode must not require a build or mutate the fixture"
fi
assert_contains "${installer_check_output}" "Native input readiness: ready"

cat > "${fixture_root}/bin/systemctl" <<'EOF'
#!/usr/bin/env bash
exit 3
EOF
chmod 0755 "${fixture_root}/bin/systemctl"
if dependency_output="$(
  PATH="${fixture_root}/bin:/usr/bin:/bin" \
  YDOTOOL_SOCKET="${fixture_root}/missing-ydotool.socket" \
  AGENTDICTATE_PROC_INPUT_DEVICES="${fixture_root}/proc-input-devices" \
  AGENTDICTATE_DEV_INPUT_DIR="${fixture_root}/dev/input" \
  AGENTDICTATE_UINPUT_PATH="${fixture_root}/dev/uinput" \
  AGENTDICTATE_YDOTOOL_COMMAND="${fixture_root}/missing-ydotool" \
  AGENTDICTATE_YDOTOOLD_COMMAND="${fixture_root}/missing-ydotoold" \
  agentdictate_check_native_readiness 2>&1
)"; then
  fail "missing ydotool dependencies must fail readiness"
fi
assert_contains "${dependency_output}" "ydotool is not installed"
assert_contains "${dependency_output}" "ydotoold is not installed"

linker_fixture="${fixture_root}/linker"
mkdir -p "${linker_fixture}/bin"
cat > "${linker_fixture}/bin/cc" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "${1#-print-file-name=}"
EOF
cat > "${linker_fixture}/bin/ldconfig" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' \
  'libxkbcommon.so.0 (libc6,x86-64) => /usr/lib/libxkbcommon.so.0' \
  'libxkbcommon-x11.so.0 (libc6,x86-64) => /usr/lib/libxkbcommon-x11.so.0'
for sequence in $(seq 1 5000); do
  printf 'libfixture%s.so.0 => /usr/lib/libfixture%s.so.0\n' \
    "${sequence}" "${sequence}"
done
EOF
chmod 0755 "${linker_fixture}/bin/cc" "${linker_fixture}/bin/ldconfig"
linker_fallback="${PROJECT_DIR}/packaging/linker-runtime-fallback.sh"
if ! (
  set -euo pipefail
  PROJECT_DIR="${linker_fixture}/project"
  PATH="${linker_fixture}/bin:/usr/bin:/bin"
  source "${linker_fallback}"
); then
  fail "linker runtime discovery must be safe under install.sh strict mode"
fi
[[ -L "${linker_fixture}/project/target/linker-shims/libxkbcommon.so" ]] || \
  fail "xkbcommon runtime shim was not created"
[[ -L "${linker_fixture}/project/target/linker-shims/libxkbcommon-x11.so" ]] || \
  fail "xkbcommon-x11 runtime shim was not created"

mkdir -p "${linker_fixture}/no-path-bin"
cp "${linker_fixture}/bin/cc" "${linker_fixture}/no-path-bin/cc"
if ! (
  set -euo pipefail
  PROJECT_DIR="${linker_fixture}/no-path-project"
  PATH="${linker_fixture}/no-path-bin:/usr/bin:/bin"
  source "${linker_fallback}"
); then
  fail "linker fallback must find the distro ldconfig outside a user PATH"
fi
[[ -L "${linker_fixture}/no-path-project/target/linker-shims/libxkbcommon.so" ]] || \
  fail "runtime shim was not created when /usr/sbin was absent from PATH"

RULE="${PROJECT_DIR}/packaging/70-agentdictate-input.rules"
SERVICE="${PROJECT_DIR}/packaging/agentdictate-ydotoold.service"
DAEMON_SERVICE="${PROJECT_DIR}/packaging/agentdictated.service"
AUTOSTART="${PROJECT_DIR}/packaging/agentdictate-autostart.desktop"
GUIDE="${PROJECT_DIR}/packaging/NATIVE_ACCESS.md"
assert_file_contains "${RULE}" 'ENV{ID_INPUT_KEYBOARD}=="1"'
assert_file_contains "${RULE}" 'TAG+="uaccess"'
assert_file_contains "${RULE}" 'MODE="0660"'
if grep -Eq 'MODE="?0?666"?|chmod[[:space:]]+0?666' "${RULE}" "${SERVICE}" "${GUIDE}"; then
  fail "native access assets must never grant world-write access"
fi
assert_file_contains "${SERVICE}" "ExecStart=/usr/bin/ydotoold"
assert_file_contains "${SERVICE}" "UMask=0077"
if grep -Eq 'ExecStartPre=.*sleep|ExecStartPost=.*sleep' "${SERVICE}"; then
  fail "ydotoold readiness must not depend on a fixed startup delay"
fi
assert_file_contains "${DAEMON_SERVICE}" "PartOf=graphical-session.target"
assert_file_contains "${DAEMON_SERVICE}" "After=graphical-session.target"
assert_file_contains "${DAEMON_SERVICE}" "ExecStart=/usr/bin/agentdictated --service"
assert_file_contains "${DAEMON_SERVICE}" "Restart=on-failure"
if grep -Fq 'WantedBy=graphical-session.target' "${DAEMON_SERVICE}"; then
  fail "daemon service must start from XDG autostart after session initialization"
fi
assert_file_contains "${AUTOSTART}" "Exec=agentdictated --start-service"
assert_file_contains "${PROJECT_DIR}/packaging/build-deb.sh" \
  'usr/lib/udev/rules.d'
assert_file_contains "${PROJECT_DIR}/packaging/build-deb.sh" \
  'usr/lib/systemd/user'
assert_file_contains "${PROJECT_DIR}/packaging/build-deb.sh" \
  'agentdictated.service'
assert_file_contains "${PROJECT_DIR}/packaging/build-deb.sh" \
  '${PKG_DIR}/postrm'
if grep -Eq 'systemctl([^#\n]*)(enable|start|restart)' \
  "${PROJECT_DIR}/install.sh" "${PROJECT_DIR}/packaging/build-deb.sh"; then
  fail "installers must not enable or start a user service"
fi
assert_file_contains "${PROJECT_DIR}/packaging/build-appimage.sh" \
  'NATIVE_ACCESS.md'
assert_file_contains "${PROJECT_DIR}/packaging/build-appimage.sh" \
  'agentdictated" --service'
assert_file_contains "${PROJECT_DIR}/install.sh" \
  '--check-native-access'
assert_file_contains "${PROJECT_DIR}/install.sh" \
  'agentdictated.service'
assert_file_contains "${PROJECT_DIR}/install.sh" \
  "grep -Fxq 'Hidden=true'"

echo "Native install readiness checks passed."
