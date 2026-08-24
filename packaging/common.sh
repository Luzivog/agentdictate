# Source this file after PROJECT_DIR is set.
#
# Shared helpers for the source installer, Debian builder, and AppImage builder.

DESKTOP_ID="local.agentdictate.AgentDictate"

agentdictate_workspace_version() {
  local version
  version="$(awk '/^\[workspace.package\]/{found=1; next} found && /^version = /{gsub(/[\" ]/, "", $3); print $3; exit}' "${PROJECT_DIR}/Cargo.toml")"
  if [[ -z "${version}" ]]; then
    echo "Could not read the workspace version from Cargo.toml" >&2
    return 1
  fi
  printf '%s\n' "${version}"
}

agentdictate_rust_host() {
  # Consume the full rustc output; exiting early can SIGPIPE the rustup shim
  # under pipefail.
  rustc -vV | awk '/^host: / { host = $2 } END { print host }'
}

agentdictate_build_release_binaries() {
  cargo build --manifest-path "${PROJECT_DIR}/Cargo.toml" \
    --locked --release --features desktop -p agentdictate-app --bins
}

agentdictate_install_shared_assets() {
  local destination_root="${1:?destination root is required}"

  install -m 0644 "${PROJECT_DIR}/agentdictate.desktop" \
    "${destination_root}/usr/share/applications/${DESKTOP_ID}.desktop"
  install -m 0644 "${PROJECT_DIR}/assets/agentdictate.svg" \
    "${destination_root}/usr/share/icons/hicolor/scalable/apps/agentdictate.svg"
  install -m 0644 "${PROJECT_DIR}/packaging/${DESKTOP_ID}.metainfo.xml" \
    "${destination_root}/usr/share/metainfo/${DESKTOP_ID}.metainfo.xml"
  install -m 0644 "${PROJECT_DIR}/LICENSE" \
    "${destination_root}/usr/share/doc/agentdictate/copyright"
}
