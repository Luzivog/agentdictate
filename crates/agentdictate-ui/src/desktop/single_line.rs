use gpui::{IntoElement, SharedString, prelude::*, px};
use gpui_component::h_flex;

/// Clips a variable, single-line value without asking GPUI to shape an
/// ellipsized zero-width flex child.
///
/// GPUI 0.2.2 can retain the ellipsis glyph run produced during its initial
/// zero-width flex measurement after the container expands. This primitive
/// keeps clipping on the final-width text surface and never enables GPUI's
/// ellipsis shaping path. A single element is deliberate: nested text surfaces
/// are culled inconsistently inside GPUI's scrolling containers.
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
