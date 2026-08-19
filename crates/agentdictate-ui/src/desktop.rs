use futures::{StreamExt, channel::mpsc};
use gpui::{
    App, Application, Bounds, Context, Entity, Hsla, IntoElement, MouseButton, Render,
    ScrollHandle, SharedString, Subscription, Window, WindowBackgroundAppearance, WindowBounds,
    WindowControlArea, WindowDecorations, WindowKind, WindowOptions, point, prelude::*, px, rgb,
    size,
};
use gpui_component::{
    Disableable, IndexPath, Root, Selectable, Sizable, TitleBar,
    button::{ButtonCustomVariant, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState, NumberInput, NumberInputEvent, StepAction},
    scroll::Scrollbar,
    select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState},
    v_flex,
};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, mpsc::Receiver},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::action::action_button;
use crate::{
    Color, ModelCatalogViewModel, NavigationItemViewModel, OverlayPresentation, OverlayState,
    ReplacementDraft, Route, SettingsDraft, ShellViewModel, ThemeTokens, WaveformFrame,
    WorkspaceAction, WorkspaceActionSink, WorkspaceViewModel,
};
use crate::{sidebar_motion::SidebarMotion, sidebar_open_for_layout};

mod history_action_lane;
mod history_page;
mod overview;
mod replacements_page;
mod settings_page;
pub(crate) mod single_line;

const SIDEBAR_WIDTH: f32 = 250.0;
const SIDEBAR_OVERLAY_BREAKPOINT: f32 = 1_100.0;
const ROUTE_SCROLLBAR_WIDTH: f32 = 16.0;
pub const APPLICATION_ID: &str = "local.agentdictate.AgentDictate";

/// Starts the native GPUI settings window from a daemon snapshot.
pub type CommandSink =
    Arc<dyn Fn(agentdictate_core::ClientCommand) -> Result<(), String> + Send + Sync>;

pub fn run_settings_shell(
    model: ShellViewModel,
    settings: agentdictate_core::Settings,
    has_api_key: bool,
    command_sink: CommandSink,
) {
    run_settings_shell_internal(model, settings, has_api_key, command_sink, None, None);
}

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

/// Runs one transient status-overlay session. The headless daemon launches this
/// in a helper process only after a visible workflow state exists.
pub fn run_recording_overlay(
    initial: OverlayPresentation,
    snapshots: Receiver<OverlayPresentation>,
    work_area: Option<crate::LogicalRect>,
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

/// GPUI content for the bottom-centered recording status window.
pub struct RecordingOverlay {
    state: OverlayState,
    active_recording: Option<crate::ActiveRecordingPresentation>,
    waveform: WaveformFrame,
    last_sample_at: Option<Instant>,
}

impl RecordingOverlay {
    pub fn new(state: OverlayState) -> Self {
        Self {
            state,
            active_recording: None,
            waveform: WaveformFrame::default(),
            last_sample_at: None,
        }
    }

    pub fn from_presentation(presentation: OverlayPresentation) -> Self {
        Self {
            state: presentation.state(),
            active_recording: presentation.active_recording,
            waveform: WaveformFrame::default(),
            last_sample_at: None,
        }
    }

    pub fn with_theme(state: OverlayState, _theme: ThemeTokens) -> Self {
        Self::new(state)
    }

    pub const fn state(&self) -> &OverlayState {
        &self.state
    }

    pub fn set_state(&mut self, state: OverlayState) {
        if self.state != state {
            self.waveform.reset();
            self.last_sample_at = None;
        }
        self.state = state;
        self.active_recording = None;
    }

    pub fn set_presentation(&mut self, presentation: OverlayPresentation) {
        let state = presentation.state();
        let recording_changed = self
            .active_recording
            .as_ref()
            .map(|recording| &recording.audio_path)
            != presentation
                .active_recording
                .as_ref()
                .map(|recording| &recording.audio_path);
        if self.state != state || recording_changed {
            self.waveform.reset();
            self.last_sample_at = None;
        }
        self.state = state;
        self.active_recording = presentation.active_recording;
    }
}

impl Render for RecordingOverlay {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let label = self.state.label().to_owned();
        let stable_id = self.state.stable_id().to_owned();
        let recording = self.state == OverlayState::Recording;
        if recording {
            window.request_animation_frame();
            let now = Instant::now();
            if self.last_sample_at.is_none_or(|sampled| {
                now.saturating_duration_since(sampled) >= Duration::from_millis(33)
            }) {
                if let Some(active) = &self.active_recording {
                    self.waveform
                        .advance(&crate::sample_recent_wav(&active.audio_path));
                }
                self.last_sample_at = Some(now);
            }
        }

        let elapsed = self.active_recording.as_ref().map_or(0.0, |active| {
            let now_millis = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(i64::MAX as u128) as i64;
            crate::elapsed_seconds(active.started_at_unix_millis, now_millis)
        });
        let timer = crate::format_elapsed(elapsed);
        let timer_color: Hsla = gpui::rgba(0xf5f5f5f0).into();
        let mut timer_style = window.text_style().highlight(gpui::FontWeight::BOLD);
        timer_style.font_family = "Sans".into();
        let timer_run = gpui::TextRun {
            len: timer.len(),
            font: timer_style.font(),
            color: timer_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let timer_width = f32::from(
            window
                .text_system()
                .layout_line(&timer, px(13.), &[timer_run], None)
                .width,
        );
        let recording_layout = crate::recording_overlay_layout(timer_width);
        let bars = crate::waveform_bars(self.waveform.levels(), recording_layout.waveform);
        let waveform_color: Hsla = gpui::rgb(0xf04a1f).into();

        gpui::div()
            .debug_selector(move || stable_id)
            .relative()
            .size_full()
            .when(self.state.is_visible(), |root| {
                root.child(
                    gpui::div()
                        .absolute()
                        .left(px(6.))
                        .top(px(8.))
                        .w(px(127.))
                        .h(px(42.))
                        .rounded(px(14.))
                        .bg(gpui::rgba(0x0000003d)),
                )
                .child(
                    gpui::div()
                        .debug_selector(|| "recording-overlay-card".to_owned())
                        .absolute()
                        .left(px(6.))
                        .top(px(6.))
                        .w(px(127.))
                        .h(px(42.))
                        .relative()
                        .overflow_hidden()
                        .rounded(px(14.))
                        .border_1()
                        .border_color(gpui::rgba(0xffffff1c))
                        .bg(gpui::rgba(0x111112f2))
                        .when(recording, |card| {
                            card.children(bars.into_iter().enumerate().map(|(index, bar)| {
                                gpui::div()
                                    .debug_selector(move || {
                                        format!("recording-overlay-wave-{index}")
                                    })
                                    .absolute()
                                    .left(px(bar.x))
                                    .top(px(bar.center_y - bar.height / 2.0))
                                    .w(px(bar.width))
                                    .h(px(bar.height))
                                    .rounded_full()
                                    .bg(waveform_color.opacity(bar.alpha))
                            }))
                            .child(
                                gpui::div()
                                    .debug_selector(|| "recording-overlay-timer".to_owned())
                                    .absolute()
                                    .left(px(recording_layout.timer_x))
                                    .top_0()
                                    .w(px(recording_layout.timer_width))
                                    .h_full()
                                    .flex()
                                    .items_center()
                                    .text_size(px(13.))
                                    .font_family("Sans")
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(timer_color)
                                    .child(timer),
                            )
                        })
                        .when(!recording, |card| {
                            card.child(
                                gpui::div()
                                    .size_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .px_3()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(gpui::rgba(0xf5f5f5f5))
                                    .child(label),
                            )
                        }),
                )
            })
    }
}

/// GPUI settings shell built from the toolkit-independent presentation model.
///
/// The shell owns navigation interactions while individual routes remain free
/// to supply their own content components as the migration proceeds.
pub struct SettingsShell {
    model: ShellViewModel,
    theme: ThemeTokens,
    settings: agentdictate_core::Settings,
    settings_baseline: agentdictate_core::Settings,
    has_api_key: bool,
    api_key_input: Option<Entity<InputState>>,
    history_search_input: Option<Entity<InputState>>,
    api_key_feedback: Option<String>,
    command_sink: Option<CommandSink>,
    workspace_action_sink: Option<WorkspaceActionSink>,
    next_request_id: u64,
    route_feedbacks: RouteFeedbacks,
    settings_editor: Option<SettingsEditorState>,
    applied_model_catalog: ModelCatalogViewModel,
    settings_dirty: bool,
    shortcut_capture_active: bool,
    shortcut_capture_error: Option<String>,
    _settings_input_subscriptions: Vec<Subscription>,
    replacement_editor: Option<ReplacementEditorState>,
    pending_destructive_action: Option<WorkspaceAction>,
    workspace_action_in_flight: bool,
    history_action_lane: history_action_lane::HistoryActionLane,
    sidebar_open: bool,
    compact_layout: Option<bool>,
    sidebar_motion: SidebarMotion,
    route_scroll_handles: RouteScrollHandles,
}

#[derive(Clone, Debug, Default)]
struct RouteScrollHandles {
    overview: ScrollHandle,
    history: ScrollHandle,
    replacements: ScrollHandle,
    settings: ScrollHandle,
}

#[derive(Clone, Debug, Default)]
struct RouteFeedbacks {
    overview: Option<String>,
    history: Option<String>,
    replacements: Option<String>,
    settings: Option<String>,
}

impl RouteFeedbacks {
    fn for_route(&self, route: Route) -> &Option<String> {
        match route {
            Route::Overview => &self.overview,
            Route::History => &self.history,
            Route::Replacements => &self.replacements,
            Route::Settings => &self.settings,
        }
    }

    fn for_route_mut(&mut self, route: Route) -> &mut Option<String> {
        match route {
            Route::Overview => &mut self.overview,
            Route::History => &mut self.history,
            Route::Replacements => &mut self.replacements,
            Route::Settings => &mut self.settings,
        }
    }
}

impl RouteScrollHandles {
    fn for_route(&self, route: Route) -> ScrollHandle {
        match route {
            Route::Overview => self.overview.clone(),
            Route::History => self.history.clone(),
            Route::Replacements => self.replacements.clone(),
            Route::Settings => self.settings.clone(),
        }
    }
}

#[derive(Clone)]
struct ReplacementEditorState {
    id: Option<i64>,
    source: Entity<InputState>,
    replacement: Entity<InputState>,
    enabled: bool,
    case_sensitive: bool,
    whole_word_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SettingOption {
    label: SharedString,
    value: String,
}

impl SettingOption {
    fn new(label: impl Into<SharedString>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

impl SelectItem for SettingOption {
    type Value = String;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.value
    }
}

type SettingSelectState = SelectState<SearchableVec<SettingOption>>;

#[derive(Clone)]
struct SettingsEditorState {
    transcription_model: Entity<SettingSelectState>,
    custom_transcription_model: Entity<InputState>,
    language: Entity<SettingSelectState>,
    transcription_prompt: Entity<InputState>,
    cleanup_model: Entity<SettingSelectState>,
    custom_cleanup_model: Entity<InputState>,
    cleanup_reasoning_effort: Entity<SettingSelectState>,
    cleanup_style: Entity<SettingSelectState>,
    cleanup_prompt: Entity<InputState>,
    recording_mode: Entity<SettingSelectState>,
    max_recording_seconds: Entity<InputState>,
    audio_ducking_volume_percent: Entity<InputState>,
    paste_shortcut: Entity<SettingSelectState>,
}

impl SettingsEditorState {
    fn new(
        settings: &agentdictate_core::Settings,
        model_catalog: &ModelCatalogViewModel,
        window: &mut Window,
        cx: &mut Context<SettingsShell>,
    ) -> Self {
        let draft = SettingsDraft::from(settings);
        let cleanup_model = settings.active_cleanup_model();
        let cleanup_reasoning_effort = model_catalog
            .normalized_reasoning_effort(cleanup_model, &draft.cleanup_reasoning_effort);
        Self {
            transcription_model: settings_select(
                transcription_model_options(model_catalog),
                &draft.transcription_model,
                true,
                window,
                cx,
            ),
            custom_transcription_model: settings_input(
                draft.custom_transcription_model,
                "Custom OpenAI model",
                window,
                cx,
            ),
            language: settings_select(language_options(), &draft.language, true, window, cx),
            transcription_prompt: settings_text_area(
                draft.transcription_prompt,
                "Names and technical context",
                2,
                5,
                window,
                cx,
            ),
            cleanup_model: settings_select(
                cleanup_model_options(model_catalog),
                &draft.cleanup_model,
                true,
                window,
                cx,
            ),
            custom_cleanup_model: settings_input(
                draft.custom_cleanup_model,
                "Custom cleanup model",
                window,
                cx,
            ),
            cleanup_reasoning_effort: settings_select(
                reasoning_effort_options(model_catalog, cleanup_model),
                &cleanup_reasoning_effort,
                false,
                window,
                cx,
            ),
            cleanup_style: settings_select(
                cleanup_style_options(),
                &draft.cleanup_style,
                false,
                window,
                cx,
            ),
            cleanup_prompt: settings_text_area(
                draft.cleanup_prompt,
                "Cleanup instructions",
                3,
                6,
                window,
                cx,
            ),
            recording_mode: settings_select(
                recording_mode_options(),
                &draft.recording_mode,
                false,
                window,
                cx,
            ),
            max_recording_seconds: settings_number_input(
                draft.max_recording_seconds,
                "300",
                u64::from(u32::MAX),
                window,
                cx,
            ),
            audio_ducking_volume_percent: settings_number_input(
                draft.audio_ducking_volume_percent,
                "15",
                100,
                window,
                cx,
            ),
            paste_shortcut: settings_select(
                paste_shortcut_options(),
                &draft.paste_shortcut,
                false,
                window,
                cx,
            ),
        }
    }

    fn draft(
        &self,
        current: &agentdictate_core::Settings,
        cx: &Context<SettingsShell>,
    ) -> SettingsDraft {
        SettingsDraft {
            transcription_model: selected_setting(
                &self.transcription_model,
                &current.transcription_model,
                cx,
            ),
            custom_transcription_model: self
                .custom_transcription_model
                .read(cx)
                .value()
                .to_string(),
            language: selected_setting(&self.language, &current.language, cx),
            transcription_prompt: self.transcription_prompt.read(cx).value().to_string(),
            cleanup_enabled: current.cleanup_enabled,
            cleanup_model: selected_setting(&self.cleanup_model, &current.cleanup_model, cx),
            custom_cleanup_model: self.custom_cleanup_model.read(cx).value().to_string(),
            cleanup_reasoning_effort: selected_setting(
                &self.cleanup_reasoning_effort,
                &current.cleanup_reasoning_effort,
                cx,
            ),
            cleanup_style: selected_setting(&self.cleanup_style, &current.cleanup_style, cx),
            cleanup_prompt: self.cleanup_prompt.read(cx).value().to_string(),
            hotkey: current.hotkey.clone(),
            recording_mode: selected_setting(&self.recording_mode, &current.recording_mode, cx),
            max_recording_seconds: self.max_recording_seconds.read(cx).value().to_string(),
            audio_ducking_enabled: current.audio_ducking_enabled,
            audio_ducking_volume_percent: self
                .audio_ducking_volume_percent
                .read(cx)
                .value()
                .to_string(),
            paste_shortcut: selected_setting(&self.paste_shortcut, &current.paste_shortcut, cx),
            start_on_login: current.start_on_login,
            save_history: current.save_history,
            preserve_temp_audio: current.preserve_temp_audio,
        }
    }

    fn inputs(&self) -> Vec<Entity<InputState>> {
        vec![
            self.custom_transcription_model.clone(),
            self.transcription_prompt.clone(),
            self.custom_cleanup_model.clone(),
            self.cleanup_prompt.clone(),
            self.max_recording_seconds.clone(),
            self.audio_ducking_volume_percent.clone(),
        ]
    }

    fn selects(&self) -> Vec<Entity<SettingSelectState>> {
        vec![
            self.transcription_model.clone(),
            self.language.clone(),
            self.cleanup_model.clone(),
            self.cleanup_reasoning_effort.clone(),
            self.cleanup_style.clone(),
            self.recording_mode.clone(),
            self.paste_shortcut.clone(),
        ]
    }

    fn reset(
        &self,
        settings: &agentdictate_core::Settings,
        catalog: &ModelCatalogViewModel,
        window: &mut Window,
        cx: &mut Context<SettingsShell>,
    ) {
        let draft = SettingsDraft::from(settings);
        let input_values = [
            (
                self.custom_transcription_model.clone(),
                draft.custom_transcription_model,
            ),
            (
                self.transcription_prompt.clone(),
                draft.transcription_prompt,
            ),
            (
                self.custom_cleanup_model.clone(),
                draft.custom_cleanup_model,
            ),
            (self.cleanup_prompt.clone(), draft.cleanup_prompt),
            (
                self.max_recording_seconds.clone(),
                draft.max_recording_seconds,
            ),
            (
                self.audio_ducking_volume_percent.clone(),
                draft.audio_ducking_volume_percent,
            ),
        ];
        for (input, value) in input_values {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        let select_values = [
            (self.transcription_model.clone(), draft.transcription_model),
            (self.language.clone(), draft.language),
            (self.cleanup_model.clone(), draft.cleanup_model),
            (self.cleanup_style.clone(), draft.cleanup_style),
            (self.recording_mode.clone(), draft.recording_mode),
            (self.paste_shortcut.clone(), draft.paste_shortcut),
        ];
        for (select, value) in select_values {
            select.update(cx, |select, cx| {
                select.set_selected_value(&value, window, cx);
            });
        }
        let cleanup_model = settings.active_cleanup_model();
        let reasoning_effort =
            catalog.normalized_reasoning_effort(cleanup_model, &draft.cleanup_reasoning_effort);
        replace_select_options(
            &self.cleanup_reasoning_effort,
            reasoning_effort_options(catalog, cleanup_model),
            &reasoning_effort,
            window,
            cx,
        );
    }

    fn sync_model_catalog(
        &self,
        catalog: &ModelCatalogViewModel,
        settings: &agentdictate_core::Settings,
        window: &mut Window,
        cx: &mut Context<SettingsShell>,
    ) {
        let transcription_model =
            selected_setting(&self.transcription_model, &settings.transcription_model, cx);
        let cleanup_model = selected_setting(&self.cleanup_model, &settings.cleanup_model, cx);
        let reasoning_effort = selected_setting(
            &self.cleanup_reasoning_effort,
            &settings.cleanup_reasoning_effort,
            cx,
        );

        replace_select_options(
            &self.transcription_model,
            transcription_model_options(catalog),
            &transcription_model,
            window,
            cx,
        );
        replace_select_options(
            &self.cleanup_model,
            cleanup_model_options(catalog),
            &cleanup_model,
            window,
            cx,
        );
        self.sync_reasoning_options(catalog, settings, &reasoning_effort, window, cx);
    }

    fn sync_reasoning_options(
        &self,
        catalog: &ModelCatalogViewModel,
        settings: &agentdictate_core::Settings,
        selected_reasoning: &str,
        window: &mut Window,
        cx: &mut Context<SettingsShell>,
    ) {
        let selected_cleanup = selected_setting(&self.cleanup_model, &settings.cleanup_model, cx);
        let cleanup_model_id = if selected_cleanup == "Custom" {
            self.custom_cleanup_model.read(cx).value().to_string()
        } else {
            selected_cleanup
        };
        let selected_reasoning =
            catalog.normalized_reasoning_effort(&cleanup_model_id, selected_reasoning);
        replace_select_options(
            &self.cleanup_reasoning_effort,
            reasoning_effort_options(catalog, &cleanup_model_id),
            &selected_reasoning,
            window,
            cx,
        );
    }
}

fn settings_input(
    value: String,
    placeholder: &'static str,
    window: &mut Window,
    cx: &mut Context<SettingsShell>,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder)
            .default_value(value)
    })
}

fn settings_text_area(
    value: String,
    placeholder: &'static str,
    min_rows: usize,
    max_rows: usize,
    window: &mut Window,
    cx: &mut Context<SettingsShell>,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder)
            .default_value(value)
            .auto_grow(min_rows, max_rows)
    })
}

fn settings_number_input(
    value: String,
    placeholder: &'static str,
    maximum: u64,
    window: &mut Window,
    cx: &mut Context<SettingsShell>,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder)
            .default_value(value)
            .validate(move |text, _| {
                text.is_empty()
                    || (text.bytes().all(|byte| byte.is_ascii_digit())
                        && text.parse::<u64>().is_ok_and(|value| value <= maximum))
            })
    })
}

fn settings_select(
    mut options: Vec<SettingOption>,
    selected: &str,
    searchable: bool,
    window: &mut Window,
    cx: &mut Context<SettingsShell>,
) -> Entity<SettingSelectState> {
    let selected_index = options.iter().position(|option| option.value == selected);
    let selected_index = selected_index.unwrap_or_else(|| {
        options.push(SettingOption::new(selected.to_owned(), selected.to_owned()));
        options.len() - 1
    });
    cx.new(|cx| {
        SelectState::new(
            SearchableVec::new(options),
            Some(IndexPath::default().row(selected_index)),
            window,
            cx,
        )
        .searchable(searchable)
    })
}

fn replace_select_options(
    state: &Entity<SettingSelectState>,
    mut options: Vec<SettingOption>,
    selected: &str,
    window: &mut Window,
    cx: &mut Context<SettingsShell>,
) {
    if !options.iter().any(|option| option.value == selected) {
        options.push(SettingOption::new(
            format!("{selected} — current value"),
            selected.to_owned(),
        ));
    }
    state.update(cx, |state, cx| {
        state.set_items(SearchableVec::new(options), window, cx);
        state.set_selected_value(&selected.to_owned(), window, cx);
    });
}

fn selected_setting(
    state: &Entity<SettingSelectState>,
    fallback: &str,
    cx: &Context<SettingsShell>,
) -> String {
    state
        .read(cx)
        .selected_value()
        .cloned()
        .unwrap_or_else(|| fallback.to_owned())
}

fn setting_options(options: &[(&str, &str)]) -> Vec<SettingOption> {
    options
        .iter()
        .map(|(label, value)| SettingOption::new((*label).to_owned(), (*value).to_owned()))
        .collect()
}

fn captured_shortcut(keystroke: &gpui::Keystroke) -> Result<String, String> {
    if keystroke.modifiers.function {
        return Err("The Function modifier is not supported".to_owned());
    }

    let normalized = keystroke.key.to_ascii_lowercase();
    let key = match normalized.as_str() {
        "space" => "Space".to_owned(),
        "tab" => "Tab".to_owned(),
        "enter" | "return" => "Enter".to_owned(),
        key if matches!(
            key,
            "f1" | "f2" | "f3" | "f4" | "f5" | "f6" | "f7" | "f8" | "f9" | "f10" | "f11" | "f12"
        ) =>
        {
            key.to_ascii_uppercase()
        }
        key if key.len() == 1 && key.bytes().all(|byte| byte.is_ascii_alphanumeric()) => {
            key.to_ascii_uppercase()
        }
        _ => return Err("Use a letter, number, Space, Tab, Enter, or F1–F12".to_owned()),
    };

    let is_function_key = normalized.starts_with('f')
        && normalized[1..]
            .parse::<u8>()
            .is_ok_and(|number| (1..=12).contains(&number));
    if !keystroke.modifiers.modified() && !is_function_key {
        return Err("Add Ctrl, Alt, Shift, or Super to this key".to_owned());
    }

    let mut parts = Vec::with_capacity(5);
    if keystroke.modifiers.control {
        parts.push("Ctrl".to_owned());
    }
    if keystroke.modifiers.alt {
        parts.push("Alt".to_owned());
    }
    if keystroke.modifiers.shift {
        parts.push("Shift".to_owned());
    }
    if keystroke.modifiers.platform {
        parts.push("Super".to_owned());
    }
    parts.push(key);
    Ok(parts.join("+"))
}

fn transcription_model_options(catalog: &ModelCatalogViewModel) -> Vec<SettingOption> {
    catalog
        .transcription_models
        .iter()
        .map(|model| SettingOption::new(model.label.clone(), model.id.clone()))
        .chain(std::iter::once(SettingOption::new("Custom…", "Custom")))
        .collect()
}

fn cleanup_model_options(catalog: &ModelCatalogViewModel) -> Vec<SettingOption> {
    catalog
        .cleanup_models
        .iter()
        .map(|model| SettingOption::new(model.label.clone(), model.id.clone()))
        .chain(std::iter::once(SettingOption::new("Custom…", "Custom")))
        .collect()
}

fn language_options() -> Vec<SettingOption> {
    setting_options(&[
        ("Auto-detect", ""),
        ("English (en)", "en"),
        ("French (fr)", "fr"),
        ("Spanish (es)", "es"),
        ("German (de)", "de"),
        ("Portuguese (pt)", "pt"),
        ("Italian (it)", "it"),
        ("Dutch (nl)", "nl"),
        ("Polish (pl)", "pl"),
        ("Arabic (ar)", "ar"),
        ("Chinese (zh)", "zh"),
        ("Japanese (ja)", "ja"),
        ("Korean (ko)", "ko"),
        ("Hindi (hi)", "hi"),
    ])
}

fn reasoning_effort_options(
    catalog: &ModelCatalogViewModel,
    cleanup_model: &str,
) -> Vec<SettingOption> {
    catalog
        .reasoning_options_for(cleanup_model)
        .into_iter()
        .map(|effort| SettingOption::new(effort.label, effort.value))
        .collect()
}

fn cleanup_style_options() -> Vec<SettingOption> {
    setting_options(&[
        ("Light cleanup", "Light cleanup"),
        ("Structured coding prompt", "Structured coding prompt"),
    ])
}

fn recording_mode_options() -> Vec<SettingOption> {
    setting_options(&[("Toggle", "toggle"), ("Hold", "hold")])
}

fn paste_shortcut_options() -> Vec<SettingOption> {
    setting_options(&[
        ("Automatic", "Automatic"),
        ("Standard (Ctrl+V)", "Standard (Ctrl+V)"),
        ("Terminal (Ctrl+Shift+V)", "Terminal (Ctrl+Shift+V)"),
    ])
}

impl SettingsShell {
    pub fn new(model: ShellViewModel) -> Self {
        let applied_model_catalog = model.workspace.model_catalog.clone();
        Self {
            model,
            theme: ThemeTokens::default(),
            settings: agentdictate_core::Settings::default(),
            settings_baseline: agentdictate_core::Settings::default(),
            has_api_key: false,
            api_key_input: None,
            history_search_input: None,
            api_key_feedback: None,
            command_sink: None,
            workspace_action_sink: None,
            next_request_id: 1,
            route_feedbacks: RouteFeedbacks::default(),
            settings_editor: None,
            applied_model_catalog,
            settings_dirty: false,
            shortcut_capture_active: false,
            shortcut_capture_error: None,
            _settings_input_subscriptions: Vec::new(),
            replacement_editor: None,
            pending_destructive_action: None,
            workspace_action_in_flight: false,
            history_action_lane: Default::default(),
            sidebar_open: true,
            compact_layout: None,
            sidebar_motion: SidebarMotion::new(),
            route_scroll_handles: RouteScrollHandles::default(),
        }
    }

    pub fn connected(
        model: ShellViewModel,
        settings: agentdictate_core::Settings,
        has_api_key: bool,
        command_sink: CommandSink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::connected_internal(model, settings, has_api_key, command_sink, None, window, cx)
    }

    pub fn connected_with_workspace_actions(
        model: ShellViewModel,
        settings: agentdictate_core::Settings,
        has_api_key: bool,
        command_sink: CommandSink,
        workspace_action_sink: WorkspaceActionSink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::connected_internal(
            model,
            settings,
            has_api_key,
            command_sink,
            Some(workspace_action_sink),
            window,
            cx,
        )
    }

    fn connected_internal(
        mut model: ShellViewModel,
        settings: agentdictate_core::Settings,
        has_api_key: bool,
        command_sink: CommandSink,
        workspace_action_sink: Option<WorkspaceActionSink>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        model.workspace = workspace_with_currency(model.workspace, &settings.currency);
        let api_key_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("sk-…").masked(true));
        let initial_history_search = model.workspace.history.search.clone();
        let history_search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search every transcript")
                .default_value(initial_history_search)
        });
        let applied_model_catalog = model.workspace.model_catalog.clone();
        let settings_editor =
            SettingsEditorState::new(&settings, &applied_model_catalog, window, cx);
        let mut settings_input_subscriptions: Vec<Subscription> = settings_editor
            .inputs()
            .into_iter()
            .map(|input| {
                cx.subscribe(&input, |shell, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        shell.recompute_settings_dirty(cx);
                        cx.notify();
                    }
                })
            })
            .collect();
        settings_input_subscriptions.extend(settings_editor.selects().into_iter().map(|select| {
            cx.subscribe(
                &select,
                |shell, _, event: &SelectEvent<SearchableVec<SettingOption>>, cx| {
                    if matches!(event, SelectEvent::Confirm(Some(_))) {
                        shell.recompute_settings_dirty(cx);
                        shell.clear_route_feedback();
                        cx.notify();
                    }
                },
            )
        }));
        settings_input_subscriptions.push(cx.subscribe_in(
            &settings_editor.cleanup_model,
            window,
            |shell, _, event: &SelectEvent<SearchableVec<SettingOption>>, window, cx| {
                if matches!(event, SelectEvent::Confirm(Some(_))) {
                    shell.cleanup_model_selection_changed(window, cx);
                }
            },
        ));
        for (input, maximum) in [
            (
                settings_editor.max_recording_seconds.clone(),
                u64::from(u32::MAX),
            ),
            (settings_editor.audio_ducking_volume_percent.clone(), 100),
        ] {
            settings_input_subscriptions.push(cx.subscribe_in(
                &input,
                window,
                move |_, input, event: &NumberInputEvent, window, cx| {
                    let NumberInputEvent::Step(step) = event;
                    input.update(cx, |input, cx| {
                        let current = input.value().parse::<u64>().unwrap_or_default();
                        let next = match step {
                            StepAction::Increment => current.saturating_add(1).min(maximum),
                            StepAction::Decrement => current.saturating_sub(1),
                        };
                        input.set_value(next.to_string(), window, cx);
                    });
                },
            ));
        }
        settings_input_subscriptions.push(cx.subscribe(
            &api_key_input,
            |shell, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    shell.api_key_feedback = None;
                    cx.notify();
                }
            },
        ));
        settings_input_subscriptions.push(cx.subscribe(
            &history_search_input,
            |shell, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let query = input.read(cx).value().to_string();
                    shell.submit_history_search(query, cx);
                }
            },
        ));
        settings_input_subscriptions.push(cx.observe_keystrokes(|shell, event, _window, cx| {
            if shell.shortcut_capture_active {
                shell.capture_shortcut(&event.keystroke, cx);
                cx.stop_propagation();
            }
        }));
        let mut next_request_id = 1;
        let mut route_feedbacks = RouteFeedbacks::default();
        if has_api_key && model.active_route == Route::Settings {
            if let Err(error) = command_sink(
                agentdictate_core::ClientCommand::refresh_model_catalog(next_request_id),
            ) {
                route_feedbacks.settings = Some(format!("Could not refresh models: {error}"));
            }
            next_request_id += 1;
        }
        Self {
            model,
            theme: ThemeTokens::default(),
            settings: settings.clone(),
            settings_baseline: settings,
            has_api_key,
            api_key_input: Some(api_key_input),
            history_search_input: Some(history_search_input),
            api_key_feedback: None,
            command_sink: Some(command_sink),
            workspace_action_sink,
            next_request_id,
            route_feedbacks,
            settings_editor: Some(settings_editor),
            applied_model_catalog,
            settings_dirty: false,
            shortcut_capture_active: false,
            shortcut_capture_error: None,
            _settings_input_subscriptions: settings_input_subscriptions,
            replacement_editor: None,
            pending_destructive_action: None,
            workspace_action_in_flight: false,
            history_action_lane: Default::default(),
            sidebar_open: true,
            compact_layout: None,
            sidebar_motion: SidebarMotion::new(),
            route_scroll_handles: RouteScrollHandles::default(),
        }
    }

    pub fn with_theme(model: ShellViewModel, theme: ThemeTokens) -> Self {
        Self {
            theme,
            ..Self::new(model)
        }
    }

    pub fn with_workspace_actions(
        model: ShellViewModel,
        workspace_action_sink: WorkspaceActionSink,
    ) -> Self {
        Self {
            workspace_action_sink: Some(workspace_action_sink),
            ..Self::new(model)
        }
    }

    pub const fn active_route(&self) -> Route {
        self.model.active_route
    }

    pub const fn view_model(&self) -> &ShellViewModel {
        &self.model
    }

    pub const fn sidebar_is_open(&self) -> bool {
        self.sidebar_open
    }

    /// Atomically replaces the workspace projection received from the daemon.
    pub fn apply_workspace_update(
        &mut self,
        workspace: WorkspaceViewModel,
        cx: &mut Context<Self>,
    ) {
        self.model.workspace = workspace_with_currency(workspace, &self.settings.currency);
        cx.notify();
    }

    fn sync_model_catalog_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let catalog = self.model.workspace.model_catalog.clone();
        if catalog == self.applied_model_catalog {
            return;
        }
        if let Some(editor) = self.settings_editor.clone() {
            editor.sync_model_catalog(&catalog, &self.settings, window, cx);
        }
        self.applied_model_catalog = catalog;
    }

    fn cleanup_model_selection_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.settings_editor.clone() else {
            return;
        };
        let selected_reasoning = selected_setting(
            &editor.cleanup_reasoning_effort,
            &self.settings.cleanup_reasoning_effort,
            cx,
        );
        editor.sync_reasoning_options(
            &self.model.workspace.model_catalog,
            &self.settings,
            &selected_reasoning,
            window,
            cx,
        );
        self.recompute_settings_dirty(cx);
        cx.notify();
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn select_cleanup_model_for_test(
        &mut self,
        model_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.settings_editor.clone() else {
            return;
        };
        editor.cleanup_model.update(cx, |state, cx| {
            state.set_selected_value(&model_id.to_owned(), window, cx);
        });
        self.cleanup_model_selection_changed(window, cx);
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn selected_cleanup_reasoning_for_test(&self, cx: &App) -> String {
        self.settings_editor.as_ref().map_or_else(
            || self.settings.cleanup_reasoning_effort.clone(),
            |editor| {
                editor
                    .cleanup_reasoning_effort
                    .read(cx)
                    .selected_value()
                    .cloned()
                    .unwrap_or_else(|| self.settings.cleanup_reasoning_effort.clone())
            },
        )
    }

    fn request_model_catalog_refresh(&mut self) {
        if !self.has_api_key {
            return;
        }
        let Some(sink) = &self.command_sink else {
            return;
        };
        let request_id = self.next_request_id;
        self.next_request_id += 1;
        if let Err(error) = sink(agentdictate_core::ClientCommand::refresh_model_catalog(
            request_id,
        )) {
            self.set_route_feedback_for(
                Route::Settings,
                format!("Could not refresh models: {error}"),
            );
        }
    }

    fn save_settings_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.settings_editor.clone() else {
            return;
        };
        match editor.draft(&self.settings, cx).apply_to(&self.settings) {
            Ok(settings) => {
                let Some(sink) = &self.command_sink else {
                    self.settings = settings.clone();
                    self.settings_baseline = settings;
                    self.settings_dirty = false;
                    self.set_route_feedback("Saved");
                    return;
                };
                let request_id = self.next_request_id;
                self.next_request_id += 1;
                match sink(agentdictate_core::ClientCommand::update_settings(
                    request_id, &settings,
                )) {
                    Ok(()) => {
                        self.settings = settings.clone();
                        self.settings_baseline = settings;
                        self.settings_dirty = false;
                        self.set_route_feedback("Saved");
                    }
                    Err(error) => {
                        self.set_route_feedback(format!("Could not save: {error}"));
                    }
                }
            }
            Err(error) => self.set_route_feedback(error.to_string()),
        }
    }

    const fn settings_is_dirty(&self) -> bool {
        self.settings_dirty
    }

    fn recompute_settings_dirty(&mut self, cx: &Context<Self>) {
        self.settings_dirty = self.settings_editor.as_ref().is_some_and(|editor| {
            editor
                .draft(&self.settings, cx)
                .is_dirty_against(&self.settings_baseline)
        });
    }

    fn update_settings_draft(
        &mut self,
        cx: &mut Context<Self>,
        update: impl FnOnce(&mut SettingsDraft),
    ) {
        let Some(editor) = self.settings_editor.as_ref() else {
            return;
        };
        let mut draft = editor.draft(&self.settings, cx);
        update(&mut draft);
        self.settings.cleanup_enabled = draft.cleanup_enabled;
        self.settings.audio_ducking_enabled = draft.audio_ducking_enabled;
        self.settings.start_on_login = draft.start_on_login;
        self.settings.save_history = draft.save_history;
        self.settings.preserve_temp_audio = draft.preserve_temp_audio;
        self.recompute_settings_dirty(cx);
        self.clear_route_feedback();
        cx.notify();
    }

    fn discard_settings_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings = self.settings_baseline.clone();
        if let Some(editor) = self.settings_editor.clone() {
            editor.reset(
                &self.settings_baseline,
                &self.model.workspace.model_catalog,
                window,
                cx,
            );
        }
        self.settings_dirty = false;
        self.shortcut_capture_active = false;
        self.shortcut_capture_error = None;
        self.clear_route_feedback();
        cx.notify();
    }

    fn begin_shortcut_capture(&mut self, cx: &mut Context<Self>) {
        self.shortcut_capture_active = true;
        self.shortcut_capture_error = None;
        cx.notify();
    }

    fn cancel_shortcut_capture(&mut self, cx: &mut Context<Self>) {
        self.shortcut_capture_active = false;
        self.shortcut_capture_error = None;
        cx.notify();
    }

    fn capture_shortcut(&mut self, keystroke: &gpui::Keystroke, cx: &mut Context<Self>) {
        if keystroke.key.eq_ignore_ascii_case("escape")
            && keystroke.modifiers == gpui::Modifiers::none()
        {
            self.cancel_shortcut_capture(cx);
            return;
        }
        match captured_shortcut(keystroke) {
            Ok(shortcut) => {
                self.settings.hotkey = shortcut;
                self.shortcut_capture_active = false;
                self.shortcut_capture_error = None;
                self.recompute_settings_dirty(cx);
            }
            Err(error) => self.shortcut_capture_error = Some(error),
        }
        cx.notify();
    }

    fn save_api_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.api_key_input.clone() else {
            return;
        };
        let api_key = input.read(cx).value().trim().to_owned();
        if api_key.is_empty() {
            self.api_key_feedback = Some("Paste an API key first".to_owned());
            return;
        }
        let Some(sink) = &self.command_sink else {
            self.api_key_feedback = Some("API key saving is not connected".to_owned());
            return;
        };
        let request_id = self.next_request_id;
        self.next_request_id += 1;
        self.api_key_feedback = Some(
            match sink(agentdictate_core::ClientCommand::set_api_key(
                request_id, api_key,
            )) {
                Ok(()) => {
                    self.has_api_key = true;
                    input.update(cx, |input, cx| input.set_value(String::new(), window, cx));
                    "API key saved".to_owned()
                }
                Err(error) => format!("Could not save: {error}"),
            },
        );
    }

    fn emit_workspace_action(&mut self, action: WorkspaceAction, cx: &mut Context<Self>) {
        if matches!(
            action,
            WorkspaceAction::SearchHistory { .. } | WorkspaceAction::LoadMoreHistory
        ) {
            self.emit_history_action(action, cx);
            return;
        }
        if self.workspace_action_in_flight {
            self.set_route_feedback("Another action is still running");
            return;
        }
        let Some(sink) = &self.workspace_action_sink else {
            self.set_route_feedback("This action is not connected yet");
            return;
        };
        let feedback_route = self.model.active_route;
        let sink = Arc::clone(sink);
        let closes_editor = matches!(
            action,
            WorkspaceAction::CreateReplacement { .. } | WorkspaceAction::UpdateReplacement { .. }
        );
        self.pending_destructive_action = None;
        self.workspace_action_in_flight = true;
        self.clear_route_feedback_for(feedback_route);
        let task = cx.background_spawn(async move { sink(action) });
        cx.spawn(async move |shell, cx| {
            let result = task.await;
            if let Some(shell) = shell.upgrade() {
                shell
                    .update(cx, |shell, cx| {
                        shell.workspace_action_in_flight = false;
                        match result {
                            Ok(workspace) => {
                                shell.model.workspace =
                                    workspace_with_currency(workspace, &shell.settings.currency);
                                shell.clear_route_feedback_for(feedback_route);
                                if closes_editor {
                                    shell.replacement_editor = None;
                                }
                            }
                            Err(error) => {
                                shell.set_route_feedback_for(
                                    feedback_route,
                                    format!("Could not complete action: {error}"),
                                );
                            }
                        }
                        cx.notify();
                    })
                    .ok();
            }
        })
        .detach();
    }

    /// History reads have their own latest-wins lane. A slow search must not
    /// block Copy, replacement edits, or other workspace mutations, and an
    /// obsolete response must never replace a newer query.
    fn emit_history_action(&mut self, action: WorkspaceAction, cx: &mut Context<Self>) {
        if !self.history_action_lane.schedule(&action) {
            return;
        }
        let Some(sink) = &self.workspace_action_sink else {
            self.set_route_feedback_for(Route::History, "History search is not connected yet");
            return;
        };
        let sink = Arc::clone(sink);
        self.clear_route_feedback_for(Route::History);
        let task = cx.background_spawn(async move { sink(action) });
        cx.spawn(async move |shell, cx| {
            let result = task.await;
            if let Some(shell) = shell.upgrade() {
                shell
                    .update(cx, |shell, cx| {
                        let completion = shell.history_action_lane.complete();
                        if completion.apply_result {
                            match result {
                                Ok(workspace) => {
                                    shell.model.workspace.history = workspace.history;
                                    shell.clear_route_feedback_for(Route::History);
                                }
                                Err(error) => shell.set_route_feedback_for(
                                    Route::History,
                                    format!("Could not search history: {error}"),
                                ),
                            }
                        }
                        if let Some(query) = completion.next_search {
                            shell.emit_history_action(WorkspaceAction::SearchHistory { query }, cx);
                        }
                        cx.notify();
                    })
                    .ok();
            }
        })
        .detach();
    }

    fn submit_history_search(&mut self, query: String, cx: &mut Context<Self>) {
        self.emit_history_action(WorkspaceAction::SearchHistory { query }, cx);
    }

    fn request_destructive_action(&mut self, action: WorkspaceAction, cx: &mut Context<Self>) {
        if self.pending_destructive_action.as_ref() == Some(&action) {
            self.pending_destructive_action = None;
            self.emit_workspace_action(action, cx);
        } else {
            self.pending_destructive_action = Some(action);
            self.set_route_feedback(
                "Click Confirm delete to permanently remove this item, or continue elsewhere to cancel."
                    .to_owned(),
            );
        }
    }

    fn open_replacement_editor(
        &mut self,
        id: Option<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let draft = id
            .and_then(|id| {
                self.model
                    .workspace
                    .replacements
                    .rules
                    .iter()
                    .find(|rule| rule.id == id)
            })
            .map(crate::ReplacementRuleViewModel::draft)
            .unwrap_or_else(|| ReplacementDraft::new("", ""));
        let source_value = draft.source.clone();
        let replacement_value = draft.replacement.clone();
        let source = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Spoken phrase")
                .default_value(source_value)
        });
        let replacement = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Replacement text")
                .default_value(replacement_value)
        });
        self.replacement_editor = Some(ReplacementEditorState {
            id,
            source,
            replacement,
            enabled: draft.enabled,
            case_sensitive: draft.case_sensitive,
            whole_word_only: draft.whole_word_only,
        });
        self.clear_route_feedback();
    }

    fn save_replacement(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &self.replacement_editor else {
            return;
        };
        let draft = ReplacementDraft {
            source: editor.source.read(cx).value().trim().to_owned(),
            replacement: editor.replacement.read(cx).value().trim().to_owned(),
            enabled: editor.enabled,
            case_sensitive: editor.case_sensitive,
            whole_word_only: editor.whole_word_only,
        };
        if !draft.is_valid() {
            self.set_route_feedback("Both phrases are required");
            return;
        }
        let action = match editor.id {
            Some(id) => WorkspaceAction::UpdateReplacement { id, draft },
            None => WorkspaceAction::CreateReplacement { draft },
        };
        self.emit_workspace_action(action, cx);
    }

    fn set_route_feedback(&mut self, message: impl Into<String>) {
        self.set_route_feedback_for(self.model.active_route, message);
    }

    fn set_route_feedback_for(&mut self, route: Route, message: impl Into<String>) {
        *self.route_feedbacks.for_route_mut(route) = Some(message.into());
    }

    fn clear_route_feedback(&mut self) {
        self.clear_route_feedback_for(self.model.active_route);
    }

    fn clear_route_feedback_for(&mut self, route: Route) {
        *self.route_feedbacks.for_route_mut(route) = None;
    }
}

fn workspace_with_currency(
    mut workspace: WorkspaceViewModel,
    currency: &str,
) -> WorkspaceViewModel {
    workspace.usage = workspace.usage.with_currency(currency);
    workspace
}

impl Render for SettingsShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_model_catalog_editor(window, cx);
        let theme = self.theme;
        let active_route = self.model.active_route;
        let navigation = self.model.navigation;
        let workspace = self.model.workspace.clone();
        let settings = self.settings.clone();
        let settings_dirty = self.settings_is_dirty();
        let has_api_key = self.has_api_key;
        let api_key_input = self.api_key_input.clone();
        let history_search_input = self.history_search_input.clone();
        let api_key_feedback = self.api_key_feedback.clone();
        let route_feedback = self.route_feedbacks.for_route(active_route).clone();
        let settings_editor = self.settings_editor.clone();
        let shortcut_capture_active = self.shortcut_capture_active;
        let shortcut_capture_error = self.shortcut_capture_error.clone();
        let replacement_editor = self.replacement_editor.clone();
        let replacement_editor_open = replacement_editor.is_some();
        let pending_destructive_action = self.pending_destructive_action.clone();
        let route_scroll_handle = self.route_scroll_handles.for_route(active_route);
        let route_scroll_selector = format!("route-scroll-{}", active_route.slug());
        let route_scroll_id = match active_route {
            Route::Overview => 0_usize,
            Route::History => 1,
            Route::Replacements => 2,
            Route::Settings => 3,
        };
        let compact = f32::from(window.viewport_size().width) < SIDEBAR_OVERLAY_BREAKPOINT;
        let first_layout = self.compact_layout.is_none();
        if self.compact_layout != Some(compact) {
            self.sidebar_open =
                sidebar_open_for_layout(self.sidebar_open, self.compact_layout, compact);
            self.compact_layout = Some(compact);
        }
        let frame =
            self.sidebar_motion
                .update(self.sidebar_open, compact, first_layout, Instant::now());
        if frame.active {
            window.request_animation_frame();
        }

        gpui::div()
            .flex()
            .flex_row()
            .relative()
            .size_full()
            .min_w(px(720.))
            .min_h(px(480.))
            .bg(gpui_color(theme.canvas))
            .text_color(gpui_color(theme.text))
            .when(!compact, |root| {
                root.child(
                    gpui::div()
                        .debug_selector(|| "sidebar-rail".to_owned())
                        .h_full()
                        .w(px(SIDEBAR_WIDTH * frame.panel))
                        .flex_shrink_0()
                        .overflow_hidden()
                        .when(frame.panel > 0.0, |rail| {
                            rail.child(
                                gpui::div()
                                    .relative()
                                    .left(px(-SIDEBAR_WIDTH * (1.0 - frame.panel)))
                                    .w(px(SIDEBAR_WIDTH))
                                    .h_full()
                                    .child(sidebar_view(navigation, false, theme, cx)),
                            )
                        }),
                )
            })
            .child(
                v_flex()
                    .h_full()
                    .min_w_0()
                    .flex_1()
                    .child(shell_title_bar(
                        active_route,
                        self.sidebar_open,
                        window,
                        theme,
                        cx,
                    ))
                    .child(
                        gpui::div()
                            .debug_selector(|| "route-content".to_owned())
                            .relative()
                            .overflow_hidden()
                            .min_h_0()
                            .flex_1()
                            .child(
                                v_flex()
                                    .id(("route-scroll", route_scroll_id))
                                    .debug_selector(move || route_scroll_selector)
                                    .size_full()
                                    .p_6()
                                    .gap_5()
                                    .child(route_surface(
                                        RouteSurfaceModel {
                                            active_route,
                                            workspace,
                                            settings,
                                            settings_dirty,
                                            has_api_key,
                                            api_key_input,
                                            history_search_input,
                                            api_key_feedback,
                                            feedback: route_feedback.clone(),
                                            settings_editor,
                                            shortcut_capture_active,
                                            shortcut_capture_error,
                                            replacement_editor,
                                            pending_destructive_action,
                                        },
                                        theme,
                                        cx,
                                    ))
                                    .when(
                                        active_route != Route::Settings
                                            && !(active_route == Route::Replacements
                                                && replacement_editor_open),
                                        |content| {
                                            content.when_some(
                                                route_feedback,
                                                |content, feedback| {
                                                    content.child(
                                                        gpui::div()
                                                            .debug_selector(|| {
                                                                "workspace-feedback".to_owned()
                                                            })
                                                            .rounded_lg()
                                                            .border_1()
                                                            .border_color(gpui_color(theme.border))
                                                            .p_3()
                                                            .text_xs()
                                                            .text_color(gpui_color(
                                                                theme.text_muted,
                                                            ))
                                                            .child(feedback),
                                                    )
                                                },
                                            )
                                        },
                                    )
                                    .track_scroll(&route_scroll_handle)
                                    .overflow_y_scroll(),
                            )
                            .child(
                                gpui::div()
                                    .debug_selector(move || {
                                        format!("route-scrollbar-{}", active_route.slug())
                                    })
                                    .absolute()
                                    .top_0()
                                    .right_0()
                                    .bottom_0()
                                    .w(px(ROUTE_SCROLLBAR_WIDTH))
                                    .child(
                                        Scrollbar::vertical(&route_scroll_handle)
                                            .id(("route-scrollbar", route_scroll_id)),
                                    ),
                            ),
                    ),
            )
            .when(
                compact && (self.sidebar_open || frame.panel > 0.0 || frame.scrim > 0.0),
                |root| {
                    root.child(
                        gpui::div()
                            .id("sidebar-dismiss")
                            .debug_selector(|| "sidebar-dismiss".to_owned())
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .left_0()
                            .cursor_pointer()
                            .occlude()
                            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.45 * frame.scrim))
                            .when(self.sidebar_open, |dismiss| {
                                dismiss.on_click(cx.listener(|shell, _, _, cx| {
                                    shell.sidebar_open = false;
                                    cx.notify();
                                }))
                            }),
                    )
                    .child(
                        gpui::div()
                            .debug_selector(|| "sidebar-overlay-panel".to_owned())
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(px(-SIDEBAR_WIDTH * (1.0 - frame.panel)))
                            .w(px(SIDEBAR_WIDTH))
                            .occlude()
                            .shadow_xl()
                            .child(sidebar_view(navigation, true, theme, cx)),
                    )
                },
            )
    }
}

fn sidebar_view(
    navigation: [NavigationItemViewModel; 4],
    overlay: bool,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    v_flex()
        .w(px(SIDEBAR_WIDTH))
        .h_full()
        .flex_shrink_0()
        .bg(gpui_color(theme.sidebar))
        .border_r_1()
        .border_color(gpui_color(theme.sidebar_border))
        .child(
            gpui::div()
                .h(px(48.))
                .flex()
                .items_center()
                .justify_center()
                .flex_shrink_0()
                .px_4()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("Agent Dictate"),
        )
        .children(navigation.into_iter().map(|item| {
            let route = item.route;
            let selector = route.navigation_id().to_owned();
            let dot_selector = format!("nav-dot-{}", route.slug());
            action_button(route.navigation_id())
                .debug_selector(move || selector)
                .custom(
                    ButtonCustomVariant::new(cx)
                        .color(if item.is_active {
                            gpui_color(route_accent(route, theme)).opacity(0.12)
                        } else {
                            gpui_color(theme.sidebar)
                        })
                        .foreground(gpui_color(theme.text))
                        .hover(gpui_color(theme.surface_hovered))
                        .active(gpui_color(theme.border)),
                )
                .selected(item.is_active)
                .small()
                .h(px(38.))
                .mx_2()
                .my(px(2.))
                .px_3()
                .gap_3()
                .rounded_lg()
                .justify_start()
                .cursor_pointer()
                .text_sm()
                .tooltip(route.accessibility_label())
                .child(
                    gpui::div()
                        .debug_selector(move || dot_selector)
                        .size_2()
                        .rounded_full()
                        .flex_shrink_0()
                        .bg(gpui_color(route_accent(route, theme))),
                )
                .child(item.label)
                .on_click(cx.listener(move |shell, _, _, cx| {
                    let previous_route = shell.model.active_route;
                    shell.model.select_route(route);
                    if route == Route::Settings && previous_route != Route::Settings {
                        shell.request_model_catalog_refresh();
                    }
                    shell.pending_destructive_action = None;
                    shell.clear_route_feedback_for(previous_route);
                    shell.api_key_feedback = None;
                    if overlay {
                        shell.sidebar_open = false;
                    }
                    cx.notify();
                }))
        }))
}

fn route_accent(route: Route, theme: ThemeTokens) -> Color {
    match route {
        Route::Overview => theme.accent,
        Route::History => theme.info,
        Route::Replacements => theme.success,
        Route::Settings => Color::rgb(167, 139, 250),
    }
}

fn shell_title_bar(
    route: Route,
    sidebar_open: bool,
    window: &Window,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    h_flex()
        .h(px(48.))
        .flex_shrink_0()
        .child(
            h_flex().h_full().w(px(112.)).flex_shrink_0().pl_3().child(
                action_button("toggle-sidebar")
                    .debug_selector(|| "toggle-sidebar".to_owned())
                    .ghost()
                    .small()
                    .tooltip(if sidebar_open {
                        "Hide sidebar"
                    } else {
                        "Show sidebar"
                    })
                    .child(panel_icon(sidebar_open, theme))
                    .on_click(cx.listener(|shell, _, _, cx| {
                        shell.sidebar_open = !shell.sidebar_open;
                        cx.notify();
                    })),
            ),
        )
        .child(
            h_flex()
                .id("window-drag-region")
                .flex_1()
                .h_full()
                .justify_center()
                .window_control_area(WindowControlArea::Drag)
                .when(cfg!(target_os = "linux"), |region| {
                    region.on_mouse_down(MouseButton::Left, |_, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                        window.start_window_move();
                    })
                })
                .child(
                    h_flex()
                        .debug_selector(|| "page-context".to_owned())
                        .gap_1p5()
                        .px_3()
                        .py_1()
                        .rounded_lg()
                        .bg(gpui_color(theme.surface_hovered))
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(gpui_color(theme.text_muted))
                        .child(
                            gpui::div()
                                .size_1p5()
                                .rounded_full()
                                .bg(gpui_color(route_accent(route, theme))),
                        )
                        .child(route.title()),
                ),
        )
        .child(window_controls(window, theme))
}

fn panel_icon(sidebar_open: bool, theme: ThemeTokens) -> gpui::Div {
    gpui::div()
        .relative()
        .w(px(17.))
        .h(px(15.))
        .rounded(px(2.))
        .border_1()
        .border_color(gpui_color(theme.text))
        .child(
            gpui::div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(5.))
                .w(px(1.))
                .bg(gpui_color(theme.text)),
        )
        .child(
            gpui::div()
                .absolute()
                .top(px(6.))
                .left(if sidebar_open { px(9.) } else { px(10.) })
                .w(px(4.))
                .h(px(1.))
                .bg(gpui_color(theme.text)),
        )
}

fn window_controls(window: &Window, theme: ThemeTokens) -> gpui::Div {
    h_flex()
        .justify_end()
        .w(px(112.))
        .flex_shrink_0()
        .gap_2()
        .pr_3()
        .child(
            gpui::div()
                .id("window-minimize")
                .debug_selector(|| "window-minimize".to_owned())
                .flex()
                .items_center()
                .justify_center()
                .size(px(28.))
                .rounded_full()
                .cursor_pointer()
                .text_lg()
                .hover(|button| button.bg(gpui_color(theme.surface_hovered)))
                .window_control_area(WindowControlArea::Min)
                .on_click(|_, window, cx| {
                    cx.stop_propagation();
                    window.minimize_window();
                })
                .child("−"),
        )
        .child(
            gpui::div()
                .id("window-maximize")
                .debug_selector(|| "window-maximize".to_owned())
                .flex()
                .items_center()
                .justify_center()
                .size(px(28.))
                .rounded_full()
                .cursor_pointer()
                .hover(|button| button.bg(gpui_color(theme.surface_hovered)))
                .window_control_area(WindowControlArea::Max)
                .on_click(|_, window, cx| {
                    cx.stop_propagation();
                    window.zoom_window();
                })
                .child(maximize_icon(window, theme)),
        )
        .child(
            gpui::div()
                .id("window-close")
                .debug_selector(|| "window-close".to_owned())
                .flex()
                .items_center()
                .justify_center()
                .size(px(28.))
                .rounded_full()
                .cursor_pointer()
                .text_lg()
                .hover(|button| {
                    button
                        .bg(gpui_color(theme.danger))
                        .text_color(gpui_color(theme.text))
                })
                .window_control_area(WindowControlArea::Close)
                .on_click(|_, window, cx| {
                    cx.stop_propagation();
                    window.remove_window();
                })
                .child("×"),
        )
}

fn maximize_icon(window: &Window, theme: ThemeTokens) -> gpui::Div {
    if window.is_maximized() {
        gpui::div()
            .relative()
            .size(px(12.))
            .child(
                gpui::div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .size(px(8.))
                    .rounded(px(1.))
                    .border_1()
                    .border_color(gpui_color(theme.text)),
            )
            .child(
                gpui::div()
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .size(px(8.))
                    .rounded(px(1.))
                    .border_1()
                    .border_color(gpui_color(theme.text))
                    .bg(gpui_color(theme.canvas)),
            )
    } else {
        gpui::div()
            .size(px(10.))
            .rounded(px(1.))
            .border_1()
            .border_color(gpui_color(theme.text))
    }
}

struct RouteSurfaceModel {
    active_route: Route,
    workspace: WorkspaceViewModel,
    settings: agentdictate_core::Settings,
    settings_dirty: bool,
    has_api_key: bool,
    api_key_input: Option<Entity<InputState>>,
    history_search_input: Option<Entity<InputState>>,
    api_key_feedback: Option<String>,
    feedback: Option<String>,
    settings_editor: Option<SettingsEditorState>,
    shortcut_capture_active: bool,
    shortcut_capture_error: Option<String>,
    replacement_editor: Option<ReplacementEditorState>,
    pending_destructive_action: Option<WorkspaceAction>,
}

struct SettingsSurfaceModel {
    settings: agentdictate_core::Settings,
    model_catalog: ModelCatalogViewModel,
    settings_dirty: bool,
    has_api_key: bool,
    api_key_input: Option<Entity<InputState>>,
    api_key_feedback: Option<String>,
    feedback: Option<String>,
    settings_editor: Option<SettingsEditorState>,
    shortcut_capture_active: bool,
    shortcut_capture_error: Option<String>,
}

fn route_surface(
    model: RouteSurfaceModel,
    theme: ThemeTokens,
    cx: &mut Context<SettingsShell>,
) -> gpui::Div {
    match model.active_route {
        Route::Overview => overview::surface(
            model.workspace.usage,
            model.workspace.history,
            model.workspace.recent_transcripts,
            theme,
            cx,
        ),
        Route::History => history_page::surface(
            model.workspace.history,
            model.history_search_input,
            model.pending_destructive_action,
            theme,
            cx,
        ),
        Route::Settings => settings_page::surface(
            SettingsSurfaceModel {
                settings: model.settings,
                model_catalog: model.workspace.model_catalog,
                settings_dirty: model.settings_dirty,
                has_api_key: model.has_api_key,
                api_key_input: model.api_key_input,
                api_key_feedback: model.api_key_feedback,
                feedback: model.feedback,
                settings_editor: model.settings_editor,
                shortcut_capture_active: model.shortcut_capture_active,
                shortcut_capture_error: model.shortcut_capture_error,
            },
            theme,
            cx,
        ),
        Route::Replacements => replacements_page::surface(
            model.workspace.replacements,
            model.replacement_editor,
            model.feedback,
            model.pending_destructive_action,
            theme,
            cx,
        ),
    }
}

const fn enabled_label(enabled: bool) -> &'static str {
    if enabled { "On" } else { "Off" }
}

fn gpui_color(color: Color) -> Hsla {
    rgb((u32::from(color.red) << 16) | (u32::from(color.green) << 8) | u32::from(color.blue)).into()
}
