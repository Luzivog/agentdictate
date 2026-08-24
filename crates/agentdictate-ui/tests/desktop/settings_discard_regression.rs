#![cfg(feature = "test-support")]

use std::{
    cell::RefCell,
    ops::Deref,
    rc::Rc,
    sync::{Arc, Mutex},
};

use agentdictate_core::{
    ClientCommand, ModelCatalogEntry, ModelCatalogOrigin, ModelCatalogSnapshot, ModelCatalogStatus,
    ModelCatalogSupport, ReasoningEffort, Settings, WorkflowPhase, WorkflowSnapshot,
};
use agentdictate_ui::{
    AgentDictateWindowFrame, ModelCatalogViewModel, Route, SettingsShell, ShellViewModel,
    WorkspaceViewModel, test_support,
};
use gpui::{
    AppContext, Bounds, Entity, TestAppContext, VisualTestContext, WindowBounds, WindowOptions,
    point, px, size,
};
use gpui_component::Root;

use super::support::DesktopHarness;

struct Harness {
    shell: Entity<SettingsShell>,
    cx: &'static mut VisualTestContext,
}

impl DesktopHarness for Harness {
    fn visual_context(&mut self) -> &mut VisualTestContext {
        self.cx
    }
}

impl Harness {
    fn open(
        cx: &mut TestAppContext,
        model: ShellViewModel,
        settings: Settings,
        commands: Arc<Mutex<Vec<ClientCommand>>>,
    ) -> Self {
        test_support::initialize(cx);
        let workspace = model.workspace.clone();
        let shell_slot = Rc::new(RefCell::new(None));
        let window_shell_slot = Rc::clone(&shell_slot);
        let window = cx.update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(1_100.), px(780.)),
                    ))),
                    ..Default::default()
                },
                move |window, cx| {
                    let captured = Arc::clone(&commands);
                    let shell = cx.new(|cx| {
                        SettingsShell::connected_with_workspace_actions(
                            model,
                            settings,
                            false,
                            Arc::new(move |command| {
                                captured.lock().expect("command lock").push(command);
                                Ok(())
                            }),
                            Arc::new(move |_| Ok(workspace.clone())),
                            window,
                            cx,
                        )
                    });
                    *window_shell_slot.borrow_mut() = Some(shell.clone());
                    let frame = cx.new(|_| AgentDictateWindowFrame::new(shell));
                    cx.new(|cx| Root::new(frame, window, cx))
                },
            )
            .expect("headless settings window opens")
        });
        let shell = shell_slot
            .borrow_mut()
            .take()
            .expect("settings shell was constructed");
        let cx = VisualTestContext::from_window(*window.deref(), cx).into_mut();
        cx.run_until_parked();
        Self { shell, cx }
    }

    fn selected_reasoning(&self) -> String {
        self.shell.read_with(self.cx, |shell, cx| {
            shell.selected_cleanup_reasoning_for_test(cx)
        })
    }
}

#[gpui::test]
fn discard_restores_cleanup_reasoning_effort_and_dependent_options_without_writing(
    cx: &mut TestAppContext,
) {
    let catalog = ModelCatalogViewModel::from(ModelCatalogSnapshot {
        cleanup_models: vec![
            ModelCatalogEntry {
                id: "gpt-reasoner".to_owned(),
                origin: ModelCatalogOrigin::Account,
                support: ModelCatalogSupport::Confirmed,
                reasoning_efforts: vec![ReasoningEffort::Default, ReasoningEffort::High],
            },
            ModelCatalogEntry {
                id: "gpt-default-only".to_owned(),
                origin: ModelCatalogOrigin::Account,
                support: ModelCatalogSupport::Unverified,
                reasoning_efforts: vec![ReasoningEffort::Default],
            },
        ],
        status: ModelCatalogStatus::Builtin,
        ..ModelCatalogSnapshot::default()
    });
    let model = ShellViewModel::from_snapshot(
        Route::Settings,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    )
    .with_workspace(WorkspaceViewModel {
        model_catalog: catalog,
        ..WorkspaceViewModel::default()
    });
    let settings = Settings {
        cleanup_model: "gpt-reasoner".to_owned(),
        cleanup_reasoning_effort: "high".to_owned(),
        ..Settings::default()
    };
    let commands = Arc::new(Mutex::new(Vec::new()));
    let mut harness = Harness::open(cx, model, settings, Arc::clone(&commands));

    assert_eq!(harness.selected_reasoning(), "high");
    harness.shell.update_in(harness.cx, |shell, window, cx| {
        shell.select_cleanup_model_for_test("gpt-default-only", window, cx);
    });
    harness.cx.run_until_parked();
    assert_eq!(harness.selected_reasoning(), "default");
    harness.bounds("settings-save-bar");

    harness.click("discard-settings");

    let restored = harness
        .shell
        .read_with(harness.cx, |shell, cx| shell.settings_draft_for_test(cx));
    assert_eq!(restored.cleanup_model, "gpt-reasoner");
    assert_eq!(restored.cleanup_reasoning_effort, "high");
    assert!(commands.lock().expect("command lock").is_empty());
}
