# Source this file after PROJECT_DIR is set.
#
# GPUI links xkbcommon by its development name. Some desktops have the runtime
# sonames but not the unversioned linker symlinks, so source builds create local
# shims under target/. Distro builds with development packages are unchanged.
LINKER_SHIM_DIR="${PROJECT_DIR}/target/linker-shims"
mkdir -p "${LINKER_SHIM_DIR}"
LDCONFIG_COMMAND="$(command -v ldconfig 2>/dev/null || true)"
if [[ -z "${LDCONFIG_COMMAND}" && -x /usr/sbin/ldconfig ]]; then
  LDCONFIG_COMMAND=/usr/sbin/ldconfig
elif [[ -z "${LDCONFIG_COMMAND}" && -x /sbin/ldconfig ]]; then
  LDCONFIG_COMMAND=/sbin/ldconfig
fi

for LINKER_LIBRARY in xkbcommon xkbcommon-x11; do
  LINKER_NAME="lib${LINKER_LIBRARY}.so"
  if [[ -n "${LDCONFIG_COMMAND}" ]] && \
    [[ "$(cc -print-file-name="${LINKER_NAME}")" == "${LINKER_NAME}" ]]; then
    # Consume the complete ldconfig output. Exiting awk on the first match can
    # SIGPIPE ldconfig and abort strict-mode installers through pipefail.
    LINKER_RUNTIME="$(
      "${LDCONFIG_COMMAND}" -p 2>/dev/null | \
        awk -v name="${LINKER_NAME}.0" \
          '$1 == name && !found { runtime = $NF; found = 1 } END { if (found) print runtime }'
    )"
    if [[ -n "${LINKER_RUNTIME}" ]]; then
      ln -sfn "${LINKER_RUNTIME}" "${LINKER_SHIM_DIR}/${LINKER_NAME}"
    fi
  fi
done

export LIBRARY_PATH="${LINKER_SHIM_DIR}${LIBRARY_PATH:+:${LIBRARY_PATH}}"
unset LDCONFIG_COMMAND LINKER_LIBRARY LINKER_NAME LINKER_RUNTIME LINKER_SHIM_DIR
