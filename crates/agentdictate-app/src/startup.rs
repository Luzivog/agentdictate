//! Runs the daemon as the `agentdictated.service` systemd user unit.
//!
//! The unit is rendered from the running install (the AppImage file, or the
//! `agentdictated` beside this binary) and written only when that text
//! changes, so an unchanged install never reloads the user manager. "Start on
//! login" is the unit's `graphical-session.target` enablement.

use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use agentdictate_core::ServerMessage;
use agentdictate_linux::command::{
    PlatformCapability, PlatformExecutable, PlatformTool, SystemCommandRunner,
};
use agentdictate_runtime::{IpcClient, IpcError, write_atomic};

use crate::{AppPaths, DaemonSupervision};

pub const DAEMON_SERVICE_NAME: &str = "agentdictated.service";
/// Runs the daemon itself; the unit's `ExecStart` passes it.
pub const SERVICE_ARGUMENT: &str = "--service";
/// Run at login by the XDG autostart entries older versions installed.
pub const START_SERVICE_ARGUMENT: &str = "--start-service";
const DAEMON_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs `systemctl --user <arguments>` and returns its trimmed stdout.
trait Systemctl {
    fn user(&self, arguments: &[&str]) -> io::Result<String>;
}

/// The systemctl executable at this path, bounded by `SYSTEMCTL_TIMEOUT` so a
/// stuck user manager cannot hang the daemon or the settings window.
impl Systemctl for Path {
    fn user(&self, arguments: &[&str]) -> io::Result<String> {
        let arguments = std::iter::once("--user")
            .chain(arguments.iter().copied())
            .map(OsString::from)
            .collect::<Vec<_>>();
        let stdout = SystemCommandRunner
            .run_output(
                PlatformCapability::ServiceManagement,
                &PlatformExecutable::at(PlatformTool::Systemctl, self),
                &arguments,
                Instant::now() + SYSTEMCTL_TIMEOUT,
            )
            .map_err(io::Error::other)?;
        Ok(String::from_utf8_lossy(&stdout).trim().to_owned())
    }
}

/// Connects to the daemon, starting it first when none answers.
///
/// A daemon that speaks this build's protocol is used as it is, so an
/// ordinary window launch runs no systemctl at all. One that speaks another
/// protocol is an upgrade the service has not picked up yet, so the service
/// is restarted. An unsupervised (`AGENTDICTATE_HOME`) instance only waits
/// for the daemon `./run.sh` starts.
pub fn connect_or_start_daemon(paths: &AppPaths) -> anyhow::Result<(IpcClient, ServerMessage)> {
    let daemon = std::env::current_exe()?.with_file_name("agentdictated");
    connect_or_start(
        &paths.runtime,
        &paths.daemon_supervision,
        &service_executable(&daemon),
        Path::new("systemctl"),
        DAEMON_STARTUP_TIMEOUT,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ServiceAction {
    Start,
    Restart,
}

fn connect_or_start(
    runtime: &Path,
    supervision: &DaemonSupervision,
    executable: &Path,
    systemctl: &(impl Systemctl + ?Sized),
    timeout: Duration,
) -> anyhow::Result<(IpcClient, ServerMessage)> {
    let deadline = Instant::now() + timeout;
    let action = match IpcClient::connect(runtime) {
        Ok(connection) => return Ok(connection),
        Err(error) if daemon_is_absent(&error) => ServiceAction::Start,
        Err(IpcError::ProtocolVersion { .. }) => ServiceAction::Restart,
        Err(error) => {
            anyhow::bail!("could not talk to the process holding AgentDictate's socket: {error}")
        }
    };
    match (supervision, action) {
        (DaemonSupervision::SystemdUser { unit_file }, action) => {
            write_unit(unit_file, executable, systemctl)?;
            let verb = match action {
                ServiceAction::Start => "start",
                ServiceAction::Restart => "restart",
            };
            systemctl.user(&[verb, DAEMON_SERVICE_NAME])?;
        }
        (DaemonSupervision::Unsupervised, ServiceAction::Start) => {}
        (DaemonSupervision::Unsupervised, ServiceAction::Restart) => anyhow::bail!(
            "an AgentDictate daemon from another build answers at {}; stop it first",
            runtime.display()
        ),
    }
    wait_for_daemon(runtime, supervision, deadline)
}

/// Polls the socket until a daemon speaking this build's protocol answers.
fn wait_for_daemon(
    runtime: &Path,
    supervision: &DaemonSupervision,
    deadline: Instant,
) -> anyhow::Result<(IpcClient, ServerMessage)> {
    loop {
        let error = match IpcClient::connect(runtime) {
            Ok(connection) => return Ok(connection),
            // Not bound yet, or the replaced daemon is still exiting.
            Err(error)
                if daemon_is_absent(&error)
                    || matches!(
                        error,
                        IpcError::ProtocolVersion { .. } | IpcError::Disconnected
                    ) =>
            {
                error
            }
            Err(error) => return Err(error.into()),
        };
        if Instant::now() >= deadline {
            match supervision {
                DaemonSupervision::SystemdUser { .. } => anyhow::bail!(
                    "AgentDictate's background service did not start ({error}); \
                     see `journalctl --user -u {DAEMON_SERVICE_NAME}`"
                ),
                DaemonSupervision::Unsupervised => anyhow::bail!(
                    "no AgentDictate daemon answered at {} ({error}); \
                     start one with `./run.sh --service`",
                    runtime.display()
                ),
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn daemon_is_absent(error: &IpcError) -> bool {
    matches!(
        error,
        IpcError::Io(error)
            if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused)
    )
}

/// Keeps `agentdictated.service` in step with the "Start on login" setting.
#[derive(Clone, Debug)]
pub(crate) struct LoginStartup {
    pub(crate) supervision: DaemonSupervision,
    pub(crate) legacy_autostart_file: PathBuf,
    /// The systemctl to run; tests point it at a stand-in.
    pub(crate) systemctl: PathBuf,
}

impl LoginStartup {
    pub(crate) fn new(paths: &AppPaths) -> Self {
        Self {
            supervision: paths.daemon_supervision.clone(),
            legacy_autostart_file: paths.legacy_autostart_file.clone(),
            systemctl: PathBuf::from("systemctl"),
        }
    }

    /// Run by the daemon at startup and whenever the setting changes: writes
    /// the unit if the install changed, enables or disables it to match
    /// `start_on_login`, then deletes the XDG autostart entry older versions
    /// used instead. An unsupervised daemon has no unit and does nothing.
    pub(crate) fn sync(&self, start_on_login: bool) -> io::Result<()> {
        let DaemonSupervision::SystemdUser { unit_file } = &self.supervision else {
            return Ok(());
        };
        let executable = service_executable(&std::env::current_exe()?);
        sync_enablement(
            unit_file,
            &executable,
            start_on_login,
            self.systemctl.as_path(),
        )?;
        match fs::remove_file(&self.legacy_autostart_file) {
            Ok(()) => {
                tracing::info!("replaced the old login autostart entry with the service unit");
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// Calls `enable`/`disable` only when the unit's enablement differs from
/// `start_on_login`. Neither uses `--now`: this runs inside the daemon, which
/// is already running and must not stop itself.
fn sync_enablement(
    unit_file: &Path,
    executable: &Path,
    start_on_login: bool,
    systemctl: &(impl Systemctl + ?Sized),
) -> io::Result<()> {
    write_unit(unit_file, executable, systemctl)?;
    let state = systemctl.user(&[
        "show",
        DAEMON_SERVICE_NAME,
        "--property",
        "UnitFileState",
        "--value",
    ])?;
    let enabled = matches!(state.as_str(), "enabled" | "enabled-runtime");
    if enabled != start_on_login {
        let verb = if start_on_login { "enable" } else { "disable" };
        systemctl.user(&[verb, DAEMON_SERVICE_NAME])?;
    }
    Ok(())
}

/// What the unit runs: `daemon`, or the AppImage file when `daemon` sits in
/// that AppImage's mount, whose path changes on every launch.
fn service_executable(daemon: &Path) -> PathBuf {
    running_app_image(daemon).unwrap_or_else(|| daemon.to_owned())
}

/// The AppImage file `executable` was mounted from. Every child process of
/// any AppImage inherits `APPIMAGE` and `APPDIR`, so they count only when
/// `APPDIR` really contains `executable`.
pub(crate) fn running_app_image(executable: &Path) -> Option<PathBuf> {
    app_image_containing(
        executable,
        std::env::var_os("APPIMAGE"),
        std::env::var_os("APPDIR"),
    )
}

fn app_image_containing(
    executable: &Path,
    app_image: Option<OsString>,
    app_dir: Option<OsString>,
) -> Option<PathBuf> {
    let app_dir = PathBuf::from(app_dir?);
    (!app_dir.as_os_str().is_empty() && executable.starts_with(&app_dir))
        .then(|| app_image.map(PathBuf::from))
        .flatten()
}

/// Writes the unit and reloads the user manager, but only when the rendered
/// text differs from the file on disk. Returns whether it wrote.
fn write_unit(
    unit_file: &Path,
    executable: &Path,
    systemctl: &(impl Systemctl + ?Sized),
) -> io::Result<bool> {
    let contents = render_unit(executable);
    if fs::read(unit_file).is_ok_and(|current| current == contents.as_bytes()) {
        return Ok(false);
    }
    write_atomic(unit_file, contents.as_bytes(), 0o600)?;
    systemctl.user(&["daemon-reload"])?;
    Ok(true)
}

/// `PartOf` stops the daemon with the desktop session; `WantedBy` is what
/// "Start on login" enables.
fn render_unit(executable: &Path) -> String {
    let executable = quote_systemd_exec_value(&executable.to_string_lossy());
    format!(
        "[Unit]\n\
Description=AgentDictate background service\n\
PartOf=graphical-session.target\n\
After=graphical-session.target\n\
\n\
[Service]\n\
Type=simple\n\
UMask=0077\n\
ExecStart={executable} {SERVICE_ARGUMENT}\n\
Restart=on-failure\n\
RestartSec=1s\n\
\n\
[Install]\n\
WantedBy=graphical-session.target\n"
    )
}

/// Quotes one `ExecStart` word, escaping systemd's `$` and `%` expansion.
fn quote_systemd_exec_value(value: &str) -> String {
    if value.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '/' | '.' | '_' | '-')
    }) {
        return value.to_owned();
    }
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "$$")
        .replace('%', "%%");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use std::{
        io::Write,
        os::unix::net::UnixListener,
        sync::{Mutex, mpsc},
    };

    use agentdictate_core::{
        AppSnapshot, ClientCommand, HotkeyReadiness, PROTOCOL_VERSION, Settings, Workflow,
    };
    use agentdictate_runtime::{IpcHandler, IpcServer};
    use tempfile::tempdir;

    use super::*;

    type Launch = Box<dyn FnOnce() + Send>;

    /// Records every `systemctl --user` call. `start` and `restart` run the
    /// `launch` closure, which stands in for systemd running the daemon.
    struct FakeSystemctl {
        calls: Mutex<Vec<String>>,
        unit_file_state: &'static str,
        launch: Mutex<Option<Launch>>,
    }

    impl FakeSystemctl {
        fn new(unit_file_state: &'static str) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                unit_file_state,
                launch: Mutex::new(None),
            }
        }

        fn on_launch(self, launch: impl FnOnce() + Send + 'static) -> Self {
            *self.launch.lock().unwrap() = Some(Box::new(launch));
            self
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Systemctl for FakeSystemctl {
        fn user(&self, arguments: &[&str]) -> io::Result<String> {
            self.calls.lock().unwrap().push(arguments.join(" "));
            match arguments.first().copied() {
                Some("start" | "restart") => {
                    if let Some(launch) = self.launch.lock().unwrap().take() {
                        launch();
                    }
                    Ok(String::new())
                }
                Some("show") => Ok(self.unit_file_state.to_owned()),
                _ => Ok(String::new()),
            }
        }
    }

    fn supervised(root: &Path) -> DaemonSupervision {
        DaemonSupervision::SystemdUser {
            unit_file: root.join("systemd/user/agentdictated.service"),
        }
    }

    /// A daemon that binds after `delay` and serves one session.
    fn daemon(runtime: PathBuf, delay: Duration) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            thread::sleep(delay);
            IpcServer::bind(runtime)
                .unwrap()
                .serve_next(&SnapshotHandler)
                .unwrap();
        })
    }

    fn connect(
        runtime: &Path,
        supervision: &DaemonSupervision,
        systemctl: &FakeSystemctl,
    ) -> anyhow::Result<(IpcClient, ServerMessage)> {
        connect_or_start(
            runtime,
            supervision,
            Path::new("/usr/bin/agentdictated"),
            systemctl,
            Duration::from_secs(1),
        )
    }

    #[test]
    fn an_answering_daemon_is_used_without_any_systemctl_call() {
        let directory = tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        let server = IpcServer::bind(&runtime).unwrap();
        let running = thread::spawn(move || server.serve_next(&SnapshotHandler).unwrap());
        let supervision = supervised(directory.path());
        let systemctl = FakeSystemctl::new("enabled");

        drop(connect(&runtime, &supervision, &systemctl).unwrap());

        running.join().unwrap();
        assert!(systemctl.calls().is_empty());
        let DaemonSupervision::SystemdUser { unit_file } = supervision else {
            unreachable!()
        };
        assert!(!unit_file.exists());
    }

    #[test]
    fn a_missing_daemon_is_started_through_a_freshly_written_unit() {
        let directory = tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        let (started, started_daemon) = mpsc::channel();
        let service_runtime = runtime.clone();
        let systemctl = FakeSystemctl::new("disabled").on_launch(move || {
            started
                .send(daemon(service_runtime, Duration::ZERO))
                .unwrap();
        });

        drop(connect(&runtime, &supervised(directory.path()), &systemctl).unwrap());

        started_daemon.recv().unwrap().join().unwrap();
        assert_eq!(
            systemctl.calls(),
            ["daemon-reload", "start agentdictated.service"]
        );
    }

    #[test]
    fn a_daemon_on_another_protocol_is_restarted() {
        let directory = tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        fs::create_dir_all(&runtime).unwrap();
        let listener = UnixListener::bind(runtime.join("agentdictate.sock")).unwrap();
        let old_daemon = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let greeting = ServerMessage {
                protocol_version: PROTOCOL_VERSION + 1,
                ..SnapshotHandler.snapshot()
            };
            writeln!(stream, "{}", serde_json::to_string(&greeting).unwrap()).unwrap();
        });
        let (started, started_daemon) = mpsc::channel();
        let service_runtime = runtime.clone();
        // Like `systemctl restart`, the old daemon is gone before it returns.
        let systemctl = FakeSystemctl::new("enabled").on_launch(move || {
            old_daemon.join().unwrap();
            started
                .send(daemon(service_runtime, Duration::ZERO))
                .unwrap();
        });

        drop(connect(&runtime, &supervised(directory.path()), &systemctl).unwrap());

        started_daemon.recv().unwrap().join().unwrap();
        assert_eq!(
            systemctl.calls(),
            ["daemon-reload", "restart agentdictated.service"]
        );
    }

    #[test]
    fn an_unsupervised_instance_waits_for_its_daemon_and_never_runs_systemctl() {
        let directory = tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        let late_daemon = daemon(runtime.clone(), Duration::from_millis(30));
        let systemctl = FakeSystemctl::new("enabled");

        drop(connect(&runtime, &DaemonSupervision::Unsupervised, &systemctl).unwrap());

        late_daemon.join().unwrap();
        assert!(systemctl.calls().is_empty());
    }

    #[test]
    fn a_malformed_greeting_never_starts_a_competing_daemon() {
        let directory = tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        fs::create_dir_all(&runtime).unwrap();
        let listener = UnixListener::bind(runtime.join("agentdictate.sock")).unwrap();
        let stranger = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"not-json\n").unwrap();
        });
        let systemctl = FakeSystemctl::new("enabled");

        let Err(error) = connect(&runtime, &supervised(directory.path()), &systemctl) else {
            panic!("a malformed greeting must not count as a daemon")
        };

        stranger.join().unwrap();
        assert!(error.to_string().contains("holding AgentDictate's socket"));
        assert!(systemctl.calls().is_empty());
    }

    #[test]
    fn the_unit_is_rewritten_and_reloaded_only_when_its_text_changes() {
        let directory = tempdir().unwrap();
        let unit_file = directory.path().join("systemd/user/agentdictated.service");
        let systemctl = FakeSystemctl::new("enabled");
        let appimage = Path::new("/opt/Agent Dictate.AppImage");

        assert!(write_unit(&unit_file, appimage, &systemctl).unwrap());
        assert!(!write_unit(&unit_file, appimage, &systemctl).unwrap());
        assert!(write_unit(&unit_file, Path::new("/usr/bin/agentdictated"), &systemctl).unwrap());

        assert_eq!(systemctl.calls(), ["daemon-reload", "daemon-reload"]);
        let unit = fs::read_to_string(&unit_file).unwrap();
        assert!(unit.contains("\nExecStart=/usr/bin/agentdictated --service\n"));
    }

    #[test]
    fn login_enablement_changes_only_when_it_differs_from_the_setting() {
        for (state, start_on_login, expected) in [
            ("enabled", true, None),
            ("disabled", false, None),
            ("disabled", true, Some("enable agentdictated.service")),
            ("static", true, Some("enable agentdictated.service")),
            ("enabled", false, Some("disable agentdictated.service")),
        ] {
            let directory = tempdir().unwrap();
            let unit_file = directory.path().join("agentdictated.service");
            let executable = Path::new("/usr/bin/agentdictated");
            fs::write(&unit_file, render_unit(executable)).unwrap();
            let systemctl = FakeSystemctl::new(state);

            sync_enablement(&unit_file, executable, start_on_login, &systemctl).unwrap();

            let show = "show agentdictated.service --property UnitFileState --value";
            let expected_calls = std::iter::once(show).chain(expected).collect::<Vec<_>>();
            assert_eq!(
                systemctl.calls(),
                expected_calls,
                "{state} -> {start_on_login}"
            );
        }
    }

    #[test]
    fn appimage_variables_inherited_from_another_app_are_ignored() {
        let mounted = Path::new("/tmp/.mount_AgentDi1/usr/bin/agentdictated");
        let installed = Path::new("/home/me/.local/bin/agentdictated");
        let ours = || Some(OsString::from("/home/me/AgentDictate.AppImage"));

        assert_eq!(
            app_image_containing(mounted, ours(), Some("/tmp/.mount_AgentDi1".into())),
            Some(PathBuf::from("/home/me/AgentDictate.AppImage"))
        );
        assert_eq!(
            app_image_containing(installed, ours(), Some("/tmp/.mount_Editor42".into())),
            None
        );
        assert_eq!(app_image_containing(installed, ours(), None), None);
    }

    #[test]
    fn exec_start_escapes_systemd_expansion_syntax() {
        assert_eq!(
            quote_systemd_exec_value("/usr/bin/agentdictated"),
            "/usr/bin/agentdictated"
        );
        assert_eq!(
            quote_systemd_exec_value("/opt/cash$ 50%"),
            "\"/opt/cash$$ 50%%\""
        );
    }

    struct SnapshotHandler;

    impl IpcHandler for SnapshotHandler {
        fn snapshot(&self) -> ServerMessage {
            ServerMessage::snapshot(
                AppSnapshot {
                    workflow: Workflow::new().snapshot(),
                    hotkey: HotkeyReadiness::Ready,
                    recoverable_count: 0,
                    last_transcript: None,
                },
                &Settings::default(),
            )
        }

        fn handle(&self, _command: ClientCommand) -> ServerMessage {
            panic!("startup sends no commands")
        }
    }
}
