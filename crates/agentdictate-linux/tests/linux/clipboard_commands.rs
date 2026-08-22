use crate::support;

use std::{
    fs,
    time::{Duration, Instant},
};

use agentdictate_linux::{
    clipboard::{ClipboardSelection, CommandClipboard},
    command::{PlatformExecutable, PlatformTool, SystemCommandRunner},
    paste::{ClipboardProtocol, ClipboardReadinessEvidence},
};
use support::TestDirectory;

#[test]
fn wayland_clipboard_is_ready_only_after_live_owner_readback_matches() {
    let directory = TestDirectory::new();
    let state = directory.path().join("clipboard.txt");
    let wl_copy = directory.executable(
        "wl-copy",
        &format!(
            "#!/bin/sh\ncat > '{}'\nexec tail -f /dev/null\n",
            state.display()
        ),
    );
    let wl_paste = directory.executable(
        "wl-paste",
        &format!("#!/bin/sh\ncat '{}'\n", state.display()),
    );
    let clipboard = CommandClipboard::new(
        SystemCommandRunner,
        PlatformExecutable::at(PlatformTool::WlCopy, wl_copy),
        PlatformExecutable::at(PlatformTool::WlPaste, wl_paste),
        PlatformExecutable::missing(PlatformTool::Xsel),
    );

    let mut publication = clipboard
        .publish(
            ClipboardProtocol::Wayland,
            b"hello from AgentDictate",
            Instant::now() + Duration::from_secs(2),
        )
        .expect("clipboard owner and readback become ready");

    assert_eq!(
        publication.evidence,
        ClipboardReadinessEvidence::ReadbackMatches
    );
    assert!(publication.owner.is_alive().expect("owner liveness"));
}

#[test]
fn wayland_primary_selection_is_published_and_verified_independently() {
    let directory = TestDirectory::new();
    let state = directory.path().join("primary.txt");
    let owner_arguments = directory.path().join("wl-copy-arguments.txt");
    let reader_arguments = directory.path().join("wl-paste-arguments.txt");
    let wl_copy = directory.executable(
        "wl-copy",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\ncat > '{}'\nexec tail -f /dev/null\n",
            owner_arguments.display(),
            state.display(),
        ),
    );
    let wl_paste = directory.executable(
        "wl-paste",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\ncat '{}'\n",
            reader_arguments.display(),
            state.display(),
        ),
    );
    let clipboard = CommandClipboard::new(
        SystemCommandRunner,
        PlatformExecutable::at(PlatformTool::WlCopy, wl_copy),
        PlatformExecutable::at(PlatformTool::WlPaste, wl_paste),
        PlatformExecutable::missing(PlatformTool::Xsel),
    );

    let mut publication = clipboard
        .publish_selection(
            ClipboardProtocol::Wayland,
            ClipboardSelection::Primary,
            b"primary transcript",
            Instant::now() + Duration::from_secs(2),
        )
        .expect("primary selection owner and readback become ready");

    assert_eq!(fs::read(&state).unwrap(), b"primary transcript");
    assert_eq!(
        fs::read_to_string(owner_arguments).unwrap(),
        "--foreground --primary\n"
    );
    assert_eq!(
        fs::read_to_string(reader_arguments).unwrap(),
        "--no-newline --primary\n"
    );
    assert!(publication.owner.is_alive().expect("owner liveness"));
}

#[test]
fn x11_clipboard_uses_a_live_non_detaching_xsel_owner_and_readback() {
    let directory = TestDirectory::new();
    let state = directory.path().join("x11-clipboard.txt");
    let xsel = directory.executable(
        "xsel",
        &format!(
            concat!(
                "#!/bin/sh\n",
                "case \"$*\" in\n",
                "  *--output*) cat '{}' ;;\n",
                "  *) cat > '{}'; exec tail -f /dev/null ;;\n",
                "esac\n",
            ),
            state.display(),
            state.display(),
        ),
    );
    let clipboard = CommandClipboard::new(
        SystemCommandRunner,
        PlatformExecutable::missing(PlatformTool::WlCopy),
        PlatformExecutable::missing(PlatformTool::WlPaste),
        PlatformExecutable::at(PlatformTool::Xsel, xsel),
    );

    let mut publication = clipboard
        .publish(
            ClipboardProtocol::X11,
            b"x11 transcript",
            Instant::now() + Duration::from_secs(2),
        )
        .expect("xsel owns readable clipboard content");

    assert_eq!(
        publication.evidence,
        ClipboardReadinessEvidence::ReadbackMatches
    );
    assert!(publication.owner.is_alive().expect("owner liveness"));
}
