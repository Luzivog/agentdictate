use crate::support;

use std::time::{Duration, Instant};

use agentdictate_linux::{
    command::{PlatformExecutable, PlatformTool, SystemCommandRunner},
    focus::X11FocusObserver,
    paste::{FocusTarget, X11FocusObservation, parse_x11_focus, resolve_focus_target},
};
use support::TestDirectory;

#[test]
fn stale_xwayland_window_is_not_mistaken_for_native_wayland_focus() {
    let stale_x11 = X11FocusObservation {
        window_id: "18874372".into(),
        window_class: "chatgpt Chatgpt".into(),
        focused: false,
    };

    assert_eq!(
        resolve_focus_target(true, Some(stale_x11)),
        FocusTarget::wayland()
    );
}

#[test]
fn xprop_focus_parser_keeps_window_identity_and_class() {
    let properties = concat!(
        "WM_CLASS(STRING) = \"chatgpt (/config/Codex)\", \"Chatgpt\"\n",
        "_NET_WM_STATE(ATOM) = _NET_WM_STATE_FOCUSED\n",
    );

    assert_eq!(
        parse_x11_focus("18874372", properties),
        X11FocusObservation {
            window_id: "18874372".into(),
            window_class: "chatgpt (/config/Codex) Chatgpt".into(),
            focused: true,
        }
    );
}

#[test]
fn x11_focus_observer_reads_the_active_window_and_its_properties() {
    let directory = TestDirectory::new();
    let xdotool = directory.executable("xdotool", "#!/bin/sh\nprintf '18874372\\n'\n");
    let xprop = directory.executable(
        "xprop",
        concat!(
            "#!/bin/sh\n",
            "printf '%s\\n' 'WM_CLASS(STRING) = \"chatgpt\", \"Chatgpt\"'\n",
            "printf '%s\\n' '_NET_WM_STATE(ATOM) = _NET_WM_STATE_FOCUSED'\n",
        ),
    );
    let observer = X11FocusObserver::new(
        SystemCommandRunner,
        PlatformExecutable::at(PlatformTool::Xdotool, xdotool),
        PlatformExecutable::at(PlatformTool::Xprop, xprop),
    );

    assert_eq!(
        observer
            .observe(Instant::now() + Duration::from_secs(2))
            .expect("focus is observable"),
        X11FocusObservation {
            window_id: "18874372".into(),
            window_class: "chatgpt Chatgpt".into(),
            focused: true,
        }
    );
}
