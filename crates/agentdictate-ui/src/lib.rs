//! GPUI presentation module for AgentDictate.

#[cfg(feature = "desktop")]
mod action;
#[cfg(feature = "desktop")]
mod assets;
#[cfg(feature = "desktop")]
mod desktop;
mod failure;
mod history;
mod overlay;
mod route;
mod settings;
mod theme;
mod usage;
mod view_model;
#[cfg(feature = "desktop")]
mod window_frame;
mod words;
mod workspace;

#[cfg(feature = "desktop")]
#[doc(hidden)]
pub use assets::AgentDictateAssets;
#[cfg(feature = "desktop")]
pub use desktop::{
    APPLICATION_ID, HotkeyCaptureSink, RecordingOverlay, SettingsShell, SettingsSink,
    SettingsWindow, run_recording_overlay, run_settings_window,
};
pub use failure::{FailureWording, failure_wording, recovery_reason};
pub use history::{
    HistoryViewModel, RecoveryItemViewModel, RecoveryStage, RecoveryViewModel, TranscriptViewModel,
};
pub use overlay::{
    ActiveRecordingPresentation, OVERLAY_BOTTOM_GAP, OVERLAY_FADE_HOLD, OVERLAY_FADE_IN,
    OVERLAY_FADE_OUT, OVERLAY_HEIGHT, OVERLAY_WIDTH, OverlayPresentation, OverlayState,
    RecordingOverlayLayout, WAVEFORM_BAR_COUNT, WAVEFORM_SOURCE_BIN_COUNT, WaveformArea,
    WaveformBar, WaveformFrame, elapsed_seconds, format_elapsed, overlay_fade_active,
    overlay_opacity, recording_overlay_layout, sample_recent_wav, waveform_bars,
};
pub use route::Route;
pub use settings::SettingsRequest;
pub use theme::{Color, ThemeTokens};
pub use usage::{UsageDayViewModel, UsagePeriod, UsageTotals, UsageViewModel};
pub use view_model::{
    HotkeyViewModel, NavigationItemViewModel, ShellViewModel, StatusTone, StatusViewModel,
};
#[cfg(feature = "desktop")]
pub use window_frame::AgentDictateWindowFrame;
pub use words::{WordRowViewModel, WordsEdit, WordsError, word_rows};
pub use workspace::{UiActionError, WorkspaceAction, WorkspaceActionSink, WorkspaceViewModel};

#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod test_support {
    /// Initialize GPUI Component inside a headless rendered-interaction test.
    pub fn initialize(cx: &mut gpui::TestAppContext) {
        cx.update(crate::theme::initialize_gpui_theme);
    }

    /// Render the production single-line clipping primitive around an
    /// inspectable text element.
    pub fn single_line_clip_element(
        selector: &'static str,
        element: impl gpui::IntoElement,
    ) -> gpui::Div {
        crate::desktop::single_line::single_line_clip_element(selector, element)
    }
}
