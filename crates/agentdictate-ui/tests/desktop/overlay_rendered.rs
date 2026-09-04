#![cfg(feature = "test-support")]

//! Headless overlay rendering contracts.

use std::{
    fs,
    ops::Deref,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use agentdictate_core::{JobId, Workflow, WorkflowSignal};
use agentdictate_ui::{
    ActiveRecordingPresentation, OVERLAY_HEIGHT, OVERLAY_WIDTH, OverlayPresentation,
    RecordingOverlay, test_support,
};
use gpui::{
    AppContext, Bounds, TestAppContext, VisualTestContext, WindowBounds, WindowOptions, point, px,
    size,
};
use gpui_component::Root;

#[gpui::test]
fn recording_overlay_restores_the_twenty_bar_waveform_and_timer(cx: &mut TestAppContext) {
    let (audio_path, _, cx) = open_recording_overlay(cx, 0);

    let card = cx
        .debug_bounds("recording-overlay-card")
        .expect("overlay card renders");
    assert_eq!(card.size, size(px(127.), px(42.)));
    assert_recording_content_fits(cx);
    for selector in [
        "recording-overlay-wave-0",
        "recording-overlay-wave-1",
        "recording-overlay-wave-2",
        "recording-overlay-wave-3",
        "recording-overlay-wave-4",
        "recording-overlay-wave-5",
        "recording-overlay-wave-6",
        "recording-overlay-wave-7",
        "recording-overlay-wave-8",
        "recording-overlay-wave-9",
        "recording-overlay-wave-10",
        "recording-overlay-wave-11",
        "recording-overlay-wave-12",
        "recording-overlay-wave-13",
        "recording-overlay-wave-14",
        "recording-overlay-wave-15",
        "recording-overlay-wave-16",
        "recording-overlay-wave-17",
        "recording-overlay-wave-18",
        "recording-overlay-wave-19",
    ] {
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing rendered selector: {selector}"));
    }

    fs::remove_file(audio_path).expect("waveform fixture removes");
}

#[gpui::test]
fn hour_long_timer_shrinks_the_waveform_without_overlap_or_clipping(cx: &mut TestAppContext) {
    let (audio_path, _, cx) = open_recording_overlay(cx, 3_661_100);

    let timer = cx
        .debug_bounds("recording-overlay-timer")
        .expect("elapsed timer renders");
    assert!(timer.size.width >= px(40.));
    assert_recording_content_fits(cx);

    fs::remove_file(audio_path).expect("waveform fixture removes");
}

fn open_recording_overlay(
    cx: &mut TestAppContext,
    elapsed_millis: i64,
) -> (
    PathBuf,
    gpui::Entity<RecordingOverlay>,
    &'static mut VisualTestContext,
) {
    test_support::initialize(cx);
    let audio_path = std::env::temp_dir().join(format!(
        "agentdictate-rendered-overlay-{}-{elapsed_millis}.wav",
        std::process::id(),
    ));
    let mut wav = vec![0_u8; 44];
    for sample in std::iter::repeat_n(16_384_i16, 2_816) {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    fs::write(&audio_path, wav).expect("waveform fixture writes");

    let job_id = JobId::new();
    let mut workflow = Workflow::new();
    workflow
        .apply(WorkflowSignal::StartRequested { job_id })
        .unwrap();
    workflow
        .apply(WorkflowSignal::FirstAudioFrameWritten { job_id })
        .unwrap();
    let presentation = OverlayPresentation {
        workflow: workflow.snapshot(),
        active_recording: Some(ActiveRecordingPresentation {
            audio_path: audio_path.clone(),
            started_at_unix_millis: now_unix_millis().saturating_sub(elapsed_millis),
        }),
    };
    let overlay = cx.new(|_| RecordingOverlay::from_presentation(presentation));
    let root = overlay.clone();
    let window = cx.update(|cx| {
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(OVERLAY_WIDTH as f32), px(OVERLAY_HEIGHT as f32)),
                ))),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Root::new(root, window, cx)),
        )
        .expect("headless overlay opens")
    });
    let cx = VisualTestContext::from_window(*window.deref(), cx).into_mut();
    cx.run_until_parked();

    (audio_path, overlay, cx)
}

fn assert_recording_content_fits(cx: &mut VisualTestContext) {
    let card = cx
        .debug_bounds("recording-overlay-card")
        .expect("overlay card renders");
    let timer = cx
        .debug_bounds("recording-overlay-timer")
        .expect("elapsed timer renders");
    let last_bar = cx
        .debug_bounds("recording-overlay-wave-19")
        .expect("last waveform bar renders");

    assert!(timer.left() >= card.left());
    assert!(timer.right() <= card.right() - px(9.));
    assert!(last_bar.right() <= timer.left() - px(7.));
}

fn now_unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[gpui::test]
fn dismissal_preserves_the_rendered_card_while_it_fades(cx: &mut TestAppContext) {
    let (audio_path, overlay, cx) = open_recording_overlay(cx, 1);
    cx.background_executor
        .advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    let opacity = |cx: &mut VisualTestContext| {
        cx.read(|cx| overlay.read(cx).opacity_at(cx.background_executor().now()))
    };
    assert_eq!(opacity(cx), 1.0);
    overlay.update(cx, |overlay, cx| {
        overlay.begin_dismissal(cx.background_executor().now());
        cx.notify();
    });
    cx.background_executor
        .advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    assert!((opacity(cx) - 0.5).abs() < 0.01);
    assert_recording_content_fits(cx);
    // A second dismissal must not reset the fade clock.
    overlay.update(cx, |overlay, cx| {
        overlay.begin_dismissal(cx.background_executor().now());
        cx.notify();
    });
    cx.background_executor
        .advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    assert_eq!(opacity(cx), 0.0);
    cx.debug_bounds("recording-overlay-card")
        .expect("card remains until its owner closes the window");
    fs::remove_file(audio_path).unwrap();
}
