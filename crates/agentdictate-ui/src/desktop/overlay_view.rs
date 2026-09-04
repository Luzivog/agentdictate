use gpui::{Context, Hsla, IntoElement, Render, Window, prelude::*, px};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::{
    OverlayPresentation, OverlayState, ThemeTokens, WaveformFrame, overlay_fade_active,
    overlay_opacity,
};

/// Per-dot opacities for the processing ellipsis: a soft sequential pulse
/// derived from wall-clock time so every frame is deterministic to render.
fn busy_dot_alphas() -> [f32; 3] {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as f64;
    let cycle = millis / 1_100.0 * std::f64::consts::TAU;
    core::array::from_fn(|index| {
        let phase = cycle - index as f64 * 0.85;
        (0.3 + 0.7 * (0.5 + 0.5 * phase.sin())) as f32
    })
}

/// GPUI content for the bottom-centered recording status window.
pub struct RecordingOverlay {
    state: OverlayState,
    active_recording: Option<crate::ActiveRecordingPresentation>,
    waveform: WaveformFrame,
    last_sample_at: Option<Instant>,
    shown_at: Option<Instant>,
    dismissed_at: Option<Instant>,
    on_frame_submitted: Option<Box<dyn FnOnce()>>,
}

impl RecordingOverlay {
    pub fn new(state: OverlayState) -> Self {
        Self {
            state,
            active_recording: None,
            waveform: WaveformFrame::default(),
            last_sample_at: None,
            shown_at: None,
            dismissed_at: None,
            on_frame_submitted: None,
        }
    }

    pub fn from_presentation(presentation: OverlayPresentation) -> Self {
        Self {
            state: presentation.state(),
            active_recording: presentation.active_recording,
            waveform: WaveformFrame::default(),
            last_sample_at: None,
            shown_at: None,
            dismissed_at: None,
            on_frame_submitted: None,
        }
    }

    pub fn on_frame_submitted(mut self, callback: impl FnOnce() + 'static) -> Self {
        self.on_frame_submitted = Some(Box::new(callback));
        self
    }

    pub fn with_theme(state: OverlayState, _theme: ThemeTokens) -> Self {
        Self::new(state)
    }

    pub const fn state(&self) -> &OverlayState {
        &self.state
    }

    pub fn set_state(&mut self, state: OverlayState) {
        if self.state != state {
            self.waveform.reset();
            self.last_sample_at = None;
        }
        self.state = state;
        self.active_recording = None;
    }

    /// Starts the fade-out while keeping the last visible content untouched.
    /// Idempotent: repeated dismissals keep the first timestamp.
    pub fn begin_dismissal(&mut self, now: Instant) {
        if self.dismissed_at.is_none() {
            self.dismissed_at = Some(now);
        }
    }

    /// Opacity of the production view, using the same clock as GPUI animation.
    pub fn opacity_at(&self, now: Instant) -> f32 {
        overlay_opacity(
            self.shown_at
                .map_or(Duration::ZERO, |shown| now.saturating_duration_since(shown)),
            self.dismissed_at
                .map(|dismissed| now.saturating_duration_since(dismissed)),
        )
    }

    pub fn set_presentation(&mut self, presentation: OverlayPresentation) {
        let state = presentation.state();
        let recording_changed = self
            .active_recording
            .as_ref()
            .map(|recording| &recording.audio_path)
            != presentation
                .active_recording
                .as_ref()
                .map(|recording| &recording.audio_path);
        if self.state != state || recording_changed {
            self.waveform.reset();
            self.last_sample_at = None;
        }
        self.state = state;
        self.active_recording = presentation.active_recording;
    }
}

impl Render for RecordingOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let label = self.state.label().to_owned();
        let stable_id = self.state.stable_id().to_owned();
        let recording = self.state == OverlayState::Recording;
        // Processing states (transcribing, cleaning) animate a small pulsing
        // ellipsis so the helper visibly shows work in progress.
        let busy = self.state.is_visible() && !recording;
        let now = cx.background_executor().now();
        let shown_at = *self.shown_at.get_or_insert(now);
        let since_shown = now.saturating_duration_since(shown_at);
        let since_dismissal = self
            .dismissed_at
            .map(|dismissed| now.saturating_duration_since(dismissed));
        let opacity = self.opacity_at(now);
        // A callback scheduled by this render runs on the next platform frame,
        // after this fully faded-in scene has been submitted to the renderer.
        // This is not a claim about compositor visibility or occlusion.
        if opacity >= 1.0
            && let Some(callback) = self.on_frame_submitted.take()
        {
            window.on_next_frame(move |_, _| callback());
        }
        if recording || busy || overlay_fade_active(since_shown, since_dismissal) {
            window.request_animation_frame();
        }
        let busy_label = label
            .trim_end_matches(['\u{2026}', '.'])
            .trim_end()
            .to_owned();
        let busy_dot_alphas = busy_dot_alphas();
        if recording
            && self.last_sample_at.is_none_or(|sampled| {
                now.saturating_duration_since(sampled) >= Duration::from_millis(33)
            })
        {
            if let Some(active) = &self.active_recording {
                self.waveform
                    .advance(&crate::sample_recent_wav(&active.audio_path));
            }
            self.last_sample_at = Some(now);
        }

        let elapsed = self.active_recording.as_ref().map_or(0.0, |active| {
            let now_millis = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(i64::MAX as u128) as i64;
            crate::elapsed_seconds(active.started_at_unix_millis, now_millis)
        });
        let timer = crate::format_elapsed(elapsed);
        let timer_color: Hsla = gpui::rgba(0xf5f5f5f0).into();
        let mut timer_style = window.text_style().highlight(gpui::FontWeight::BOLD);
        timer_style.font_family = "Sans".into();
        let timer_run = gpui::TextRun {
            len: timer.len(),
            font: timer_style.font(),
            color: timer_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let timer_width = f32::from(
            window
                .text_system()
                .layout_line(&timer, px(13.), &[timer_run], None)
                .width,
        );
        let recording_layout = crate::recording_overlay_layout(timer_width);
        let bars = crate::waveform_bars(self.waveform.levels(), recording_layout.waveform);
        let waveform_color: Hsla = gpui::rgb(0xf04a1f).into();

        gpui::div()
            .debug_selector(move || stable_id)
            .relative()
            .size_full()
            .opacity(opacity)
            .when(self.state.is_visible(), |root| {
                root.child(
                    gpui::div()
                        .absolute()
                        .left(px(6.))
                        .top(px(8.))
                        .w(px(127.))
                        .h(px(42.))
                        .rounded(px(14.))
                        .bg(gpui::rgba(0x0000003d)),
                )
                .child(
                    gpui::div()
                        .debug_selector(|| "recording-overlay-card".to_owned())
                        .absolute()
                        .left(px(6.))
                        .top(px(6.))
                        .w(px(127.))
                        .h(px(42.))
                        .relative()
                        .overflow_hidden()
                        .rounded(px(14.))
                        .border_1()
                        .border_color(gpui::rgba(0xffffff1c))
                        .bg(gpui::rgba(0x111112f2))
                        .when(recording, |card| {
                            card.children(bars.into_iter().enumerate().map(|(index, bar)| {
                                gpui::div()
                                    .debug_selector(move || {
                                        format!("recording-overlay-wave-{index}")
                                    })
                                    .absolute()
                                    .left(px(bar.x))
                                    .top(px(bar.center_y - bar.height / 2.0))
                                    .w(px(bar.width))
                                    .h(px(bar.height))
                                    .rounded_full()
                                    .bg(waveform_color.opacity(bar.alpha))
                            }))
                            .child(
                                gpui::div()
                                    .debug_selector(|| "recording-overlay-timer".to_owned())
                                    .absolute()
                                    .left(px(recording_layout.timer_x))
                                    .top_0()
                                    .w(px(recording_layout.timer_width))
                                    .h_full()
                                    .flex()
                                    .items_center()
                                    .text_size(px(13.))
                                    .font_family("Sans")
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(timer_color)
                                    .child(timer),
                            )
                        })
                        .when(!recording, |card| {
                            card.child(
                                gpui::div()
                                    .size_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .gap(px(5.))
                                    .px_3()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(gpui::rgba(0xf5f5f5f5))
                                    .child(if busy { busy_label } else { label })
                                    .when(busy, |row| {
                                        row.child(
                                            gpui::div()
                                                .flex()
                                                .items_center()
                                                .gap(px(3.))
                                                .children(
                                                    busy_dot_alphas.into_iter().enumerate().map(
                                                        |(index, alpha)| {
                                                            let dot_color: Hsla =
                                                                gpui::rgb(0xf5f5f5).into();
                                                            gpui::div()
                                                                .debug_selector(move || {
                                                                    format!(
                                                                        "recording-overlay-busy-dot-{index}"
                                                                    )
                                                                })
                                                                .w(px(3.5))
                                                                .h(px(3.5))
                                                                .rounded_full()
                                                                .bg(dot_color.opacity(alpha))
                                                        },
                                                    ),
                                                ),
                                        )
                                    }),
                            )
                        }),
                )
            })
    }
}
