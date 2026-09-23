# Source this file after PROJECT_DIR is set.
#
# Read-only native-input diagnostics shared by install.sh and packaging tests.
# Every path is overrideable so the checks can be exercised without touching
# the host's input devices or user service manager.

agentdictate_keyboard_event_paths() {
  local devices_file="${AGENTDICTATE_PROC_INPUT_DEVICES:-/proc/bus/input/devices}"
  local input_dir="${AGENTDICTATE_DEV_INPUT_DIR:-/dev/input}"

  [[ -r "${devices_file}" ]] || return 0
  awk -v input_dir="${input_dir}" '
    BEGIN { RS = ""; FS = "\n" }
    {
      handlers = ""
      keys = ""
      for (line = 1; line <= NF; line++) {
        if ($line ~ /^H: Handlers=/) {
          handlers = $line
          sub(/^H: Handlers=/, "", handlers)
        }
        if ($line ~ /^B: KEY=/) {
          keys = $line
          sub(/^B: KEY=/, "", keys)
          sub(/[[:space:]]+$/, "", keys)
        }
      }
      # Like udev, a keyboard has every key from Esc to S: bits 1-31 of the
      # lowest bitmap word. Cameras, power buttons and hotkey panels also get
      # the kbd handler, but the access rule never covers them.
      words = split(keys, key_words, /[[:space:]]+/)
      if (key_words[words] !~ /[fF][fF][fF][fF][fF][fF][fF][eEfF]$/) next
      count = split(handlers, names, /[[:space:]]+/)
      for (entry = 1; entry <= count; entry++) {
        if (names[entry] ~ /^event[0-9]+$/) {
          print input_dir "/" names[entry]
        }
      }
    }
  ' "${devices_file}"
}

agentdictate_world_permission_is_set() {
  local path="$1"
  local mask="$2"
  local mode
  mode="$(stat -c '%a' -- "${path}" 2>/dev/null)" || return 1
  local world_digit="${mode: -1}"
  (( (10#${world_digit} & mask) != 0 ))
}

agentdictate_command_exists() {
  local command_name="$1"
  if [[ "${command_name}" == */* ]]; then
    [[ -x "${command_name}" ]]
  else
    command -v -- "${command_name}" >/dev/null 2>&1
  fi
}

# Lists udev rules that make input devices world-accessible (mode 0666).
agentdictate_world_access_rules() {
  local directory rule
  for directory in ${AGENTDICTATE_UDEV_RULES_DIRS:-/etc/udev/rules.d /usr/lib/udev/rules.d}; do
    for rule in "${directory}"/*.rules; do
      [[ -f "${rule}" ]] || continue
      if awk '/MODE[[:space:]]*:?=[[:space:]]*"0?666"/ && /uinput|event|input/ { found = 1 }
        END { exit !found }' "${rule}"; then
        printf '%s\n' "${rule}"
      fi
    done
  done
}

# Reports native input readiness. Returns 0 when ready, 2 when keyboard or
# /dev/uinput access is missing, and 3 when both work but are world-accessible,
# which AgentDictate's own rule never does.
agentdictate_check_native_readiness() {
  local missing=0
  local keyboard_count=0
  local keyboard_path
  local -a unreadable_keyboards=()
  local -a world_accessible=()

  while IFS= read -r keyboard_path; do
    [[ -n "${keyboard_path}" ]] || continue
    ((keyboard_count += 1))
    [[ -r "${keyboard_path}" ]] || unreadable_keyboards+=("${keyboard_path}")
    if [[ -e "${keyboard_path}" ]] && \
      agentdictate_world_permission_is_set "${keyboard_path}" 4; then
      world_accessible+=("${keyboard_path}")
    fi
  done < <(agentdictate_keyboard_event_paths)

  if (( keyboard_count == 0 )) || (( ${#unreadable_keyboards[@]} > 0 )); then
    echo "Native input issue: No readable keyboard event device is available for the global shortcut." >&2
    if (( ${#unreadable_keyboards[@]} > 0 )); then
      printf '  Unreadable: %s\n' "${unreadable_keyboards[@]}" >&2
    fi
    missing=1
  fi

  local uinput_path="${AGENTDICTATE_UINPUT_PATH:-/dev/uinput}"
  if [[ ! -w "${uinput_path}" ]]; then
    echo "Native input issue: Cannot write ${uinput_path}; paste injection is unavailable." >&2
    missing=1
  elif agentdictate_world_permission_is_set "${uinput_path}" 2; then
    world_accessible+=("${uinput_path}")
  fi

  if (( missing != 0 )); then
    echo "Native input readiness: needs setup (run ./install.sh --setup-native-access)" >&2
    return 2
  fi
  if (( ${#world_accessible[@]} > 0 )); then
    local -a rules=()
    mapfile -t rules < <(agentdictate_world_access_rules)
    echo "Native input works, but any local user or program can read your keystrokes or type into your apps:" >&2
    echo "  World-accessible: ${world_accessible[*]}" >&2
    if (( ${#rules[@]} > 0 )); then
      echo "  Another app's udev rule grants this, not AgentDictate's:" >&2
      printf '    %s\n' "${rules[@]}" >&2
      echo "  AgentDictate works as is. To close the hole, remove or narrow that rule (the app that" >&2
      echo "  installed it may stop working), then run ./install.sh --setup-native-access." >&2
    else
      echo "  AgentDictate's own rule keeps these devices at mode 0660; look for a udev rule or manual" >&2
      echo "  chmod that sets mode 0666." >&2
    fi
    echo "Native input readiness: working, but insecure" >&2
    return 3
  fi
  echo "Native input readiness: ready"
}

# Shows exactly what `helper` (grant-access.sh) runs as root, asks, then runs
# it with sudo. Offered only while access is missing: AgentDictate's rule
# cannot override another app's world-accessible one. Returns the readiness
# status afterwards.
agentdictate_setup_native_access() {
  local helper="$1"
  local status=0
  agentdictate_check_native_readiness >/dev/null 2>&1 || status=$?
  if (( status != 2 )); then
    agentdictate_check_native_readiness
    return
  fi
  echo "AgentDictate needs administrator rights once, so your desktop session can read the"
  echo "keyboard (for the shortcut) and use /dev/uinput (for pasting). This runs:"
  echo
  echo "  sudo ${helper}"
  echo
  echo "which does:"
  "${helper}" --dry-run
  echo
  local answer=""
  read -r -p "Run it now? [y/N] " answer || true
  if [[ ! "${answer}" =~ ^[Yy]([Ee][Ss])?$ ]]; then
    echo "Nothing was changed."
    return 2
  fi
  sudo "${helper}"
  status=0
  agentdictate_check_native_readiness || status=$?
  if (( status == 2 )); then
    echo "The rule is installed, but this session has not received the new access yet." >&2
    echo "Log out and back in, then run ./install.sh --check-native-access." >&2
  fi
  return "${status}"
}
