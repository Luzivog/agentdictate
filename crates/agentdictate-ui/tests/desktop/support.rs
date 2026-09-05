#![cfg(feature = "test-support")]

use gpui::{
    Bounds, Modifiers, MouseButton, Pixels, ScrollDelta, ScrollWheelEvent, VisualTestContext,
    point, px,
};

pub(crate) trait DesktopHarness {
    fn visual_context(&mut self) -> &mut VisualTestContext;

    fn bounds(&mut self, selector: &'static str) -> Bounds<Pixels> {
        bounds(self.visual_context(), selector)
    }

    fn has(&mut self, selector: &'static str) -> bool {
        self.visual_context().debug_bounds(selector).is_some()
    }

    fn click(&mut self, selector: &'static str) {
        click(self.visual_context(), selector);
    }

    /// Find the control in the current layout instead of assuming a fixed page height.
    fn scroll_to(&mut self, selector: &'static str) {
        let viewport = self.bounds("route-content");
        let control = self.bounds(selector);
        let cx = self.visual_context();
        cx.simulate_mouse_move(viewport.center(), None::<MouseButton>, Modifiers::none());
        cx.simulate_event(ScrollWheelEvent {
            position: viewport.center(),
            delta: ScrollDelta::Pixels(point(px(0.), viewport.center().y - control.center().y)),
            ..Default::default()
        });
        cx.run_until_parked();
    }
}

fn bounds(cx: &mut VisualTestContext, selector: &'static str) -> Bounds<Pixels> {
    cx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing rendered selector: {selector}"))
}

pub(crate) fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let position = bounds(cx, selector).center();
    cx.simulate_mouse_move(position, None::<MouseButton>, Modifiers::none());
    cx.simulate_click(position, Modifiers::none());
    cx.run_until_parked();
}
