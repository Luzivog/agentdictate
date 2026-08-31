//! GPUI presentation module for AgentDictate.

#[cfg(feature = "desktop")]
mod action;
#[cfg(feature = "desktop")]
mod assets;
#[cfg(feature = "desktop")]
mod desktop;
mod history;
mod model_catalog;
mod overlay;
mod replacements;
mod route;
mod settings;
mod shell_layout;
#[cfg(feature = "desktop")]
mod sidebar_motion;
mod theme;
mod usage;
mod view_model;
#[cfg(feature = "desktop")]
mod window_frame;
mod workspace;

#[cfg(feature = "desktop")]
#[doc(hidden)]
pub use assets::AgentDictateAssets;
#[cfg(feature = "desktop")]
pub use desktop::{
    APPLICATION_ID, RecordingOverlay, SettingsShell, run_recording_overlay_with_ready,
    run_settings_shell_with_workspace_actions,
    run_settings_shell_with_workspace_actions_and_updates,
};
pub use history::{
    HistoryViewModel, RecoveryItemViewModel, RecoveryStage, RecoveryViewModel, TranscriptViewModel,
};
pub use model_catalog::{
    ModelCatalogOptionViewModel, ModelCatalogStatusSource, ModelCatalogStatusViewModel,
    ModelCatalogViewModel, ReasoningOptionViewModel,
};
pub use overlay::{
    ActiveRecordingPresentation, LogicalRect, LogicalSize, OVERLAY_BOTTOM_GAP, OVERLAY_FADE_HOLD,
    OVERLAY_FADE_IN, OVERLAY_FADE_OUT, OVERLAY_HEIGHT, OVERLAY_WIDTH, OverlayPlacement,
    OverlayPresentation, OverlayState, OverlayWindowPolicy,
    RecordingOverlayLayout, WAVEFORM_BAR_COUNT, WAVEFORM_SOURCE_BIN_COUNT, WaveformArea,
    WaveformBar, WaveformFrame, elapsed_seconds, fit_waveform, format_elapsed,
    intersect_logical_rects, overlay_fade_active, overlay_opacity, recording_overlay_layout,
    sample_recent_wav, waveform_bars,
};
pub use replacements::{ReplacementDraft, ReplacementRuleViewModel, ReplacementsViewModel};
pub use route::{Route, RouteParseError};
pub use settings::{SettingsDraft, SettingsDraftError};
pub use shell_layout::{
    SIDEBAR_OVERLAY_BREAKPOINT, ShellLayout, SidebarMode, sidebar_open_for_layout,
};
pub use theme::{Color, RadiusTokens, SpacingTokens, ThemeTokens, TypographyTokens};
pub use usage::{UsageDayViewModel, UsagePeriod, UsageTotals, UsageViewModel};
pub use view_model::{
    HotkeyViewModel, NavigationItemViewModel, ShellViewModel, StatusTone, StatusViewModel,
};
#[cfg(feature = "desktop")]
pub use window_frame::AgentDictateWindowFrame;
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
