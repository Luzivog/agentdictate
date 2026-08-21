use std::{
    ffi::CString,
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::Path,
};

use evdev::{EventType, InputEvent};

use crate::hotkey::{KeyInput, KeyState};

pub fn evdev_key_input(event: InputEvent) -> Option<KeyInput> {
    if event.event_type() != EventType::KEY {
        return None;
    }
    let state = match event.value() {
        0 => KeyState::Released,
        1 => KeyState::Pressed,
        2 => KeyState::Repeated,
        _ => return None,
    };
    Some(KeyInput::new(event.code(), state))
}

pub(super) const fn poll_descriptor(file_descriptor: i32) -> libc::pollfd {
    libc::pollfd {
        fd: file_descriptor,
        events: libc::POLLIN | libc::POLLERR | libc::POLLHUP,
        revents: 0,
    }
}

pub(super) struct InputDirectoryWatcher {
    file: OwnedFd,
}

impl InputDirectoryWatcher {
    pub(super) fn new(path: &Path) -> io::Result<Self> {
        // SAFETY: `inotify_init1` has no pointer parameters.
        let descriptor = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: ownership of the newly-created descriptor transfers here.
        let file = unsafe { OwnedFd::from_raw_fd(descriptor) };
        let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "input path contains a NUL byte",
            )
        })?;
        let mask = libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_MOVED_FROM
            | libc::IN_MOVED_TO
            | libc::IN_ATTRIB;
        // SAFETY: the C string is NUL-terminated and valid for this call.
        let watch = unsafe { libc::inotify_add_watch(file.as_raw_fd(), path.as_ptr(), mask) };
        if watch < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { file })
    }

    pub(super) fn drain(&self) -> io::Result<()> {
        let mut buffer = [0_u8; 4096];
        loop {
            // SAFETY: the buffer points to writable memory of the stated size.
            let read = unsafe {
                libc::read(
                    self.file.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if read > 0 {
                continue;
            }
            if read == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                return Ok(());
            }
            return Err(error);
        }
    }
}

impl AsRawFd for InputDirectoryWatcher {
    fn as_raw_fd(&self) -> i32 {
        self.file.as_raw_fd()
    }
}
