//! How an open settings window learns that the daemon's status changed.
//! The daemon writes an empty `status` file in the runtime directory when it
//! starts and whenever something the window shows changes; the window's
//! workspace watcher then asks the daemon for its status snapshot.

use std::{io, path::Path, sync::Arc, thread::JoinHandle};

use crate::DaemonStatus;

/// The daemon's status signal in the runtime directory.
pub const STATUS_FILE: &str = "status";

/// Writes `runtime_directory/status` now and after every status change, on
/// its own thread, for the daemon's lifetime.
pub fn signal_status_changes(
    status: Arc<DaemonStatus>,
    runtime_directory: &Path,
) -> io::Result<JoinHandle<()>> {
    let file = runtime_directory.join(STATUS_FILE);
    std::thread::Builder::new()
        .name("agentdictate-status-signal".into())
        .spawn(move || {
            let mut seen = 0;
            loop {
                if let Err(error) = std::fs::write(&file, []) {
                    tracing::warn!(%error, "could not signal a status change to the window");
                }
                seen = status.wait_for_change(seen);
            }
        })
}
