//! One settings window per session. The first `agentdictate` window holds a
//! lock in the runtime directory. A later launch, from the tray's "Open
//! AgentDictate" or the app menu, writes the raise file that window watches
//! and exits, so the open window comes to the front instead of a second one
//! opening beside it.

use std::{
    fs::{self, OpenOptions},
    io,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, channel},
};

use crate::workspace::FileWatcher;

const LOCK_FILE_NAME: &str = "window.lock";
const RAISE_FILE_NAME: &str = "window.raise";

/// Whether this launch shows the window or hands off to the one already open.
pub enum WindowInstance {
    /// This process shows the window, holding the lock until it exits.
    Primary(WindowLock),
    /// Another process shows the window; see `raise_open_window`.
    Secondary,
}

/// The held window lock. Dropping it lets the next launch open a window.
pub struct WindowLock {
    _lock: fs::File,
    raise_file: PathBuf,
}

impl WindowInstance {
    /// Takes the window lock in `runtime_directory`, unless another
    /// process holds it.
    pub fn acquire(runtime_directory: &Path) -> io::Result<Self> {
        fs::create_dir_all(runtime_directory)?;
        fs::set_permissions(runtime_directory, fs::Permissions::from_mode(0o700))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(runtime_directory.join(LOCK_FILE_NAME))?;
        match lock.try_lock() {
            Ok(()) => Ok(Self::Primary(WindowLock {
                _lock: lock,
                raise_file: runtime_directory.join(RAISE_FILE_NAME),
            })),
            Err(fs::TryLockError::WouldBlock) => Ok(Self::Secondary),
            Err(fs::TryLockError::Error(error)) => Err(error),
        }
    }
}

impl WindowLock {
    /// Yields once each time a later launch asks this window to come to the
    /// front, until the returned receiver is dropped.
    pub fn raise_requests(&self) -> io::Result<Receiver<()>> {
        let mut watcher = FileWatcher::empty()?;
        watcher.add_file(&self.raise_file)?;
        let (sender, receiver) = channel();
        std::thread::Builder::new()
            .name("agentdictate-window-raise".into())
            .spawn(move || {
                while watcher.wait_for_change().is_ok() {
                    if sender.send(()).is_err() {
                        return;
                    }
                }
            })?;
        Ok(receiver)
    }
}

/// Asks the window that holds the lock to come to the front.
pub fn raise_open_window(runtime_directory: &Path) -> io::Result<()> {
    OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(runtime_directory.join(RAISE_FILE_NAME))
        .map(drop)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn a_second_launch_raises_the_open_window_until_it_closes() {
        let directory = tempdir().unwrap();
        let runtime = directory.path().join("runtime");

        let WindowInstance::Primary(open) = WindowInstance::acquire(&runtime).unwrap() else {
            panic!("the first launch should show the window");
        };
        let raises = open.raise_requests().unwrap();
        assert!(matches!(
            WindowInstance::acquire(&runtime).unwrap(),
            WindowInstance::Secondary
        ));
        raise_open_window(&runtime).unwrap();
        raises.recv_timeout(Duration::from_secs(5)).unwrap();

        drop(open);
        assert!(matches!(
            WindowInstance::acquire(&runtime).unwrap(),
            WindowInstance::Primary(_)
        ));
    }
}
