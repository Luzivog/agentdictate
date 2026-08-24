#![cfg(feature = "test-support")]

//! Headless replacements layout contracts.

use super::support;

use std::{
    ops::Deref,
    sync::{Arc, Mutex},
};

use agentdictate_core::{WorkflowPhase, WorkflowSnapshot};
use agentdictate_ui::{
    AgentDictateWindowFrame, ReplacementRuleViewModel, ReplacementsViewModel, Route, SettingsShell,
    ShellViewModel, WorkspaceAction, WorkspaceViewModel, test_support,
};
use gpui::{
    AppContext, Bounds, TestAppContext, VisualTestContext, WindowBounds, WindowOptions, point, px,
    size,
};
use gpui_component::Root;

#[gpui::test]
fn replacement_rows_are_dense_and_deletion_requires_confirmation(cx: &mut TestAppContext) {
    test_support::initialize(cx);
    let workspace = WorkspaceViewModel {
        replacements: ReplacementsViewModel::new(vec![
            ReplacementRuleViewModel::new(
                7,
                "a deliberately long spoken phrase that must not make the row taller",
                "a deliberately long replacement that should remain on one line",
                true,
                false,
                true,
            ),
            ReplacementRuleViewModel::new(8, "codex", "Codex", false, true, false),
        ]),
        ..WorkspaceViewModel::default()
    };
    let model = ShellViewModel::from_snapshot(
        Route::Replacements,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    )
    .with_workspace(workspace.clone());
    let actions = Arc::new(Mutex::new(Vec::new()));
    let captured_actions = Arc::clone(&actions);
    let shell = cx.new(|_| {
        SettingsShell::with_workspace_actions(
            model,
            Arc::new(move |action| {
                captured_actions.lock().expect("action lock").push(action);
                Ok(workspace.clone())
            }),
        )
    });
    let root = shell.clone();
    let window = cx.update(|cx| {
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(1_100.), px(780.)),
                ))),
                ..Default::default()
            },
            |window, cx| {
                let frame = cx.new(|_| AgentDictateWindowFrame::new(root));
                cx.new(|cx| Root::new(frame, window, cx))
            },
        )
        .expect("headless settings window opens")
    });
    let cx = VisualTestContext::from_window(*window.deref(), cx).into_mut();
    cx.run_until_parked();

    let page = cx
        .debug_bounds("replacements-page")
        .expect("replacements page renders");
    let header = cx
        .debug_bounds("replacements-header")
        .expect("replacements header renders");
    let title = cx
        .debug_bounds("replacements-title")
        .expect("replacements title renders");
    let title_glyphs = cx
        .debug_bounds("replacements-title-glyphs")
        .expect("replacements title keeps its intrinsic glyph run");
    let first = cx
        .debug_bounds("replacement-item-7")
        .expect("first replacement renders");
    let second = cx
        .debug_bounds("replacement-item-8")
        .expect("second replacement renders");
    assert!(first.size.height >= px(48.) && first.size.height <= px(52.));
    assert!(second.size.height >= px(48.) && second.size.height <= px(52.));
    assert!(first.right() <= page.right());
    assert!(second.right() <= page.right());
    assert!(first.center().y < second.center().y);
    assert!(page.size.width > px(700.));
    assert!((header.size.width - page.size.width).abs() <= px(1.));
    assert!(title.size.width > px(120.));
    assert!(title.size.height < px(28.));
    assert!(title_glyphs.size.width > px(120.));

    support::click(cx, "replacement-delete-7");
    assert!(actions.lock().expect("action lock").is_empty());
    support::click(cx, "confirm-replacement-delete-7");
    assert_eq!(
        *actions.lock().expect("action lock"),
        vec![WorkspaceAction::DeleteReplacement { id: 7 }]
    );
}
