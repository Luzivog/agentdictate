//! AgentDictate process composition and production adapters.

use std::{io, os::unix::fs::PermissionsExt, path::PathBuf};

mod audio_ducking;
mod daemon;
pub mod diagnostics;
mod model_catalog;
mod openai;
mod overlay_process;
mod process;
mod startup;
mod system;
mod tray;
mod workspace;

pub use daemon::{CapturedRecording, Daemon, DaemonError, RecordingController};
pub use diagnostics::init_file_logging;
pub use openai::{
    CleanupRequest, OpenAiTranscriber, OpenAiTransport, ReqwestOpenAiTransport,
    TranscriptionRequest,
};
pub use overlay_process::{
    ActiveRecordingUpdate, OverlayProcessAction, OverlayProcessState, OverlayUpdate,
    is_overlay_helper_argument, overlay_work_area_from_environment, start_overlay_presenter,
};
pub use process::{AgentProcess, HotkeyReconfigurer, ProductionDaemon, command_for_hotkey};
pub use startup::{sync_autostart, sync_autostart_command};
pub use system::{
    SystemDeliverer, SystemRecordingController, detect_primary_work_area, parse_x11_work_area,
};
pub use tray::{
    SystemTrayHandle, TrayAction, settings_executable_for_current_process, start_system_tray,
    tray_command_for_phase,
};
pub use workspace::{WorkspaceClient, workspace_view_model};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppPaths {
    pub config_file: PathBuf,
    pub autostart_file: PathBuf,
    pub database_file: PathBuf,
    pub recordings: PathBuf,
    pub logs: PathBuf,
    pub cache: PathBuf,
    pub runtime: PathBuf,
}

impl AppPaths {
    #[must_use]
    pub fn model_catalog_cache_file(&self) -> PathBuf {
        self.cache.join("model-catalog.json")
    }

    pub fn from_environment() -> io::Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
        let xdg =
            |name: &str, fallback: PathBuf| std::env::var_os(name).map_or(fallback, PathBuf::from);
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                // SAFETY: `geteuid` has no preconditions and does not access
                // caller-provided memory.
                let effective_user = unsafe { libc::geteuid() };
                PathBuf::from(format!("/run/user/{effective_user}"))
            });
        Ok(Self::from_roots(
            xdg("XDG_CONFIG_HOME", home.join(".config")),
            xdg("XDG_DATA_HOME", home.join(".local/share")),
            xdg("XDG_STATE_HOME", home.join(".local/state")),
            xdg("XDG_CACHE_HOME", home.join(".cache")),
            runtime,
        ))
    }

    pub fn ensure_directories(&self) -> io::Result<()> {
        for directory in [
            self.config_file.parent(),
            self.database_file.parent(),
            Some(self.recordings.as_path()),
            Some(self.logs.as_path()),
            Some(self.cache.as_path()),
            Some(self.runtime.as_path()),
        ]
        .into_iter()
        .flatten()
        {
            std::fs::create_dir_all(directory)?;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    #[must_use]
    pub fn from_roots(
        config_root: impl Into<PathBuf>,
        data_root: impl Into<PathBuf>,
        state_root: impl Into<PathBuf>,
        cache_root: impl Into<PathBuf>,
        runtime_root: impl Into<PathBuf>,
    ) -> Self {
        let config_root = config_root.into();
        let config = config_root.join("agentdictate");
        let data = data_root.into().join("agentdictate");
        let state = state_root.into().join("agentdictate");
        let cache = cache_root.into().join("agentdictate");
        let runtime = runtime_root.into().join("agentdictate");
        Self {
            config_file: config.join("config.json"),
            autostart_file: config_root.join("autostart/local.agentdictate.AgentDictate.desktop"),
            database_file: data.join("agentdictate.sqlite"),
            recordings: data.join("recordings"),
            logs: state.join("logs"),
            cache,
            runtime,
        }
    }
}
