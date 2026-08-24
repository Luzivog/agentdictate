use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Replaces `path` with fully written bytes using the requested Unix mode.
///
/// The temporary file and its containing directory are synced before the
/// function reports success.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    temporary.write_all(bytes)?;
    temporary.as_file_mut().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn atomic_write_replaces_content_and_applies_the_explicit_mode() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/settings.json");

        write_atomic(&path, b"first\n", 0o640).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"first\n");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );

        write_atomic(&path, b"second\n", 0o600).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second\n");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
