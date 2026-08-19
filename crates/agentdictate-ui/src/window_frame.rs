#[cfg(not(any(test, feature = "test-support")))]
use gpui::Decorations;
use gpui::{
    AnyElement, AnyView, App, Context, CursorStyle, IntoElement, MouseButton, ParentElement,
    Render, RenderOnce, ResizeEdge, Window, div, prelude::*, px,
};

const CLIENT_INSET: gpui::Pixels = px(0.0);
const RESIZE_HIT_SIZE: gpui::Pixels = px(6.0);

/// Client-decorated root that keeps resizing available on Linux.
pub struct AgentDictateWindowFrame {
    view: AnyView,
}

impl AgentDictateWindowFrame {
    pub fn new(view: impl Into<AnyView>) -> Self {
        Self { view: view.into() }
    }
}

impl Render for AgentDictateWindowFrame {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        ClientFrame::new().child(
            div()
                .id("agentdictate-root")
                .debug_selector(|| "agentdictate-root".to_owned())
                .relative()
                .size_full()
                .bg(gpui::rgb(0x0a0a0a))
                .text_color(gpui::rgb(0xededed))
                .capture_any_mouse_down(|event, window, _| {
                    if event.button == MouseButton::Left {
                        window.blur();
                    }
                })
                .child(self.view.clone()),
        )
    }
}

#[derive(gpui::IntoElement, Default)]
struct ClientFrame {
    children: Vec<AnyElement>,
}

impl ClientFrame {
    fn new() -> Self {
        Self::default()
    }
}

impl ParentElement for ClientFrame {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for ClientFrame {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        #[cfg(any(test, feature = "test-support"))]
        let edges = {
            window.set_client_inset(CLIENT_INSET);
            Some(FrameEdges::ALL)
        };
        #[cfg(not(any(test, feature = "test-support")))]
        let edges = match window.window_decorations() {
            Decorations::Server => None,
            Decorations::Client { tiling } => {
                window.set_client_inset(CLIENT_INSET);
                Some(FrameEdges {
                    top: !tiling.top,
                    right: !tiling.right,
                    bottom: !tiling.bottom,
                    left: !tiling.left,
                })
            }
        };
        let content = div()
            .cursor(CursorStyle::Arrow)
            .size_full()
            .overflow_hidden()
            // gpui-component 0.5.1's Root inserts a full-window Linux resize
            // hitbox behind its child. Keep that legacy hitbox out of content
            // hover/click routing; the explicit edge zones below remain above
            // this surface and are the sole owners of resize input.
            .occlude()
            .children(self.children);

        div()
            .id("agentdictate-window-frame")
            .debug_selector(|| "agentdictate-window-frame".to_owned())
            .relative()
            .size_full()
            .bg(gpui::rgb(0x0a0a0a))
            .child(content)
            .when_some(edges, |frame, edges| frame.children(resize_zones(edges)))
    }
}

#[derive(Clone, Copy)]
struct FrameEdges {
    top: bool,
    right: bool,
    bottom: bool,
    left: bool,
}

impl FrameEdges {
    #[cfg(any(test, feature = "test-support"))]
    const ALL: Self = Self {
        top: true,
        right: true,
        bottom: true,
        left: true,
    };
}

fn resize_zones(edges: FrameEdges) -> Vec<AnyElement> {
    let mut zones = Vec::with_capacity(8);
    if edges.top {
        zones.push(horizontal_zone("resize-top", ResizeEdge::Top, true).into_any_element());
    }
    if edges.bottom {
        zones.push(horizontal_zone("resize-bottom", ResizeEdge::Bottom, false).into_any_element());
    }
    if edges.left {
        zones.push(vertical_zone("resize-left", ResizeEdge::Left, true).into_any_element());
    }
    if edges.right {
        zones.push(vertical_zone("resize-right", ResizeEdge::Right, false).into_any_element());
    }
    if edges.top && edges.left {
        zones.push(
            corner_zone("resize-top-left", ResizeEdge::TopLeft, true, true).into_any_element(),
        );
    }
    if edges.top && edges.right {
        zones.push(
            corner_zone("resize-top-right", ResizeEdge::TopRight, true, false).into_any_element(),
        );
    }
    if edges.bottom && edges.left {
        zones.push(
            corner_zone("resize-bottom-left", ResizeEdge::BottomLeft, false, true)
                .into_any_element(),
        );
    }
    if edges.bottom && edges.right {
        zones.push(
            corner_zone("resize-bottom-right", ResizeEdge::BottomRight, false, false)
                .into_any_element(),
        );
    }
    zones
}

fn resize_zone(
    id: &'static str,
    edge: ResizeEdge,
    cursor: CursorStyle,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .absolute()
        .cursor(cursor)
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            window.prevent_default();
            cx.stop_propagation();
            window.start_window_resize(edge);
        })
}

fn horizontal_zone(id: &'static str, edge: ResizeEdge, top: bool) -> gpui::Stateful<gpui::Div> {
    resize_zone(id, edge, CursorStyle::ResizeUpDown)
        .left(RESIZE_HIT_SIZE)
        .right(RESIZE_HIT_SIZE)
        .h(RESIZE_HIT_SIZE)
        .when(top, |zone| zone.top_0())
        .when(!top, |zone| zone.bottom_0())
}

fn vertical_zone(id: &'static str, edge: ResizeEdge, left: bool) -> gpui::Stateful<gpui::Div> {
    resize_zone(id, edge, CursorStyle::ResizeLeftRight)
        .top(RESIZE_HIT_SIZE)
        .bottom(RESIZE_HIT_SIZE)
        .w(RESIZE_HIT_SIZE)
        .when(left, |zone| zone.left_0())
        .when(!left, |zone| zone.right_0())
}

fn corner_zone(
    id: &'static str,
    edge: ResizeEdge,
    top: bool,
    left: bool,
) -> gpui::Stateful<gpui::Div> {
    let cursor = if top == left {
        CursorStyle::ResizeUpLeftDownRight
    } else {
        CursorStyle::ResizeUpRightDownLeft
    };
    resize_zone(id, edge, cursor)
        .size(RESIZE_HIT_SIZE)
        .when(top, |zone| zone.top_0())
        .when(!top, |zone| zone.bottom_0())
        .when(left, |zone| zone.left_0())
        .when(!left, |zone| zone.right_0())
}
