use crate::support;

use std::{
    fs,
    time::{Duration, Instant},
};

use agentdictate_linux::{
    command::{PlatformCommandError, PlatformExecutable, PlatformTool, SystemCommandRunner},
    injection::PasteInjector,
    paste::{ClipboardProtocol, PasteShortcut},
};
use support::TestDirectory;

#[test]
fn wayland_injection_paces_the_chord_for_busy_event_loops() {
    let directory = TestDirectory::new();
    let log = directory.path().join("ydotool.log");
    let ydotool = directory.executable(
        "ydotool",
        &format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display()),
    );
    let injector = PasteInjector::new(
        SystemCommandRunner,
        PlatformExecutable::at(PlatformTool::Ydotool, ydotool),
        PlatformExecutable::missing(PlatformTool::Xdotool),
    );

    let receipt = injector
        .inject(
            ClipboardProtocol::Wayland,
            PasteShortcut::Standard,
            Instant::now() + Duration::from_secs(2),
        )
        .expect("one paste chord is sent");

    assert_eq!(receipt.tool, PlatformTool::Ydotool);
    assert_eq!(
        fs::read_to_string(log).expect("invocation log"),
        "key --delay 50 --key-delay 25 ctrl+v\n"
    );
}

#[test]
fn wayland_universal_injection_sends_one_paced_shift_insert_chord() {
    let directory = TestDirectory::new();
    let log = directory.path().join("ydotool.log");
    let ydotool = directory.executable(
        "ydotool",
        &format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display()),
    );
    let injector = PasteInjector::new(
        SystemCommandRunner,
        PlatformExecutable::at(PlatformTool::Ydotool, ydotool),
        PlatformExecutable::missing(PlatformTool::Xdotool),
    );

    let receipt = injector
        .inject(
            ClipboardProtocol::Wayland,
            PasteShortcut::Universal,
            Instant::now() + Duration::from_secs(2),
        )
        .expect("one universal paste chord is sent");

    assert_eq!(receipt.tool, PlatformTool::Ydotool);
    assert_eq!(
        fs::read_to_string(log).expect("invocation log"),
        "key --delay 50 --key-delay 25 shift+insert\n"
    );
}

#[test]
fn x11_injection_paces_the_chord_for_busy_event_loops() {
    let directory = TestDirectory::new();
    let log = directory.path().join("xdotool.log");
    let xdotool = directory.executable(
        "xdotool",
        &format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display()),
    );
    let injector = PasteInjector::new(
        SystemCommandRunner,
        PlatformExecutable::missing(PlatformTool::Ydotool),
        PlatformExecutable::at(PlatformTool::Xdotool, xdotool),
    );

    let receipt = injector
        .inject(
            ClipboardProtocol::X11,
            PasteShortcut::Standard,
            Instant::now() + Duration::from_secs(2),
        )
        .expect("one paste chord is sent");

    assert_eq!(receipt.tool, PlatformTool::Xdotool);
    assert_eq!(
        fs::read_to_string(log).expect("invocation log"),
        "key --clearmodifiers --delay 50 ctrl+v\n"
    );
}

#[test]
fn failed_x11_injection_is_reported_after_exactly_one_xdotool_attempt() {
    let directory = TestDirectory::new();
    let log = directory.path().join("xdotool.log");
    let xdotool = directory.executable(
        "xdotool",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 9\n",
            log.display()
        ),
    );
    let injector = PasteInjector::new(
        SystemCommandRunner,
        PlatformExecutable::missing(PlatformTool::Ydotool),
        PlatformExecutable::at(PlatformTool::Xdotool, xdotool),
    );

    let error = injector
        .inject(
            ClipboardProtocol::X11,
            PasteShortcut::Terminal,
            Instant::now() + Duration::from_secs(2),
        )
        .expect_err("ambiguous command failure is surfaced without retry");

    assert!(matches!(
        error,
        PlatformCommandError::Failed {
            tool: PlatformTool::Xdotool,
            code: Some(9),
            ..
        }
    ));
    assert_eq!(
        fs::read_to_string(log).expect("invocation log"),
        "key --clearmodifiers --delay 50 ctrl+shift+v\n"
    );
}
