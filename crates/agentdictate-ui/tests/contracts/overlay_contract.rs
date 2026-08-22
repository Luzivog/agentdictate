//! Overlay model, placement, and waveform contracts.

use agentdictate_core::{JobId, Workflow, WorkflowSignal};
use agentdictate_ui::{
    ActiveRecordingPresentation, LogicalRect, LogicalSize, OVERLAY_BOTTOM_GAP, OVERLAY_HEIGHT,
    OVERLAY_WIDTH, OverlayPlacement, OverlayPresentation, OverlayState, OverlayWindowPolicy,
    StatusTone, WaveformArea, WaveformFrame, fit_waveform, format_elapsed, intersect_logical_rects,
    recording_overlay_layout, sample_recent_wav, waveform_bars,
};
use std::{fs, path::PathBuf};

#[test]
fn hidden_workflow_does_not_require_an_overlay_window() {
    assert!(!OverlayState::Hidden.is_visible());
}

#[test]
fn overlay_window_policy_never_takes_focus_or_input_from_the_paste_target() {
    let policy = OverlayWindowPolicy::focus_neutral();

    assert!(!policy.focusable);
    assert!(!policy.accepts_input);
    assert!(!policy.requests_activation);
    assert!(!policy.show_in_taskbar);
    assert_eq!(OverlayState::Recording.window_policy(), policy);
    assert_eq!(
        OverlayState::Recording.stable_id(),
        "recording-overlay-recording"
    );
    assert_eq!(
        OverlayState::Recording.accessibility_label(),
        "Agent Dictate: Listening…"
    );
}

#[test]
fn vendored_gpui_x11_popup_bypasses_the_window_manager() {
    let x11_window_source =
        include_str!("../../../../vendor/gpui-0.2.2/src/platform/linux/x11/window.rs");

    assert!(
        x11_window_source.contains(".override_redirect((params.kind == WindowKind::PopUp) as u32)")
    );
}

#[test]
fn overlay_visibility_matches_the_previous_three_active_presentations() {
    for state in [
        OverlayState::Recording,
        OverlayState::Transcribing,
        OverlayState::Cleaning,
    ] {
        assert!(state.is_visible(), "{state:?} should open the overlay");
    }
    assert_eq!(OverlayState::Transcribing.label(), "Transcribing");
    assert_eq!(OverlayState::Cleaning.label(), "Cleaning up...");

    let state = OverlayState::recoverable_failure("Could not paste", "Copy again");
    for state in [
        OverlayState::Hidden,
        OverlayState::Starting,
        OverlayState::Finishing,
        OverlayState::ReadyToDeliver,
        OverlayState::Delivering,
        state.clone(),
    ] {
        assert!(!state.is_visible(), "{state:?} belongs outside the overlay");
    }

    assert_eq!(state.label(), "Could not paste");
    assert_eq!(state.action_label(), Some("Copy again"));
    assert_eq!(state.tone(), StatusTone::Danger);
}

#[test]
fn overlay_is_centered_above_the_primary_monitor_work_area_bottom() {
    let placement = OverlayPlacement::bottom_centered(
        LogicalRect::new(1_920, 0, 1_440, 860),
        LogicalSize::new(336, 64),
        24,
    );

    assert_eq!(placement.frame, LogicalRect::new(2_472, 772, 336, 64));
}

#[test]
fn overlay_fits_inside_a_constrained_work_area() {
    let placement = OverlayPlacement::bottom_centered(
        LogicalRect::new(-280, 24, 280, 48),
        LogicalSize::new(336, 64),
        24,
    );

    assert_eq!(placement.frame, LogicalRect::new(-280, 24, 280, 24));
}

#[test]
fn restored_overlay_keeps_the_previous_size_and_bottom_offset() {
    let placement = OverlayPlacement::bottom_centered(
        LogicalRect::new(0, 0, 1_920, 1_040),
        LogicalSize::new(OVERLAY_WIDTH, OVERLAY_HEIGHT),
        OVERLAY_BOTTOM_GAP,
    );

    assert_eq!(placement.frame, LogicalRect::new(888, 912, 143, 56));
}

#[test]
fn virtual_x11_work_area_is_clipped_to_the_primary_monitor_before_placement() {
    let virtual_work_area = LogicalRect::new(0, 0, 3_840, 1_040);
    let primary_display = LogicalRect::new(1_920, 0, 1_920, 1_080);

    let primary_work_area = intersect_logical_rects(virtual_work_area, primary_display)
        .expect("the virtual work area overlaps the primary monitor");
    assert_eq!(primary_work_area, LogicalRect::new(1_920, 0, 1_920, 1_040));
    assert_eq!(
        OverlayPlacement::bottom_centered(
            primary_work_area,
            LogicalSize::new(OVERLAY_WIDTH, OVERLAY_HEIGHT),
            OVERLAY_BOTTOM_GAP,
        )
        .frame,
        LogicalRect::new(2_808, 912, 143, 56),
    );
    assert_eq!(
        intersect_logical_rects(LogicalRect::new(-1_920, 0, 1_920, 1_040), primary_display,),
        None,
    );
}

#[test]
fn recording_presentation_keeps_audio_telemetry_outside_the_workflow_snapshot() {
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
            audio_path: PathBuf::from("/tmp/active-recording.wav"),
            started_at_unix_millis: 1_726_000_000_250,
        }),
    };

    assert_eq!(presentation.state(), OverlayState::Recording);
    assert_eq!(
        presentation.active_recording.unwrap().audio_path,
        PathBuf::from("/tmp/active-recording.wav")
    );
}

#[test]
fn stopping_keeps_the_existing_helper_visible_as_transcribing() {
    let job_id = JobId::new();
    let mut workflow = Workflow::new();
    workflow
        .apply(WorkflowSignal::StartRequested { job_id })
        .unwrap();
    workflow
        .apply(WorkflowSignal::FirstAudioFrameWritten { job_id })
        .unwrap();
    workflow.apply(WorkflowSignal::StopRequested).unwrap();

    let state = OverlayState::from(workflow.snapshot());
    assert_eq!(state, OverlayState::Transcribing);
    assert!(state.is_visible());
}

#[test]
fn recording_elapsed_time_uses_an_injected_clock_and_never_goes_negative() {
    let presentation = OverlayPresentation {
        workflow: Workflow::new().snapshot(),
        active_recording: Some(ActiveRecordingPresentation {
            audio_path: PathBuf::from("/tmp/active-recording.wav"),
            started_at_unix_millis: 10_000,
        }),
    };

    assert_eq!(presentation.elapsed_seconds(12_345), 2.345);
    assert_eq!(presentation.elapsed_seconds(9_000), 0.0);
}

#[test]
fn growing_wav_is_sampled_as_44_signed_little_endian_bins() {
    let path = temporary_wav("growing");
    let mut bytes = vec![0_u8; 44];
    let fixture = [0_i16, 0, 32_767, 32_767, -32_768, 0, 8_192, -8_192];
    for sample in fixture.into_iter().chain(std::iter::repeat_n(0, 80)) {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    fs::write(&path, bytes).unwrap();

    let bins = sample_recent_wav(&path);
    fs::remove_file(&path).unwrap();

    assert_eq!(bins.len(), 44);
    assert_close(bins[0], 0.0);
    assert_close(bins[1], 0.999_969_482_421_875);
    assert_close(bins[2], 0.897_487_373_415_291_7);
    assert_close(bins[3], 0.25);
    assert!(bins[4..].iter().all(|value| *value == 0.0));
}

#[test]
fn wav_sampler_uses_only_the_most_recent_2816_samples() {
    let path = temporary_wav("long");
    let mut bytes = vec![0_u8; 44];
    for sample in std::iter::repeat_n(32_767_i16, 64) {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    for sample in std::iter::repeat_n(0_i16, 2_816) {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    fs::write(&path, bytes).unwrap();

    let bins = sample_recent_wav(&path);
    fs::remove_file(&path).unwrap();
    assert!(bins.iter().all(|value| *value == 0.0));
}

#[test]
fn forty_four_source_bins_are_max_fitted_into_the_twenty_visible_bars() {
    let source = (0..44).map(|value| value as f32).collect::<Vec<_>>();

    assert_eq!(
        fit_waveform(&source, 20),
        vec![
            1.0, 3.0, 5.0, 7.0, 10.0, 12.0, 14.0, 16.0, 18.0, 21.0, 23.0, 25.0, 27.0, 29.0, 32.0,
            34.0, 36.0, 38.0, 40.0, 43.0,
        ]
    );
}

#[test]
fn waveform_frame_applies_the_original_noise_gate_and_asymmetric_smoothing() {
    let mut frame = WaveformFrame::from_levels([
        0.0, 0.0, 0.0, 0.8, 0.25, 0.0, 0.9, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 0.0,
    ]);
    let mut targets = [0.0; 20];
    targets[..7].copy_from_slice(&[0.004, 0.005, 0.07, 0.07, 0.2, 0.135, 1.0]);

    frame.advance(&targets);

    let levels = frame.levels();
    assert_close(levels[0], 0.0);
    assert_close(levels[1], 0.0);
    assert_close(levels[2], 0.31);
    assert_close(levels[3], 0.698);
    assert_close(levels[4], 0.715);
    assert_close(levels[5], 0.62);
    assert_close(levels[6], 0.962);
}

#[test]
fn waveform_geometry_matches_the_previous_143_by_56_overlay() {
    let mut levels = [0.0; 20];
    levels[10] = 0.25;
    levels[19] = 1.0;

    let bars = waveform_bars(&levels, WaveformArea::new(18.0, 60.0, 27.0));

    assert_eq!(bars.len(), 20);
    assert_close(bars[0].x, 18.0);
    assert_close(bars[0].height, 2.5);
    assert_close(bars[0].alpha, 0.308);
    assert_close(bars[10].x, 48.625);
    assert_close(bars[10].height, 14.620_314_697_154_756);
    assert_close(bars[10].alpha, 0.478);
    assert_close(bars[19].x, 76.1875);
    assert_close(bars[19].height, 22.78);
    assert_close(bars[19].alpha, 0.92);
    assert_close(bars[0].width, 1.8125);
}

#[test]
fn timer_width_dynamically_reserves_non_overlapping_waveform_space() {
    for timer_width in [30.0, 48.0] {
        let layout = recording_overlay_layout(timer_width);
        let bars = waveform_bars(&[0.0; 20], layout.waveform);
        let last_bar = bars.last().expect("twenty bars are laid out");

        assert_close(layout.timer_x + layout.timer_width, 117.0);
        assert!(last_bar.x + last_bar.width <= layout.timer_x - 8.0 + f32::EPSILON);
        assert!(layout.waveform.x >= 12.0);
        assert!(layout.timer_x >= 0.0);
    }
}

#[test]
fn elapsed_timer_uses_the_previous_minute_and_hour_format() {
    assert_eq!(format_elapsed(-1.0), "0:00");
    assert_eq!(format_elapsed(59.9), "0:59");
    assert_eq!(format_elapsed(60.0), "1:00");
    assert_eq!(format_elapsed(3_661.0), "1:01:01");
}

fn assert_close(actual: f32, expected: f64) {
    assert!(
        (f64::from(actual) - expected).abs() < 0.000_01,
        "expected {expected}, got {actual}"
    );
}

fn temporary_wav(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "agentdictate-overlay-{label}-{}-{}.wav",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ))
}
