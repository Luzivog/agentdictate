use std::{
    ffi::{OsStr, OsString},
    fmt, fs,
    io::{self, Read, Write},
    os::unix::fs::PermissionsExt,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::Instant,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlatformTool {
    Xdotool,
    Xprop,
    Xsel,
}

impl PlatformTool {
    pub const fn executable_name(self) -> &'static str {
        match self {
            Self::Xdotool => "xdotool",
            Self::Xprop => "xprop",
            Self::Xsel => "xsel",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlatformCapability {
    FocusObservation,
    Clipboard,
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
            Self::Failed { tool, code, .. } => {
                write!(formatter, "{tool:?} failed with exit code {code:?}")
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

    pub fn run_output(
        &self,
        capability: PlatformCapability,
        executable: &PlatformExecutable,
        arguments: &[OsString],
        deadline: Instant,
    ) -> Result<Vec<u8>, PlatformCommandError> {
        let Some(program) = executable.path() else {
            return Err(PlatformCommandError::Unavailable(AvailabilityDiagnostic {
                capability,
                missing_tools: vec![executable.tool()],
            }));
        };
        let mut child = Command::new(program)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|source| PlatformCommandError::Start {
                tool: executable.tool(),
                source,
            })?;

        let Some(mut stdout_pipe) = child.stdout.take() else {
            kill_group(&mut child);
            return Err(PlatformCommandError::Communicate {
                tool: executable.tool(),
                source: io::Error::new(io::ErrorKind::BrokenPipe, "child stdout is unavailable"),
            });
        };
        let Some(mut stderr_pipe) = child.stderr.take() else {
            kill_group(&mut child);
            return Err(PlatformCommandError::Communicate {
                tool: executable.tool(),
                source: io::Error::new(io::ErrorKind::BrokenPipe, "child stderr is unavailable"),
            });
        };

        let (status, stdout, stderr) = thread::scope(|scope| {
            let stdout_reader = scope.spawn(move || {
                let mut output = Vec::new();
                stdout_pipe.read_to_end(&mut output).map(|_| output)
            });
            let stderr_reader = scope.spawn(move || {
                let mut output = Vec::new();
                stderr_pipe.read_to_end(&mut output).map(|_| output)
            });

            let mut exit_status = None;
            let status = loop {
                match child.try_wait() {
                    Ok(status) => exit_status = status.or(exit_status),
                    Err(source) => {
                        kill_group(&mut child);
                        break Err(PlatformCommandError::Communicate {
                            tool: executable.tool(),
                            source,
                        });
                    }
                }
                if stdout_reader.is_finished()
                    && stderr_reader.is_finished()
                    && let Some(status) = exit_status
                {
                    break Ok(status);
                }
                if Instant::now() >= deadline {
                    kill_group(&mut child);
                    break Err(PlatformCommandError::Deadline {
                        tool: executable.tool(),
                    });
                }
                thread::yield_now();
            };
            (status, stdout_reader.join(), stderr_reader.join())
        });
        let status = status?;
        let stdout =
            join_pipe_reader(stdout).map_err(|source| PlatformCommandError::Communicate {
                tool: executable.tool(),
                source,
            })?;
        let stderr =
            join_pipe_reader(stderr).map_err(|source| PlatformCommandError::Communicate {
                tool: executable.tool(),
                source,
            })?;
        if status.success() {
            Ok(stdout)
        } else {
            Err(PlatformCommandError::Failed {
                tool: executable.tool(),
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

fn join_pipe_reader(result: thread::Result<io::Result<Vec<u8>>>) -> io::Result<Vec<u8>> {
    result.map_err(|_| io::Error::other("command pipe reader panicked"))?
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
