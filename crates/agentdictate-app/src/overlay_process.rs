mod child;
mod helper;
mod protocol;
mod supervisor;

#[cfg(feature = "desktop")]
pub use helper::run_overlay_helper;
pub use helper::{is_overlay_helper_argument, overlay_work_area_from_environment};
pub use protocol::{ActiveRecordingUpdate, OverlayUpdate};
pub use supervisor::{
    OVERLAY_TEARDOWN_TIMEOUT, OverlayController, OverlayProcessAction, OverlayProcessState,
    OverlayTeardownError,
    start_overlay_presenter, start_overlay_presenter_with_timeout,
};
