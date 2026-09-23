use gpui::{IntoElement, SharedString, prelude::*, px};
use gpui_component::h_flex;

/// Clips a variable, single-line value at its container's width, without an
/// ellipsis. A single element is deliberate: nested text surfaces are culled
/// inconsistently inside GPUI's scrolling containers.
pub(super) fn single_line_clip(
    selector: impl Into<SharedString>,
    text: impl Into<SharedString>,
) -> gpui::Div {
    single_line_clip_element(selector, text.into())
}

pub(crate) fn single_line_clip_element(
    selector: impl Into<SharedString>,
    element: impl IntoElement,
) -> gpui::Div {
    let outer_selector = selector.into();

    h_flex()
        .debug_selector(move || outer_selector.to_string())
        .w_full()
        .h(px(20.))
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .child(element)
}
