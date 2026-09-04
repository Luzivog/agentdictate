#![cfg(feature = "test-support")]

//! Headless shell interaction contracts.

use super::support::{self, DesktopHarness};

use std::{
    cell::RefCell,
    ops::Deref,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Duration,
};

use agentdictate_core::{
    ClientCommand, ClientCommandKind, ModelCatalogEntry, ModelCatalogFallback, ModelCatalogOrigin,
    ModelCatalogSnapshot, ModelCatalogStatus, ModelCatalogSupport, ReasoningEffort, Settings,
    TranscriptionProvider, WorkflowPhase, WorkflowSnapshot,
};
use agentdictate_ui::{
    AgentDictateWindowFrame, HistoryViewModel, ModelCatalogViewModel, RecoveryItemViewModel,
    RecoveryStage, ReplacementDraft, ReplacementRuleViewModel, ReplacementsViewModel, Route,
    SIDEBAR_OVERLAY_BREAKPOINT, SettingsShell, ShellViewModel, UsageDayViewModel, UsagePeriod,
    UsageTotals, UsageViewModel, WorkspaceAction, WorkspaceViewModel, test_support,
};
use gpui::{
    AppContext, Bounds, Entity, Modifiers, MouseButton, Pixels, ScrollDelta, ScrollWheelEvent,
    Size, StyledText, TestAppContext, VisualTestContext, WindowBounds, WindowOptions, point,
    prelude::*, px, size,
};
use gpui_component::{Root, Theme};

struct Harness {
    shell: Entity<SettingsShell>,
    cx: &'static mut VisualTestContext,
}

impl DesktopHarness for Harness {
    fn visual_context(&mut self) -> &mut VisualTestContext {
        self.cx
    }
}

#[gpui::test]
fn overlay_failure_notice_follows_workspace_health(cx: &mut TestAppContext) {
    let mut harness = Harness::open(cx);
    assert!(!harness.has("overlay-unavailable-notice"));
    for unavailable in [true, false] {
        harness.shell.update(harness.cx, |shell, cx| {
            let workspace = shell
                .view_model()
                .workspace
                .clone()
                .with_overlay_unavailable(unavailable);
            shell.apply_workspace_update(workspace, cx);
        });
        harness.cx.run_until_parked();
        assert_eq!(harness.has("overlay-unavailable-notice"), unavailable);
    }
}

#[gpui::test]
fn gpui_root_uses_the_tokscope_dark_palette(cx: &mut TestAppContext) {
    test_support::initialize(cx);

    cx.update(|cx| {
        let theme = Theme::global(cx);
        assert_eq!(theme.background, gpui::rgb(0x0a0a0a).into());
        assert_eq!(theme.secondary, gpui::rgb(0x121212).into());
        assert_eq!(theme.border, gpui::rgb(0x212121).into());
        assert_eq!(theme.window_border, gpui::rgb(0x212121).into());
    });
}

#[gpui::test]
fn single_line_clip_preserves_the_complete_shaped_text_run(cx: &mut TestAppContext) {
    let harness = Harness::open(cx);
    let value = "A complete transcript must remain shaped beyond the clipping viewport.";

    let truncated_text = StyledText::new(value);
    let truncated_layout = truncated_text.layout().clone();
    harness
        .cx
        .draw(point(px(0.), px(0.)), size(px(8.), px(20.)), |_, _| {
            gpui::div().w(px(8.)).truncate().child(truncated_text)
        });
    assert!(
        truncated_layout.position_for_index(value.len()).is_none(),
        "the old ellipsis path should discard the end of the shaped text run"
    );

    let clipped_text = StyledText::new(value);
    let clipped_layout = clipped_text.layout().clone();
    harness
        .cx
        .draw(point(px(0.), px(24.)), size(px(8.), px(20.)), |_, _| {
            test_support::single_line_clip_element("clip-regression", clipped_text).w(px(8.))
        });
    let shaped_end = clipped_layout
        .position_for_index(value.len())
        .expect("clipping must preserve the complete shaped text run");
    assert!(shaped_end.x > px(8.));
}

impl Harness {
    fn open(cx: &mut TestAppContext) -> Self {
        Self::open_with_size(cx, size(px(1_100.), px(780.)))
    }

    fn open_with_size(cx: &mut TestAppContext, viewport: Size<Pixels>) -> Self {
        let model = ShellViewModel::from_snapshot(
            Route::Overview,
            WorkflowSnapshot {
                phase: WorkflowPhase::Ready,
            },
        )
        .with_history(HistoryViewModel::new(18, 2));
        let refreshed = model.workspace.clone();
        Self::open_model_with_actions(
            cx,
            viewport,
            model,
            Arc::new(move |_| Ok(refreshed.clone())),
        )
    }

    fn open_model_with_actions(
        cx: &mut TestAppContext,
        viewport: Size<Pixels>,
        model: ShellViewModel,
        action_sink: agentdictate_ui::WorkspaceActionSink,
    ) -> Self {
        test_support::initialize(cx);
        let shell = cx.new(|_| SettingsShell::with_workspace_actions(model, action_sink));
        let root = shell.clone();
        let window = cx.update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        viewport,
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
        Self { shell, cx }
    }

    fn open_connected(cx: &mut TestAppContext, commands: Arc<Mutex<Vec<ClientCommand>>>) -> Self {
        Self::open_connected_with(
            cx,
            ShellViewModel::from_snapshot(
                Route::Settings,
                WorkflowSnapshot {
                    phase: WorkflowPhase::Ready,
                },
            ),
            Settings::default(),
            false,
            commands,
        )
    }

    fn open_connected_with(
        cx: &mut TestAppContext,
        model: ShellViewModel,
        settings: Settings,
        has_api_key: bool,
        commands: Arc<Mutex<Vec<ClientCommand>>>,
    ) -> Self {
        test_support::initialize(cx);
        let workspace = model.workspace.clone();
        let shell_slot = Rc::new(RefCell::new(None));
        let window_slot = Rc::clone(&shell_slot);
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
                            has_api_key,
                            Arc::new(move |command| {
                                captured.lock().expect("command lock").push(command);
                                Ok(())
                            }),
                            Arc::new(move |_| Ok(workspace.clone())),
                            window,
                            cx,
                        )
                    });
                    *window_slot.borrow_mut() = Some(shell.clone());
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

    fn resize(&mut self, viewport: Size<Pixels>) {
        self.cx.simulate_resize(viewport);
        self.cx.run_until_parked();
        self.cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    fn move_to(&mut self, selector: &'static str) {
        let position = self.bounds(selector).center();
        self.cx
            .simulate_mouse_move(position, None::<MouseButton>, Modifiers::none());
    }

    fn click(&mut self, selector: &'static str) {
        self.move_to("resize-right");
        self.click_direct(selector);
    }

    fn click_direct(&mut self, selector: &'static str) {
        support::click(self.cx, selector);
    }

    fn type_text(&mut self, selector: &'static str, text: &str) {
        self.click(selector);
        self.cx.simulate_input(text);
        self.cx.run_until_parked();
    }

    fn scroll_route_by(&mut self, delta_y: f32) {
        let viewport = self.bounds("route-content");
        self.cx
            .simulate_mouse_move(viewport.center(), None::<MouseButton>, Modifiers::none());
        self.cx.simulate_event(ScrollWheelEvent {
            position: viewport.center(),
            delta: ScrollDelta::Pixels(point(px(0.), px(delta_y))),
            ..Default::default()
        });
        self.cx.run_until_parked();
    }

    fn active_route(&mut self) -> Route {
        self.shell
            .read_with(self.cx, |shell, _| shell.active_route())
    }

    fn sidebar_is_open(&mut self) -> bool {
        self.shell
            .read_with(self.cx, |shell, _| shell.sidebar_is_open())
    }

    fn sidebar_width(&mut self) -> Pixels {
        self.bounds("sidebar-rail").size.width
    }

    fn settle_sidebar_motion(&mut self) {
        std::thread::sleep(Duration::from_millis(230));
        self.shell.update(self.cx, |_, cx| cx.notify());
        self.cx.run_until_parked();
    }

    fn usage_period(&mut self) -> UsagePeriod {
        self.shell.read_with(self.cx, |shell, _| {
            shell.view_model().workspace.usage.period
        })
    }
}

#[gpui::test]
fn settings_explains_when_cached_model_choices_are_shown(cx: &mut TestAppContext) {
    let model = ShellViewModel::from_snapshot(
        Route::Settings,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    )
    .with_workspace(WorkspaceViewModel {
        model_catalog: ModelCatalogViewModel::from(ModelCatalogSnapshot {
            status: ModelCatalogStatus::Failed {
                fallback: ModelCatalogFallback::Cached,
                message: "Network unavailable".to_owned(),
            },
            ..ModelCatalogSnapshot::default()
        }),
        ..WorkspaceViewModel::default()
    });
    let refreshed = model.workspace.clone();
    let mut harness = Harness::open_model_with_actions(
        cx,
        size(px(1_100.), px(780.)),
        model,
        Arc::new(move |_| Ok(refreshed.clone())),
    );

    harness.bounds("settings-model-catalog-cached");
}

#[gpui::test]
fn opening_connected_settings_refreshes_the_account_catalog_once(cx: &mut TestAppContext) {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let _harness = Harness::open_connected_with(
        cx,
        ShellViewModel::from_snapshot(
            Route::Settings,
            WorkflowSnapshot {
                phase: WorkflowPhase::Ready,
            },
        ),
        Settings::default(),
        true,
        Arc::clone(&commands),
    );

    let commands = commands.lock().expect("command lock");
    assert_eq!(commands.len(), 1);
    assert!(matches!(
        commands[0].kind,
        ClientCommandKind::RefreshModelCatalog { .. }
    ));
}

#[gpui::test]
fn entering_settings_refreshes_once_without_render_polling(cx: &mut TestAppContext) {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let mut harness = Harness::open_connected_with(
        cx,
        ShellViewModel::from_snapshot(
            Route::Overview,
            WorkflowSnapshot {
                phase: WorkflowPhase::Ready,
            },
        ),
        Settings::default(),
        true,
        Arc::clone(&commands),
    );

    assert!(commands.lock().expect("command lock").is_empty());
    harness.click(Route::Settings.navigation_id());
    harness.click(Route::Settings.navigation_id());
    harness.cx.run_until_parked();

    let commands = commands.lock().expect("command lock");
    assert_eq!(commands.len(), 1);
    assert!(matches!(
        commands[0].kind,
        ClientCommandKind::RefreshModelCatalog { .. }
    ));
}

#[gpui::test]
fn catalog_updates_do_not_reset_the_current_dirty_settings_draft(cx: &mut TestAppContext) {
    let model = ShellViewModel::from_snapshot(
        Route::Settings,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    )
    .with_workspace(WorkspaceViewModel {
        model_catalog: ModelCatalogViewModel::from(ModelCatalogSnapshot {
            status: ModelCatalogStatus::Failed {
                fallback: ModelCatalogFallback::Cached,
                message: "Offline".to_owned(),
            },
            ..ModelCatalogSnapshot::default()
        }),
        ..WorkspaceViewModel::default()
    });
    let commands = Arc::new(Mutex::new(Vec::new()));
    let mut harness =
        Harness::open_connected_with(cx, model, Settings::default(), false, Arc::clone(&commands));

    harness.scroll_route_by(-120.);
    harness.click("toggle-cleanup");
    harness.bounds("settings-save-bar");
    harness.shell.update(harness.cx, |shell, cx| {
        shell.apply_workspace_update(
            WorkspaceViewModel {
                model_catalog: ModelCatalogViewModel::default(),
                ..shell.view_model().workspace.clone()
            },
            cx,
        );
    });
    harness.cx.run_until_parked();
    harness.bounds("settings-model-catalog-builtin");
    harness.scroll_route_by(10_000.);
    harness.click("save-settings");

    let commands = commands.lock().expect("command lock");
    assert_eq!(commands.len(), 1);
    assert!(matches!(
        &commands[0].kind,
        ClientCommandKind::UpdateSettings { settings, .. }
            if settings.transcription_model == "gpt-transcribe"
                && settings.cleanup_model == "gpt-5.4-nano"
                && !settings.cleanup_enabled
    ));
}

#[gpui::test]
fn selecting_a_cleanup_model_normalizes_an_unsupported_reasoning_effort(cx: &mut TestAppContext) {
    let catalog = ModelCatalogViewModel::from(ModelCatalogSnapshot {
        cleanup_models: vec![
            ModelCatalogEntry {
                id: "gpt-reasoner".to_owned(),
                origin: ModelCatalogOrigin::Account,
                support: ModelCatalogSupport::Confirmed,
                reasoning_efforts: vec![ReasoningEffort::Default, ReasoningEffort::High],
            },
            ModelCatalogEntry {
                id: "gpt-6".to_owned(),
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
    let mut harness =
        Harness::open_connected_with(cx, model, settings, false, Arc::clone(&commands));

    assert_eq!(
        harness.shell.read_with(harness.cx, |shell, cx| {
            shell.selected_cleanup_reasoning_for_test(cx)
        }),
        "high"
    );
    harness.shell.update_in(harness.cx, |shell, window, cx| {
        shell.select_cleanup_model_for_test("gpt-6", window, cx);
    });
    harness.cx.run_until_parked();
    assert_eq!(
        harness.shell.read_with(harness.cx, |shell, cx| {
            shell.selected_cleanup_reasoning_for_test(cx)
        }),
        "default"
    );

    harness.click("save-settings");
    let commands = commands.lock().expect("command lock");
    assert_eq!(commands.len(), 1);
    assert!(matches!(
        &commands[0].kind,
        ClientCommandKind::UpdateSettings { settings, .. }
            if settings.cleanup_model == "gpt-6"
                && settings.cleanup_reasoning_effort == "default"
    ));
}

#[gpui::test]
fn opening_settings_rejects_an_effort_the_selected_model_does_not_support(cx: &mut TestAppContext) {
    let catalog = ModelCatalogViewModel::from(ModelCatalogSnapshot {
        cleanup_models: vec![ModelCatalogEntry {
            id: "gpt-default-only".to_owned(),
            origin: ModelCatalogOrigin::Account,
            support: ModelCatalogSupport::Unverified,
            reasoning_efforts: vec![ReasoningEffort::Default],
        }],
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
        cleanup_model: "gpt-default-only".to_owned(),
        cleanup_reasoning_effort: "max".to_owned(),
        ..Settings::default()
    };
    let commands = Arc::new(Mutex::new(Vec::new()));
    let harness = Harness::open_connected_with(cx, model, settings, false, commands);

    assert_eq!(
        harness.shell.read_with(harness.cx, |shell, cx| {
            shell.selected_cleanup_reasoning_for_test(cx)
        }),
        "default"
    );
}

#[gpui::test]
fn recovery_rows_emit_typed_retry_actions(cx: &mut TestAppContext) {
    let actions = Arc::new(Mutex::new(Vec::new()));
    let captured_actions = Arc::clone(&actions);
    let model = ShellViewModel::from_snapshot(
        Route::History,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    )
    .with_workspace(WorkspaceViewModel {
        history: HistoryViewModel::from_records(
            vec![RecoveryItemViewModel::new(
                "job-42",
                RecoveryStage::Delivery,
                "Today, 14:32",
                "2m 08s",
                "Paste target disappeared",
                Some("The transcript is safe".to_owned()),
            )],
            Vec::new(),
        ),
        ..WorkspaceViewModel::default()
    });
    let refreshed = WorkspaceViewModel::default();
    let mut harness = Harness::open_model_with_actions(
        cx,
        size(px(1_100.), px(780.)),
        model,
        Arc::new(move |action| {
            captured_actions.lock().expect("action lock").push(action);
            Ok(refreshed.clone())
        }),
    );

    harness.bounds("history-recovery-item-job-42");
    harness.click("history-retry-recovery-job-42");

    assert_eq!(
        *actions.lock().expect("action lock"),
        vec![WorkspaceAction::RetryRecovery {
            id: "job-42".to_owned(),
            stage: RecoveryStage::Delivery,
        }]
    );
}

#[gpui::test]
fn deleting_recoverable_audio_requires_an_explicit_second_click(cx: &mut TestAppContext) {
    let actions = Arc::new(Mutex::new(Vec::new()));
    let captured_actions = Arc::clone(&actions);
    let model = ShellViewModel::from_snapshot(
        Route::History,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    )
    .with_workspace(WorkspaceViewModel {
        history: HistoryViewModel::from_records(
            vec![RecoveryItemViewModel::new(
                "job-42",
                RecoveryStage::Transcription,
                "Today, 14:32",
                "2m 08s",
                "Network unavailable",
                None,
            )],
            Vec::new(),
        ),
        ..WorkspaceViewModel::default()
    });
    let refreshed = WorkspaceViewModel::default();
    let mut harness = Harness::open_model_with_actions(
        cx,
        size(px(1_100.), px(780.)),
        model,
        Arc::new(move |action| {
            captured_actions.lock().expect("action lock").push(action);
            Ok(refreshed.clone())
        }),
    );

    harness.click("history-delete-recovery-job-42");
    assert!(actions.lock().expect("action lock").is_empty());
    harness.click("confirm-history-delete-recovery-job-42");

    assert_eq!(
        *actions.lock().expect("action lock"),
        vec![WorkspaceAction::DeleteRecovery {
            id: "job-42".to_owned(),
        }]
    );
}

#[gpui::test]
fn leaving_a_route_clears_its_destructive_confirmation_guidance(cx: &mut TestAppContext) {
    let model = ShellViewModel::from_snapshot(
        Route::History,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    )
    .with_workspace(WorkspaceViewModel {
        history: HistoryViewModel::from_records(
            vec![RecoveryItemViewModel::new(
                "job-42",
                RecoveryStage::Transcription,
                "Today, 14:32",
                "2m 08s",
                "Network unavailable",
                None,
            )],
            Vec::new(),
        ),
        ..WorkspaceViewModel::default()
    });
    let refreshed = model.workspace.clone();
    let mut harness = Harness::open_model_with_actions(
        cx,
        size(px(1_100.), px(780.)),
        model,
        Arc::new(move |_| Ok(refreshed.clone())),
    );

    harness.click("history-delete-recovery-job-42");
    harness.bounds("workspace-feedback");

    harness.click(Route::Settings.navigation_id());

    assert_eq!(harness.active_route(), Route::Settings);
    assert!(!harness.has("settings-feedback"));
}

#[gpui::test]
fn replacement_rows_emit_typed_toggle_actions(cx: &mut TestAppContext) {
    let actions = Arc::new(Mutex::new(Vec::new()));
    let captured_actions = Arc::clone(&actions);
    let model = ShellViewModel::from_snapshot(
        Route::Replacements,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    )
    .with_workspace(WorkspaceViewModel {
        replacements: ReplacementsViewModel::new(vec![ReplacementRuleViewModel::new(
            7,
            "agent dictate",
            "AgentDictate",
            true,
            false,
            true,
        )]),
        ..WorkspaceViewModel::default()
    });
    let refreshed = WorkspaceViewModel {
        replacements: ReplacementsViewModel::new(vec![ReplacementRuleViewModel::new(
            7,
            "agent dictate",
            "AgentDictate",
            false,
            false,
            true,
        )]),
        ..WorkspaceViewModel::default()
    };
    let mut harness = Harness::open_model_with_actions(
        cx,
        size(px(1_100.), px(780.)),
        model,
        Arc::new(move |action| {
            captured_actions.lock().expect("action lock").push(action);
            Ok(refreshed.clone())
        }),
    );

    harness.bounds("replacement-item-7");
    harness.bounds("replacement-add");
    harness.click("replacement-toggle-7");

    assert_eq!(
        *actions.lock().expect("action lock"),
        vec![WorkspaceAction::SetReplacementEnabled {
            id: 7,
            enabled: false,
        }]
    );
}

#[gpui::test]
fn replacement_editor_emits_complete_create_payload_and_applies_refresh(cx: &mut TestAppContext) {
    let actions = Arc::new(Mutex::new(Vec::new()));
    let captured_actions = Arc::clone(&actions);
    let model = ShellViewModel::from_snapshot(
        Route::Replacements,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    );
    let refreshed = WorkspaceViewModel {
        replacements: ReplacementsViewModel::new(vec![ReplacementRuleViewModel::new(
            7,
            "agent dictate",
            "AgentDictate",
            true,
            false,
            true,
        )]),
        ..WorkspaceViewModel::default()
    };
    let mut harness = Harness::open_model_with_actions(
        cx,
        size(px(1_100.), px(780.)),
        model,
        Arc::new(move |action| {
            captured_actions.lock().expect("action lock").push(action);
            Ok(refreshed.clone())
        }),
    );

    harness.click("replacement-add");
    harness.bounds("replacement-editor");
    harness.type_text("replacement-editor-source", "agent dictate");
    harness.type_text("replacement-editor-output", "AgentDictate");
    harness.click("replacement-save-new");

    assert_eq!(
        *actions.lock().expect("action lock"),
        vec![WorkspaceAction::CreateReplacement {
            draft: ReplacementDraft::new("agent dictate", "AgentDictate"),
        }]
    );
    harness.bounds("replacement-item-7");
}

#[gpui::test]
fn replacement_editor_emits_complete_update_payload(cx: &mut TestAppContext) {
    let actions = Arc::new(Mutex::new(Vec::new()));
    let captured_actions = Arc::clone(&actions);
    let workspace = WorkspaceViewModel {
        replacements: ReplacementsViewModel::new(vec![ReplacementRuleViewModel::new(
            7,
            "agent dictate",
            "AgentDictate",
            false,
            true,
            false,
        )]),
        ..WorkspaceViewModel::default()
    };
    let model = ShellViewModel::from_snapshot(
        Route::Replacements,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    )
    .with_workspace(workspace.clone());
    let mut harness = Harness::open_model_with_actions(
        cx,
        size(px(1_100.), px(780.)),
        model,
        Arc::new(move |action| {
            captured_actions.lock().expect("action lock").push(action);
            Ok(workspace.clone())
        }),
    );

    harness.click("replacement-edit-7");
    harness.click("replacement-save-7");

    assert_eq!(
        *actions.lock().expect("action lock"),
        vec![WorkspaceAction::UpdateReplacement {
            id: 7,
            draft: ReplacementDraft {
                source: "agent dictate".to_owned(),
                replacement: "AgentDictate".to_owned(),
                enabled: false,
                case_sensitive: true,
                whole_word_only: false,
            },
        }]
    );
}

#[gpui::test]
fn overview_uses_tokscope_activity_layout_and_recent_history(cx: &mut TestAppContext) {
    let actions = Arc::new(Mutex::new(Vec::new()));
    let captured_actions = Arc::clone(&actions);
    let model = ShellViewModel::from_snapshot(
        Route::Overview,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    )
    .with_workspace(WorkspaceViewModel {
        history: HistoryViewModel::from_records(
            Vec::new(),
            vec![agentdictate_ui::TranscriptViewModel::new(
                17,
                "Today, 14:32",
                "The latest completed dictation stays close at hand.",
                9,
                "0:08",
            )],
        ),
        recent_transcripts: vec![agentdictate_ui::TranscriptViewModel::new(
            17,
            "Today, 14:32",
            "The latest completed dictation stays close at hand.",
            9,
            "0:08",
        )],
        usage: UsageViewModel::new(
            UsagePeriod::Last30Days,
            UsageTotals {
                dictations: 23,
                words: 4_891,
                audio_seconds: 754,
                estimated_cost_usd: 0.1842,
            },
            vec![
                UsageDayViewModel::new("Mon", 8, 820, 40, 0.031),
                UsageDayViewModel::new("Tue", 2, 120, 113, 0.012),
            ],
        ),
        ..WorkspaceViewModel::default()
    });
    let refreshed = WorkspaceViewModel {
        usage: UsageViewModel::new(
            UsagePeriod::Last7Days,
            UsageTotals {
                dictations: 4,
                words: 820,
                audio_seconds: 113,
                estimated_cost_usd: 0.031,
            },
            vec![UsageDayViewModel::new("Mon", 4, 820, 113, 0.031)],
        ),
        ..WorkspaceViewModel::default()
    };
    let mut harness = Harness::open_model_with_actions(
        cx,
        size(px(1_100.), px(780.)),
        model,
        Arc::new(move |action| {
            captured_actions.lock().expect("action lock").push(action);
            Ok(refreshed.clone())
        }),
    );

    let summary = harness.bounds("overview-activity-summary");
    let plot = harness.bounds("overview-activity-plot");
    assert!(summary.right() <= plot.left());
    let dictations = harness.bounds("overview-summary-dictations");
    let words = harness.bounds("overview-summary-words");
    let audio = harness.bounds("overview-summary-audio");
    let wpm = harness.bounds("overview-summary-wpm");
    let cost = harness.bounds("overview-summary-cost");
    let metrics = [audio, dictations, words, wpm, cost];
    assert!(
        metrics
            .windows(2)
            .all(|pair| pair[0].bottom() <= pair[1].top())
    );
    assert!(
        metrics
            .into_iter()
            .all(|metric| metric.left() == summary.left() && metric.right() == summary.right())
    );
    assert_eq!(summary.size.height, plot.size.height);
    let shorter_day = harness.bounds("overview-activity-marker-0");
    let longer_day = harness.bounds("overview-activity-marker-1");
    assert!(
        longer_day.top() < shorter_day.top(),
        "the chart should place the day with more dictation time higher"
    );
    harness.bounds("overview-recent-history");
    harness.bounds("overview-recent-transcript-17");
    assert!(!harness.has("overview-metric-dictations"));
    assert!(!harness.has("overview-workflow-health"));
    harness.click("usage-period-7-days");

    assert_eq!(harness.usage_period(), UsagePeriod::Last7Days);
    assert_eq!(
        *actions.lock().expect("action lock"),
        vec![WorkspaceAction::SelectUsagePeriod(UsagePeriod::Last7Days)]
    );

    harness.click("overview-history-view-all");
    assert_eq!(harness.active_route(), Route::History);
}

#[gpui::test]
fn overview_starts_with_ten_and_can_reveal_twenty_more_independently_of_history_search(
    cx: &mut TestAppContext,
) {
    let recent_transcripts = (0..31)
        .map(|id| {
            agentdictate_ui::TranscriptViewModel::new(
                id,
                "Today, 14:32",
                format!("Recent transcript {id}"),
                3,
                "0:03",
            )
        })
        .collect();
    let model = ShellViewModel::from_snapshot(
        Route::Overview,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    )
    .with_workspace(WorkspaceViewModel {
        history: HistoryViewModel::from_page(
            Vec::new(),
            vec![agentdictate_ui::TranscriptViewModel::new(
                77,
                "Yesterday, 09:10",
                "A search result must not replace Overview recents.",
                8,
                "0:05",
            )],
            1,
            "search result".to_owned(),
            false,
        ),
        recent_transcripts,
        ..WorkspaceViewModel::default()
    });
    let refreshed = model.workspace.clone();
    let actions = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&actions);
    let mut harness = Harness::open_model_with_actions(
        cx,
        size(px(1_100.), px(780.)),
        model,
        Arc::new(move |action| {
            captured.lock().expect("action lock").push(action);
            Ok(refreshed.clone())
        }),
    );

    for selector in [
        "overview-recent-transcript-0",
        "overview-recent-transcript-1",
        "overview-recent-transcript-2",
        "overview-recent-transcript-3",
        "overview-recent-transcript-4",
        "overview-recent-transcript-5",
        "overview-recent-transcript-6",
        "overview-recent-transcript-7",
        "overview-recent-transcript-8",
        "overview-recent-transcript-9",
    ] {
        assert!(harness.has(selector));
    }
    assert!(!harness.has("overview-recent-transcript-10"));
    assert!(harness.has("overview-recent-show-more"));
    assert!(!harness.has("overview-recent-transcript-77"));
    let title_clip = harness.bounds("overview-recent-transcript-title-9");
    assert!(title_clip.size.width > px(240.));

    harness.scroll_route_by(-1_000.);
    harness.click("overview-recent-show-more");
    harness.cx.run_until_parked();
    assert!(harness.has("overview-recent-transcript-10"));
    assert!(harness.has("overview-recent-transcript-29"));
    assert!(!harness.has("overview-recent-transcript-30"));

    harness.scroll_route_by(-2_000.);
    harness.click("history-copy-transcript-29");
    assert_eq!(
        actions.lock().expect("action lock").as_slice(),
        &[WorkspaceAction::CopyTranscript { id: 29 }]
    );
}

#[gpui::test]
fn live_workspace_update_replaces_overview_and_history_while_the_shell_stays_open(
    cx: &mut TestAppContext,
) {
    let model = ShellViewModel::from_snapshot(
        Route::Overview,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    );
    let refreshed = model.workspace.clone();
    let mut harness = Harness::open_model_with_actions(
        cx,
        size(px(1_100.), px(780.)),
        model,
        Arc::new(move |_| Ok(refreshed.clone())),
    );
    assert!(!harness.has("overview-recent-transcript-77"));

    let workspace = WorkspaceViewModel {
        history: HistoryViewModel::from_records(
            Vec::new(),
            vec![agentdictate_ui::TranscriptViewModel::new(
                77,
                "Just now",
                "A live daemon update appeared without reopening the window.",
                10,
                "0:06",
            )],
        ),
        recent_transcripts: vec![agentdictate_ui::TranscriptViewModel::new(
            77,
            "Just now",
            "A live daemon update appeared without reopening the window.",
            10,
            "0:06",
        )],
        usage: UsageViewModel::new(
            UsagePeriod::Last7Days,
            UsageTotals {
                dictations: 1,
                words: 10,
                audio_seconds: 6,
                estimated_cost_usd: 0.01,
            },
            vec![UsageDayViewModel::new("Today", 1, 10, 6, 0.01)],
        ),
        ..WorkspaceViewModel::default()
    };
    harness.shell.update(harness.cx, |shell, cx| {
        shell.apply_workspace_update(workspace, cx);
    });
    harness.cx.run_until_parked();

    harness.bounds("overview-recent-transcript-77");
    assert_eq!(harness.usage_period(), UsagePeriod::Last7Days);
    harness.click(Route::History.navigation_id());
    harness.bounds("history-transcript-item-77");
}

#[gpui::test]
fn failed_workspace_refresh_preserves_the_previous_usage_snapshot(cx: &mut TestAppContext) {
    let model = ShellViewModel::from_snapshot(
        Route::Overview,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    )
    .with_workspace(WorkspaceViewModel {
        usage: UsageViewModel::new(
            UsagePeriod::Last30Days,
            UsageTotals {
                dictations: 23,
                words: 4_891,
                audio_seconds: 754,
                estimated_cost_usd: 0.1842,
            },
            Vec::new(),
        ),
        ..WorkspaceViewModel::default()
    });
    let mut harness = Harness::open_model_with_actions(
        cx,
        size(px(1_100.), px(780.)),
        model,
        Arc::new(|_| Err(Box::new(std::io::Error::other("daemon unavailable")))),
    );

    harness.click("usage-period-7-days");

    assert_eq!(harness.usage_period(), UsagePeriod::Last30Days);
}

#[gpui::test]
fn compact_sidebar_opens_and_dismisses_without_changing_routes(cx: &mut TestAppContext) {
    let mut harness = Harness::open_with_size(cx, size(px(900.), px(700.)));
    assert!(!harness.has(Route::Overview.navigation_id()));

    harness.click("toggle-sidebar");
    assert!(harness.has(Route::Overview.navigation_id()));
    assert!(harness.has("sidebar-dismiss"));
    assert!(harness.has("sidebar-overlay-panel"));

    harness.click("sidebar-dismiss");
    assert!(!harness.sidebar_is_open());
    assert_eq!(harness.active_route(), Route::Overview);
}

#[gpui::test]
fn resizing_across_the_breakpoint_switches_the_rendered_sidebar_presentation(
    cx: &mut TestAppContext,
) {
    let wide_width = SIDEBAR_OVERLAY_BREAKPOINT as f32 + 1.0;
    let compact_width = SIDEBAR_OVERLAY_BREAKPOINT as f32 - 1.0;
    let mut harness = Harness::open_with_size(cx, size(px(wide_width), px(700.)));

    assert!(harness.has("sidebar-rail"));
    assert!(!harness.has("sidebar-overlay-panel"));
    let wide_content_width = harness.bounds("route-content").size.width;

    harness.resize(size(px(compact_width), px(700.)));
    harness.settle_sidebar_motion();
    assert!(!harness.sidebar_is_open());
    let compact_content_width = harness.bounds("route-content").size.width;
    assert!(compact_content_width > wide_content_width);

    harness.click("toggle-sidebar");
    harness.settle_sidebar_motion();
    assert!(harness.has("sidebar-overlay-panel"));
    assert!(harness.has("sidebar-dismiss"));

    harness.resize(size(px(wide_width), px(700.)));
    harness.settle_sidebar_motion();
    assert_eq!(harness.sidebar_width(), px(250.));
    assert_eq!(
        harness.bounds("route-content").size.width,
        wide_content_width
    );
}

#[gpui::test]
fn wide_sidebar_collapses_and_reopens_without_losing_the_toggle(cx: &mut TestAppContext) {
    let mut harness = Harness::open(cx);
    assert_eq!(harness.sidebar_width(), px(250.));

    harness.click("toggle-sidebar");
    harness.settle_sidebar_motion();
    assert!(!harness.sidebar_is_open());
    assert_eq!(harness.sidebar_width(), px(0.));

    harness.click("toggle-sidebar");
    harness.settle_sidebar_motion();
    assert!(harness.sidebar_is_open());
    assert_eq!(harness.sidebar_width(), px(250.));
}

#[gpui::test]
fn flat_sidebar_and_integrated_window_chrome_are_stable(cx: &mut TestAppContext) {
    let mut harness = Harness::open(cx);

    let frame = harness.bounds("agentdictate-window-frame");
    let content = harness.bounds("agentdictate-root");
    assert_eq!(content, frame, "the content must reach every window edge");

    for route in Route::ALL {
        harness.bounds(route.navigation_id());
    }
    harness.bounds("nav-dot-overview");
    harness.bounds("nav-dot-history");
    harness.bounds("nav-dot-replacements");
    harness.bounds("nav-dot-settings");
    harness.bounds("page-context");
    harness.bounds("window-minimize");
    harness.bounds("window-maximize");
    harness.bounds("window-close");
    harness.bounds("resize-top");
    harness.bounds("resize-right");
    harness.bounds("resize-bottom");
    harness.bounds("resize-left");

    harness.click(Route::History.navigation_id());
    assert_eq!(harness.active_route(), Route::History);
}

#[gpui::test]
fn buttons_receive_clicks_after_every_resize_zone(cx: &mut TestAppContext) {
    let mut harness = Harness::open(cx);

    for edge in [
        "resize-top",
        "resize-right",
        "resize-bottom",
        "resize-left",
        "resize-top-left",
        "resize-top-right",
        "resize-bottom-left",
        "resize-bottom-right",
    ] {
        harness.move_to(edge);
        harness.click_direct("overview-history-view-all");
        assert_eq!(harness.active_route(), Route::History, "after {edge}");

        harness.move_to(edge);
        harness.click_direct(Route::Overview.navigation_id());
        assert_eq!(harness.active_route(), Route::Overview, "after {edge}");
    }
}

#[gpui::test]
fn history_is_a_flat_dense_recovery_and_transcript_list(cx: &mut TestAppContext) {
    let model = ShellViewModel::from_snapshot(
        Route::History,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    )
    .with_workspace(WorkspaceViewModel {
        history: HistoryViewModel::from_records(
            vec![RecoveryItemViewModel::new(
                "job-dense",
                RecoveryStage::Transcription,
                "Today, 14:32",
                "0:08",
                "The recording is safe and ready to retry",
                None,
            )],
            vec![agentdictate_ui::TranscriptViewModel::new(
                31,
                "Today, 14:31",
                "A compact transcript row keeps the archive easy to scan.",
                10,
                "0:09",
            )],
        ),
        ..WorkspaceViewModel::default()
    });
    let refreshed = model.workspace.clone();
    let mut harness = Harness::open_model_with_actions(
        cx,
        size(px(1_100.), px(780.)),
        model,
        Arc::new(move |_| Ok(refreshed.clone())),
    );

    let recovery = harness.bounds("history-recovery-item-job-dense");
    let transcript = harness.bounds("history-transcript-item-31");
    assert!(recovery.size.height <= px(64.));
    assert!(transcript.size.height <= px(52.));
    assert!(recovery.center().y < transcript.center().y);
    assert!(!harness.has("history-review-recovery"));

    let title_clip = harness.bounds("history-transcript-title-31");
    let metadata_clip = harness.bounds("history-transcript-metadata-31");
    assert!(title_clip.size.width > px(240.));
    assert!(metadata_clip.size.width > px(160.));
}

#[gpui::test]
fn history_wheel_scroll_reaches_transcripts_after_many_recoveries(cx: &mut TestAppContext) {
    let recoveries = (0..8)
        .map(|index| {
            RecoveryItemViewModel::new(
                format!("job-{index}"),
                RecoveryStage::Transcription,
                "Today, 14:32",
                "0:08",
                "The recording is safe and ready to retry",
                None,
            )
        })
        .collect();
    let transcripts = (0..60)
        .map(|index| {
            agentdictate_ui::TranscriptViewModel::new(
                index,
                "Today, 14:31",
                format!("Transcript {index} remains reachable by normal wheel scrolling."),
                9,
                "0:08",
            )
        })
        .collect();
    let model = ShellViewModel::from_snapshot(
        Route::History,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    )
    .with_workspace(WorkspaceViewModel {
        history: HistoryViewModel::from_records(recoveries, transcripts),
        ..WorkspaceViewModel::default()
    });
    let refreshed = model.workspace.clone();
    let mut harness = Harness::open_model_with_actions(
        cx,
        size(px(720.), px(520.)),
        model,
        Arc::new(move |_| Ok(refreshed.clone())),
    );

    let viewport = harness.bounds("route-content");
    let before = harness.bounds("history-transcript-item-59");
    assert!(
        before.top() >= viewport.bottom(),
        "final row should begin below the viewport before scrolling: row={before:?}, viewport={viewport:?}"
    );

    harness
        .cx
        .simulate_mouse_move(viewport.center(), None::<MouseButton>, Modifiers::none());
    harness.cx.simulate_event(ScrollWheelEvent {
        position: viewport.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(-10_000.))),
        ..Default::default()
    });
    harness.cx.run_until_parked();

    let after = harness.bounds("history-transcript-item-59");
    assert!(after.top() < before.top());
    assert!(after.bottom() <= viewport.bottom());
    assert!(after.bottom() > viewport.top());
}

#[gpui::test]
fn history_page_fills_the_route_and_loads_more_without_rendering_the_archive(
    cx: &mut TestAppContext,
) {
    let transcripts = (0..20)
        .map(|index| {
            agentdictate_ui::TranscriptViewModel::new(
                index,
                "Today, 14:31",
                format!("Transcript {index} remains readable in the bounded first page."),
                9,
                "0:08",
            )
        })
        .collect();
    let model = ShellViewModel::from_snapshot(
        Route::History,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    )
    .with_workspace(WorkspaceViewModel {
        history: HistoryViewModel::from_page(Vec::new(), transcripts, 2_553, String::new(), true),
        ..WorkspaceViewModel::default()
    });
    let refreshed = model.workspace.clone();
    let actions = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&actions);
    let mut harness = Harness::open_model_with_actions(
        cx,
        size(px(1_100.), px(780.)),
        model,
        Arc::new(move |action| {
            captured.lock().unwrap().push(action);
            Ok(refreshed.clone())
        }),
    );

    let route = harness.bounds("route-content");
    let page = harness.bounds("history-page");
    assert!(page.size.width >= route.size.width - px(56.));
    assert!(harness.has("history-transcript-item-0"));
    assert!(harness.has("history-transcript-item-19"));
    assert!(!harness.has("history-transcript-item-20"));

    harness
        .cx
        .simulate_mouse_move(route.center(), None::<MouseButton>, Modifiers::none());
    harness.cx.simulate_event(ScrollWheelEvent {
        position: route.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(-10_000.))),
        ..Default::default()
    });
    harness.cx.run_until_parked();

    harness.click("history-load-more");
    assert_eq!(
        actions.lock().unwrap().as_slice(),
        &[WorkspaceAction::LoadMoreHistory]
    );
}

#[gpui::test]
fn connected_history_search_emits_the_latest_query_without_a_fixed_delay(cx: &mut TestAppContext) {
    test_support::initialize(cx);
    let model = ShellViewModel::from_snapshot(
        Route::History,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    );
    let refreshed = model.workspace.clone();
    let actions = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&actions);
    let shell_slot = Rc::new(RefCell::new(None));
    let window_slot = Rc::clone(&shell_slot);
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
                let sink_workspace = refreshed.clone();
                let shell = cx.new(|cx| {
                    SettingsShell::connected_with_workspace_actions(
                        model,
                        agentdictate_core::Settings::default(),
                        false,
                        Arc::new(|_| Ok(())),
                        Arc::new(move |action| {
                            captured.lock().unwrap().push(action);
                            Ok(sink_workspace.clone())
                        }),
                        window,
                        cx,
                    )
                });
                *window_slot.borrow_mut() = Some(shell.clone());
                let frame = cx.new(|_| AgentDictateWindowFrame::new(shell));
                cx.new(|cx| Root::new(frame, window, cx))
            },
        )
        .expect("headless history window opens")
    });
    let shell = shell_slot.borrow_mut().take().unwrap();
    let visual = VisualTestContext::from_window(*window.deref(), cx).into_mut();
    visual.run_until_parked();
    let mut harness = Harness { shell, cx: visual };

    harness.type_text("history-search-input", "needle");

    assert_eq!(
        actions.lock().unwrap().last(),
        Some(&WorkspaceAction::SearchHistory {
            query: "needle".to_owned(),
        })
    );
}
#[gpui::test]
fn navigating_from_deep_history_opens_settings_at_its_own_top(cx: &mut TestAppContext) {
    let transcripts = (0..60)
        .map(|index| {
            agentdictate_ui::TranscriptViewModel::new(
                index,
                "Today, 14:32",
                format!("Transcript {index} keeps the history page tall."),
                9,
                "0:08",
            )
        })
        .collect();
    let model = ShellViewModel::from_snapshot(
        Route::History,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    )
    .with_workspace(WorkspaceViewModel {
        history: HistoryViewModel::from_records(Vec::new(), transcripts),
        ..WorkspaceViewModel::default()
    });
    let refreshed = model.workspace.clone();
    let mut harness = Harness::open_model_with_actions(
        cx,
        size(px(720.), px(520.)),
        model,
        Arc::new(move |_| Ok(refreshed.clone())),
    );

    let viewport = harness.bounds("route-content");
    harness
        .cx
        .simulate_mouse_move(viewport.center(), None::<MouseButton>, Modifiers::none());
    harness.cx.simulate_event(ScrollWheelEvent {
        position: viewport.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(-10_000.))),
        ..Default::default()
    });
    harness.cx.run_until_parked();
    assert!(harness.bounds("history-transcript-item-59").bottom() <= viewport.bottom());

    harness.click("toggle-sidebar");
    harness.settle_sidebar_motion();
    harness.click(Route::Settings.navigation_id());

    let settings = harness.bounds("settings-page");
    assert!(settings.top() >= viewport.top());
    assert!(settings.top() <= viewport.top() + px(32.));
}

#[gpui::test]
fn settings_is_one_aligned_page_with_clear_section_hierarchy(cx: &mut TestAppContext) {
    let mut harness = Harness::open(cx);
    harness.click(Route::Settings.navigation_id());

    let account = harness.bounds("settings-group-account");
    let dictation = harness.bounds("settings-group-dictation");
    let cleanup = harness.bounds("settings-group-cleanup");
    let recording = harness.bounds("settings-group-recording-audio");
    let delivery = harness.bounds("settings-group-delivery-storage");
    for section in [dictation, cleanup, recording, delivery] {
        assert!((section.left() - account.left()).abs() <= px(1.));
        assert!((section.size.width - account.size.width).abs() <= px(1.));
    }
    assert!(account.top() < dictation.top());
    assert!(dictation.top() < cleanup.top());
    assert!(cleanup.top() < recording.top());
    assert!(recording.top() < delivery.top());
    assert!(!harness.has("settings-group-advanced"));
}

#[gpui::test]
fn settings_scrollbar_keeps_the_viewport_height_while_content_moves(cx: &mut TestAppContext) {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let mut harness = Harness::open_connected(cx, commands);
    let viewport = harness.bounds("route-content");
    let before = harness.bounds("route-scrollbar-settings");
    assert!((before.size.height - viewport.size.height).abs() <= px(1.));

    harness.scroll_route_by(-700.);

    let after = harness.bounds("route-scrollbar-settings");
    assert_eq!(after, before);
    assert!(harness.bounds("settings-group-recording-audio").top() < viewport.bottom());
}

#[gpui::test]
fn connected_settings_exposes_runtime_inputs_and_saves_one_validated_snapshot(
    cx: &mut TestAppContext,
) {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let mut harness = Harness::open_connected(cx, Arc::clone(&commands));

    harness.bounds("settings-input-transcription-provider");
    harness.bounds("settings-input-transcription-model");
    harness.bounds("settings-input-language");
    harness.bounds("settings-input-cleanup-model");
    harness.bounds("settings-hotkey-change");
    harness.bounds("settings-input-recording-mode");
    harness.bounds("settings-input-max-recording");
    harness.bounds("settings-input-ducked-volume");
    harness.bounds("settings-input-ducking-fade-out");
    harness.bounds("settings-input-ducking-fade-in");
    harness.bounds("settings-input-paste-shortcut");
    assert!(!harness.has("settings-clipboard"));
    assert!(!harness.has("settings-save-bar"));
    harness.scroll_route_by(-120.);
    harness.click("toggle-cleanup");
    assert!(commands.lock().expect("command lock").is_empty());
    harness.scroll_route_by(10_000.);
    harness.bounds("settings-save-bar");
    harness.click("save-settings");

    let commands = commands.lock().expect("command lock");
    assert_eq!(commands.len(), 1);
    assert!(matches!(
        &commands[0].kind,
        ClientCommandKind::UpdateSettings { settings, .. }
            if settings.hotkey == "Ctrl+Space"
                && settings.recording_mode == "toggle"
                && settings.max_recording_seconds == 300
                && settings.audio_ducking_fade_out_ms == 600
                && settings.audio_ducking_fade_in_ms == 600
                && settings.cleanup_enabled != Settings::default().cleanup_enabled
    ));
}

#[gpui::test]
fn chatgpt_subscription_replaces_api_controls_with_one_managed_model_status(
    cx: &mut TestAppContext,
) {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let mut harness = Harness::open_connected(cx, Arc::clone(&commands));

    harness.shell.update_in(harness.cx, |shell, window, cx| {
        shell.select_transcription_provider_for_test(
            TranscriptionProvider::ChatGptSubscription,
            window,
            cx,
        );
    });
    harness.cx.run_until_parked();

    harness.bounds("settings-input-transcription-provider");
    harness.bounds("settings-transcription-managed-by-chatgpt");
    harness.bounds("settings-input-language");
    harness.bounds("settings-api-key");
    assert!(!harness.has("settings-input-transcription-model"));
    assert!(!harness.has("settings-input-transcription-prompt"));
    assert!(!harness.has("settings-model-catalog-builtin"));
    harness.click("save-settings");

    let commands = commands.lock().expect("command lock");
    assert_eq!(commands.len(), 1);
    assert!(matches!(
        &commands[0].kind,
        ClientCommandKind::UpdateSettings { settings, .. }
            if settings.transcription_provider == TranscriptionProvider::ChatGptSubscription
    ));
}

#[gpui::test]
fn shortcut_capture_accepts_a_supported_chord_and_saves_it(cx: &mut TestAppContext) {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let mut harness = Harness::open_connected(cx, Arc::clone(&commands));

    harness.scroll_route_by(-600.);
    harness.click("settings-hotkey-change");
    harness.bounds("settings-hotkey-capture");
    harness.bounds("settings-hotkey-cancel");
    harness.cx.simulate_keystrokes("ctrl-alt-d");
    harness.cx.run_until_parked();

    assert!(!harness.has("settings-hotkey-capture"));
    harness.bounds("settings-save-bar");
    harness.scroll_route_by(10_000.);
    harness.click("save-settings");

    let commands = commands.lock().expect("command lock");
    assert!(matches!(
        &commands[0].kind,
        ClientCommandKind::UpdateSettings { settings, .. }
            if settings.hotkey == "Ctrl+Alt+D"
    ));
}

#[gpui::test]
fn successful_api_key_save_clears_the_secret_field(cx: &mut TestAppContext) {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let mut harness = Harness::open_connected(cx, Arc::clone(&commands));

    harness.type_text("settings-api-key-input", "sk-test-secret");
    harness.click("save-api-key");
    assert!(!harness.has("settings-feedback"));
    harness.click("save-api-key");
    harness.bounds("api-key-feedback");

    let commands = commands.lock().expect("command lock");
    assert_eq!(commands.len(), 1);
    assert!(matches!(
        &commands[0].kind,
        ClientCommandKind::SetApiKey { api_key, .. }
            if api_key.expose_secret() == "sk-test-secret"
    ));
}
