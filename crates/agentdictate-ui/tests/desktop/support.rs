#![cfg(feature = "test-support")]

use gpui::{Bounds, Modifiers, MouseButton, Pixels, VisualTestContext};

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
