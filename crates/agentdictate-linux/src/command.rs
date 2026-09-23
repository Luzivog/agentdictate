use std::{
    ffi::{OsStr, OsString},
    fmt, fs,
    io::{self, Read, Write},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    os::unix::fs::PermissionsExt,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    time::Instant,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlatformTool {
    Xsel,
    Pactl,
    Ffmpeg,
    Systemctl,
}

impl PlatformTool {
    pub const fn executable_name(self) -> &'static str {
        match self {
            Self::Xsel => "xsel",
            Self::Pactl => "pactl",
            Self::Ffmpeg => "ffmpeg",
            Self::Systemctl => "systemctl",
        }
    }

    /// Environment a tool needs for output the adapters can parse.
    const fn environment(self) -> &'static [(&'static str, &'static str)] {
        match self {
            // Volume lines are parsed, so they must not be localized.
            Self::Pactl => &[("LC_ALL", "C")],
            Self::Xsel | Self::Ffmpeg | Self::Systemctl => &[],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlatformCapability {
    Clipboard,
    AudioDucking,
    AudioCompression,
    ServiceManagement,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvailabilityDiagnostic {
    pub capability: PlatformCapability,
    pub missing_tools: Vec<PlatformTool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlatformExecutable {
    tool: PlatformTool,
    path: Option<PathBuf>,
}

impl PlatformExecutable {
    pub fn at(tool: PlatformTool, path: impl Into<PathBuf>) -> Self {
        Self {
            tool,
            path: Some(path.into()),
        }
    }

    pub const fn missing(tool: PlatformTool) -> Self {
        Self { tool, path: None }
    }

    pub fn discover(tool: PlatformTool) -> Self {
        let path = std::env::var_os("PATH").and_then(|search_path| {
            std::env::split_paths(&search_path)
                .map(|directory| directory.join(tool.executable_name()))
                .find(|candidate| is_executable(candidate))
        });
        Self { tool, path }
    }

    pub const fn tool(&self) -> PlatformTool {
        self.tool
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[derive(Debug)]
pub enum PlatformCommandError {
    Unavailable(AvailabilityDiagnostic),
    Start {
        tool: PlatformTool,
        source: io::Error,
    },
    Communicate {
        tool: PlatformTool,
        source: io::Error,
    },
    Failed {
        tool: PlatformTool,
        code: Option<i32>,
        stderr: String,
    },
    Deadline {
        tool: PlatformTool,
    },
    UnexpectedOutput {
        tool: PlatformTool,
        detail: &'static str,
    },
}

impl fmt::Display for PlatformCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(diagnostic) => write!(
                formatter,
                "platform capability {:?} is unavailable; missing {:?}",
                diagnostic.capability, diagnostic.missing_tools
            ),
            Self::Start { tool, .. } => write!(formatter, "could not start {tool:?}"),
            Self::Communicate { tool, .. } => {
                write!(formatter, "could not communicate with {tool:?}")
            }
            Self::Failed { tool, code, stderr } => {
                write!(formatter, "{tool:?} failed with exit code {code:?}")?;
                match stderr.trim() {
                    "" => Ok(()),
                    detail => write!(formatter, ": {detail}"),
                }
            }
            Self::Deadline { tool } => write!(formatter, "{tool:?} exceeded its deadline"),
            Self::UnexpectedOutput { tool, detail } => {
                write!(formatter, "{tool:?} returned unexpected output: {detail}")
            }
        }
    }
}

impl std::error::Error for PlatformCommandError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Start { source, .. } | Self::Communicate { source, .. } => Some(source),
            Self::Unavailable(_)
            | Self::Failed { .. }
            | Self::Deadline { .. }
            | Self::UnexpectedOutput { .. } => None,
        }
    }
}

pub fn require_tools(
    capability: PlatformCapability,
    tools: &[&PlatformExecutable],
) -> Result<(), PlatformCommandError> {
    let mut missing_tools = Vec::new();
    for tool in tools.iter().filter(|tool| tool.path().is_none()) {
        if !missing_tools.contains(&tool.tool()) {
            missing_tools.push(tool.tool());
        }
    }
    if missing_tools.is_empty() {
        Ok(())
    } else {
        Err(PlatformCommandError::Unavailable(AvailabilityDiagnostic {
            capability,
            missing_tools,
        }))
    }
}

/// Runs Linux platform tools without involving a shell.
///
/// Process groups let long-running adapters stop the complete tool tree rather
/// than leaving a helper process behind.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemCommandRunner;

#[derive(Debug)]
pub struct PlatformProcess {
    tool: PlatformTool,
    child: Child,
}

impl PlatformProcess {
    pub fn is_alive(&mut self) -> Result<bool, PlatformCommandError> {
        self.child
            .try_wait()
            .map(|status| status.is_none())
            .map_err(|source| PlatformCommandError::Communicate {
                tool: self.tool,
                source,
            })
    }

    pub fn tool(&self) -> PlatformTool {
        self.tool
    }
}

impl Drop for PlatformProcess {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let process_id = self.child.id();
            if let Ok(process_group) = i32::try_from(process_id) {
                // SAFETY: `kill` does not dereference pointers. This child was
                // started in its own process group below.
                unsafe {
                    libc::kill(-process_group, libc::SIGTERM);
                }
            }
            if matches!(self.child.try_wait(), Ok(None)) {
                kill_group(&mut self.child);
            }
        }
        let _ = self.child.wait();
    }
}

impl SystemCommandRunner {
    pub fn spawn_group(
        &self,
        program: &Path,
        arguments: impl IntoIterator<Item = impl AsRef<OsStr>>,
    ) -> io::Result<Child> {
        let arguments = arguments
            .into_iter()
            .map(|argument| argument.as_ref().to_os_string())
            .collect::<Vec<OsString>>();
        let mut command = Command::new(program);
        command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        command.spawn()
    }

    /// Starts the microphone recorder in its own process group and asks the
    /// kernel to interrupt it if the owning daemon disappears abruptly.
    pub fn spawn_recording_group(
        &self,
        program: &Path,
        arguments: impl IntoIterator<Item = impl AsRef<OsStr>>,
    ) -> io::Result<Child> {
        let arguments = arguments
            .into_iter()
            .map(|argument| argument.as_ref().to_os_string())
            .collect::<Vec<OsString>>();
        // Capturing this before fork closes the classic race where the parent
        // exits before the child installs PR_SET_PDEATHSIG.
        // SAFETY: getpid has no preconditions.
        let expected_parent = unsafe { libc::getpid() };
        let mut command = Command::new(program);
        command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        // SAFETY: the closure calls only async-signal-safe libc functions. If
        // the parent changed before the death signal was installed, `_exit`
        // terminates the child immediately; otherwise the kernel owns the
        // remaining parent-death race.
        unsafe {
            command.pre_exec(move || {
                if libc::signal(libc::SIGINT, libc::SIG_DFL) == libc::SIG_ERR {
                    return Err(io::Error::last_os_error());
                }
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGINT) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != expected_parent {
                    libc::_exit(128 + libc::SIGINT);
                }
                Ok(())
            });
        }
        command.spawn()
    }

    pub fn interrupt_group(&self, process_id: u32) -> io::Result<()> {
        let process_group = i32::try_from(process_id)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "process id is too large"))?;
        // SAFETY: `kill` does not dereference pointers. A negative pid targets
        // the process group created by `spawn_group` and SIGINT lets recorders
        // finalize their output container before exiting.
        let result = unsafe { libc::kill(-process_group, libc::SIGINT) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Runs a tool to completion and returns its stdout. The whole tool
    /// process group is killed if it outlives `deadline`, including helpers
    /// that inherited its pipes.
    pub fn run_output(
        &self,
        capability: PlatformCapability,
        executable: &PlatformExecutable,
        arguments: &[OsString],
        deadline: Instant,
    ) -> Result<Vec<u8>, PlatformCommandError> {
        let tool = executable.tool();
        let Some(program) = executable.path() else {
            return Err(PlatformCommandError::Unavailable(AvailabilityDiagnostic {
                capability,
                missing_tools: vec![tool],
            }));
        };
        let mut child = Command::new(program)
            .args(arguments)
            .envs(tool.environment().iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|source| PlatformCommandError::Start { tool, source })?;
        let (status, stdout, stderr) = match collect_output(&mut child, deadline) {
            Ok(output) => output,
            Err(failure) => {
                kill_group(&mut child);
                return Err(match failure {
                    CollectFailure::Deadline => PlatformCommandError::Deadline { tool },
                    CollectFailure::Io(source) => {
                        PlatformCommandError::Communicate { tool, source }
                    }
                });
            }
        };
        if status.success() {
            Ok(stdout)
        } else {
            Err(PlatformCommandError::Failed {
                tool,
                code: status.code(),
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
            })
        }
    }

    pub fn spawn_owner(
        &self,
        capability: PlatformCapability,
        executable: &PlatformExecutable,
        arguments: &[OsString],
        input: &[u8],
    ) -> Result<PlatformProcess, PlatformCommandError> {
        let Some(program) = executable.path() else {
            return Err(PlatformCommandError::Unavailable(AvailabilityDiagnostic {
                capability,
                missing_tools: vec![executable.tool()],
            }));
        };
        let mut child = Command::new(program)
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|source| PlatformCommandError::Start {
                tool: executable.tool(),
                source,
            })?;
        let Some(mut stdin) = child.stdin.take() else {
            kill_group(&mut child);
            return Err(PlatformCommandError::Communicate {
                tool: executable.tool(),
                source: io::Error::new(io::ErrorKind::BrokenPipe, "child stdin is unavailable"),
            });
        };
        if let Err(source) = stdin.write_all(input) {
            drop(stdin);
            kill_group(&mut child);
            return Err(PlatformCommandError::Communicate {
                tool: executable.tool(),
                source,
            });
        }
        drop(stdin);
        Ok(PlatformProcess {
            tool: executable.tool(),
            child,
        })
    }
}

enum CollectFailure {
    Deadline,
    Io(io::Error),
}

impl From<io::Error> for CollectFailure {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Drains stdout and stderr and waits for the child to exit without
/// spinning: one `poll` covers both pipes and a pidfd for the child, bounded
/// by `deadline`. The caller kills the process group on failure.
fn collect_output(
    child: &mut Child,
    deadline: Instant,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), CollectFailure> {
    let unavailable = |pipe| io::Error::new(io::ErrorKind::BrokenPipe, pipe);
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| unavailable("child stdout is unavailable"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| unavailable("child stderr is unavailable"))?;
    let exit = pidfd_open(child.id())?;
    let (mut stdout_bytes, mut stderr_bytes) = (Vec::new(), Vec::new());
    let (mut stdout_open, mut stderr_open, mut exited) = (true, true, false);
    while stdout_open || stderr_open || !exited {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(CollectFailure::Deadline);
        }
        let watch = |open: bool, fd: i32| libc::pollfd {
            // poll skips negative descriptors, so finished sources stay quiet.
            fd: if open { fd } else { -1 },
            events: libc::POLLIN,
            revents: 0,
        };
        let mut descriptors = [
            watch(stdout_open, stdout.as_raw_fd()),
            watch(stderr_open, stderr.as_raw_fd()),
            watch(!exited, exit.as_raw_fd()),
        ];
        // Round up so a sub-millisecond remainder cannot become a busy loop.
        let timeout = i32::try_from(remaining.as_micros().div_ceil(1000)).unwrap_or(i32::MAX);
        // SAFETY: `descriptors` is an initialized array that outlives the call,
        // and its length is passed alongside the pointer.
        let ready = unsafe {
            libc::poll(
                descriptors.as_mut_ptr(),
                descriptors.len() as libc::nfds_t,
                timeout,
            )
        };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        let [stdout_event, stderr_event, exit_event] = descriptors.map(|fd| fd.revents);
        if stdout_event != 0 {
            stdout_open = read_available(&mut stdout, &mut stdout_bytes)?;
        }
        if stderr_event != 0 {
            stderr_open = read_available(&mut stderr, &mut stderr_bytes)?;
        }
        if exit_event & libc::POLLNVAL != 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "pidfd is invalid").into());
        }
        exited |= exit_event != 0;
    }
    // The pidfd reported exit, so this reaps a zombie without blocking.
    let status = child.wait()?;
    Ok((status, stdout_bytes, stderr_bytes))
}

/// Reads what one ready pipe holds. Returns whether the pipe is still open.
fn read_available(pipe: &mut impl Read, output: &mut Vec<u8>) -> io::Result<bool> {
    let mut chunk = [0_u8; 16 * 1024];
    match pipe.read(&mut chunk) {
        Ok(0) => Ok(false),
        Ok(read) => {
            output.extend_from_slice(&chunk[..read]);
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(true),
        Err(error) => Err(error),
    }
}

/// Opens a pidfd, a descriptor that becomes readable when `process_id`
/// exits. Waiting on it never reaps the child.
pub(crate) fn pidfd_open(process_id: u32) -> io::Result<OwnedFd> {
    // SAFETY: `pidfd_open` receives only integer values and returns a new
    // owned descriptor on success.
    let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, process_id, 0) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = i32::try_from(descriptor)
        .map_err(|_| io::Error::other("pidfd does not fit in a file descriptor"))?;
    // SAFETY: ownership of the fresh descriptor returned by pidfd_open is
    // transferred exactly once to OwnedFd.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}

fn kill_group(child: &mut Child) {
    if let Ok(process_group) = i32::try_from(child.id()) {
        // SAFETY: `kill` does not dereference pointers. The negative pid is the
        // isolated process group created for this command.
        unsafe {
            libc::kill(-process_group, libc::SIGKILL);
        }
    } else {
        let _ = child.kill();
    }
    let _ = child.wait();
}
