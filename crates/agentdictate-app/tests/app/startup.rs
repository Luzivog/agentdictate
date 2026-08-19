use std::path::Path;

use agentdictate_app::{sync_autostart, sync_autostart_command};
use tempfile::tempdir;

#[test]
fn enabling_autostart_writes_the_native_daemon_entry() {
    let directory = tempdir().unwrap();
    let entry = directory
        .path()
        .join("local.agentdictate.AgentDictate.desktop");
    let daemon = Path::new("/opt/Agent Dictate/agentdictated");

    sync_autostart(&entry, true, daemon).unwrap();

    let contents = std::fs::read_to_string(&entry).unwrap();
    assert!(contents.contains("Name=AgentDictate background service"));
    assert!(contents.contains("Exec=\"/opt/Agent Dictate/agentdictated\""));
    assert!(contents.contains("X-GNOME-Autostart-enabled=true"));
}

#[test]
fn appimage_autostart_command_keeps_the_persistent_launcher_and_background_arg() {
    let directory = tempdir().unwrap();
    let entry = directory
        .path()
        .join("local.agentdictate.AgentDictate.desktop");

    sync_autostart_command(
        &entry,
        true,
        Path::new("/apps/Agent Dictate.AppImage"),
        &["--background".to_owned()],
    )
    .unwrap();

    let contents = std::fs::read_to_string(entry).unwrap();
    assert!(contents.contains("Exec=\"/apps/Agent Dictate.AppImage\" --background"));
}

#[test]
fn disabling_autostart_writes_a_user_override_for_system_packages() {
    let directory = tempdir().unwrap();
    let entry = directory
        .path()
        .join("local.agentdictate.AgentDictate.desktop");
    std::fs::write(&entry, "legacy").unwrap();

    sync_autostart(&entry, false, Path::new("/unused")).unwrap();

    let contents = std::fs::read_to_string(&entry).unwrap();
    assert!(contents.contains("Hidden=true"));
    assert!(!contents.contains("Exec="));
}
