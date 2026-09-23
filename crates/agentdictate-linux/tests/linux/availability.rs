use std::time::{Duration, Instant};

use agentdictate_linux::{
    clipboard::CommandClipboard,
    command::{
        AvailabilityDiagnostic, PlatformCapability, PlatformCommandError, PlatformExecutable,
        PlatformTool, SystemCommandRunner,
    },
};

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(1)
}

#[test]
fn one_missing_xsel_binary_is_reported_once_even_when_it_serves_both_selections() {
    let clipboard = CommandClipboard::new(
        SystemCommandRunner,
        PlatformExecutable::missing(PlatformTool::Xsel),
    );

    let error = clipboard
        .publish(b"transcript", deadline())
        .expect_err("xsel is missing");

    assert!(matches!(
        error,
        PlatformCommandError::Unavailable(AvailabilityDiagnostic {
            capability: PlatformCapability::Clipboard,
            missing_tools,
        }) if missing_tools == [PlatformTool::Xsel]
    ));
}
