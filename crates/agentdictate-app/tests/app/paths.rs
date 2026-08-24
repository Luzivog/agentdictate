use agentdictate_app::AppPaths;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use tempfile::tempdir;

#[test]
fn app_paths_preserve_the_existing_xdg_layout() {
    let paths = AppPaths::from_roots(
        "/tmp/config",
        "/tmp/data",
        "/tmp/state",
        "/tmp/cache",
        "/tmp/runtime",
    );

    assert_eq!(
        paths.config_file,
        PathBuf::from("/tmp/config/agentdictate/config.json")
    );
    assert_eq!(
        paths.database_file,
        PathBuf::from("/tmp/data/agentdictate/agentdictate.sqlite")
    );
    assert_eq!(
        paths.recordings,
        PathBuf::from("/tmp/data/agentdictate/recordings")
    );
    assert_eq!(paths.logs, PathBuf::from("/tmp/state/agentdictate/logs"));
    assert_eq!(paths.cache, PathBuf::from("/tmp/cache/agentdictate"));
    assert_eq!(paths.runtime, PathBuf::from("/tmp/runtime/agentdictate"));
    assert_eq!(
        paths.daemon_service_file,
        PathBuf::from("/tmp/data/systemd/user/agentdictated.service")
    );
}

#[test]
fn ensuring_paths_prepares_every_runtime_parent_without_touching_files() {
    let directory = tempdir().unwrap();
    let paths = AppPaths::from_roots(
        directory.path().join("config"),
        directory.path().join("data"),
        directory.path().join("state"),
        directory.path().join("cache"),
        directory.path().join("runtime"),
    );

    paths.ensure_directories().unwrap();

    for expected in [
        paths.config_file.parent().unwrap(),
        paths.database_file.parent().unwrap(),
        &paths.recordings,
        &paths.logs,
        &paths.cache,
        &paths.runtime,
    ] {
        assert!(expected.is_dir(), "{} was not created", expected.display());
        assert_eq!(
            std::fs::metadata(expected).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    assert!(!paths.config_file.exists());
    assert!(!paths.database_file.exists());
}
