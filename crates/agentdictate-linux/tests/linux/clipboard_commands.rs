use crate::support;

use std::{
    fs,
    time::{Duration, Instant},
};

use agentdictate_linux::{
    clipboard::{ClipboardSelection, CommandClipboard},
    command::{PlatformExecutable, PlatformTool, SystemCommandRunner},
    paste::ClipboardReadinessEvidence,
};
use support::TestDirectory;

fn fake_xsel(directory: &TestDirectory, state: &std::path::Path, log: &std::path::Path) -> std::path::PathBuf {
    directory.executable(
        "xsel",
        &format!(
            concat!(
                "#!/bin/sh\n",
                "printf '%s\\n' \"$*\" >> '{}'\n",
                "case \"$*\" in\n",
                "  *--output*) cat '{}' ;;\n",
                "  *) cat > '{}'; exec tail -f /dev/null ;;\n",
                "esac\n",
            ),
            log.display(),
            state.display(),
            state.display(),
        ),
    )
}

#[test]
fn clipboard_is_ready_only_after_live_owner_readback_matches() {
    let directory = TestDirectory::new();
    let state = directory.path().join("clipboard.txt");
    let log = directory.path().join("xsel.log");
    let xsel = fake_xsel(&directory, &state, &log);
    let clipboard = CommandClipboard::new(
        SystemCommandRunner,
        PlatformExecutable::at(PlatformTool::Xsel, xsel),
    );

    let mut publication = clipboard
        .publish(
            b"hello from AgentDictate",
            Instant::now() + Duration::from_secs(2),
        )
        .expect("clipboard owner and readback become ready");

    assert_eq!(
        publication.evidence,
        ClipboardReadinessEvidence::ReadbackMatches
    );
    assert!(publication.owner.is_alive().expect("owner liveness"));
    let arguments = fs::read_to_string(log).unwrap();
    assert!(
        arguments
            .lines()
            .any(|line| line == "--clipboard --input --nodetach")
    );
    assert!(arguments.lines().any(|line| line == "--clipboard --output"));
}

#[test]
fn primary_selection_is_published_and_verified_independently() {
    let directory = TestDirectory::new();
    let state = directory.path().join("primary.txt");
    let log = directory.path().join("xsel.log");
    let xsel = fake_xsel(&directory, &state, &log);
    let clipboard = CommandClipboard::new(
        SystemCommandRunner,
        PlatformExecutable::at(PlatformTool::Xsel, xsel),
    );

    let mut publication = clipboard
        .publish_selection(
            ClipboardSelection::Primary,
            b"primary transcript",
            Instant::now() + Duration::from_secs(2),
        )
        .expect("primary selection owner and readback become ready");

    assert_eq!(fs::read(&state).unwrap(), b"primary transcript");
    let arguments = fs::read_to_string(log).unwrap();
    assert!(
        arguments
            .lines()
            .any(|line| line == "--primary --input --nodetach")
    );
    assert!(arguments.lines().any(|line| line == "--primary --output"));
    assert!(publication.owner.is_alive().expect("owner liveness"));
}
