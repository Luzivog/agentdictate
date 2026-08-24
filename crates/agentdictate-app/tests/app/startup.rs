use std::{fs, os::unix::fs::PermissionsExt, path::Path};

use agentdictate_app::sync_startup_command;
use tempfile::tempdir;

#[test]
fn enabling_startup_writes_a_session_owned_service_and_service_bootstrap() {
    let directory = tempdir().unwrap();
    let entry = directory
        .path()
        .join("autostart/local.agentdictate.AgentDictate.desktop");
    let service = directory.path().join("systemd/user/agentdictated.service");

    sync_startup_command(
        &entry,
        &service,
        true,
        Path::new("/opt/Agent Dictate/agentdictated"),
        &["--service".to_owned()],
        Path::new("/opt/Agent Dictate/agentdictated"),
        &["--start-service".to_owned()],
        Path::new("/bin/true"),
    )
    .unwrap();

    let service_contents = fs::read_to_string(&service).unwrap();
    assert!(service_contents.contains("PartOf=graphical-session.target"));
    assert!(service_contents.contains("After=graphical-session.target"));
    assert!(service_contents.contains("Restart=on-failure"));
    assert!(!service_contents.contains("WantedBy="));
    assert!(service_contents.contains("Environment=AGENTDICTATE_SERVICE_ROUTE="));
    assert!(service_contents.contains("ExecStart=\"/opt/Agent Dictate/agentdictated\" --service"));

    assert_eq!(
        fs::metadata(&service).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let entry_contents = fs::read_to_string(&entry).unwrap();
    assert!(entry_contents.contains("Name=AgentDictate background service"));
    assert!(entry_contents.contains("Exec=\"/opt/Agent Dictate/agentdictated\" --start-service"));
    assert!(!entry_contents.contains("Exec=\"/opt/Agent Dictate/agentdictated\" --service"));
    assert_eq!(
        fs::metadata(&entry).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn appimage_startup_uses_distinct_bootstrap_and_daemon_service_arguments() {
    let directory = tempdir().unwrap();
    let entry = directory.path().join("autostart/agentdictate.desktop");
    let service = directory.path().join("systemd/agentdictated.service");

    sync_startup_command(
        &entry,
        &service,
        true,
        Path::new("/apps/Agent Dictate.AppImage"),
        &["--service".to_owned()],
        Path::new("/apps/Agent Dictate.AppImage"),
        &["--background".to_owned()],
        Path::new("/bin/true"),
    )
    .unwrap();

    let service_contents = fs::read_to_string(service).unwrap();
    assert!(service_contents.contains("ExecStart=\"/apps/Agent Dictate.AppImage\" --service"));
    let entry_contents = fs::read_to_string(entry).unwrap();
    assert!(entry_contents.contains("Exec=\"/apps/Agent Dictate.AppImage\" --background"));
}

#[test]
fn disabling_startup_hides_the_bootstrap_and_disables_the_service() {
    let directory = tempdir().unwrap();
    let entry = directory.path().join("autostart/agentdictate.desktop");
    let service = directory.path().join("systemd/agentdictated.service");
    fs::create_dir_all(entry.parent().unwrap()).unwrap();
    fs::write(&entry, "legacy direct daemon entry").unwrap();

    sync_startup_command(
        &entry,
        &service,
        false,
        Path::new("/usr/bin/agentdictated"),
        &["--service".to_owned()],
        Path::new("/usr/bin/agentdictated"),
        &["--start-service".to_owned()],
        Path::new("/bin/true"),
    )
    .unwrap();

    let entry_contents = fs::read_to_string(entry).unwrap();
    assert!(entry_contents.contains("Hidden=true"));
    assert!(!entry_contents.contains("Exec="));
}

#[test]
fn packaged_startup_contract_never_launches_the_daemon_directly() {
    let package_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging");
    let service = fs::read_to_string(package_root.join("agentdictated.service")).unwrap();
    let autostart =
        fs::read_to_string(package_root.join("agentdictate-autostart.desktop")).unwrap();

    assert!(service.contains("PartOf=graphical-session.target"));
    assert!(service.contains("After=graphical-session.target"));
    assert!(service.contains("ExecStart=/usr/bin/agentdictated --service"));
    assert!(service.contains("Restart=on-failure"));
    assert!(!service.contains("WantedBy="));
    assert!(autostart.contains("Exec=agentdictated --start-service"));
    assert!(!autostart.contains("Exec=agentdictated\n"));
}
