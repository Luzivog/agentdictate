use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::Duration,
};

use agentdictate_core::{ProcessingStage, WorkflowPhase, WorkflowSnapshot};

use crate::StatusTone;

pub const OVERLAY_WIDTH: u32 = 143;
pub const OVERLAY_HEIGHT: u32 = 56;
pub const OVERLAY_BOTTOM_GAP: u32 = 72;
pub const WAVEFORM_SOURCE_BIN_COUNT: usize = 44;
pub const WAVEFORM_BAR_COUNT: usize = 20;
/// Fade timings for the transient overlay window. Destroying the dark card
/// abruptly beside the taskbar reads as a flash at paste time, so the helper
/// fades the card in and out and only destroys a fully transparent surface.
pub const OVERLAY_FADE_IN: Duration = Duration::from_millis(100);
pub const OVERLAY_FADE_OUT: Duration = Duration::from_millis(120);
/// Hold between dismissal and window destruction: the fade-out plus two
/// 60 Hz frames of margin so the destroyed frame is fully transparent.
pub const OVERLAY_FADE_HOLD: Duration = Duration::from_millis(150);
const WAV_HEADER_BYTES: u64 = 44;
const RECENT_SAMPLE_COUNT: usize = 2_816;

/// Recording-only data required by the transient presentation.
///
/// This deliberately lives beside the overlay rather than in `AppSnapshot`:
/// the settings client has no reason to learn the current temporary WAV path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveRecordingPresentation {
    pub audio_path: PathBuf,
    pub started_at_unix_millis: i64,
}

/// One event-driven presentation update sent from the daemon to its helper.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OverlayPresentation {
    pub workflow: WorkflowSnapshot,
    pub active_recording: Option<ActiveRecordingPresentation>,
}

impl OverlayPresentation {
    pub fn state(&self) -> OverlayState {
        OverlayState::from(self.workflow)
    }

    /// Elapsed recording time against an injected clock, kept deterministic
    /// for the overlay's timer tests and clamped across wall-clock corrections.
    pub fn elapsed_seconds(&self, now_unix_millis: i64) -> f64 {
        self.active_recording.as_ref().map_or(0.0, |recording| {
            elapsed_seconds(recording.started_at_unix_millis, now_unix_millis)
        })
    }
}

/// Reads the recent PCM tail of a growing mono S16_LE WAV and summarizes it
/// into the same 44 peak/RMS bins used by the original overlay.
pub fn sample_recent_wav(path: &Path) -> [f32; WAVEFORM_SOURCE_BIN_COUNT] {
    let samples = recent_wav_samples(path, RECENT_SAMPLE_COUNT);
    waveform_bins(&samples)
}

fn recent_wav_samples(path: &Path, sample_count: usize) -> Vec<i16> {
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let Ok(metadata) = file.metadata() else {
        return Vec::new();
    };
    let size = metadata.len();
    if size <= WAV_HEADER_BYTES {
        return Vec::new();
    }

    let byte_count = (sample_count as u64 * 2).min(size - WAV_HEADER_BYTES);
    let mut offset = WAV_HEADER_BYTES.max(size - byte_count);
    if !offset.is_multiple_of(2) {
        offset += 1;
    }
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return Vec::new();
    }
    let mut bytes = vec![0; usize::try_from(byte_count).unwrap_or(usize::MAX)];
    let Ok(read) = file.read(&mut bytes) else {
        return Vec::new();
    };
    bytes.truncate(read - (read % 2));
    bytes
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect()
}

fn waveform_bins(samples: &[i16]) -> [f32; WAVEFORM_SOURCE_BIN_COUNT] {
    let mut values = [0.0; WAVEFORM_SOURCE_BIN_COUNT];
    if samples.is_empty() {
        return values;
    }
    let chunk_size = (samples.len() / WAVEFORM_SOURCE_BIN_COUNT).max(1);
    for (index, value) in values.iter_mut().enumerate() {
        let start = index * chunk_size;
        let end = if index == WAVEFORM_SOURCE_BIN_COUNT - 1 {
            samples.len()
        } else {
            samples.len().min(start + chunk_size)
        };
        let Some(chunk) = samples.get(start..end) else {
            continue;
        };
        if chunk.is_empty() {
            continue;
        }
        let peak = chunk
            .iter()
            .map(|sample| i32::from(*sample).abs() as f64)
            .fold(0.0, f64::max);
        let mean_square = chunk
            .iter()
            .map(|sample| {
                let sample = f64::from(*sample);
                sample * sample
            })
            .sum::<f64>()
            / chunk.len() as f64;
        let rms = mean_square.sqrt();
        *value = (((peak * 0.65) + (rms * 0.35)) / 32_768.0).min(1.0) as f32;
    }
    values
}

/// Max-pools an arbitrary source waveform into a fixed display width. This is
/// intentionally identical to the prior 44-to-20 fitting behavior.
pub fn fit_waveform(values: &[f32], count: usize) -> Vec<f32> {
    if count == 0 {
        return Vec::new();
    }
    if values.len() == count {
        return values.to_vec();
    }
    if values.len() < count {
        let mut fitted = values.to_vec();
        fitted.resize(count, 0.0);
        return fitted;
    }
    let scale = values.len() as f64 / count as f64;
    (0..count)
        .map(|index| {
            let start = (index as f64 * scale) as usize;
            let end = values
                .len()
                .min((start + 1).max(((index + 1) as f64 * scale) as usize));
            values[start..end].iter().copied().fold(0.0, f32::max)
        })
        .collect()
}

/// Smoothed display levels retained between the helper's local 33 ms ticks.
#[derive(Clone, Debug, PartialEq)]
pub struct WaveformFrame {
    levels: [f32; WAVEFORM_BAR_COUNT],
}

impl Default for WaveformFrame {
    fn default() -> Self {
        Self {
            levels: [0.0; WAVEFORM_BAR_COUNT],
        }
    }
}

impl WaveformFrame {
    pub const fn from_levels(levels: [f32; WAVEFORM_BAR_COUNT]) -> Self {
        Self { levels }
    }

    pub const fn levels(&self) -> &[f32; WAVEFORM_BAR_COUNT] {
        &self.levels
    }

    pub fn reset(&mut self) {
        self.levels = [0.0; WAVEFORM_BAR_COUNT];
    }

    pub fn advance(&mut self, source: &[f32]) {
        let targets = fit_waveform(source, WAVEFORM_BAR_COUNT);
        for (current, target) in self.levels.iter_mut().zip(targets) {
            let gated = if target < 0.005 {
                0.0
            } else {
                ((target - 0.005) / 0.13).min(1.0)
            };
            let factor = if gated > *current { 0.62 } else { 0.34 };
            *current = (*current * (1.0 - factor)) + (gated * factor);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaveformArea {
    pub x: f32,
    pub width: f32,
    pub center_y: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecordingOverlayLayout {
    pub waveform: WaveformArea,
    pub timer_x: f32,
    pub timer_width: f32,
}

/// Reproduces the previous Cairo overlay's timer-first layout.
///
/// The timer is right-aligned ten pixels inside the 127-pixel card. The
/// waveform starts twelve pixels from the left and consumes only the space
/// remaining before the fixed eight-pixel timer gap.
pub fn recording_overlay_layout(timer_width: f32) -> RecordingOverlayLayout {
    let timer_width = if timer_width.is_finite() {
        timer_width.max(0.0)
    } else {
        0.0
    };
    let timer_x = 127.0 - timer_width - 10.0;
    RecordingOverlayLayout {
        waveform: WaveformArea::new(12.0, (timer_x - 12.0 - 8.0).max(1.0), 21.0),
        timer_x,
        timer_width,
    }
}

impl WaveformArea {
    pub const fn new(x: f32, width: f32, center_y: f32) -> Self {
        Self { x, width, center_y }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaveformBar {
    pub x: f32,
    pub center_y: f32,
    pub width: f32,
    pub height: f32,
    pub alpha: f32,
}

/// Produces toolkit-independent waveform geometry for the renderer.
pub fn waveform_bars(levels: &[f32], area: WaveformArea) -> Vec<WaveformBar> {
    if levels.is_empty() {
        return Vec::new();
    }
    let gap = 1.25;
    let width =
        ((area.width - (levels.len() - 1) as f32 * gap) / levels.len() as f32).clamp(0.8, 2.4);
    let denominator = (levels.len() - 1).max(1) as f32;
    levels
        .iter()
        .enumerate()
        .map(|(index, level)| {
            let contour = 0.78 + 0.22 * ((index as f32 / denominator) * std::f32::consts::PI).sin();
            WaveformBar {
                x: area.x + index as f32 * (width + gap),
                center_y: area.center_y,
                width,
                height: 2.5 + level.powf(0.55) * 26.0 * contour,
                alpha: 0.24 + 0.68 * (level + 0.10).min(1.0),
            }
        })
        .collect()
}

pub fn elapsed_seconds(started_at_unix_millis: i64, now_unix_millis: i64) -> f64 {
    now_unix_millis
        .saturating_sub(started_at_unix_millis)
        .max(0) as f64
        / 1_000.0
}

pub fn format_elapsed(seconds: f64) -> String {
    let seconds = if seconds.is_finite() {
        seconds.max(0.0) as u64
    } else {
        0
    };
    let minutes = seconds / 60;
    let seconds = seconds % 60;
    let hours = minutes / 60;
    let minutes = minutes % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Window-manager contract for the transient recording presentation.
///
/// The overlay reports status only. Keeping these invariants in the
/// toolkit-independent model prevents a desktop adapter from accidentally
/// stealing the focused paste target while showing or updating the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverlayWindowPolicy {
    pub focusable: bool,
    pub accepts_input: bool,
    pub requests_activation: bool,
    pub show_in_taskbar: bool,
}

impl OverlayWindowPolicy {
    pub const fn focus_neutral() -> Self {
        Self {
            focusable: false,
            accepts_input: false,
            requests_activation: false,
            show_in_taskbar: false,
        }
    }
}

/// Card opacity for the time since the window was shown and, once dismissal
/// began, the time since dismissal. The fade-in level is frozen at the
/// dismissal instant and then multiplied by the fade-out ramp, so a dismissal
/// that lands mid-fade-in can only ever lower the opacity.
#[must_use]
pub fn overlay_opacity(since_shown: Duration, since_dismissal: Option<Duration>) -> f32 {
    let shown_before_dismissal =
        since_dismissal.map_or(since_shown, |elapsed| since_shown.saturating_sub(elapsed));
    let fade_in = fade_progress(shown_before_dismissal, OVERLAY_FADE_IN);
    let fade_out = since_dismissal.map_or(0.0, |elapsed| fade_progress(elapsed, OVERLAY_FADE_OUT));
    (fade_in * (1.0 - fade_out)).clamp(0.0, 1.0)
}

/// Whether a fade ramp is still progressing; the helper keeps requesting
/// animation frames while this is true.
#[must_use]
pub fn overlay_fade_active(since_shown: Duration, since_dismissal: Option<Duration>) -> bool {
    match since_dismissal {
        Some(elapsed) => elapsed < OVERLAY_FADE_OUT,
        None => since_shown < OVERLAY_FADE_IN,
    }
}

fn fade_progress(elapsed: Duration, span: Duration) -> f32 {
    (elapsed.as_secs_f32() / span.as_secs_f32()).clamp(0.0, 1.0)
}

/// Presentation state derived from the workflow.
///
/// The transient window intentionally mirrors the previous overlay and opens
/// only while recording, transcribing, or cleaning, then lingers up to
/// `OVERLAY_FADE_HOLD` while it fades out. Recovery remains durable in
/// History rather than turning the overlay into a second action surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverlayState {
    Hidden,
    Starting,
    Recording,
    Finishing,
    Transcribing,
    Cleaning,
    ReadyToDeliver,
    Delivering,
    RecoverableFailure { message: String, action: String },
}

impl OverlayState {
    pub fn recoverable_failure(message: impl Into<String>, action: impl Into<String>) -> Self {
        Self::RecoverableFailure {
            message: message.into(),
            action: action.into(),
        }
    }

    pub const fn is_visible(&self) -> bool {
        matches!(self, Self::Recording | Self::Transcribing | Self::Cleaning)
    }

    pub const fn window_policy(&self) -> OverlayWindowPolicy {
        OverlayWindowPolicy::focus_neutral()
    }

    /// Stable presentation identity for rendered-interaction diagnostics.
    pub const fn stable_id(&self) -> &'static str {
        match self {
            Self::Hidden => "recording-overlay-hidden",
            Self::Starting => "recording-overlay-starting",
            Self::Recording => "recording-overlay-recording",
            Self::Finishing => "recording-overlay-finishing",
            Self::Transcribing => "recording-overlay-transcribing",
            Self::Cleaning => "recording-overlay-cleaning",
            Self::ReadyToDeliver => "recording-overlay-ready-to-deliver",
            Self::Delivering => "recording-overlay-delivering",
            Self::RecoverableFailure { .. } => "recording-overlay-recoverable-failure",
        }
    }

    pub fn accessibility_label(&self) -> String {
        format!("Agent Dictate: {}", self.label())
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Hidden => "",
            Self::Starting => "Starting…",
            Self::Recording => "Listening…",
            Self::Finishing => "Securing recording…",
            Self::Transcribing => "Transcribing",
            Self::Cleaning => "Cleaning up...",
            Self::ReadyToDeliver => "Ready to paste",
            Self::Delivering => "Pasting…",
            Self::RecoverableFailure { message, .. } => message,
        }
    }

    pub fn action_label(&self) -> Option<&str> {
        match self {
            Self::RecoverableFailure { action, .. } => Some(action),
            _ => None,
        }
    }

    pub const fn tone(&self) -> StatusTone {
        match self {
            Self::Hidden => StatusTone::Neutral,
            Self::Starting => StatusTone::Starting,
            Self::Recording => StatusTone::Recording,
            Self::Finishing | Self::Transcribing | Self::Cleaning => StatusTone::Processing,
            Self::ReadyToDeliver | Self::Delivering => StatusTone::Success,
            Self::RecoverableFailure { .. } => StatusTone::Danger,
        }
    }
}

impl From<WorkflowSnapshot> for OverlayState {
    fn from(snapshot: WorkflowSnapshot) -> Self {
        match snapshot.phase {
            WorkflowPhase::Ready => Self::Hidden,
            WorkflowPhase::Starting { .. } => Self::Starting,
            WorkflowPhase::Recording { .. } => Self::Recording,
            // The previous overlay transitioned directly from its waveform to
            // "Transcribing". Keeping the helper visible across the brief
            // recorder-finalization phase avoids a close/relaunch flicker.
            WorkflowPhase::Stopping { .. } => Self::Transcribing,
            WorkflowPhase::Processing { stage, .. } => match stage {
                ProcessingStage::Transcribing => Self::Transcribing,
                ProcessingStage::Cleaning => Self::Cleaning,
                ProcessingStage::ReadyToDeliver => Self::ReadyToDeliver,
                ProcessingStage::Delivering => Self::Delivering,
            },
            WorkflowPhase::NeedsAttention { .. } => {
                Self::recoverable_failure("Dictation needs attention", "Open recovery")
            }
        }
    }
}
