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
      for (line = 1; line <= NF; line++) {
        if ($line ~ /^H: Handlers=/) {
          handlers = $line
          sub(/^H: Handlers=/, "", handlers)
        }
      }
      count = split(handlers, names, /[[:space:]]+/)
      keyboard = 0
      for (entry = 1; entry <= count; entry++) {
        if (names[entry] == "kbd") keyboard = 1
      }
      if (!keyboard) next
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

agentdictate_check_native_readiness() {
  local failed=0
  local keyboard_count=0
  local keyboard_path
  local -a unreadable_keyboards=()
  local -a insecure_keyboards=()

  while IFS= read -r keyboard_path; do
    [[ -n "${keyboard_path}" ]] || continue
    ((keyboard_count += 1))
    [[ -r "${keyboard_path}" ]] || unreadable_keyboards+=("${keyboard_path}")
    if [[ -e "${keyboard_path}" ]] && \
      agentdictate_world_permission_is_set "${keyboard_path}" 4; then
      insecure_keyboards+=("${keyboard_path}")
    fi
  done < <(agentdictate_keyboard_event_paths)

  if (( keyboard_count == 0 )) || (( ${#unreadable_keyboards[@]} > 0 )); then
    echo "Native input issue: No readable keyboard event device is available for the global shortcut." >&2
    if (( ${#unreadable_keyboards[@]} > 0 )); then
      printf '  Unreadable: %s\n' "${unreadable_keyboards[@]}" >&2
    fi
    failed=1
  fi
  if (( ${#insecure_keyboards[@]} > 0 )); then
    echo "Native input issue: keyboard event devices are world-accessible; replace the permissive rule with 70-agentdictate-input.rules." >&2
    printf '  Insecure: %s\n' "${insecure_keyboards[@]}" >&2
    failed=1
  fi

  local uinput_path="${AGENTDICTATE_UINPUT_PATH:-/dev/uinput}"
  if [[ ! -w "${uinput_path}" ]]; then
    echo "Native input issue: Cannot write ${uinput_path}; paste injection is unavailable." >&2
    failed=1
  elif agentdictate_world_permission_is_set "${uinput_path}" 2; then
    echo "Native input issue: ${uinput_path} is world-accessible; replace the permissive rule with 70-agentdictate-input.rules." >&2
    failed=1
  fi

  if (( failed != 0 )); then
    echo "Native input readiness: needs attention" >&2
    return 1
  fi
  echo "Native input readiness: ready"
}
