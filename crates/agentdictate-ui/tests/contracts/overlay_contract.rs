//! Overlay model, waveform, timer and fade contracts.

use agentdictate_core::{JobId, Workflow, WorkflowSignal};
use agentdictate_ui::{
    ActiveRecordingPresentation, OverlayPresentation, OverlayState, StatusTone, WAVEFORM_BAR_COUNT,
    WaveformFrame, format_elapsed, recording_overlay_layout, sample_recent_wav, waveform_bars,
};
use agentdictate_ui::{
    OVERLAY_FADE_HOLD, OVERLAY_FADE_IN, OVERLAY_FADE_OUT, overlay_fade_active, overlay_opacity,
};
use std::{fs, path::PathBuf, time::Duration};

#[test]
fn overlay_opens_at_the_start_request_and_stays_through_processing() {
    for state in [
        OverlayState::Starting,
        OverlayState::Recording,
        OverlayState::Transcribing,
    ] {
        assert!(state.is_visible(), "{state:?} should open the overlay");
    }
    assert_eq!(OverlayState::Transcribing.label(), "Transcribing");

    let state = OverlayState::recoverable_failure("Could not paste", "Copy again");
    for state in [
        OverlayState::Hidden,
        OverlayState::Finishing,
        OverlayState::ReadyToDeliver,
        OverlayState::Delivering,
        state.clone(),
    ] {
        assert!(!state.is_visible(), "{state:?} belongs outside the overlay");
    }

    assert_eq!(state.label(), "Could not paste");
    assert_eq!(state.tone(), StatusTone::Danger);
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
fn waveform_levels_ignore_noise_and_rise_faster_than_they_fall() {
    let mut frame = WaveformFrame::default();
    frame.advance(&[0.004; WAVEFORM_BAR_COUNT]);
    assert!(frame.levels().iter().all(|level| *level == 0.0));

    frame.advance(&[1.0; WAVEFORM_BAR_COUNT]);
    let risen = frame.levels()[0];
    frame.advance(&[0.0; WAVEFORM_BAR_COUNT]);
    let fallen = risen - frame.levels()[0];
    assert!(
        risen > fallen,
        "attack {risen} should outpace release {fallen}"
    );
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

#[test]
fn overlay_fades_in_from_transparent_to_opaque() {
    assert_eq!(overlay_opacity(Duration::ZERO, None), 0.0);
    let mid = overlay_opacity(OVERLAY_FADE_IN / 2, None);
    assert!(mid > 0.0 && mid < 1.0);
    assert_eq!(overlay_opacity(OVERLAY_FADE_IN, None), 1.0);
    assert_eq!(overlay_opacity(Duration::from_secs(60), None), 1.0);
}

#[test]
fn dismissal_fades_out_monotonically_to_exactly_zero() {
    let shown = Duration::from_secs(5);
    let mut previous = overlay_opacity(shown, Some(Duration::ZERO));
    assert_eq!(previous, 1.0);
    for step in 1..=12 {
        let elapsed = OVERLAY_FADE_OUT * step / 12;
        let opacity = overlay_opacity(shown + elapsed, Some(elapsed));
        assert!(opacity <= previous, "fade-out must never brighten");
        previous = opacity;
    }
    assert_eq!(overlay_opacity(shown, Some(OVERLAY_FADE_OUT)), 0.0);
    assert_eq!(overlay_opacity(shown, Some(Duration::from_secs(9))), 0.0);
}

#[test]
fn dismissal_during_fade_in_never_increases_opacity() {
    let at_dismissal = overlay_opacity(OVERLAY_FADE_IN / 4, None);
    for step in 0..=8 {
        let elapsed = OVERLAY_FADE_OUT * step / 8;
        let opacity = overlay_opacity(OVERLAY_FADE_IN / 4 + elapsed, Some(elapsed));
        assert!(opacity <= at_dismissal);
    }
}

#[test]
fn fade_is_active_only_while_a_ramp_is_progressing() {
    assert!(overlay_fade_active(Duration::ZERO, None));
    assert!(!overlay_fade_active(OVERLAY_FADE_IN, None));
    assert!(overlay_fade_active(
        Duration::from_secs(5),
        Some(Duration::ZERO)
    ));
    assert!(!overlay_fade_active(
        Duration::from_secs(5),
        Some(OVERLAY_FADE_OUT)
    ));
}

#[test]
fn destruction_hold_outlasts_the_fade_out() {
    assert!(OVERLAY_FADE_HOLD > OVERLAY_FADE_OUT);
}
