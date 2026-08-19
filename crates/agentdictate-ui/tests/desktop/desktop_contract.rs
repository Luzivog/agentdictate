#![cfg(feature = "desktop")]

//! Desktop type contracts.

use agentdictate_core::{WorkflowPhase, WorkflowSnapshot};
use agentdictate_ui::{OverlayState, RecordingOverlay, Route, SettingsShell, ShellViewModel};
#[test]
fn gpui_settings_shell_owns_the_typed_view_model_and_is_renderable() {
    fn assert_gpui_render<T: gpui::Render>() {}

    let shell = SettingsShell::new(ShellViewModel::from_snapshot(
        Route::Settings,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    ));

    assert_eq!(shell.active_route(), Route::Settings);
    assert_gpui_render::<SettingsShell>();

    let overlay = RecordingOverlay::new(OverlayState::Recording);
    assert_eq!(overlay.state(), &OverlayState::Recording);
    assert_gpui_render::<RecordingOverlay>();
}
