use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use agentdictate_core::Settings;

use crate::RuntimeError;
use crate::fs::write_atomic;

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
    let mut contents = serde_json::to_string_pretty(settings)?;
    contents.push('\n');
    write_atomic(path, contents.as_bytes(), 0o600)?;
    Ok(())
}
