use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use agentdictate_core::Settings;
use serde_json::Value;

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
    let mut stored: Value = serde_json::from_str(&contents)?;
    merge_retired_project_context(&mut stored);
    Ok(serde_json::from_value(stored)?)
}

pub fn save_settings(path: impl AsRef<Path>, settings: &Settings) -> Result<(), RuntimeError> {
    let path = path.as_ref();
    let mut contents = serde_json::to_string_pretty(settings)?;
    contents.push('\n');
    write_atomic(path, contents.as_bytes(), 0o600)?;
    Ok(())
}

/// Appends the retired "Current work context" (`project_context`) to the
/// "About your work" prompt that replaced it. The old key is gone from
/// config.json after the next save.
fn merge_retired_project_context(stored: &mut Value) {
    let Some(object) = stored.as_object_mut() else {
        return;
    };
    let Some(Value::String(context)) = object.remove("project_context") else {
        return;
    };
    let context = context.trim();
    if context.is_empty() {
        return;
    }
    let prompt = object
        .get("transcription_prompt")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let merged = if prompt.is_empty() {
        context.to_owned()
    } else {
        format!("{prompt}\n{context}")
    };
    object.insert("transcription_prompt".to_owned(), Value::String(merged));
}
