#!/bin/bash
# Grants the desktop session AgentDictate's native input access. Runs as root,
# and only when the user asks: `./install.sh --setup-native-access` runs it
# with sudo, `agentdictate setup-access` with pkexec. It installs
# 70-agentdictate-input.rules from beside this script (unless the AgentDictate
# package already ships the rule), then applies the rules to existing devices.
#
#   --dry-run  print the commands instead of running them; needs no root
set -euo pipefail

RULE_NAME="70-agentdictate-input.rules"
# Packaging tests point this at a fake root. pkexec and sudo clear it.
ROOT="${AGENTDICTATE_GRANT_ROOT:-}"
HERE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  "") DRY_RUN=0 ;;
  *)
    echo "Usage: $0 [--dry-run]" >&2
    exit 64
    ;;
esac

step() {
  if (( DRY_RUN )); then
    printf ' '
    printf ' %q' "$@"
    printf '\n'
  else
    "$@"
  fi
}

if [[ ! -e "${ROOT}/usr/lib/udev/rules.d/${RULE_NAME}" ]]; then
  step install -D -m 0644 "${HERE}/${RULE_NAME}" "${ROOT}/etc/udev/rules.d/${RULE_NAME}"
fi
step udevadm control --reload-rules
step udevadm trigger --subsystem-match=input --action=change
step udevadm trigger --subsystem-match=misc --sysname-match=uinput --action=change
# Wait until udev has applied the rules, so a readiness check that follows
# sees the session's new access.
step udevadm settle --timeout=10
