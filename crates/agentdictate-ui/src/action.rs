use gpui::{ElementId, prelude::*};
use gpui_component::button::Button;

/// Creates the shared clickable control used throughout the desktop UI.
///
/// `gpui-component` defaults ordinary buttons to the arrow cursor, so the
/// application owns the pointing-hand policy at this single construction seam.
pub(crate) fn action_button(id: impl Into<ElementId>) -> Button {
    Button::new(id).cursor_pointer()
}

#[cfg(test)]
mod tests {
    use gpui::{CursorStyle, Styled};

    use super::*;

    #[test]
    fn action_buttons_use_the_pointer_cursor_by_default() {
        let mut button = action_button("cursor-contract");

        assert_eq!(button.style().mouse_cursor, Some(CursorStyle::PointingHand));
    }
}
