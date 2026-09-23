use std::{
    error::Error,
    ffi::OsString,
    fmt,
    fs::{self, File},
    io,
    os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd},
    path::{Path, PathBuf},
    process::{Child, ExitStatus},
    time::{Duration, Instant},
};

use crate::command::{SystemCommandRunner, pidfd_open};

const DROP_FINALIZATION_GRACE: Duration = Duration::from_millis(500);
/// How often start checks the file for its first samples. The wait wakes at
/// once if the recorder exits instead.
const READINESS_CHECK_INTERVAL: Duration = Duration::from_millis(2);

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
            // pw-record asks for a 100 ms node latency by default, which delays
            // the first buffer (readiness) and truncates the tail at stop.
            OsString::from("--latency=20ms"),
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
        let process_id = child.id();
        let exit = match pidfd_open(process_id) {
            Ok(exit) => exit,
            Err(source) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RecorderError::ObserveExit { process_id, source });
            }
        };
        match wait_for_first_samples(&mut child, exit.as_fd(), output, deadline) {
            Ok(data_start) => Ok(Recording {
                runner: self.runner,
                child,
                exit,
                path: output.to_path_buf(),
                data_start,
            }),
            Err(error) => {
                if matches!(child.try_wait(), Ok(None)) {
                    stop_child(&self.runner, &mut child, exit.as_fd(), deadline);
                }
                Err(error)
            }
        }
    }
}

/// Waits until the recorder has written samples past its WAV header and
/// returns the offset of the first sample. Fails if it exits first.
fn wait_for_first_samples(
    child: &mut Child,
    exit: BorrowedFd<'_>,
    output: &Path,
    deadline: Instant,
) -> Result<u64, RecorderError> {
    let inspect = |source| RecorderError::Inspect {
        path: output.to_path_buf(),
        source,
    };
    loop {
        if let Some(status) = child.try_wait().map_err(inspect)? {
            return Err(RecorderError::ExitedBeforeReady { status });
        }
        if let Some(data_start) = first_samples(output)? {
            // The file and the child must both be live in the same observed
            // readiness cycle; a helper that wrote a header and died is not
            // a usable recording session.
            if let Some(status) = child.try_wait().map_err(inspect)? {
                return Err(RecorderError::ExitedBeforeReady { status });
            }
            return Ok(data_start);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(RecorderError::ReadinessDeadline);
        }
        wait_for_pidfd(exit, Some(remaining.min(READINESS_CHECK_INTERVAL))).map_err(inspect)?;
    }
}

/// The offset of the first sample once `path` holds at least one byte past
/// its header. A header still being written means not yet.
fn first_samples(path: &Path) -> Result<Option<u64>, RecorderError> {
    let inspect = |source| RecorderError::Inspect {
        path: path.to_path_buf(),
        source,
    };
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(inspect(source)),
    };
    let data_start = match crate::wav::data_start(&mut file) {
        Ok(data_start) => data_start,
        Err(source) if source.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(source) => return Err(inspect(source)),
    };
    let bytes = file.metadata().map_err(inspect)?.len();
    Ok((bytes > data_start).then_some(data_start))
}

#[derive(Debug)]
pub struct Recording {
    runner: SystemCommandRunner,
    child: Child,
    /// Becomes readable when the recorder exits; never reaps it.
    exit: OwnedFd,
    path: PathBuf,
    /// Offset of the first sample, found when the recording became ready.
    data_start: u64,
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
        wait_for_pidfd(self.pidfd.as_fd(), None).map(drop)
    }
}

/// Blocks until the process behind `pidfd` exits, for at most `timeout`
/// (forever when `None`). Returns whether it exited. Never reaps it.
fn wait_for_pidfd(pidfd: BorrowedFd<'_>, timeout: Option<Duration>) -> io::Result<bool> {
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    loop {
        let timeout_millis = deadline.map_or(-1, |deadline| {
            // Round up so a sub-millisecond remainder cannot become a busy loop.
            let remaining = deadline.saturating_duration_since(Instant::now());
            i32::try_from(remaining.as_micros().div_ceil(1000)).unwrap_or(i32::MAX)
        });
        let mut descriptor = libc::pollfd {
            fd: pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `descriptor` points to one initialized pollfd for the
        // duration of the call. A negative timeout blocks for fd activity.
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout_millis) };
        if result > 0 {
            if descriptor.revents & libc::POLLNVAL != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "recording pidfd is invalid",
                ));
            }
            return Ok(true);
        }
        if result == 0 {
            return Ok(false);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Waits until `child` exits or `deadline` passes, reaping it on exit.
/// Returns whether it exited.
fn wait_until_exit(child: &mut Child, exit: BorrowedFd<'_>, deadline: Instant) -> io::Result<bool> {
    loop {
        if child.try_wait()?.is_some() {
            return Ok(true);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        wait_for_pidfd(exit, Some(remaining))?;
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
        // The child remains owned by `Recording`; the pidfd only observes it.
        let pidfd = self
            .exit
            .try_clone()
            .map_err(|source| RecorderError::ObserveExit { process_id, source })?;
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

        let exited =
            wait_until_exit(&mut self.child, self.exit.as_fd(), deadline).map_err(|source| {
                RecorderError::Inspect {
                    path: self.path.clone(),
                    source,
                }
            })?;
        if !exited {
            let _ = self.child.kill();
            let _ = self.child.wait();
            return Err(RecorderError::StopDeadline);
        }

        let bytes = recording_bytes(&self.path)?;
        if bytes <= self.data_start {
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
                self.exit.as_fd(),
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

/// Interrupts the recorder so it can finalize its file, and kills it if it
/// is still running at `deadline`.
fn stop_child(
    runner: &SystemCommandRunner,
    child: &mut Child,
    exit: BorrowedFd<'_>,
    deadline: Instant,
) {
    let _ = runner.interrupt_group(child.id());
    if !wait_until_exit(child, exit, deadline).unwrap_or(false) {
        let _ = child.kill();
    }
    let _ = child.wait();
}
