//! Headless shell interaction contracts.

use super::support::{self, DesktopHarness};

use std::{
    cell::RefCell,
    collections::BTreeSet,
    ops::Deref,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Duration,
};

use agentdictate_core::{
    DictationMode, Hotkey, HotkeyCaptureOutcome, HotkeyModifier, KeepTranscripts, SettingChange,
    Settings, SettingsSnapshot, VocabularyEntry, WorkflowPhase, WorkflowSnapshot, parse_vocabulary,
};
use agentdictate_ui::{
    AgentDictateWindowFrame, HistoryViewModel, HotkeyCaptureSink, RecoveryItemViewModel,
    RecoveryStage, Route, SettingsRequest, SettingsShell, SettingsSink, ShellViewModel,
    TranscriptViewModel, UiActionError, UsageDayViewModel, UsagePeriod, UsageTotals,
    UsageViewModel, WorkspaceAction, WorkspaceActionSink, WorkspaceViewModel, test_support,
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
fn history_set_aside_notice_follows_the_workspace(cx: &mut TestAppContext) {
    let mut harness = Harness::open(cx);
    assert!(!harness.has("history-set-aside-notice"));
    for set_aside in [Some("/data/agentdictate.sqlite.corrupt-1".to_owned()), None] {
        let shown = set_aside.is_some();
        harness.shell.update(harness.cx, |shell, cx| {
            let workspace = shell
                .view_model()
                .workspace
                .clone()
                .with_history_set_aside(set_aside);
            shell.apply_workspace_update(workspace, cx);
        });
        harness.cx.run_until_parked();
        assert_eq!(harness.has("history-set-aside-notice"), shown);
    }
}

#[gpui::test]
fn an_outdated_window_asks_to_be_reopened(cx: &mut TestAppContext) {
    let mut harness = Harness::open(cx);
    assert!(!harness.has("window-outdated-notice"));
    harness.shell.update(harness.cx, |shell, cx| {
        let workspace = shell
            .view_model()
            .workspace
            .clone()
            .with_window_outdated(true);
        shell.apply_workspace_update(workspace, cx);
    });
    harness.cx.run_until_parked();
    assert!(harness.has("window-outdated-notice"));
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
            Route::Home,
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
        let daemon = FakeDaemon::new(Settings::default());
        Self::open_shell(cx, viewport, model, &daemon, action_sink)
    }

    /// Opens Settings on a fake daemon.
    fn open_connected(cx: &mut TestAppContext, daemon: &FakeDaemon) -> Self {
        Self::open_connected_with(
            cx,
            ShellViewModel::from_snapshot(Route::Settings, ready()),
            daemon,
        )
    }

    fn open_connected_with(
        cx: &mut TestAppContext,
        model: ShellViewModel,
        daemon: &FakeDaemon,
    ) -> Self {
        let workspace = model.workspace.clone();
        Self::open_shell(
            cx,
            size(px(1_100.), px(780.)),
            model,
            daemon,
            Arc::new(move |_| Ok(workspace.clone())),
        )
    }

    /// Opens the production window composition (Root, frame, shell) headlessly.
    fn open_shell(
        cx: &mut TestAppContext,
        viewport: Size<Pixels>,
        model: ShellViewModel,
        daemon: &FakeDaemon,
        action_sink: WorkspaceActionSink,
    ) -> Self {
        test_support::initialize(cx);
        let settings = daemon.snapshot();
        let settings_sink = daemon.sink();
        let hotkey_capture = daemon.capture_sink();
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
                            settings_sink,
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

    /// The settings the window shows, with unanswered changes applied.
    fn shown_settings(&mut self) -> Settings {
        self.shell
            .read_with(self.cx, |shell, _| shell.shown_settings_for_test())
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

fn ready() -> WorkflowSnapshot {
    WorkflowSnapshot {
        phase: WorkflowPhase::Ready,
    }
}

/// Stands in for the daemon behind the settings sinks: records each request
/// and answers with the settings it then holds. Like the daemon, it refuses
/// what core refuses, and `refused_hotkey` plays a keyboard that lacks a key.
/// Every shortcut capture ends with `capture`.
#[derive(Clone)]
struct FakeDaemon {
    settings: Arc<Mutex<SettingsSnapshot>>,
    requests: Arc<Mutex<Vec<SettingsRequest>>>,
    refused_hotkey: Option<&'static str>,
    capture: HotkeyCaptureOutcome,
}

impl FakeDaemon {
    fn new(settings: Settings) -> Self {
        Self {
            settings: Arc::new(Mutex::new(SettingsSnapshot::from(&settings))),
            requests: Arc::default(),
            refused_hotkey: None,
            capture: HotkeyCaptureOutcome::Cancelled,
        }
    }

    fn capture_sink(&self) -> HotkeyCaptureSink {
        let outcome = self.capture.clone();
        Arc::new(move || Ok(outcome.clone()))
    }

    fn snapshot(&self) -> SettingsSnapshot {
        self.settings.lock().unwrap().clone()
    }

    fn sink(&self) -> SettingsSink {
        let daemon = self.clone();
        Arc::new(move |request| daemon.handle(request))
    }

    fn handle(&self, request: SettingsRequest) -> Result<SettingsSnapshot, UiActionError> {
        self.requests.lock().unwrap().push(request.clone());
        let mut snapshot = self.settings.lock().unwrap();
        match request {
            SettingsRequest::Change(SettingChange::Hotkey(hotkey))
                if Some(hotkey.label()) == self.refused_hotkey =>
            {
                return Err(format!("{hotkey} is not supported by an active keyboard").into());
            }
            SettingsRequest::Change(change) => change.apply(&mut snapshot.values)?,
            SettingsRequest::SetApiKey(_) => snapshot.has_api_key = true,
            SettingsRequest::CancelHotkeyCapture => {}
        }
        Ok(snapshot.clone())
    }

    fn requests(&self) -> Vec<SettingsRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// The vocabulary each Words save sent, in order.
    fn saved_vocabularies(&self) -> Vec<Vec<VocabularyEntry>> {
        self.requests()
            .into_iter()
            .filter_map(|request| match request {
                SettingsRequest::Change(SettingChange::Vocabulary(vocabulary)) => Some(vocabulary),
                _ => None,
            })
            .collect()
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
        Route::Home,
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
        Route::Home,
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
                "A search result must not replace Home's recent transcripts.",
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
        Route::Home,
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
        Route::Home,
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
        harness.click_direct(Route::Home.navigation_id());
        assert_eq!(harness.active_route(), Route::Home, "after {edge}");
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
    let daemon = FakeDaemon::new(Settings::default());
    let mut harness = Harness::open_connected(cx, &daemon);
    harness.click("settings-show-advanced");
    let viewport = harness.bounds("route-content");
    let before = harness.bounds("route-scrollbar-settings");
    assert!((before.size.height - viewport.size.height).abs() <= px(1.));

    harness.scroll_to("settings-keep-audio");

    let after = harness.bounds("route-scrollbar-settings");
    assert_eq!(after, before);
    assert!(harness.bounds("settings-keep-audio").top() < viewport.bottom());
}

#[gpui::test]
fn a_switch_applies_its_setting_at_once(cx: &mut TestAppContext) {
    let daemon = FakeDaemon::new(Settings::default());
    let mut harness = Harness::open_connected(cx, &daemon);

    harness.click("toggle-lower-sounds");

    assert_eq!(
        daemon.requests(),
        [SettingsRequest::Change(SettingChange::AudioDuckingEnabled(
            false
        ))]
    );
    assert!(!daemon.snapshot().values.audio_ducking_enabled);
    assert!(!harness.shown_settings().audio_ducking_enabled);

    harness.click("toggle-lower-sounds");
    assert!(daemon.snapshot().values.audio_ducking_enabled);
}

#[gpui::test]
fn keeping_transcripts_for_less_time_asks_before_it_deletes(cx: &mut TestAppContext) {
    let daemon = FakeDaemon::new(Settings::default());
    let mut harness = Harness::open_connected(cx, &daemon);
    let choose = |harness: &mut Harness, choice| {
        harness.shell.update(harness.cx, |shell, cx| {
            shell.choose_keep_transcripts_for_test(choice, cx);
        });
        harness.cx.run_until_parked();
    };

    choose(&mut harness, KeepTranscripts::Days30);
    assert!(harness.has("settings-keep-transcripts-warning"));
    harness.click("settings-keep-transcripts-cancel");
    assert!(!harness.has("settings-keep-transcripts-warning"));
    assert!(daemon.requests().is_empty());

    choose(&mut harness, KeepTranscripts::Never);
    harness.click("settings-keep-transcripts-confirm");
    assert_eq!(
        daemon.requests(),
        [SettingsRequest::Change(SettingChange::KeepTranscripts(
            KeepTranscripts::Never
        ))]
    );
    assert!(!harness.has("settings-keep-transcripts-warning"));

    // Keeping more deletes nothing, so it applies at once.
    choose(&mut harness, KeepTranscripts::Forever);
    assert!(!harness.has("settings-keep-transcripts-warning"));
    assert_eq!(
        daemon.snapshot().values.keep_transcripts,
        KeepTranscripts::Forever
    );
}

#[gpui::test]
fn advanced_settings_stay_folded_away_until_asked_for(cx: &mut TestAppContext) {
    let daemon = FakeDaemon::new(Settings::default());
    let mut harness = Harness::open_connected(cx, &daemon);
    assert!(harness.has("settings-language"));
    assert!(harness.has("toggle-start-on-login"));
    assert!(!harness.has("settings-advanced"));

    harness.click("settings-show-advanced");
    harness.scroll_to("toggle-exact-mode");
    harness.click("toggle-exact-mode");

    assert_eq!(
        daemon.snapshot().values.dictation_mode,
        DictationMode::Literal
    );
    harness.scroll_to("settings-show-advanced");
    harness.click("settings-show-advanced");
    assert!(!harness.has("settings-advanced"));
}

/// Ctrl+A on AZERTY: the physical Q key, which QWERTY names "Q".
fn azerty_ctrl_a() -> Hotkey {
    Hotkey::captured(BTreeSet::from([HotkeyModifier::Ctrl]), 16, "A")
}

#[gpui::test]
fn the_shortcut_the_daemon_captures_applies_at_once(cx: &mut TestAppContext) {
    let daemon = FakeDaemon {
        capture: HotkeyCaptureOutcome::Captured {
            hotkey: azerty_ctrl_a(),
        },
        ..FakeDaemon::new(Settings::default())
    };
    let mut harness = Harness::open_connected(cx, &daemon);

    harness.click("settings-hotkey-change");

    assert!(!harness.has("settings-hotkey-capture"));
    assert_eq!(
        daemon.requests(),
        [SettingsRequest::Change(SettingChange::Hotkey(
            azerty_ctrl_a()
        ))]
    );
    assert_eq!(harness.shown_settings().hotkey.key(), 16);
}

#[gpui::test]
fn a_capture_that_times_out_says_so_and_keeps_the_shortcut(cx: &mut TestAppContext) {
    let daemon = FakeDaemon {
        capture: HotkeyCaptureOutcome::TimedOut,
        ..FakeDaemon::new(Settings::default())
    };
    let mut harness = Harness::open_connected(cx, &daemon);

    harness.click("settings-hotkey-change");

    assert!(harness.has("settings-hotkey-capture-error"));
    assert!(harness.has("settings-hotkey-change"));
    assert!(daemon.requests().is_empty());
}

#[gpui::test]
fn a_refused_change_says_why_and_shows_the_saved_value_again(cx: &mut TestAppContext) {
    let daemon = FakeDaemon {
        refused_hotkey: Some("Ctrl+A"),
        capture: HotkeyCaptureOutcome::Captured {
            hotkey: azerty_ctrl_a(),
        },
        ..FakeDaemon::new(Settings::default())
    };
    let mut harness = Harness::open_connected(cx, &daemon);

    harness.click("settings-hotkey-change");

    let row = harness.bounds("settings-hotkey-row");
    assert!(row.contains(&harness.bounds("settings-error").center()));
    assert_eq!(harness.shown_settings().hotkey, Hotkey::default());

    // The next change on the row clears the refusal.
    harness.click("settings-recording-mode-hold");
    assert!(!harness.has("settings-error"));
    assert_eq!(
        daemon.snapshot().values.recording_mode,
        agentdictate_core::RecordingMode::Hold
    );
}

#[gpui::test]
fn about_your_work_saves_when_it_loses_focus_and_says_saved(cx: &mut TestAppContext) {
    let daemon = FakeDaemon::new(Settings {
        transcription_prompt:
            "The speaker is describing software changes, filenames, and project terminology."
                .repeat(3),
        ..Settings::default()
    });
    let mut harness = Harness::open_connected(cx, &daemon);
    // Only an active window tells inputs they lost focus.
    harness.cx.update(|window, _| window.activate_window());
    harness.click("settings-show-advanced");
    harness.scroll_to("settings-about-your-work-control");

    // Clicks anywhere across the wrapped text land in the box.
    let control = harness.bounds("settings-about-your-work-control");
    assert!(control.size.width > px(500.));
    harness.click_at(point(
        control.left() + control.size.width * 0.75,
        control.center().y,
    ));
    harness.cx.simulate_input("Z");
    harness.cx.run_until_parked();
    assert!(daemon.requests().is_empty());

    // Clicking the row's label, not a control, moves focus away.
    let label = harness.bounds("settings-about-your-work").origin + point(px(8.), px(24.));
    harness.click_at(label);

    let requests = daemon.requests();
    let [SettingsRequest::Change(SettingChange::TranscriptionPrompt(prompt))] = requests.as_slice()
    else {
        panic!("expected one prompt change, got {:?}", daemon.requests());
    };
    assert!(prompt.contains('Z'));
    assert!(harness.has("settings-saved"));

    harness
        .cx
        .executor()
        .advance_clock(Duration::from_millis(1_600));
    harness.cx.run_until_parked();
    assert!(!harness.has("settings-saved"));

    // Enter saves too, without adding a line.
    harness.click("settings-about-your-work-control");
    harness.cx.simulate_keystrokes("end Y enter");
    harness.cx.run_until_parked();
    assert!(matches!(
        daemon.requests().last(),
        Some(SettingsRequest::Change(SettingChange::TranscriptionPrompt(prompt)))
            if prompt.ends_with('Y')
    ));
}

#[gpui::test]
fn saving_an_api_key_shows_the_key_is_saved_with_a_way_to_replace_it(cx: &mut TestAppContext) {
    let daemon = FakeDaemon::new(Settings::default());
    let mut harness = Harness::open_connected(cx, &daemon);

    harness.click("save-api-key");
    assert!(harness.has("settings-error"));
    assert!(daemon.requests().is_empty());

    harness.type_text("settings-api-key-input", "sk-test-secret");
    harness.click("save-api-key");

    assert_eq!(
        daemon.requests(),
        [SettingsRequest::SetApiKey("sk-test-secret".to_owned())]
    );
    assert!(harness.has("settings-api-key-saved"));
    assert!(!harness.has("settings-api-key-input"));
    assert!(!harness.has("settings-error"));

    harness.click("settings-api-key-replace");
    assert!(harness.has("settings-api-key-input"));
}

/// Opens `model` with `vocabulary` saved on a fake daemon.
fn open_with_vocabulary(
    cx: &mut TestAppContext,
    model: ShellViewModel,
    vocabulary: &str,
) -> (Harness, FakeDaemon) {
    let daemon = FakeDaemon::new(Settings {
        vocabulary: parse_vocabulary(vocabulary).unwrap(),
        ..Settings::default()
    });
    let harness = Harness::open_connected_with(cx, model, &daemon);
    (harness, daemon)
}

fn open_words(cx: &mut TestAppContext, vocabulary: &str) -> (Harness, FakeDaemon) {
    open_with_vocabulary(
        cx,
        ShellViewModel::from_snapshot(Route::Words, ready()),
        vocabulary,
    )
}

#[gpui::test]
fn adding_a_word_saves_it_at_once_and_says_saved(cx: &mut TestAppContext) {
    let (mut harness, daemon) = open_words(cx, "");
    assert!(harness.has("words-empty"));

    harness.type_text("words-new-spelling", "Siobhan");
    harness.type_text("words-new-sounds-like", "shiv on, shiv awn");
    harness.click("words-add");

    assert_eq!(
        daemon.saved_vocabularies(),
        [parse_vocabulary("Siobhan = shiv on, shiv awn").unwrap()]
    );
    assert!(harness.has("words-saved"));
    assert!(harness.has("word-row-0"));
    assert!(!harness.has("words-empty"));

    // The add row was cleared, so adding again asks for a spelling.
    harness.click("words-add");
    assert!(harness.has("words-error"));
    assert_eq!(daemon.saved_vocabularies().len(), 1);

    harness
        .cx
        .executor()
        .advance_clock(Duration::from_millis(1_600));
    harness.cx.run_until_parked();
    assert!(!harness.has("words-saved"));
}

#[gpui::test]
fn editing_a_word_saves_its_new_sounds_like(cx: &mut TestAppContext) {
    let (mut harness, daemon) = open_words(cx, "Siobhan = shiv on\nKubernetes");

    harness.click("word-edit-0");
    harness.click("word-editor-sounds-like");
    harness.cx.simulate_keystrokes("ctrl-a");
    harness.cx.simulate_input("shiv on, shiv awn");
    harness.cx.run_until_parked();
    harness.click("word-editor-done");

    assert_eq!(
        daemon.saved_vocabularies(),
        [parse_vocabulary("Siobhan = shiv on, shiv awn\nKubernetes").unwrap()]
    );
    assert!(!harness.has("word-editor-done"));
    assert!(harness.has("words-saved"));
}

#[gpui::test]
fn deleting_a_word_saves_the_rest(cx: &mut TestAppContext) {
    let (mut harness, daemon) = open_words(cx, "Siobhan = shiv on\nKubernetes");

    harness.click("word-delete-0");

    assert_eq!(
        daemon.saved_vocabularies(),
        [parse_vocabulary("Kubernetes").unwrap()]
    );
    assert!(harness.has("word-row-0"));
    assert!(!harness.has("word-row-1"));
}

#[gpui::test]
fn a_duplicate_spelling_is_refused_inline_without_saving(cx: &mut TestAppContext) {
    let (mut harness, daemon) = open_words(cx, "Siobhan = shiv on");

    harness.type_text("words-new-spelling", "siobhan");
    harness.click("words-add");

    assert!(daemon.saved_vocabularies().is_empty());
    assert!(harness.has("words-error"));
    assert!(!harness.has("words-saved"));
}

#[gpui::test]
fn fix_a_word_in_an_expanded_transcript_adds_what_was_heard_to_words(cx: &mut TestAppContext) {
    let model =
        ShellViewModel::from_snapshot(Route::History, ready()).with_workspace(WorkspaceViewModel {
            history: history(
                Vec::new(),
                vec![TranscriptViewModel::new(
                    41,
                    "Today 14:18",
                    "I met shiv on today.",
                    5,
                    "0:04",
                )],
            ),
            ..WorkspaceViewModel::default()
        });
    let (mut harness, daemon) = open_with_vocabulary(cx, model, "Siobhan\nKubernetes");
    assert!(!harness.has("history-fix-word-41"));

    harness.click("history-transcript-toggle-41");
    harness.click("history-fix-word-41");
    harness.click("history-fix-word-save");
    assert!(harness.has("history-fix-word-error"));
    assert!(daemon.saved_vocabularies().is_empty());

    harness.type_text("history-fix-word-heard", "shiv on");
    harness.type_text("history-fix-word-spelling", "Siobhan");
    harness.click("history-fix-word-save");

    assert_eq!(
        daemon.saved_vocabularies(),
        [parse_vocabulary("Siobhan = shiv on\nKubernetes").unwrap()]
    );
    assert!(!harness.has("history-fix-word-editor-41"));
    assert!(harness.has("added-history-fix-word-41"));
}
