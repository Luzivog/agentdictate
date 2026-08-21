use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use agentdictate_core::Settings;

use crate::RuntimeError;

pub fn load_settings(path: impl AsRef<Path>) -> Result<Settings, RuntimeError> {
    let path = path.as_ref();
    if !path.exists() {
        let settings = Settings::default();
        save_settings(path, &settings)?;
        return Ok(settings);
    }

    let contents = fs::read_to_string(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    let mut settings: Settings = serde_json::from_str(&contents)?;
    if settings.repair_pricing_defaults() {
        save_settings(path, &settings)?;
    }
    Ok(settings)
}

pub fn save_settings(path: impl AsRef<Path>, settings: &Settings) -> Result<(), RuntimeError> {
    let path = path.as_ref();
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    let mut contents = serde_json::to_string_pretty(settings)?;
    contents.push('\n');
    temporary.write_all(contents.as_bytes())?;
    temporary.as_file_mut().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
