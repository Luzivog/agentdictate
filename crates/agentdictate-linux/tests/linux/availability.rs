use std::time::{Duration, Instant};

use agentdictate_linux::{
    clipboard::CommandClipboard,
    command::{
        AvailabilityDiagnostic, PlatformCapability, PlatformCommandError, PlatformExecutable,
        PlatformTool, SystemCommandRunner,
    },
    focus::X11FocusObserver,
    injection::PasteInjector,
    paste::{ClipboardProtocol, PasteShortcut},
};

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(1)
}

#[test]
fn focus_reports_every_missing_tool_as_typed_availability_data() {
    let observer = X11FocusObserver::new(
        SystemCommandRunner,
        PlatformExecutable::missing(PlatformTool::Xdotool),
        PlatformExecutable::missing(PlatformTool::Xprop),
    );

    let error = observer.observe(deadline()).expect_err("tools are missing");

    assert!(matches!(
        error,
        PlatformCommandError::Unavailable(AvailabilityDiagnostic {
            capability: PlatformCapability::FocusObservation,
            missing_tools,
        }) if missing_tools == [PlatformTool::Xdotool, PlatformTool::Xprop]
    ));
}

#[test]
fn clipboard_and_injection_report_the_protocol_specific_capability() {
    let clipboard = CommandClipboard::new(
        SystemCommandRunner,
        PlatformExecutable::missing(PlatformTool::WlCopy),
        PlatformExecutable::missing(PlatformTool::WlPaste),
        PlatformExecutable::missing(PlatformTool::Xsel),
    );
    let injector = PasteInjector::new(
        SystemCommandRunner,
        PlatformExecutable::missing(PlatformTool::Ydotool),
        PlatformExecutable::missing(PlatformTool::Xdotool),
    );

    let clipboard_error = clipboard
        .publish(ClipboardProtocol::Wayland, b"transcript", deadline())
        .expect_err("Wayland clipboard tools are missing");
    let injection_error = injector
        .inject(
            ClipboardProtocol::Wayland,
            PasteShortcut::Standard,
            deadline(),
        )
        .expect_err("Wayland injection tool is missing");

    assert!(matches!(
        clipboard_error,
        PlatformCommandError::Unavailable(AvailabilityDiagnostic {
            capability: PlatformCapability::WaylandClipboard,
            missing_tools,
        }) if missing_tools == [PlatformTool::WlCopy, PlatformTool::WlPaste]
    ));
    assert!(matches!(
        injection_error,
        PlatformCommandError::Unavailable(AvailabilityDiagnostic {
            capability: PlatformCapability::WaylandPasteInjection,
            missing_tools,
        }) if missing_tools == [PlatformTool::Ydotool]
    ));
}

#[test]
fn one_missing_xsel_binary_is_reported_once_even_when_it_serves_both_roles() {
    let clipboard = CommandClipboard::new(
        SystemCommandRunner,
        PlatformExecutable::missing(PlatformTool::WlCopy),
        PlatformExecutable::missing(PlatformTool::WlPaste),
        PlatformExecutable::missing(PlatformTool::Xsel),
    );

    let error = clipboard
        .publish(ClipboardProtocol::X11, b"transcript", deadline())
        .expect_err("xsel is missing");

    assert!(matches!(
        error,
        PlatformCommandError::Unavailable(AvailabilityDiagnostic {
            capability: PlatformCapability::X11Clipboard,
            missing_tools,
        }) if missing_tools == [PlatformTool::Xsel]
    ));
}
