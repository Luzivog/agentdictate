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

fixture_root="$(mktemp -d)"
trap 'rm -rf -- "${fixture_root}"' EXIT
mkdir -p "${fixture_root}/dev/input" "${fixture_root}/bin" "${fixture_root}/rules"
cat > "${fixture_root}/proc-input-devices" <<'EOF'
N: Name="USB Keyboard"
H: Handlers=sysrq kbd event4 leds
B: KEY=1000000000007 ff9f207ac14057ff febeffdfffefffff fffffffffffffffe 

N: Name="Webcam button"
H: Handlers=kbd event20
B: KEY=100000 0 0 0

N: Name="Mouse"
H: Handlers=mouse0 event7
EOF
# A camera button has the kbd handler but is no keyboard: unreadable is fine.
touch "${fixture_root}/dev/input/event20"
chmod 0000 "${fixture_root}/dev/input/event20"
# Stand-ins for the privileged tools: they only log their arguments.
for tool in udevadm sudo; do
  cat > "${fixture_root}/bin/${tool}" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "${fixture_root}/${tool}.log"
EOF
done
printf 'exec "$@"\n' >> "${fixture_root}/bin/sudo"
chmod 0755 "${fixture_root}/bin/udevadm" "${fixture_root}/bin/sudo"
# Everything below sees only these fixture devices, rules and tools.
export PATH="${fixture_root}/bin:/usr/bin:/bin"
export AGENTDICTATE_PROC_INPUT_DEVICES="${fixture_root}/proc-input-devices"
export AGENTDICTATE_DEV_INPUT_DIR="${fixture_root}/dev/input"
export AGENTDICTATE_UINPUT_PATH="${fixture_root}/dev/uinput"
export AGENTDICTATE_UDEV_RULES_DIRS="${fixture_root}/rules"
export AGENTDICTATE_GRANT_ROOT="${fixture_root}/root"

# Runs a command, keeping its combined output in `output` and exit status in
# `status`.
capture() {
  status=0
  output="$("$@" 2>&1)" || status=$?
}

set_device_mode() {
  touch "${fixture_root}/dev/input/event4" "${fixture_root}/dev/uinput"
  chmod "$1" "${fixture_root}/dev/input/event4" "${fixture_root}/dev/uinput"
}

set_device_mode 0660
capture agentdictate_check_native_readiness
(( status == 0 )) || fail "secure native-access fixture should be ready (${status})"
assert_contains "${output}" "Native input readiness: ready"

set_device_mode 0666
printf '%s\n' 'KERNEL=="null|zero", MODE="0666"' > "${fixture_root}/rules/50-default.rules"
printf '%s\n' 'KERNEL=="uinput", MODE="0666"' 'KERNEL=="event*", SUBSYSTEM=="input", MODE="0666"' \
  > "${fixture_root}/rules/99-other-app.rules"
capture agentdictate_check_native_readiness
(( status == 3 )) || fail "world-accessible devices must report exit 3 (${status})"
assert_contains "${output}" "World-accessible: ${fixture_root}/dev/input/event4 ${fixture_root}/dev/uinput"
assert_contains "${output}" "${fixture_root}/rules/99-other-app.rules"
[[ "${output}" != *"50-default.rules"* ]] || fail "rules for other devices were blamed"
assert_contains "${output}" "working, but insecure"
capture "${PROJECT_DIR}/install.sh" --check-native-access
(( status == 3 )) || fail "install.sh --check-native-access must pass exit 3 through (${status})"

rm -f "${fixture_root}/dev/input/event4" "${fixture_root}/dev/uinput"
capture agentdictate_check_native_readiness
(( status == 2 )) || fail "missing input devices must report exit 2 (${status})"
assert_contains "${output}" "No readable keyboard event device"
assert_contains "${output}" "Cannot write"
assert_contains "${output}" "--setup-native-access"

GRANT="${PROJECT_DIR}/packaging/grant-access.sh"
RULE="${PROJECT_DIR}/packaging/70-agentdictate-input.rules"
INSTALLED_RULE="${fixture_root}/root/etc/udev/rules.d/70-agentdictate-input.rules"
EXPECTED_UDEVADM=$'control --reload-rules
trigger --subsystem-match=input --action=change
trigger --subsystem-match=misc --sysname-match=uinput --action=change
settle --timeout=10'

capture "${GRANT}" --dry-run
(( status == 0 )) || fail "grant helper dry run failed: ${output}"
assert_contains "${output}" "install -D -m 0644 ${RULE} ${INSTALLED_RULE}"
assert_contains "${output}" "udevadm trigger --subsystem-match=misc --sysname-match=uinput --action=change"
[[ ! -e "${fixture_root}/root" && ! -e "${fixture_root}/udevadm.log" ]] || \
  fail "grant helper dry run must not change anything"

capture "${PROJECT_DIR}/install.sh" --setup-native-access <<< "n"
(( status == 2 )) || fail "declined setup must leave access missing (${status})"
assert_contains "${output}" "sudo ${GRANT}"
assert_contains "${output}" "Nothing was changed."
[[ ! -e "${fixture_root}/sudo.log" && ! -e "${INSTALLED_RULE}" ]] || \
  fail "declined setup ran the helper"

capture "${PROJECT_DIR}/install.sh" --setup-native-access <<< "y"
(( status == 2 )) || fail "setup must report the still-missing fixture access (${status})"
[[ "$(cat "${fixture_root}/sudo.log")" == "${GRANT}" ]] || fail "setup must run only the helper with sudo"
cmp -s "${RULE}" "${INSTALLED_RULE}" || fail "grant helper did not install the rule"
[[ "$(stat -c '%a' "${INSTALLED_RULE}")" == 644 ]] || fail "installed rule must be mode 0644"
[[ "$(cat "${fixture_root}/udevadm.log")" == "${EXPECTED_UDEVADM}" ]] || \
  fail "grant helper must reload and retrigger udev"
assert_contains "${output}" "Log out and back in"

# A package that ships the rule leaves /etc alone and only reapplies it.
rm -rf "${fixture_root}/root" "${fixture_root}/udevadm.log"
mkdir -p "${fixture_root}/root/usr/lib/udev/rules.d"
cp "${RULE}" "${fixture_root}/root/usr/lib/udev/rules.d/"
capture "${GRANT}"
(( status == 0 )) || fail "grant helper failed with a packaged rule: ${output}"
[[ ! -e "${INSTALLED_RULE}" ]] || fail "grant helper duplicated the packaged rule"
[[ "$(cat "${fixture_root}/udevadm.log")" == "${EXPECTED_UDEVADM}" ]] || \
  fail "grant helper must reapply a packaged rule"

set_device_mode 0660
capture "${PROJECT_DIR}/install.sh" --setup-native-access < /dev/null
(( status == 0 )) || fail "setup must not prompt when access is ready (${status})"
[[ "$(wc -l < "${fixture_root}/sudo.log")" == 1 ]] || fail "setup used sudo although access is ready"
capture "${PROJECT_DIR}/install.sh" --check-native-access
(( status == 0 )) || fail "install.sh readiness mode must not require a build or mutate the fixture"
assert_contains "${output}" "Native input readiness: ready"

# Policy guards: the shipped access assets never make input devices
# world-writable, and installers never enable or start a user service.
GUIDE="${PROJECT_DIR}/packaging/NATIVE_ACCESS.md"
if grep -Eq 'MODE="?0?666"?|chmod[[:space:]]+0?666' "${RULE}" "${GUIDE}"; then
  fail "native access assets must never grant world-write access"
fi
if grep -Eq 'systemctl[^#]*[[:space:]](enable|start|restart)([[:space:]]|$)' \
  "${PROJECT_DIR}/install.sh" "${PROJECT_DIR}/packaging/build-deb.sh"; then
  fail "installers must never enable or start a user service"
fi

echo "Native install readiness checks passed."
