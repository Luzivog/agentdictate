use std::{
    io::{self, Read, Write},
    os::fd::AsRawFd,
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::mpsc::Sender,
    time::{Duration, Instant},
};

use agentdictate_ui::LogicalRect;

use super::{
    protocol::{
        OVERLAY_HELPER_ARGUMENT, OVERLAY_WORK_AREA, OverlayHelperStatus, OverlayUpdate,
        format_work_area,
    },
    supervisor::PresenterEvent,
};

const MAX_OVERLAY_STATUS_BYTES: usize = 64 * 1024;

fn wait_for_overlay_child(process_id: u32) -> io::Result<()> {
    let process_id = i32::try_from(process_id)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "process id is too large"))?;
    let mut status = 0;
    loop {
        // SAFETY: `status` points to valid writable memory and `process_id`
        // identifies a direct child created by this process. Blocking waitpid
        // is the kernel event source; no timer or polling loop is involved.
        let result = unsafe { libc::waitpid(process_id, &mut status, 0) };
        if result == process_id {
            return Ok(());
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

fn read_overlay_helper_status(
    mut output: ChildStdout,
    timeout: Duration,
) -> Result<OverlayHelperStatus, String> {
    let deadline = Instant::now() + timeout;
    let mut status = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        wait_for_overlay_helper_status(&output, deadline)?;
        match output.read(&mut buffer) {
            Ok(0) if status.is_empty() => {
                return Err("overlay helper exited without reporting readiness".to_owned());
            }
            Ok(0) => return Err("overlay helper readiness message was incomplete".to_owned()),
            Ok(read) => {
                status.extend_from_slice(&buffer[..read]);
                if status.len() > MAX_OVERLAY_STATUS_BYTES {
                    return Err("overlay helper readiness message was too large".to_owned());
                }
                if let Some(newline) = status.iter().position(|byte| *byte == b'\n') {
                    if status[newline + 1..]
                        .iter()
                        .any(|byte| !byte.is_ascii_whitespace())
                    {
                        return Err(
                            "overlay helper wrote unexpected output after readiness".to_owned()
                        );
                    }
                    return serde_json::from_slice(&status[..newline]).map_err(|error| {
                        format!("overlay helper readiness message was invalid: {error}")
                    });
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                return Err(format!(
                    "overlay helper readiness could not be read: {error}"
                ));
            }
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
        work_area: Option<LogicalRect>,
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
            .stderr(Stdio::null())
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
        if let Some(work_area) = work_area {
            command.env(OVERLAY_WORK_AREA, format_work_area(work_area));
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
        if let Err(error) = std::thread::Builder::new()
            .name("agentdictate-overlay-monitor".into())
            .spawn(move || {
                let status = read_overlay_helper_status(output, ready_timeout);
                let _ = events.send(PresenterEvent::HelperStatus { generation, status });
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
