use futures::{StreamExt, channel::mpsc};
use gpui::{
    App, Bounds, Subscription, WindowBackgroundAppearance, WindowBounds, WindowDecorations,
    WindowKind, WindowOptions, point, prelude::*, px, size,
};
use gpui_component::{Root, TitleBar};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, mpsc::Receiver},
};

use crate::theme::gpui_color;
use crate::{
    OverlayPresentation, SettingsRequest, ShellViewModel, ThemeTokens, UiActionError,
    WorkspaceActionSink, WorkspaceViewModel,
};

mod history_action_lane;
mod history_page;
mod overlay_view;
mod overview;
mod row_actions;
mod settings_actions;
mod settings_form;
mod settings_page;
mod settings_shell;
mod shell_chrome;
mod shell_render;
pub(crate) mod single_line;
mod words_actions;
mod words_page;
mod workspace_actions;

use settings_form::SettingsForm;
use settings_shell::{RouteUiState, WorkspaceActionState};

pub use overlay_view::RecordingOverlay;

const SIDEBAR_WIDTH: f32 = 200.0;
const ROUTE_SCROLLBAR_WIDTH: f32 = 16.0;
pub const APPLICATION_ID: &str = "local.agentdictate.AgentDictate";

/// Sends one settings request to the daemon and returns the settings it then
/// holds. The window calls it off the UI thread, one request at a time.
pub type SettingsSink = Arc<
    dyn Fn(SettingsRequest) -> Result<agentdictate_core::SettingsSnapshot, UiActionError>
        + Send
        + Sync,
>;

/// Asks the daemon to capture the next shortcut pressed on any keyboard. It
/// blocks until the chord, Esc, a cancel or the daemon's timeout, so the
/// window calls it off the UI thread.
pub type HotkeyCaptureSink =
    Arc<dyn Fn() -> Result<agentdictate_core::HotkeyCaptureOutcome, UiActionError> + Send + Sync>;

/// Everything the settings window needs from the process that opens it.
pub struct SettingsWindow {
    pub model: ShellViewModel,
    pub settings: agentdictate_core::SettingsSnapshot,
    pub settings_sink: SettingsSink,
    pub hotkey_capture: HotkeyCaptureSink,
    pub action_sink: WorkspaceActionSink,
    /// A fresh workspace after each daemon database write, when watched.
    pub workspace_updates: Option<Receiver<WorkspaceViewModel>>,
    /// One message each time a later launch asks this window to come to the
    /// front instead of opening a second window.
    pub raise_requests: Option<Receiver<()>>,
}

/// Opens the settings window and runs until it closes.
pub fn run_settings_window(settings_window: SettingsWindow) {
    let SettingsWindow {
        model,
        settings,
        settings_sink,
        hotkey_capture,
        action_sink,
        workspace_updates,
        raise_requests,
    } = settings_window;
    gpui_platform::application()
        .with_assets(crate::AgentDictateAssets)
        .run(move |cx: &mut App| {
            crate::theme::initialize_gpui_theme(cx);
            let bounds = Bounds::centered(None, size(px(1180.), px(760.)), cx);
            let shell_slot = Rc::new(RefCell::new(None));
            let window_shell_slot = Rc::clone(&shell_slot);
            let window = cx
                .open_window(
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
                            SettingsShell::new(
                                model,
                                settings,
                                settings_sink,
                                hotkey_capture,
                                action_sink,
                                window,
                                cx,
                            )
                        });
                        *window_shell_slot.borrow_mut() = Some(view.clone());
                        let frame = cx.new(|_| crate::AgentDictateWindowFrame::new(view));
                        // AgentDictateWindowFrame owns the client-side frame and
                        // its resize zones; Root's own border would add a second.
                        cx.new(|cx| Root::new(frame, window, cx).bordered(false))
                    },
                )
                .expect("AgentDictate settings window should open");
            if let Some(workspace_updates) = workspace_updates {
                let shell = shell_slot
                    .borrow_mut()
                    .take()
                    .expect("settings shell should exist after its window opens")
                    .downgrade();
                forward_to_ui("agentdictate-workspace-updates", workspace_updates, cx, {
                    move |workspace, cx| {
                        shell.update(cx, |shell, cx| {
                            shell.apply_workspace_update(workspace, cx);
                        })
                    }
                });
            }
            if let Some(raise_requests) = raise_requests {
                forward_to_ui("agentdictate-window-raise", raise_requests, cx, {
                    move |(), cx| window.update(cx, |_, window, _| window.activate_window())
                });
            }
            cx.activate(true);
        });
}

/// Hands each message from `messages` to `apply` on the UI thread, until
/// either side closes or `apply` fails because the window is gone.
fn forward_to_ui<T: Send + 'static>(
    thread_name: &str,
    messages: Receiver<T>,
    cx: &mut App,
    mut apply: impl FnMut(T, &mut gpui::AsyncApp) -> gpui::Result<()> + 'static,
) {
    let (sender, mut receiver) = mpsc::unbounded();
    std::thread::Builder::new()
        .name(thread_name.to_owned())
        .spawn(move || {
            while let Ok(message) = messages.recv() {
                if sender.unbounded_send(message).is_err() {
                    return;
                }
            }
        })
        .expect("UI message bridge should start");
    cx.spawn(async move |cx| {
        while let Some(message) = receiver.next().await {
            if apply(message, cx).is_err() {
                return;
            }
        }
    })
    .detach();
}

/// Runs a focus-neutral X11 overlay. Placement completes before its first
/// animated frame; frame submission is acknowledged separately from creation.
#[doc(hidden)]
pub fn run_recording_overlay(
    initial: OverlayPresentation,
    snapshots: Receiver<OverlayPresentation>,
    on_created: impl FnOnce(u32, f32) + 'static,
    on_frame_submitted: impl FnOnce() + 'static,
) {
    // A fresh helper opens this popup for every dictation, so skip what it
    // never uses: AccessKit (the popup is invisible to assistive technology)
    // and gpui-component (it draws only GPUI primitives, no icons).
    gpui::Application::new_inaccessible(gpui_platform::current_platform(false)).run(
        move |cx: &mut App| {
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
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(
                        px(crate::OVERLAY_WIDTH as f32),
                        px(crate::OVERLAY_HEIGHT as f32),
                    ),
                ))),
                titlebar: None,
                focus: false,
                show: true,
                kind: WindowKind::PopUp,
                is_movable: false,
                app_owns_titlebar_drag: false,
                // An override-redirect popup is never the active window, so
                // the default inactive throttle would cap the waveform and
                // fades at 30 fps.
                inactive_frame_interval: None,
                is_resizable: false,
                is_minimizable: false,
                display_id: None,
                window_background: WindowBackgroundAppearance::Transparent,
                // PopUp maps to _NET_WM_WINDOW_TYPE_NOTIFICATION on X11. Sharing
                // the main application id also prevents a second app identity.
                app_id: Some(APPLICATION_ID.to_owned()),
                window_min_size: None,
                window_decorations: None,
                icon: None,
                tabbing_identifier: None,
            };
            let overlay_window = cx
                .open_window(options, move |window, cx| {
                    let handle = HasWindowHandle::window_handle(window)
                        .expect("overlay native window handle should exist");
                    let id = match handle.as_raw() {
                        RawWindowHandle::Xcb(handle) => handle.window.get(),
                        RawWindowHandle::Xlib(handle) => {
                            u32::try_from(handle.window).expect("X11 window id fits in u32")
                        }
                        _ => panic!("focus-neutral recording overlay requires X11 or XWayland"),
                    };
                    on_created(id, window.scale_factor());
                    cx.new(|_| {
                        RecordingOverlay::from_presentation(initial)
                            .on_frame_submitted(on_frame_submitted)
                    })
                })
                .expect("recording overlay should open");

            cx.spawn(async move |cx| {
                while let Some(presentation) = receiver.next().await {
                    if !presentation.state().is_visible() {
                        break;
                    }
                    let _ = overlay_window.update(cx, |overlay, _, cx| {
                        overlay.set_presentation(presentation);
                        cx.notify();
                    });
                }
                // Fade before destroying: an instant destroy of the dark card
                // beside the taskbar reads as a flash at paste time. A
                // dictation restarted within the hold spawns a fresh helper
                // while this one finishes fading — a sub-150ms overlap of a
                // mostly transparent card.
                let _ = overlay_window.update(cx, |overlay, _, cx| {
                    overlay.begin_dismissal(cx.background_executor().now());
                    cx.notify();
                });
                cx.background_executor()
                    .timer(crate::OVERLAY_FADE_HOLD)
                    .await;
                let _ = overlay_window.update(cx, |_, window, _| window.remove_window());
                cx.update(|cx| cx.quit())
            })
            .detach();
        },
    );
}

/// GPUI settings shell built from the toolkit-independent presentation model.
///
/// The shell owns navigation interactions while individual routes remain free
/// to supply their own content components as the migration proceeds.
pub struct SettingsShell {
    model: ShellViewModel,
    theme: ThemeTokens,
    settings: SettingsForm,
    workspace_actions: WorkspaceActionState,
    routes: RouteUiState,
    _subscriptions: Vec<Subscription>,
}
