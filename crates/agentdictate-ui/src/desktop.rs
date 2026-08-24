use futures::{StreamExt, channel::mpsc};
use gpui::{
    App, Application, Bounds, Hsla, Subscription, WindowBackgroundAppearance, WindowBounds,
    WindowDecorations, WindowKind, WindowOptions, point, prelude::*, px, rgb, size,
};
use gpui_component::{Root, TitleBar};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, mpsc::Receiver},
};

use crate::{
    Color, OverlayPresentation, ShellViewModel, ThemeTokens, UiActionError, WorkspaceActionSink,
    WorkspaceViewModel,
};

mod history_action_lane;
mod history_page;
mod overlay_view;
mod overview;
mod replacements_page;
mod settings_actions;
mod settings_form;
mod settings_page;
mod settings_shell;
mod shell_chrome;
mod shell_render;
pub(crate) mod single_line;
mod workspace_actions;

use settings_shell::{
    RouteUiState, SettingsCommandState, SettingsEditState, ShellLayoutState, WorkspaceActionState,
};

pub use overlay_view::RecordingOverlay;

const SIDEBAR_WIDTH: f32 = 250.0;
const ROUTE_SCROLLBAR_WIDTH: f32 = 16.0;
pub const APPLICATION_ID: &str = "local.agentdictate.AgentDictate";

/// Starts the native GPUI settings window from a daemon snapshot.
pub type CommandSink =
    Arc<dyn Fn(agentdictate_core::ClientCommand) -> Result<(), UiActionError> + Send + Sync>;

/// Starts the settings window with connected workspace actions.
pub fn run_settings_shell_with_workspace_actions(
    model: ShellViewModel,
    settings: agentdictate_core::Settings,
    has_api_key: bool,
    command_sink: CommandSink,
    action_sink: WorkspaceActionSink,
) {
    run_settings_shell_internal(
        model,
        settings,
        has_api_key,
        command_sink,
        Some(action_sink),
        None,
    );
}

/// Starts the settings window with actions and event-driven daemon workspace
/// updates. Existing entrypoints remain available for callers that provide a
/// fixed startup snapshot.
pub fn run_settings_shell_with_workspace_actions_and_updates(
    model: ShellViewModel,
    settings: agentdictate_core::Settings,
    has_api_key: bool,
    command_sink: CommandSink,
    action_sink: WorkspaceActionSink,
    updates: Receiver<WorkspaceViewModel>,
) {
    run_settings_shell_internal(
        model,
        settings,
        has_api_key,
        command_sink,
        Some(action_sink),
        Some(updates),
    );
}

fn run_settings_shell_internal(
    model: ShellViewModel,
    settings: agentdictate_core::Settings,
    has_api_key: bool,
    command_sink: CommandSink,
    action_sink: Option<WorkspaceActionSink>,
    workspace_updates: Option<Receiver<WorkspaceViewModel>>,
) {
    Application::new()
        .with_assets(crate::AgentDictateAssets)
        .run(move |cx: &mut App| {
            crate::theme::initialize_gpui_theme(cx);
            let bounds = Bounds::centered(None, size(px(1180.), px(760.)), cx);
            let shell_slot = Rc::new(RefCell::new(None));
            let window_shell_slot = Rc::clone(&shell_slot);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitleBar::title_bar_options()),
                    window_background: WindowBackgroundAppearance::Opaque,
                    window_decorations: Some(WindowDecorations::Client),
                    app_id: Some(APPLICATION_ID.to_owned()),
                    window_min_size: Some(size(px(720.), px(480.))),
                    ..Default::default()
                },
                move |window, cx| {
                    let view = cx.new(|cx| {
                        SettingsShell::connected_internal(
                            model,
                            settings,
                            has_api_key,
                            command_sink,
                            action_sink,
                            window,
                            cx,
                        )
                    });
                    *window_shell_slot.borrow_mut() = Some(view.clone());
                    let frame = cx.new(|_| crate::AgentDictateWindowFrame::new(view));
                    cx.new(|cx| Root::new(frame, window, cx))
                },
            )
            .expect("AgentDictate settings window should open");
            if let Some(workspace_updates) = workspace_updates {
                let shell = shell_slot
                    .borrow_mut()
                    .take()
                    .expect("settings shell should exist after its window opens")
                    .downgrade();
                let (sender, mut receiver) = mpsc::unbounded();
                std::thread::Builder::new()
                    .name("agentdictate-workspace-updates".into())
                    .spawn(move || {
                        while let Ok(workspace) = workspace_updates.recv() {
                            if sender.unbounded_send(workspace).is_err() {
                                return;
                            }
                        }
                    })
                    .expect("workspace update bridge should start");
                cx.spawn(async move |cx| {
                    while let Some(workspace) = receiver.next().await {
                        if shell
                            .update(cx, |shell, cx| {
                                shell.apply_workspace_update(workspace, cx);
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                })
                .detach();
            }
            cx.activate(true);
        });
}

/// Runs an overlay session and reports when GPUI has created its platform
/// window. The daemon helper uses this to distinguish spawn from readiness.
#[doc(hidden)]
pub fn run_recording_overlay_with_ready(
    initial: OverlayPresentation,
    snapshots: Receiver<OverlayPresentation>,
    work_area: Option<crate::LogicalRect>,
    on_ready: impl FnOnce() + 'static,
) {
    Application::new()
        .with_assets(crate::AgentDictateAssets)
        .run(move |cx: &mut App| {
            crate::theme::initialize_gpui_theme(cx);
            let display = cx.primary_display();
            let display_id = display.as_ref().map(|display| display.id());
            let primary_bounds = display.as_ref().map(|display| {
                let bounds = display.bounds();
                crate::LogicalRect::new(
                    f32::from(bounds.origin.x).round() as i32,
                    f32::from(bounds.origin.y).round() as i32,
                    f32::from(bounds.size.width).max(0.0).round() as u32,
                    f32::from(bounds.size.height).max(0.0).round() as u32,
                )
            });
            let available = match (work_area, primary_bounds) {
                (Some(work_area), Some(primary_bounds)) => {
                    crate::intersect_logical_rects(work_area, primary_bounds)
                        .unwrap_or(primary_bounds)
                }
                (Some(work_area), None) => work_area,
                (None, Some(primary_bounds)) => primary_bounds,
                (None, None) => crate::LogicalRect::new(0, 0, 0, 0),
            };
            let (sender, mut receiver) = mpsc::unbounded();
            std::thread::Builder::new()
                .name("agentdictate-overlay-events".into())
                .spawn(move || {
                    while let Ok(snapshot) = snapshots.recv() {
                        if sender.unbounded_send(snapshot).is_err() {
                            return;
                        }
                    }
                })
                .expect("overlay event bridge should start");

            let initial_state = initial.state();
            if !initial_state.is_visible() {
                cx.quit();
                return;
            }
            let placement = crate::OverlayPlacement::bottom_centered(
                available,
                crate::LogicalSize::new(crate::OVERLAY_WIDTH, crate::OVERLAY_HEIGHT),
                crate::OVERLAY_BOTTOM_GAP,
            );
            let frame = placement.frame;
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(frame.x as f32), px(frame.y as f32)),
                    size(px(frame.width as f32), px(frame.height as f32)),
                ))),
                titlebar: None,
                focus: false,
                show: true,
                kind: WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                display_id,
                window_background: WindowBackgroundAppearance::Transparent,
                // PopUp maps to _NET_WM_WINDOW_TYPE_NOTIFICATION on X11. Sharing
                // the main application id also prevents a second app identity.
                app_id: Some(APPLICATION_ID.to_owned()),
                window_min_size: None,
                window_decorations: None,
                tabbing_identifier: None,
            };
            let overlay_window = cx
                .open_window(options, move |_, cx| {
                    cx.new(|_| RecordingOverlay::from_presentation(initial))
                })
                .expect("recording overlay should open");
            on_ready();

            cx.spawn(async move |cx| {
                while let Some(presentation) = receiver.next().await {
                    let state = presentation.state();
                    if !state.is_visible() {
                        let _ = overlay_window.update(cx, |_, window, _| window.remove_window());
                        return cx.update(|cx| cx.quit());
                    }
                    let _ = overlay_window.update(cx, |overlay, _, cx| {
                        overlay.set_presentation(presentation);
                        cx.notify();
                    });
                }
                let _ = overlay_window.update(cx, |_, window, _| window.remove_window());
                cx.update(|cx| cx.quit())
            })
            .detach();
        });
}

/// GPUI settings shell built from the toolkit-independent presentation model.
///
/// The shell owns navigation interactions while individual routes remain free
/// to supply their own content components as the migration proceeds.
pub struct SettingsShell {
    model: ShellViewModel,
    theme: ThemeTokens,
    settings: SettingsEditState,
    settings_commands: SettingsCommandState,
    workspace_actions: WorkspaceActionState,
    routes: RouteUiState,
    layout: ShellLayoutState,
    _subscriptions: Vec<Subscription>,
}

const fn enabled_label(enabled: bool) -> &'static str {
    if enabled { "On" } else { "Off" }
}

fn gpui_color(color: Color) -> Hsla {
    rgb((u32::from(color.red) << 16) | (u32::from(color.green) << 8) | u32::from(color.blue)).into()
}
