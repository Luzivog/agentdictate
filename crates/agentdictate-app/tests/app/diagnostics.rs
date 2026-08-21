use agentdictate_app::diagnostics::init_file_logging;
use std::fs;
use tempfile::tempdir;

#[test]
fn file_logging_surfaces_a_filesystem_error_for_an_occupied_path() {
    let directory = tempdir().unwrap();
    let occupied = directory.path().join("occupied");
    fs::write(&occupied, b"not a directory").unwrap();

    assert!(
        init_file_logging(&occupied, "agentdictate").is_err(),
        "creating logs under an existing file must fail instead of silently disabling logs"
    );
}
