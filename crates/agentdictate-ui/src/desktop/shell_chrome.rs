use gpui::{Context, MouseButton, Window, WindowControlArea, prelude::*, px};
use gpui_component::{
    Selectable, Sizable,
    button::{ButtonCustomVariant, ButtonVariants},
    h_flex, v_flex,
};

use crate::action::action_button;
use crate::{Color, NavigationItemViewModel, Route, ThemeTokens};

use super::{SIDEBAR_WIDTH, SettingsShell, gpui_color};

pub(super) fn sidebar_view(
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

pub(super) fn shell_title_bar(
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
