use std::{
    io::{self, Read, Write},
    os::fd::AsRawFd,
    os::unix::process::{CommandExt, ExitStatusExt},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
    sync::mpsc::Sender,
    time::{Duration, Instant},
};

use super::{
    protocol::{OVERLAY_HELPER_ARGUMENT, OverlayHelperStatus, OverlayUpdate},
    supervisor::PresenterEvent,
};

const MAX_OVERLAY_STATUS_BYTES: usize = 64 * 1024;

fn wait_for_overlay_child(process_id: u32) -> io::Result<ExitStatus> {
    let process_id = i32::try_from(process_id)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "process id is too large"))?;
    let mut status = 0;
    loop {
        // SAFETY: `status` points to valid writable memory and `process_id`
        // identifies a direct child created by this process. Blocking waitpid
        // is the kernel event source; no timer or polling loop is involved.
        let result = unsafe { libc::waitpid(process_id, &mut status, 0) };
        if result == process_id {
            return Ok(ExitStatus::from_raw(status));
        }
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
    }
}

fn monitor_overlay_status(
    mut output: ChildStdout,
    timeout: Duration,
    generation: u64,
    events: &Sender<PresenterEvent>,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let mut pending = Vec::new();
    let mut buffer = [0_u8; 1024];
    let mut submitted = false;
    let mut created = false;
    loop {
        if !submitted {
            wait_for_overlay_helper_status(&output, deadline)?;
        }
        match output.read(&mut buffer) {
            Ok(0) if !pending.is_empty() => {
                return Err("overlay helper status message was incomplete".into());
            }
            Ok(0) if !submitted => {
                return Err("overlay helper exited without submitting a frame".into());
            }
            Ok(0) => return Ok(()),
            Ok(read) => {
                pending.extend_from_slice(&buffer[..read]);
                if pending.len() > MAX_OVERLAY_STATUS_BYTES {
                    return Err("overlay helper status message was too large".into());
                }
                while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
                    let status: OverlayHelperStatus = serde_json::from_slice(&pending[..newline])
                        .map_err(|error| {
                        format!("overlay helper status message was invalid: {error}")
                    })?;
                    pending.drain(..=newline);
                    match &status {
                        OverlayHelperStatus::WindowCreated if !created && !submitted => {
                            created = true
                        }
                        OverlayHelperStatus::FrameSubmitted if !submitted => submitted = true,
                        OverlayHelperStatus::Error { .. } => {}
                        _ => {
                            return Err(
                                "overlay helper repeated or reordered a startup milestone".into()
                            );
                        }
                    }
                    let failed = matches!(status, OverlayHelperStatus::Error { .. });
                    let _ = events.send(PresenterEvent::HelperStatus {
                        generation,
                        status: Ok(status),
                    });
                    if failed {
                        return Ok(());
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(format!("overlay helper status could not be read: {error}")),
        }
    }
}

fn wait_for_overlay_helper_status(output: &ChildStdout, deadline: Instant) -> Result<(), String> {
    let mut descriptor = libc::pollfd {
        fd: output.as_raw_fd(),
        events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
        revents: 0,
    };
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("overlay helper did not submit a frame before the deadline".into());
        }
        let timeout_millis = i32::try_from(remaining.as_millis()).unwrap_or(i32::MAX);
        // SAFETY: `descriptor` points to one initialized pollfd for the live
        // child stdout descriptor. poll does not retain the pointer.
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout_millis) };
        if result > 0 {
            return Ok(());
        }
        if result == 0 {
            return Err("overlay helper did not report readiness before the deadline".to_owned());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(format!(
                "overlay helper readiness could not be monitored: {error}"
            ));
        }
        if Instant::now() >= deadline {
            return Err("overlay helper did not report readiness before the deadline".to_owned());
        }
    }
}

pub(super) struct OverlayChild {
    child: Child,
    input: Option<ChildStdin>,
    generation: u64,
    ready: bool,
}

impl OverlayChild {
    pub(super) fn launch(
        executable: &Path,
        ready_timeout: Duration,
        generation: u64,
        events: Sender<PresenterEvent>,
    ) -> io::Result<Self> {
        // The presenter thread lives for the daemon's entire lifetime, so the
        // helper's parent-death signal is attached to a stable owner.
        // SAFETY: `getpid` has no preconditions.
        let expected_parent = unsafe { libc::getpid() };
        let mut command = Command::new(executable);
        command
            .arg(OVERLAY_HELPER_ARGUMENT)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .process_group(0);
        if std::env::var_os("DISPLAY").is_some() {
            // The pinned GPUI patch maps X11 PopUp windows as unmanaged
            // notification surfaces. Its Wayland backend still treats PopUp as
            // a normal toplevel, so use XWayland to keep this overlay out of
            // focus handling, the app switcher, and the taskbar.
            command
                .env_remove("WAYLAND_DISPLAY")
                .env("XDG_SESSION_TYPE", "x11");
        }
        // SAFETY: the closure uses only async-signal-safe libc calls between
        // fork and exec. The parent check closes the signal-installation race.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != expected_parent {
                    libc::_exit(128 + libc::SIGTERM);
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        let Some(input) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("overlay helper stdin is unavailable"));
        };
        let Some(output) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other(
                "overlay helper status pipe is unavailable",
            ));
        };
        let process_id = child.id();
        tracing::info!(generation, process_id, "recording overlay helper launched");
        if let Err(error) = std::thread::Builder::new()
            .name("agentdictate-overlay-monitor".into())
            .spawn(move || {
                if let Err(error) =
                    monitor_overlay_status(output, ready_timeout, generation, &events)
                {
                    let _ = events.send(PresenterEvent::HelperStatus {
                        generation,
                        status: Err(error),
                    });
                }
                let result = wait_for_overlay_child(process_id);
                let _ = events.send(PresenterEvent::HelperExited { generation, result });
            })
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok(Self {
            child,
            input: Some(input),
            generation,
            ready: false,
        })
    }

    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    pub(super) fn mark_ready(&mut self) {
        self.ready = true;
    }

    pub(super) fn is_ready(&self) -> bool {
        self.ready
    }

    pub(super) fn send(&mut self, update: &OverlayUpdate) -> io::Result<()> {
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "overlay input is closed"))?;
        serde_json::to_writer(&mut *input, update).map_err(io::Error::other)?;
        input.write_all(b"\n")?;
        input.flush()
    }

    pub(super) fn finish(&mut self) {
        drop(self.input.take());
    }

    pub(super) fn terminate(&mut self) {
        drop(self.input.take());
        let _ = self.child.kill();
    }
}
