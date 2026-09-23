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
    ClientCommand, ClientCommandKind, Hotkey, HotkeyCaptureOutcome, HotkeyModifier, Settings,
    VocabularyEntry, WorkflowPhase, WorkflowSnapshot, parse_vocabulary,
};
use agentdictate_ui::{
    AgentDictateWindowFrame, CommandSink, HistoryViewModel, HotkeyCaptureSink,
    RecoveryItemViewModel, RecoveryStage, Route, SettingsShell, ShellViewModel,
    TranscriptViewModel, UsageDayViewModel, UsagePeriod, UsageTotals, UsageViewModel,
    WorkspaceAction, WorkspaceActionSink, WorkspaceViewModel, test_support,
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

/// Root, tooltips and popovers paint from gpui-component's resolved token
/// copy, which only follows the app palette when the theme setup syncs it.
#[gpui::test]
fn component_surfaces_paint_with_the_app_palette(cx: &mut TestAppContext) {
    test_support::initialize(cx);

    cx.update(|cx| {
        let theme = Theme::global(cx);
        assert_eq!(theme.tokens.background.color, theme.background);
        assert_eq!(theme.tokens.popover.color, theme.popover);
        assert_eq!(theme.tokens.border.color, theme.border);
    });
}

#[gpui::test]
fn single_line_clip_preserves_the_complete_shaped_text_run(cx: &mut TestAppContext) {
    let harness = Harness::open(cx);
    let value = "A complete transcript must remain shaped beyond the clipping viewport.";

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
        action_sink: WorkspaceActionSink,
    ) -> Self {
        Self::open_shell(
            cx,
            viewport,
            model,
            Settings::default(),
            false,
            Arc::new(|_| Ok(())),
            no_capture(),
            action_sink,
        )
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
        Self::open_connected_capturing(cx, model, settings, has_api_key, commands, no_capture())
    }

    /// Opens Settings on a fake daemon whose shortcut capture answers with
    /// `hotkey_capture`.
    fn open_connected_capturing(
        cx: &mut TestAppContext,
        model: ShellViewModel,
        settings: Settings,
        has_api_key: bool,
        commands: Arc<Mutex<Vec<ClientCommand>>>,
        hotkey_capture: HotkeyCaptureSink,
    ) -> Self {
        let workspace = model.workspace.clone();
        Self::open_shell(
            cx,
            size(px(1_100.), px(780.)),
            model,
            settings,
            has_api_key,
            Arc::new(move |command| {
                commands.lock().expect("command lock").push(command);
                Ok(())
            }),
            hotkey_capture,
            Arc::new(move |_| Ok(workspace.clone())),
        )
    }

    fn open_settings(
        cx: &mut TestAppContext,
        commands: Arc<Mutex<Vec<ClientCommand>>>,
        viewport: Size<Pixels>,
    ) -> Self {
        Self::open_shell(
            cx,
            viewport,
            ShellViewModel::from_snapshot(
                Route::Settings,
                WorkflowSnapshot {
                    phase: WorkflowPhase::Ready,
                },
            ),
            Settings::default(),
            false,
            Arc::new(move |command| {
                commands.lock().expect("command lock").push(command);
                Ok(())
            }),
            no_capture(),
            Arc::new(|_| Ok(WorkspaceViewModel::default())),
        )
    }

    /// Opens the production window composition (Root, frame, shell) headlessly.
    #[allow(clippy::too_many_arguments)]
    fn open_shell(
        cx: &mut TestAppContext,
        viewport: Size<Pixels>,
        model: ShellViewModel,
        settings: Settings,
        has_api_key: bool,
        command_sink: CommandSink,
        hotkey_capture: HotkeyCaptureSink,
        action_sink: WorkspaceActionSink,
    ) -> Self {
        test_support::initialize(cx);
        let shell_slot = Rc::new(RefCell::new(None));
        let window_slot = Rc::clone(&shell_slot);
        let window = cx.update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        viewport,
                    ))),
                    ..Default::default()
                },
                move |window, cx| {
                    let shell = cx.new(|cx| {
                        SettingsShell::new(
                            model,
                            settings,
                            has_api_key,
                            command_sink,
                            hotkey_capture,
                            action_sink,
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

    fn move_to(&mut self, selector: &'static str) {
        let position = self.bounds(selector).center();
        self.cx
            .simulate_mouse_move(position, None::<MouseButton>, Modifiers::none());
    }

    fn click(&mut self, selector: &'static str) {
        self.move_to("resize-right");
        self.click_direct(selector);
    }

    fn click_at(&mut self, position: gpui::Point<Pixels>) {
        self.cx
            .simulate_mouse_move(position, None::<MouseButton>, Modifiers::none());
        self.cx.simulate_click(position, Modifiers::none());
        self.cx.run_until_parked();
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

    fn usage_period(&mut self) -> UsagePeriod {
        self.shell.read_with(self.cx, |shell, _| {
            shell.view_model().workspace.usage.period
        })
    }
}

/// A complete, unsearched history page.
fn history(
    recoveries: Vec<RecoveryItemViewModel>,
    transcripts: Vec<TranscriptViewModel>,
) -> HistoryViewModel {
    let count = transcripts.len() as u64;
    HistoryViewModel::from_page(recoveries, transcripts, count, String::new(), false)
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
        history: history(
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
    // Recovery copies instead of pasting into this window, so say so.
    harness.bounds("workspace-feedback");
    let feedback = harness.shell.read_with(harness.cx, |shell, _| {
        shell
            .route_feedback_for_test(Route::History)
            .map(str::to_owned)
    });
    assert_eq!(
        feedback.as_deref(),
        Some("Copied — press Ctrl+V where you want it")
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
        history: history(
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
        history: history(
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
fn overview_charts_activity_switches_periods_and_links_to_history(cx: &mut TestAppContext) {
    let actions = Arc::new(Mutex::new(Vec::new()));
    let captured_actions = Arc::clone(&actions);
    let model = ShellViewModel::from_snapshot(
        Route::Overview,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    )
    .with_workspace(WorkspaceViewModel {
        history: history(
            Vec::new(),
            vec![TranscriptViewModel::new(
                17,
                "Today, 14:32",
                "The latest completed dictation stays close at hand.",
                9,
                "0:08",
            )],
        ),
        recent_transcripts: vec![TranscriptViewModel::new(
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

    let shorter_day = harness.bounds("overview-activity-marker-0");
    let longer_day = harness.bounds("overview-activity-marker-1");
    assert!(
        longer_day.top() < shorter_day.top(),
        "the chart should place the day with more dictation time higher"
    );
    harness.bounds("overview-recent-history");
    harness.bounds("overview-recent-transcript-17");
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
            TranscriptViewModel::new(
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
            vec![TranscriptViewModel::new(
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
        history: history(
            Vec::new(),
            vec![TranscriptViewModel::new(
                77,
                "Just now",
                "A live daemon update appeared without reopening the window.",
                10,
                "0:06",
            )],
        ),
        recent_transcripts: vec![TranscriptViewModel::new(
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
fn content_fills_the_frame_and_buttons_click_after_every_resize_zone(cx: &mut TestAppContext) {
    let mut harness = Harness::open(cx);
    assert_eq!(
        harness.bounds("agentdictate-root"),
        harness.bounds("agentdictate-window-frame"),
        "the content must reach every window edge"
    );

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
            TranscriptViewModel::new(
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
        history: history(recoveries, transcripts),
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
fn history_loads_more_without_rendering_the_archive(cx: &mut TestAppContext) {
    let transcripts = (0..20)
        .map(|index| {
            TranscriptViewModel::new(
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

/// Opens History on `transcripts` and records every workspace action.
fn open_history_recording_actions(
    cx: &mut TestAppContext,
    transcripts: Vec<TranscriptViewModel>,
) -> (Harness, Arc<Mutex<Vec<WorkspaceAction>>>) {
    let model = ShellViewModel::from_snapshot(
        Route::History,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    )
    .with_workspace(WorkspaceViewModel {
        history: history(Vec::new(), transcripts),
        ..WorkspaceViewModel::default()
    });
    let refreshed = model.workspace.clone();
    let actions = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&actions);
    let harness = Harness::open_model_with_actions(
        cx,
        size(px(1_100.), px(780.)),
        model,
        Arc::new(move |action| {
            captured.lock().expect("action lock").push(action);
            Ok(refreshed.clone())
        }),
    );
    (harness, actions)
}

#[gpui::test]
fn clicking_a_transcript_expands_its_whole_text_and_clicking_again_collapses_it(
    cx: &mut TestAppContext,
) {
    let text = format!(
        "{}and that is the whole dictation.",
        "A long dictated thought ".repeat(30)
    );
    let (mut harness, actions) = open_history_recording_actions(
        cx,
        vec![TranscriptViewModel::new(
            41,
            "Today 14:18",
            text,
            125,
            "0:48",
        )],
    );
    let collapsed = harness.bounds("history-transcript-item-41");
    assert!(harness.has("history-transcript-title-41"));
    assert!(!harness.has("history-transcript-text-41"));

    harness.click("history-transcript-toggle-41");

    let row = harness.bounds("history-transcript-item-41");
    let text = harness.bounds("history-transcript-text-41");
    assert!(!harness.has("history-transcript-title-41"));
    // The whole transcript wraps onto several lines inside the row.
    assert!(text.size.height > collapsed.size.height);
    assert!(row.size.height > text.size.height);
    assert!(text.right() <= row.right());
    assert!(actions.lock().expect("action lock").is_empty());

    harness.click("history-transcript-toggle-41");

    assert!(!harness.has("history-transcript-text-41"));
    assert!(harness.has("history-transcript-title-41"));
    assert_eq!(harness.bounds("history-transcript-item-41"), collapsed);
}

#[gpui::test]
fn a_successful_copy_says_copied_on_its_button_for_a_moment(cx: &mut TestAppContext) {
    let (mut harness, actions) = open_history_recording_actions(
        cx,
        vec![
            TranscriptViewModel::new(41, "Today 14:18", "Ship the recovery flow.", 4, "0:03"),
            TranscriptViewModel::new(42, "Today 14:20", "And the release notes.", 4, "0:03"),
        ],
    );

    harness.click("history-copy-transcript-41");

    assert_eq!(
        actions.lock().expect("action lock").as_slice(),
        &[WorkspaceAction::CopyTranscript { id: 41 }]
    );
    assert!(harness.has("copied-history-copy-transcript-41"));
    assert!(harness.has("history-copy-transcript-42"));
    assert!(!harness.has("workspace-feedback"));

    harness
        .cx
        .executor()
        .advance_clock(Duration::from_millis(1_400));
    harness.cx.run_until_parked();
    assert!(harness.has("copied-history-copy-transcript-41"));

    harness
        .cx
        .executor()
        .advance_clock(Duration::from_millis(200));
    harness.cx.run_until_parked();
    assert!(!harness.has("copied-history-copy-transcript-41"));
    assert!(harness.has("history-copy-transcript-41"));
}

#[gpui::test]
fn deleting_a_transcript_asks_to_confirm_delete_first(cx: &mut TestAppContext) {
    let (mut harness, actions) = open_history_recording_actions(
        cx,
        vec![TranscriptViewModel::new(
            41,
            "Today 14:18",
            "A private note.",
            3,
            "0:02",
        )],
    );

    harness.click("history-delete-transcript-41");

    assert!(actions.lock().expect("action lock").is_empty());
    assert!(harness.has("confirm-history-delete-transcript-41"));
    let guidance = harness.shell.read_with(harness.cx, |shell, _| {
        shell
            .route_feedback_for_test(Route::History)
            .map(str::to_owned)
    });
    assert_eq!(
        guidance.as_deref(),
        Some("Click Confirm delete to delete it permanently, or continue elsewhere to cancel.")
    );

    harness.click("confirm-history-delete-transcript-41");

    assert_eq!(
        actions.lock().expect("action lock").as_slice(),
        &[WorkspaceAction::DeleteTranscript { id: 41 }]
    );
    assert!(!harness.has("confirm-history-delete-transcript-41"));
}

#[gpui::test]
fn deleting_all_history_from_settings_asks_to_confirm_delete_first(cx: &mut TestAppContext) {
    let actions = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&actions);
    let mut harness = Harness::open_model_with_actions(
        cx,
        size(px(1_100.), px(780.)),
        ShellViewModel::from_snapshot(
            Route::Settings,
            WorkflowSnapshot {
                phase: WorkflowPhase::Ready,
            },
        ),
        Arc::new(move |action| {
            captured.lock().expect("action lock").push(action);
            Ok(WorkspaceViewModel::default())
        }),
    );

    harness.scroll_to("settings-delete-all-history");
    harness.click("settings-delete-all-history");
    assert!(actions.lock().expect("action lock").is_empty());
    harness.click("confirm-settings-delete-all-history");

    assert_eq!(
        actions.lock().expect("action lock").as_slice(),
        &[WorkspaceAction::ClearHistory]
    );
    let feedback = harness.shell.read_with(harness.cx, |shell, _| {
        shell
            .route_feedback_for_test(Route::Settings)
            .map(str::to_owned)
    });
    assert_eq!(feedback.as_deref(), Some("All history deleted"));
}

#[gpui::test]
fn connected_history_search_emits_the_latest_query_without_a_fixed_delay(cx: &mut TestAppContext) {
    let model = ShellViewModel::from_snapshot(
        Route::History,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    );
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
            TranscriptViewModel::new(
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
        history: history(Vec::new(), transcripts),
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

    harness.click(Route::Settings.navigation_id());

    let settings = harness.bounds("settings-page");
    assert!(settings.top() >= viewport.top());
    assert!(settings.top() <= viewport.top() + px(32.));
}

#[gpui::test]
fn settings_scrollbar_keeps_the_viewport_height_while_content_moves(cx: &mut TestAppContext) {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let mut harness = Harness::open_connected(cx, commands);
    let viewport = harness.bounds("route-content");
    let before = harness.bounds("route-scrollbar-settings");
    assert!((before.size.height - viewport.size.height).abs() <= px(1.));

    harness.scroll_to("settings-group-recording-audio");

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

    harness.bounds("settings-input-language");
    harness.bounds("settings-hotkey-change");
    harness.bounds("settings-input-recording-mode");
    harness.bounds("settings-input-max-recording");
    harness.bounds("settings-input-ducked-volume");
    harness.bounds("settings-input-ducking-fade-out");
    harness.bounds("settings-input-ducking-fade-in");
    harness.bounds("settings-input-paste-shortcut");
    harness.bounds("settings-input-keep-transcripts");
    assert!(!harness.has("settings-save-bar"));
    harness.scroll_route_by(-120.);
    harness.scroll_to("toggle-streaming");
    harness.click("toggle-streaming");
    assert!(commands.lock().expect("command lock").is_empty());
    harness.scroll_route_by(10_000.);
    harness.bounds("settings-save-bar");
    harness.click("save-settings");

    let commands = commands.lock().expect("command lock");
    assert_eq!(commands.len(), 1);
    assert!(matches!(
        &commands[0].kind,
        ClientCommandKind::UpdateSettings { settings, .. }
            if settings.hotkey == Hotkey::default()
                && settings.recording_mode == agentdictate_core::RecordingMode::Toggle
                && settings.max_recording_seconds == 300
                && settings.audio_ducking_fade_out_ms == 600
                && settings.audio_ducking_fade_in_ms == 600
                && settings.streaming_enabled != Settings::default().streaming_enabled
    ));
}

#[gpui::test]
fn shortcut_capture_saves_the_physical_key_the_daemon_captured(cx: &mut TestAppContext) {
    // Ctrl+A on AZERTY: the physical Q key, which QWERTY names "Q".
    let captured = Hotkey::captured(
        std::collections::BTreeSet::from([HotkeyModifier::Ctrl]),
        16,
        "A",
    );
    let commands = Arc::new(Mutex::new(Vec::new()));
    let mut harness = open_settings_capturing(
        cx,
        Arc::clone(&commands),
        HotkeyCaptureOutcome::Captured {
            hotkey: captured.clone(),
        },
    );

    harness.scroll_to("settings-hotkey-change");
    harness.click("settings-hotkey-change");

    assert!(!harness.has("settings-hotkey-capture"));
    assert!(!harness.has("settings-hotkey-capture-error"));
    harness.bounds("settings-save-bar");
    harness.scroll_route_by(10_000.);
    harness.click("save-settings");

    let commands = commands.lock().expect("command lock");
    assert!(matches!(
        &commands[0].kind,
        ClientCommandKind::UpdateSettings { settings, .. }
            if settings.hotkey == captured && settings.hotkey.key() == 16
    ));
}

#[gpui::test]
fn a_capture_that_times_out_says_so_and_keeps_the_shortcut(cx: &mut TestAppContext) {
    let mut harness = open_settings_capturing(
        cx,
        Arc::new(Mutex::new(Vec::new())),
        HotkeyCaptureOutcome::TimedOut,
    );

    harness.scroll_to("settings-hotkey-change");
    harness.click("settings-hotkey-change");

    harness.bounds("settings-hotkey-capture-error");
    harness.bounds("settings-hotkey-change");
    assert!(!harness.has("settings-save-bar"));
}

/// A fake daemon that never captures, for tests that do not press Change.
fn no_capture() -> HotkeyCaptureSink {
    Arc::new(|| Ok(HotkeyCaptureOutcome::Cancelled))
}

/// Opens Settings on a fake daemon that answers every capture with `outcome`.
fn open_settings_capturing(
    cx: &mut TestAppContext,
    commands: Arc<Mutex<Vec<ClientCommand>>>,
    outcome: HotkeyCaptureOutcome,
) -> Harness {
    Harness::open_connected_capturing(
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
        Arc::new(move || Ok(outcome.clone())),
    )
}

#[gpui::test]
fn populated_multiline_fields_accept_clicks_across_their_visible_width(cx: &mut TestAppContext) {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let settings = Settings {
        transcription_prompt:
            "The speaker is describing software changes, filenames, and project terminology."
                .repeat(3),
        project_context:
            "Current project context includes a long description that wraps across the editor."
                .repeat(3),
        ..Settings::default()
    };
    let model = ShellViewModel::from_snapshot(
        Route::Settings,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    );
    let mut harness =
        Harness::open_connected_with(cx, model, settings, true, Arc::clone(&commands));
    for selector in [
        "settings-input-transcription-prompt-control",
        "settings-input-project-context-control",
    ] {
        harness.scroll_to(selector);
        let control = harness.bounds(selector);
        assert!(control.size.width > px(500.));
        let position = point(
            control.left() + control.size.width * 0.75,
            control.center().y,
        );
        harness.cx.simulate_click(position, Modifiers::none());
        harness.cx.simulate_input("Z");
        harness.cx.run_until_parked();
        harness.click("save-settings");
    }
    let commands = commands.lock().unwrap();
    let commands: Vec<_> = commands
        .iter()
        .filter(|command| matches!(command.kind, ClientCommandKind::UpdateSettings { .. }))
        .collect();
    assert_eq!(commands.len(), 2);
    assert!(
        matches!(&commands[1].kind, ClientCommandKind::UpdateSettings { settings, .. }
        if settings.transcription_prompt.contains('Z') && settings.project_context.contains('Z'))
    );
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

#[gpui::test]
fn save_and_discard_remain_clickable_at_the_bottom_of_settings(cx: &mut TestAppContext) {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let mut harness = Harness::open_settings(cx, Arc::clone(&commands), size(px(720.), px(520.)));
    let viewport = harness.bounds("route-content");
    harness.cx.simulate_event(ScrollWheelEvent {
        position: viewport.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(-10_000.))),
        ..Default::default()
    });
    harness.cx.run_until_parked();
    harness.click("toggle-preserve-audio");

    let scroll_area = harness.bounds("route-content");
    for selector in ["save-settings", "discard-settings"] {
        let button = harness.bounds(selector);
        assert!(
            button.top() >= scroll_area.bottom(),
            "{selector} scrolled out of view"
        );
        assert!(
            button.bottom() <= px(520.),
            "{selector} is below the window"
        );
    }
    harness.click("discard-settings");
    assert!(commands.lock().unwrap().is_empty());
    assert_eq!(harness.bounds("route-content"), viewport);

    harness.click("toggle-preserve-audio");
    harness.click("save-settings");
    let commands = commands.lock().unwrap();
    assert_eq!(commands.len(), 1);
    assert!(matches!(
        &commands[0].kind,
        ClientCommandKind::UpdateSettings { settings, .. } if settings.preserve_temp_audio
    ));
    let feedback = harness.bounds("settings-feedback");
    assert!(feedback.top() >= viewport.top());
    assert!(feedback.bottom() <= px(520.));
}

#[gpui::test]
fn maximum_recording_step_buttons_are_real_click_targets(cx: &mut TestAppContext) {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let mut harness =
        Harness::open_settings(cx, Arc::clone(&commands), size(px(1_100.), px(1_400.)));
    harness.scroll_to("settings-input-max-recording-control");
    let control = harness.bounds("settings-input-max-recording-control");

    harness.click_at(point(control.right() - px(14.), control.center().y));
    harness.click("save-settings");
    assert!(matches!(
        &commands.lock().expect("command lock")[0].kind,
        ClientCommandKind::UpdateSettings { settings, .. }
            if settings.max_recording_seconds == Settings::default().max_recording_seconds + 1
    ));

    let viewport = harness.bounds("route-content");
    harness.cx.simulate_event(ScrollWheelEvent {
        position: viewport.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(-200.))),
        ..Default::default()
    });
    harness.cx.run_until_parked();
    let control = harness.bounds("settings-input-max-recording-control");
    harness.click_at(point(control.left() + px(14.), control.center().y));
    harness.click("save-settings");
    assert!(matches!(
        &commands.lock().expect("command lock")[1].kind,
        ClientCommandKind::UpdateSettings { settings, .. }
            if settings.max_recording_seconds == Settings::default().max_recording_seconds
    ));
}

#[gpui::test]
fn discard_restores_the_persisted_toggle_without_writing(cx: &mut TestAppContext) {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let mut harness =
        Harness::open_settings(cx, Arc::clone(&commands), size(px(1_100.), px(1_400.)));

    harness.scroll_to("toggle-streaming");
    harness.click("toggle-streaming");
    harness.bounds("settings-save-bar");
    harness.click("discard-settings");

    assert!(commands.lock().expect("command lock").is_empty());

    // A second toggle must start from the persisted `true` value. Saving it as
    // `false` proves that Discard restored the draft instead of leaving the
    // first click in memory.
    harness.scroll_to("toggle-streaming");
    harness.click("toggle-streaming");
    harness.click("save-settings");
    let commands = commands.lock().expect("command lock");
    assert_eq!(commands.len(), 1);
    assert!(matches!(
        &commands[0].kind,
        ClientCommandKind::UpdateSettings { settings, .. } if settings.streaming_enabled
    ));
}

/// Opens `route` with `vocabulary` saved and records every daemon command.
fn open_with_vocabulary(
    cx: &mut TestAppContext,
    model: ShellViewModel,
    vocabulary: &str,
) -> (Harness, Arc<Mutex<Vec<ClientCommand>>>) {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let settings = Settings {
        vocabulary: parse_vocabulary(vocabulary).unwrap(),
        ..Settings::default()
    };
    let harness = Harness::open_connected_with(cx, model, settings, false, Arc::clone(&commands));
    (harness, commands)
}

fn open_words(
    cx: &mut TestAppContext,
    vocabulary: &str,
) -> (Harness, Arc<Mutex<Vec<ClientCommand>>>) {
    let model = ShellViewModel::from_snapshot(
        Route::Words,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    );
    open_with_vocabulary(cx, model, vocabulary)
}

/// The vocabulary each settings update sent, in order.
fn saved_vocabularies(commands: &Mutex<Vec<ClientCommand>>) -> Vec<Vec<VocabularyEntry>> {
    commands
        .lock()
        .expect("command lock")
        .iter()
        .filter_map(|command| match &command.kind {
            ClientCommandKind::UpdateSettings { settings, .. } => Some(settings.vocabulary.clone()),
            _ => None,
        })
        .collect()
}

#[gpui::test]
fn adding_a_word_saves_it_at_once_and_says_saved(cx: &mut TestAppContext) {
    let (mut harness, commands) = open_words(cx, "");
    assert!(harness.has("words-empty"));

    harness.type_text("words-new-spelling", "Leadlord");
    harness.type_text("words-new-sounds-like", "lead lord, lead load");
    harness.click("words-add");

    assert_eq!(
        saved_vocabularies(&commands),
        [parse_vocabulary("Leadlord = lead lord, lead load").unwrap()]
    );
    assert!(harness.has("words-saved"));
    assert!(harness.has("word-row-0"));
    assert!(!harness.has("words-empty"));

    // The add row was cleared, so adding again asks for a spelling.
    harness.click("words-add");
    assert!(harness.has("words-error"));
    assert_eq!(saved_vocabularies(&commands).len(), 1);

    harness
        .cx
        .executor()
        .advance_clock(Duration::from_millis(1_600));
    harness.cx.run_until_parked();
    assert!(!harness.has("words-saved"));
}

#[gpui::test]
fn editing_a_word_saves_its_new_sounds_like(cx: &mut TestAppContext) {
    let (mut harness, commands) = open_words(cx, "Leadlord = lead lord\nClaude Code");

    harness.click("word-edit-0");
    harness.click("word-editor-sounds-like");
    harness.cx.simulate_keystrokes("ctrl-a");
    harness.cx.simulate_input("lead lord, lead load");
    harness.cx.run_until_parked();
    harness.click("word-editor-done");

    assert_eq!(
        saved_vocabularies(&commands),
        [parse_vocabulary("Leadlord = lead lord, lead load\nClaude Code").unwrap()]
    );
    assert!(!harness.has("word-editor-done"));
    assert!(harness.has("words-saved"));
}

#[gpui::test]
fn deleting_a_word_saves_the_rest(cx: &mut TestAppContext) {
    let (mut harness, commands) = open_words(cx, "Leadlord = lead lord\nClaude Code");

    harness.click("word-delete-0");

    assert_eq!(
        saved_vocabularies(&commands),
        [parse_vocabulary("Claude Code").unwrap()]
    );
    assert!(harness.has("word-row-0"));
    assert!(!harness.has("word-row-1"));
}

#[gpui::test]
fn a_duplicate_spelling_is_refused_inline_without_saving(cx: &mut TestAppContext) {
    let (mut harness, commands) = open_words(cx, "Leadlord = lead lord");

    harness.type_text("words-new-spelling", "leadlord");
    harness.click("words-add");

    assert!(saved_vocabularies(&commands).is_empty());
    assert!(harness.has("words-error"));
    assert!(!harness.has("words-saved"));
}

/// Words saves at once while Settings keeps its Save button, so neither may
/// overwrite the other's change.
#[gpui::test]
fn a_words_change_leaves_unsaved_settings_for_settings_to_save(cx: &mut TestAppContext) {
    let model = ShellViewModel::from_snapshot(
        Route::Settings,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    );
    let (mut harness, commands) = open_with_vocabulary(cx, model, "");
    harness.scroll_to("toggle-streaming");
    harness.click("toggle-streaming");

    harness.click(Route::Words.navigation_id());
    harness.type_text("words-new-spelling", "Codex");
    harness.click("words-add");
    harness.click(Route::Settings.navigation_id());
    harness.click("save-settings");

    let commands = commands.lock().expect("command lock");
    let [words, settings] = commands.as_slice() else {
        panic!("expected two settings updates, got {commands:?}");
    };
    let codex = parse_vocabulary("Codex").unwrap();
    assert!(matches!(
        &words.kind,
        ClientCommandKind::UpdateSettings { settings, .. }
            if settings.vocabulary == codex
                && settings.streaming_enabled == Settings::default().streaming_enabled
    ));
    assert!(matches!(
        &settings.kind,
        ClientCommandKind::UpdateSettings { settings, .. }
            if settings.vocabulary == codex
                && settings.streaming_enabled != Settings::default().streaming_enabled
    ));
}

#[gpui::test]
fn fix_a_word_in_an_expanded_transcript_adds_what_was_heard_to_words(cx: &mut TestAppContext) {
    let model = ShellViewModel::from_snapshot(
        Route::History,
        WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
    )
    .with_workspace(WorkspaceViewModel {
        history: history(
            Vec::new(),
            vec![TranscriptViewModel::new(
                41,
                "Today 14:18",
                "We shipped lead lord today.",
                5,
                "0:04",
            )],
        ),
        ..WorkspaceViewModel::default()
    });
    let (mut harness, commands) = open_with_vocabulary(cx, model, "Leadlord\nClaude Code");
    assert!(!harness.has("history-fix-word-41"));

    harness.click("history-transcript-toggle-41");
    harness.click("history-fix-word-41");
    harness.click("history-fix-word-save");
    assert!(harness.has("history-fix-word-error"));
    assert!(saved_vocabularies(&commands).is_empty());

    harness.type_text("history-fix-word-heard", "lead lord");
    harness.type_text("history-fix-word-spelling", "Leadlord");
    harness.click("history-fix-word-save");

    assert_eq!(
        saved_vocabularies(&commands),
        [parse_vocabulary("Leadlord = lead lord\nClaude Code").unwrap()]
    );
    assert!(!harness.has("history-fix-word-editor-41"));
    assert!(harness.has("added-history-fix-word-41"));
}
