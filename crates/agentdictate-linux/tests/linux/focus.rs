use agentdictate_linux::{
    focus::focus_observation,
    paste::{FocusTarget, WindowProtocol, resolve_focus_target},
};

const FOCUSED: u32 = 401;
const MAXIMIZED: u32 = 402;

#[test]
fn window_class_joins_the_instance_and_class_names() {
    let observation = focus_observation(
        18_874_372,
        b"chatgpt (/config/Codex)\0Chatgpt\0",
        [MAXIMIZED, FOCUSED],
        FOCUSED,
    );

    assert_eq!(observation.window_id, 18_874_372);
    assert_eq!(observation.window_class, "chatgpt (/config/Codex) Chatgpt");
    assert!(observation.focused);
}

#[test]
fn x11_session_pastes_into_the_active_window_even_without_a_focus_state() {
    let observation = focus_observation(84, b"kitty\0kitty\0", [], FOCUSED);

    assert!(!observation.focused);
    assert_eq!(
        resolve_focus_target(false, Some(observation)),
        FocusTarget::x11(84, "kitty kitty")
    );
}

#[test]
fn wayland_session_trusts_only_an_xwayland_window_that_holds_focus() {
    let focused = focus_observation(42, b"code\0Code\0", [FOCUSED], FOCUSED);
    let stale = focus_observation(42, b"code\0Code\0", [MAXIMIZED], FOCUSED);

    assert_eq!(
        resolve_focus_target(true, Some(focused)).protocol(),
        WindowProtocol::X11
    );
    assert_eq!(
        resolve_focus_target(true, Some(stale)),
        FocusTarget::wayland()
    );
    assert_eq!(resolve_focus_target(true, None), FocusTarget::wayland());
}
