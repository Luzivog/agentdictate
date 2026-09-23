//! The backend of one-click native input access: runs grant-access.sh as root
//! through pkexec. Only call it when the user asks for it; pkexec asks for an
//! administrator password.

use std::{
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
};

use agentdictate_runtime::write_atomic;

const GRANT_HELPER: &str = include_str!("../../../packaging/grant-access.sh");
const INPUT_RULE_NAME: &str = "70-agentdictate-input.rules";
const INPUT_RULE: &str = include_str!("../../../packaging/70-agentdictate-input.rules");
/// The root-owned copy the Debian package installs.
const PACKAGED_GRANT_HELPER: &str = "/usr/lib/agentdictate/grant-access";

#[derive(Debug, thiserror::Error)]
pub enum NativeAccessError {
    #[error("pkexec is not installed; run `./install.sh --setup-native-access` instead")]
    PkexecMissing,
    #[error("administrator authorization was cancelled or refused")]
    NotAuthorized,
    #[error("the native access helper failed ({0})")]
    HelperFailed(ExitStatus),
    #[error("could not prepare the native access helper: {0}")]
    Io(#[from] io::Error),
}

/// Installs AgentDictate's udev rule and applies it to existing devices, as
/// root through pkexec. Uses the packaged helper when there is one; otherwise
/// writes this build's copy and its rule into `directory`, since root cannot
/// read an AppImage's FUSE mount.
pub fn grant_native_access(directory: &Path) -> Result<(), NativeAccessError> {
    let packaged = Path::new(PACKAGED_GRANT_HELPER);
    let helper = if packaged.exists() {
        packaged.to_owned()
    } else {
        write_grant_helper(directory)?
    };
    run_elevated(Path::new("pkexec"), &helper)
}

fn write_grant_helper(directory: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(directory)?;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    write_atomic(
        &directory.join(INPUT_RULE_NAME),
        INPUT_RULE.as_bytes(),
        0o644,
    )?;
    let helper = directory.join("grant-access.sh");
    write_atomic(&helper, GRANT_HELPER.as_bytes(), 0o700)?;
    Ok(helper)
}

fn run_elevated(pkexec: &Path, helper: &Path) -> Result<(), NativeAccessError> {
    let status = Command::new(pkexec)
        .arg(helper)
        .status()
        .map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => NativeAccessError::PkexecMissing,
            _ => error.into(),
        })?;
    match status.code() {
        Some(0) => Ok(()),
        // pkexec's own codes: 126 when the dialog is dismissed, 127 when the
        // user is not authorized.
        Some(126 | 127) => Err(NativeAccessError::NotAuthorized),
        _ => Err(NativeAccessError::HelperFailed(status)),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    /// A pkexec stand-in that runs the helper against a fake root, with a
    /// udevadm that only logs.
    fn fake_pkexec(root: &Path) -> PathBuf {
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let udevadm = bin.join("udevadm");
        fs::write(
            &udevadm,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n",
                root.join("udevadm.log").display()
            ),
        )
        .unwrap();
        let pkexec = bin.join("pkexec");
        fs::write(
            &pkexec,
            format!(
                "#!/bin/sh\nexport AGENTDICTATE_GRANT_ROOT='{}' PATH='{}':\"$PATH\"\nexec \"$@\"\n",
                root.join("root").display(),
                bin.display()
            ),
        )
        .unwrap();
        for tool in [&udevadm, &pkexec] {
            fs::set_permissions(tool, fs::Permissions::from_mode(0o755)).unwrap();
        }
        pkexec
    }

    #[test]
    fn the_embedded_helper_installs_its_rule_through_pkexec() {
        let directory = tempdir().unwrap();
        let pkexec = fake_pkexec(directory.path());
        let helper = write_grant_helper(&directory.path().join("native-access")).unwrap();

        run_elevated(&pkexec, &helper).unwrap();

        let installed = directory
            .path()
            .join("root/etc/udev/rules.d")
            .join(INPUT_RULE_NAME);
        assert_eq!(fs::read_to_string(installed).unwrap(), INPUT_RULE);
        let udevadm = fs::read_to_string(directory.path().join("udevadm.log")).unwrap();
        assert!(udevadm.starts_with("control --reload-rules\n"));
    }

    #[test]
    fn a_dismissed_password_prompt_is_reported_as_not_authorized() {
        let directory = tempdir().unwrap();
        let pkexec = directory.path().join("pkexec");
        fs::write(&pkexec, "#!/bin/sh\nexit 126\n").unwrap();
        fs::set_permissions(&pkexec, fs::Permissions::from_mode(0o755)).unwrap();

        let error = run_elevated(&pkexec, Path::new("/unused")).unwrap_err();

        assert!(matches!(error, NativeAccessError::NotAuthorized));
    }
}
