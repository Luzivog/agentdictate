use std::{
    error::Error,
    ffi::OsString,
    fmt, fs, io,
    os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
    path::{Path, PathBuf},
    process::{Child, ExitStatus},
    thread,
    time::Instant,
};

use crate::command::SystemCommandRunner;

const WAV_HEADER_BYTES: u64 = 44;
const DROP_FINALIZATION_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingStatus {
    Capturing { bytes: u64 },
    Exited { status: ExitStatus },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordingArtifact {
    pub path: PathBuf,
    pub bytes: u64,
}

#[derive(Debug)]
pub enum RecorderError {
    CreateParent { path: PathBuf, source: io::Error },
    Spawn { program: PathBuf, source: io::Error },
    Inspect { path: PathBuf, source: io::Error },
    ExitedBeforeReady { status: ExitStatus },
    ReadinessDeadline,
    Interrupt(io::Error),
    ObserveExit { process_id: u32, source: io::Error },
    StopDeadline,
    EmptyRecording { path: PathBuf, bytes: u64 },
}

impl fmt::Display for RecorderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateParent { path, .. } => {
                write!(
                    formatter,
                    "could not create recording directory: {}",
                    path.display()
                )
            }
            Self::Spawn { program, .. } => {
                write!(formatter, "could not start recorder: {}", program.display())
            }
            Self::Inspect { path, .. } => {
                write!(formatter, "could not inspect recording: {}", path.display())
            }
            Self::ExitedBeforeReady { status } => {
                write!(
                    formatter,
                    "recorder exited before audio was captured: {status}"
                )
            }
            Self::ReadinessDeadline => {
                formatter.write_str("recorder did not capture audio before the deadline")
            }
            Self::Interrupt(_) => formatter.write_str("could not stop recorder cleanly"),
            Self::ObserveExit { process_id, .. } => {
                write!(formatter, "could not observe recorder process {process_id}")
            }
            Self::StopDeadline => formatter.write_str("recorder did not stop before the deadline"),
            Self::EmptyRecording { path, bytes } => write!(
                formatter,
                "recording contains no audio samples: {} ({bytes} bytes)",
                path.display()
            ),
        }
    }
}

impl Error for RecorderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CreateParent { source, .. }
            | Self::Spawn { source, .. }
            | Self::Inspect { source, .. }
            | Self::ObserveExit { source, .. } => Some(source),
            Self::Interrupt(source) => Some(source),
            Self::ExitedBeforeReady { .. }
            | Self::ReadinessDeadline
            | Self::StopDeadline
            | Self::EmptyRecording { .. } => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PwRecordRecorder {
    runner: SystemCommandRunner,
    program: PathBuf,
}

impl PwRecordRecorder {
    pub fn new(runner: SystemCommandRunner, program: impl Into<PathBuf>) -> Self {
        Self {
            runner,
            program: program.into(),
        }
    }

    pub fn start(&self, output: &Path, deadline: Instant) -> Result<Recording, RecorderError> {
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(|source| RecorderError::CreateParent {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        match fs::remove_file(output) {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(RecorderError::Inspect {
                    path: output.to_path_buf(),
                    source,
                });
            }
        }

        let arguments = [
            OsString::from("--media-category=Capture"),
            OsString::from("--rate=16000"),
            OsString::from("--channels=1"),
            OsString::from("--format=s16"),
            output.as_os_str().to_os_string(),
        ];
        let mut child = self
            .runner
            .spawn_recording_group(&self.program, arguments)
            .map_err(|source| RecorderError::Spawn {
                program: self.program.clone(),
                source,
            })?;

        loop {
            if let Some(status) = child.try_wait().map_err(|source| RecorderError::Inspect {
                path: output.to_path_buf(),
                source,
            })? {
                return Err(RecorderError::ExitedBeforeReady { status });
            }
            if recording_bytes(output)? > WAV_HEADER_BYTES {
                // The file and the child must both be live in the same observed
                // readiness cycle; a helper that wrote a header and died is not
                // a usable recording session.
                if let Some(status) = child.try_wait().map_err(|source| RecorderError::Inspect {
                    path: output.to_path_buf(),
                    source,
                })? {
                    return Err(RecorderError::ExitedBeforeReady { status });
                }
                return Ok(Recording {
                    runner: self.runner,
                    child,
                    path: output.to_path_buf(),
                });
            }
            if Instant::now() >= deadline {
                stop_child(&self.runner, &mut child, deadline);
                return Err(RecorderError::ReadinessDeadline);
            }
            // Readiness is the file/liveness condition above, never this yield.
            // Yielding merely avoids starving the recorder while checking it.
            thread::yield_now();
        }
    }
}

#[derive(Debug)]
pub struct Recording {
    runner: SystemCommandRunner,
    child: Child,
    path: PathBuf,
}

/// An independent kernel handle that becomes readable when the recorder exits.
///
/// Waiting on this handle never reaps or consumes the child process; `Recording`
/// remains the sole owner responsible for stop/finalization and exit status.
#[derive(Debug)]
pub struct RecordingExitObserver {
    process_id: u32,
    pidfd: OwnedFd,
}

impl RecordingExitObserver {
    pub const fn process_id(&self) -> u32 {
        self.process_id
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            process_id: self.process_id,
            pidfd: self.pidfd.try_clone()?,
        })
    }

    /// Blocks on the pidfd until the kernel reports process exit. No process is
    /// reaped here, and no polling interval or correctness delay is involved.
    pub fn wait(&self) -> io::Result<()> {
        let mut descriptor = libc::pollfd {
            fd: self.pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            // SAFETY: `descriptor` points to one initialized pollfd for the
            // duration of the call. A negative timeout blocks for fd activity.
            let result = unsafe { libc::poll(&mut descriptor, 1, -1) };
            if result > 0 {
                if descriptor.revents & libc::POLLNVAL != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "recording pidfd is invalid",
                    ));
                }
                if descriptor.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                    return Ok(());
                }
                continue;
            }
            if result == 0 {
                continue;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

impl AsFd for RecordingExitObserver {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.pidfd.as_fd()
    }
}

impl AsRawFd for RecordingExitObserver {
    fn as_raw_fd(&self) -> i32 {
        self.pidfd.as_raw_fd()
    }
}

impl Recording {
    pub fn exit_observer(&self) -> Result<RecordingExitObserver, RecorderError> {
        let process_id = self.child.id();
        // SAFETY: `pidfd_open` receives only integer values and returns a new
        // owned descriptor on success. The child remains owned by `Recording`.
        let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, process_id, 0) };
        if descriptor < 0 {
            return Err(RecorderError::ObserveExit {
                process_id,
                source: io::Error::last_os_error(),
            });
        }
        let descriptor = i32::try_from(descriptor).map_err(|_| RecorderError::ObserveExit {
            process_id,
            source: io::Error::other("pidfd does not fit in a file descriptor"),
        })?;
        // SAFETY: ownership of the fresh descriptor returned by pidfd_open is
        // transferred exactly once to OwnedFd.
        let pidfd = unsafe { OwnedFd::from_raw_fd(descriptor) };
        Ok(RecordingExitObserver { process_id, pidfd })
    }

    pub fn status(&mut self) -> Result<RecordingStatus, RecorderError> {
        if let Some(status) = self
            .child
            .try_wait()
            .map_err(|source| RecorderError::Inspect {
                path: self.path.clone(),
                source,
            })?
        {
            return Ok(RecordingStatus::Exited { status });
        }
        Ok(RecordingStatus::Capturing {
            bytes: recording_bytes(&self.path)?,
        })
    }

    pub fn stop(mut self, deadline: Instant) -> Result<RecordingArtifact, RecorderError> {
        if self
            .child
            .try_wait()
            .map_err(|source| RecorderError::Inspect {
                path: self.path.clone(),
                source,
            })?
            .is_none()
            && let Err(source) = self.runner.interrupt_group(self.child.id())
            && self
                .child
                .try_wait()
                .map_err(|wait_source| RecorderError::Inspect {
                    path: self.path.clone(),
                    source: wait_source,
                })?
                .is_none()
        {
            return Err(RecorderError::Interrupt(source));
        }

        while self
            .child
            .try_wait()
            .map_err(|source| RecorderError::Inspect {
                path: self.path.clone(),
                source,
            })?
            .is_none()
        {
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                return Err(RecorderError::StopDeadline);
            }
            thread::yield_now();
        }

        let bytes = recording_bytes(&self.path)?;
        if bytes <= WAV_HEADER_BYTES {
            return Err(RecorderError::EmptyRecording {
                path: self.path.clone(),
                bytes,
            });
        }
        Ok(RecordingArtifact {
            path: self.path.clone(),
            bytes,
        })
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            stop_child(
                &self.runner,
                &mut self.child,
                Instant::now() + DROP_FINALIZATION_GRACE,
            );
        }
    }
}

fn recording_bytes(path: &Path) -> Result<u64, RecorderError> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(source) => Err(RecorderError::Inspect {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn stop_child(runner: &SystemCommandRunner, child: &mut Child, deadline: Instant) {
    let _ = runner.interrupt_group(child.id());
    while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
        thread::yield_now();
    }
    if matches!(child.try_wait(), Ok(None)) {
        let _ = child.kill();
    }
    let _ = child.wait();
}
