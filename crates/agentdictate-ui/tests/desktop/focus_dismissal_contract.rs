#![cfg(feature = "test-support")]

//! Headless contracts for the window-level pointer focus policy.

use super::support::DesktopHarness;

use std::ops::Deref;

use agentdictate_ui::{AgentDictateWindowFrame, test_support};
use gpui::{
    Bounds, Context, ElementInputHandler, Entity, Focusable, InputHandler, Modifiers, MouseButton,
    ParentElement, Render, Styled, Subscription, TestAppContext, VisualTestContext, Window,
    WindowBounds, WindowOptions, div, point, prelude::*, px, size,
};
use gpui_component::{
    Root,
    input::{Input, InputEvent, InputState},
};

struct FocusSurface {
    first: Entity<InputState>,
    second: Entity<InputState>,
    first_focus_events: usize,
    first_blur_events: usize,
    action_count: usize,
    _subscriptions: Vec<Subscription>,
}

impl FocusSurface {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let first = cx.new(|cx| InputState::new(window, cx).default_value("kept"));
        let second = cx.new(|cx| InputState::new(window, cx));
        let first_input_subscription =
            cx.subscribe(&first, |surface, _, event: &InputEvent, _| match event {
                InputEvent::Focus => surface.first_focus_events += 1,
                InputEvent::Blur => surface.first_blur_events += 1,
                InputEvent::Change | InputEvent::PressEnter { .. } => {}
            });
        Self {
            first,
            second,
            first_focus_events: 0,
            first_blur_events: 0,
            action_count: 0,
            _subscriptions: vec![first_input_subscription],
        }
    }
}

impl Render for FocusSurface {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_4()
            .p_8()
            .child(
                div()
                    .id("focus-test-first")
                    .debug_selector(|| "focus-test-first".to_owned())
                    .w(px(320.))
                    .child(Input::new(&self.first)),
            )
            .child(
                div()
                    .id("focus-test-second")
                    .debug_selector(|| "focus-test-second".to_owned())
                    .w(px(320.))
                    .child(Input::new(&self.second)),
            )
            .child(
                div()
                    .id("focus-test-action")
                    .debug_selector(|| "focus-test-action".to_owned())
                    .w(px(120.))
                    .h(px(32.))
                    .on_click(cx.listener(|surface, _, _, _| surface.action_count += 1))
                    .child("Action"),
            )
            .child(
                div()
                    .id("focus-test-background")
                    .debug_selector(|| "focus-test-background".to_owned())
                    .w_full()
                    .flex_1(),
            )
    }
}

struct Harness {
    surface: Entity<FocusSurface>,
    cx: &'static mut VisualTestContext,
}

impl DesktopHarness for Harness {
    fn visual_context(&mut self) -> &mut VisualTestContext {
        self.cx
    }
}

impl Harness {
    fn open(cx: &mut TestAppContext) -> Self {
        test_support::initialize(cx);
        let surface_slot = std::rc::Rc::new(std::cell::RefCell::new(None));
        let window_slot = surface_slot.clone();
        let window = cx.update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(800.), px(600.)),
                    ))),
                    ..Default::default()
                },
                move |window, cx| {
                    let surface = cx.new(|cx| FocusSurface::new(window, cx));
                    *window_slot.borrow_mut() = Some(surface.clone());
                    let frame = cx.new(|_| AgentDictateWindowFrame::new(surface));
                    cx.new(|cx| Root::new(frame, window, cx))
                },
            )
            .expect("headless focus window opens")
        });
        let surface = surface_slot
            .borrow_mut()
            .take()
            .expect("focus surface was constructed");
        let cx = VisualTestContext::from_window(*window.deref(), cx).into_mut();
        cx.run_until_parked();
        Self { surface, cx }
    }

    fn mouse_down(&mut self, selector: &'static str, button: MouseButton) {
        let position = self.bounds(selector).center();
        self.mouse_down_at(position, button);
    }

    fn mouse_down_at(&mut self, position: gpui::Point<gpui::Pixels>, button: MouseButton) {
        self.cx
            .simulate_mouse_move(position, None::<MouseButton>, Modifiers::none());
        self.cx
            .simulate_mouse_down(position, button, Modifiers::none());
        self.cx
            .simulate_mouse_up(position, button, Modifiers::none());
        self.cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        self.cx.run_until_parked();
    }

    fn click(&mut self, selector: &'static str) {
        self.mouse_down(selector, MouseButton::Left);
    }

    fn input(&mut self, index: usize) -> Entity<InputState> {
        self.surface.read_with(self.cx, |surface, _| match index {
            0 => surface.first.clone(),
            1 => surface.second.clone(),
            _ => panic!("unsupported input index: {index}"),
        })
    }

    fn input_is_focused(&mut self, index: usize) -> bool {
        let input = self.input(index);
        self.cx
            .update(|window, cx| input.read(cx).focus_handle(cx).is_focused(window))
    }

    fn first_value(&mut self) -> String {
        self.input(0)
            .read_with(self.cx, |input, _| input.value().to_string())
    }

    fn first_focus_event_counts(&mut self) -> (usize, usize) {
        self.surface.read_with(self.cx, |surface, _| {
            (surface.first_focus_events, surface.first_blur_events)
        })
    }

    fn reset_first_focus_event_counts(&mut self) {
        self.surface.update(self.cx, |surface, _| {
            surface.first_focus_events = 0;
            surface.first_blur_events = 0;
        });
    }

    fn mark_first_input_text(&mut self, text: &str, selected_range: std::ops::Range<usize>) {
        let input = self.input(0);
        let bounds = self.bounds("focus-test-first");
        self.cx.update(|window, cx| {
            let mut handler = ElementInputHandler::new(bounds, input);
            handler.replace_and_mark_text_in_range(None, text, Some(selected_range), window, cx);
        });
        self.cx.run_until_parked();
    }

    fn first_editor_state(
        &mut self,
    ) -> (
        String,
        std::ops::Range<usize>,
        bool,
        Option<std::ops::Range<usize>>,
    ) {
        let input = self.input(0);
        let bounds = self.bounds("focus-test-first");
        self.cx.update(|window, cx| {
            let value = input.read(cx).value().to_string();
            let mut handler = ElementInputHandler::new(bounds, input);
            let selection = handler
                .selected_text_range(false, window, cx)
                .expect("focused input exposes its UTF-16 selection");
            let marked_range = handler.marked_text_range(window, cx);
            (value, selection.range, selection.reversed, marked_range)
        })
    }

    fn action_count(&mut self) -> usize {
        self.surface
            .read_with(self.cx, |surface, _| surface.action_count)
    }
}

#[gpui::test]
fn left_clicking_the_background_blurs_the_focused_input_without_losing_its_value(
    cx: &mut TestAppContext,
) {
    let mut harness = Harness::open(cx);
    harness.click("focus-test-first");
    assert!(harness.input_is_focused(0));

    harness.click("focus-test-background");

    assert!(!harness.input_is_focused(0));
    assert_eq!(harness.first_value(), "kept");
}

#[gpui::test]
fn clicking_the_same_input_preserves_focus_and_the_in_progress_value(cx: &mut TestAppContext) {
    let mut harness = Harness::open(cx);
    harness.click("focus-test-first");
    harness.cx.simulate_input(" value");
    harness.cx.run_until_parked();
    harness.reset_first_focus_event_counts();
    let editor_state_before_click = harness.first_editor_state();

    harness.click("focus-test-first");

    assert!(harness.input_is_focused(0));
    assert_eq!(harness.first_focus_event_counts(), (0, 0));
    assert_eq!(harness.first_editor_state(), editor_state_before_click);
}

#[gpui::test]
fn clicking_the_focused_input_does_not_cycle_focus_or_disrupt_active_composition(
    cx: &mut TestAppContext,
) {
    let mut harness = Harness::open(cx);
    harness.click("focus-test-first");
    harness.mark_first_input_text("日本", 2..2);
    harness.reset_first_focus_event_counts();
    let (value_before_click, _, _, marked_range_before_click) = harness.first_editor_state();

    harness.click("focus-test-first");

    assert!(harness.input_is_focused(0));
    assert_eq!(harness.first_focus_event_counts(), (0, 0));
    let (value_after_click, selection_after_click, _, marked_range_after_click) =
        harness.first_editor_state();
    assert_eq!(value_after_click, value_before_click);
    assert_eq!(marked_range_after_click, marked_range_before_click);
    let marked_range = marked_range_after_click.expect("IME composition remains marked");
    assert!(selection_after_click.start >= marked_range.start);
    assert!(selection_after_click.end <= marked_range.end);
}

#[gpui::test]
fn clicking_another_input_transfers_focus(cx: &mut TestAppContext) {
    let mut harness = Harness::open(cx);
    harness.click("focus-test-first");
    assert!(harness.input_is_focused(0));

    harness.click("focus-test-second");

    assert!(!harness.input_is_focused(0));
    assert!(harness.input_is_focused(1));
}

#[gpui::test]
fn clicking_an_action_blurs_the_input_and_still_runs_the_action_once(cx: &mut TestAppContext) {
    let mut harness = Harness::open(cx);
    harness.click("focus-test-first");

    harness.click("focus-test-action");

    assert!(!harness.input_is_focused(0));
    assert_eq!(harness.action_count(), 1);
}

#[gpui::test]
fn right_clicking_the_background_preserves_input_focus(cx: &mut TestAppContext) {
    let mut harness = Harness::open(cx);
    harness.click("focus-test-first");

    harness.mouse_down("focus-test-background", MouseButton::Right);

    assert!(harness.input_is_focused(0));
    assert_eq!(harness.first_value(), "kept");
}
