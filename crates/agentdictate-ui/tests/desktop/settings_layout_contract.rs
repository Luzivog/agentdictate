#![cfg(feature = "test-support")]

//! Headless settings layout contracts.

use super::support::DesktopHarness;

use std::{
    cell::RefCell,
    ops::Deref,
    rc::Rc,
    sync::{Arc, Mutex},
};

use agentdictate_core::{
    ClientCommand, ClientCommandKind, Settings, WorkflowPhase, WorkflowSnapshot,
};
use agentdictate_ui::{
    AgentDictateWindowFrame, Route, SettingsShell, ShellViewModel, test_support,
};
use gpui::{
    AppContext, Bounds, Entity, Modifiers, MouseButton, Pixels, ScrollDelta, ScrollWheelEvent,
    TestAppContext, VisualTestContext, WindowBounds, WindowOptions, point, px, size,
};
use gpui_component::Root;

struct SettingsHarness {
    cx: &'static mut VisualTestContext,
}

#[gpui::test]
fn save_and_discard_remain_clickable_at_the_bottom_of_settings(cx: &mut TestAppContext) {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let mut harness =
        SettingsHarness::open_with_size(cx, Arc::clone(&commands), size(px(720.), px(520.)));
    let viewport = harness.bounds("route-content");
    harness.cx.simulate_event(ScrollWheelEvent {
        position: viewport.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(-10_000.))),
        ..Default::default()
    });
    harness.cx.run_until_parked();
    harness.click("toggle-save-history");

    let scroll_area = harness.bounds("route-content");
    for selector in ["save-settings", "discard-settings"] {
        let button = harness.bounds(selector);
        assert!(
            button.top() >= scroll_area.bottom(),
            "{selector} scrolled out of view"
        );
        assert!(
            button.bottom() <= px(520.),
            "{selector} is below the window"
        );
    }
    harness.click("discard-settings");
    assert!(commands.lock().unwrap().is_empty());
    assert_eq!(harness.bounds("route-content"), viewport);

    harness.click("toggle-save-history");
    harness.click("save-settings");
    let commands = commands.lock().unwrap();
    assert_eq!(commands.len(), 1);
    assert!(matches!(
        &commands[0].kind,
        ClientCommandKind::UpdateSettings { settings, .. } if !settings.save_history
    ));
    let feedback = harness.bounds("settings-feedback");
    assert!(feedback.top() >= viewport.top());
    assert!(feedback.bottom() <= px(520.));
}

impl DesktopHarness for SettingsHarness {
    fn visual_context(&mut self) -> &mut VisualTestContext {
        self.cx
    }
}

impl SettingsHarness {
    fn open(cx: &mut TestAppContext, commands: Arc<Mutex<Vec<ClientCommand>>>) -> Self {
        Self::open_with_size(cx, commands, size(px(1_100.), px(780.)))
    }

    fn open_with_size(
        cx: &mut TestAppContext,
        commands: Arc<Mutex<Vec<ClientCommand>>>,
        viewport: gpui::Size<Pixels>,
    ) -> Self {
        test_support::initialize(cx);
        let model = ShellViewModel::from_snapshot(
            Route::Settings,
            WorkflowSnapshot {
                phase: WorkflowPhase::Ready,
            },
        );
        let shell_slot: Rc<RefCell<Option<Entity<SettingsShell>>>> = Rc::new(RefCell::new(None));
        let window_slot = Rc::clone(&shell_slot);
        let window = cx.update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        viewport,
                    ))),
                    ..Default::default()
                },
                move |window, cx| {
                    let commands = Arc::clone(&commands);
                    let shell = cx.new(|cx| {
                        SettingsShell::connected(
                            model,
                            Settings::default(),
                            false,
                            Arc::new(move |command| {
                                commands.lock().expect("command lock").push(command);
                                Ok(())
                            }),
                            window,
                            cx,
                        )
                    });
                    *window_slot.borrow_mut() = Some(shell.clone());
                    let frame = cx.new(|_| AgentDictateWindowFrame::new(shell));
                    cx.new(|cx| Root::new(frame, window, cx))
                },
            )
            .expect("headless settings window opens")
        });
        let _shell = shell_slot
            .borrow_mut()
            .take()
            .expect("settings shell was constructed");
        let cx = VisualTestContext::from_window(*window.deref(), cx).into_mut();
        cx.run_until_parked();
        Self { cx }
    }

    fn click_at(&mut self, position: gpui::Point<Pixels>) {
        self.cx
            .simulate_mouse_move(position, None::<MouseButton>, Modifiers::none());
        self.cx.simulate_click(position, Modifiers::none());
        self.cx.run_until_parked();
    }
}

fn assert_content_aware_control_geometry(cx: &mut TestAppContext, viewport_width: f32) {
    let mut harness = SettingsHarness::open_with_size(
        cx,
        Arc::new(Mutex::new(Vec::new())),
        size(px(viewport_width), px(900.)),
    );

    let choice = harness.bounds("settings-input-transcription-provider-control");
    let shortcut = harness.bounds("settings-hotkey-row-control");
    let number = harness.bounds("settings-input-max-recording-control");
    let prompt = harness.bounds("settings-input-transcription-prompt-control");
    let choice_row = harness.bounds("settings-input-transcription-provider");
    let number_row = harness.bounds("settings-input-max-recording");
    let prompt_row = harness.bounds("settings-input-transcription-prompt");

    assert!(choice.size.width <= px(300.));
    assert!(shortcut.size.width <= px(300.));
    assert!(number.size.width <= px(180.));
    assert!(number.size.width < choice.size.width);
    assert!(prompt.size.width >= prompt_row.size.width - px(2.));

    for (row, control) in [(choice_row, choice), (number_row, number)] {
        assert!(control.left() >= row.left());
        assert!(control.right() <= row.right());
    }
}

#[gpui::test]
fn settings_controls_are_content_sized_at_720_pixels(cx: &mut TestAppContext) {
    assert_content_aware_control_geometry(cx, 720.);
}

#[gpui::test]
fn settings_controls_are_content_sized_at_1100_pixels(cx: &mut TestAppContext) {
    assert_content_aware_control_geometry(cx, 1_100.);
}

#[gpui::test]
fn settings_controls_are_content_sized_at_1920_pixels(cx: &mut TestAppContext) {
    assert_content_aware_control_geometry(cx, 1_920.);
}

#[gpui::test]
fn maximum_recording_step_buttons_are_real_click_targets(cx: &mut TestAppContext) {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let mut harness =
        SettingsHarness::open_with_size(cx, Arc::clone(&commands), size(px(1_100.), px(1_400.)));
    harness.scroll_to("settings-input-max-recording-control");
    let control = harness.bounds("settings-input-max-recording-control");

    harness.click_at(point(control.right() - px(14.), control.center().y));
    harness.click("save-settings");
    assert!(matches!(
        &commands.lock().expect("command lock")[0].kind,
        ClientCommandKind::UpdateSettings { settings, .. }
            if settings.max_recording_seconds == Settings::default().max_recording_seconds + 1
    ));

    let viewport = harness.bounds("route-content");
    harness.cx.simulate_event(ScrollWheelEvent {
        position: viewport.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(-200.))),
        ..Default::default()
    });
    harness.cx.run_until_parked();
    let control = harness.bounds("settings-input-max-recording-control");
    harness.click_at(point(control.left() + px(14.), control.center().y));
    harness.click("save-settings");
    assert!(matches!(
        &commands.lock().expect("command lock")[1].kind,
        ClientCommandKind::UpdateSettings { settings, .. }
            if settings.max_recording_seconds == Settings::default().max_recording_seconds
    ));
}

#[gpui::test]
fn settings_sections_form_one_flat_ordered_column(cx: &mut TestAppContext) {
    let mut harness = SettingsHarness::open(cx, Arc::new(Mutex::new(Vec::new())));
    let sections = [
        harness.bounds("settings-group-account"),
        harness.bounds("settings-group-dictation"),
        harness.bounds("settings-group-cleanup"),
        harness.bounds("settings-group-recording-audio"),
        harness.bounds("settings-group-delivery-storage"),
    ];

    for pair in sections.windows(2) {
        assert!(
            pair[0].bottom() <= pair[1].top(),
            "settings sections should be vertically ordered: {pair:?}"
        );
        assert_eq!(pair[0].left(), pair[1].left());
        assert_eq!(pair[0].right(), pair[1].right());
    }

    assert!(!harness.has("settings-group-advanced"));
    assert!(!harness.has("settings-diagnostics"));
}

#[gpui::test]
fn toggles_are_draft_changes_until_the_user_saves(cx: &mut TestAppContext) {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let mut harness =
        SettingsHarness::open_with_size(cx, Arc::clone(&commands), size(px(1_100.), px(1_400.)));

    assert!(!harness.has("settings-save-bar"));
    harness.click("toggle-cleanup");

    assert!(commands.lock().expect("command lock").is_empty());
    harness.bounds("settings-save-bar");

    harness.click("save-settings");
    let commands = commands.lock().expect("command lock");
    assert_eq!(commands.len(), 1);
    assert!(matches!(
        &commands[0].kind,
        ClientCommandKind::UpdateSettings { settings, .. } if !settings.cleanup_enabled
    ));
}

#[gpui::test]
fn discard_restores_the_persisted_toggle_without_writing(cx: &mut TestAppContext) {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let mut harness =
        SettingsHarness::open_with_size(cx, Arc::clone(&commands), size(px(1_100.), px(1_400.)));

    harness.click("toggle-cleanup");
    harness.bounds("settings-save-bar");
    harness.click("discard-settings");

    assert!(commands.lock().expect("command lock").is_empty());

    // A second toggle must start from the persisted `true` value. Saving it as
    // `false` proves that Discard restored the draft instead of leaving the
    // first click in memory.
    harness.click("toggle-cleanup");
    harness.click("save-settings");
    let commands = commands.lock().expect("command lock");
    assert_eq!(commands.len(), 1);
    assert!(matches!(
        &commands[0].kind,
        ClientCommandKind::UpdateSettings { settings, .. } if !settings.cleanup_enabled
    ));
}
