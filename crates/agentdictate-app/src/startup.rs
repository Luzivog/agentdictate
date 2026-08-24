use std::{
    fs, io,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use agentdictate_core::{ClientCommand, ServerMessageKind};
use agentdictate_runtime::{IpcClient, IpcError};
use sha2::{Digest, Sha256};

const AUTOSTART_ENTRY: &str = "local.agentdictate.AgentDictate.desktop";
pub const DAEMON_SERVICE_NAME: &str = "agentdictated.service";
pub const START_SERVICE_ARGUMENT: &str = "--start-service";
pub const SERVICE_ARGUMENT: &str = "--service";
const APPIMAGE_BOOTSTRAP_ARGUMENT: &str = "--background";
const SERVICE_ROUTE_ENVIRONMENT: &str = "AGENTDICTATE_SERVICE_ROUTE";
const SERVICE_IDENTITY_FILE_ENVIRONMENT: &str = "AGENTDICTATE_SERVICE_IDENTITY_FILE";
const DAEMON_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
static STARTUP_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
struct StartupCommand {
    executable: PathBuf,
    arguments: Vec<String>,
    identity_files: Vec<PathBuf>,
}

trait UserServiceManager {
    fn daemon_reload(&self) -> io::Result<()>;
    fn is_active(&self) -> io::Result<bool>;
    fn start(&self) -> io::Result<()>;
    fn restart(&self) -> io::Result<()>;
    fn owns_process(&self, pid: u32) -> io::Result<bool>;
    fn process_route_identity(&self, pid: u32) -> io::Result<Option<String>>;
}

struct SystemctlServiceManager<'a> {
    command: &'a Path,
}

impl UserServiceManager for SystemctlServiceManager<'_> {
    fn daemon_reload(&self) -> io::Result<()> {
        run_systemctl(self.command, &["--user", "daemon-reload"])
    }

    fn is_active(&self) -> io::Result<bool> {
        service_is_active(self.command)
    }

    fn start(&self) -> io::Result<()> {
        run_systemctl(self.command, &["--user", "start", DAEMON_SERVICE_NAME])
    }

    fn restart(&self) -> io::Result<()> {
        run_systemctl(self.command, &["--user", "restart", DAEMON_SERVICE_NAME])
    }

    fn owns_process(&self, pid: u32) -> io::Result<bool> {
        let control_group = systemctl_value(self.command, "ControlGroup")?;
        if control_group.is_empty() || control_group == "/" {
            return Ok(false);
        }
        let process_groups = fs::read_to_string(format!("/proc/{pid}/cgroup"))?;
        Ok(process_groups.lines().any(|line| {
            let mut fields = line.splitn(3, ':');
            let _hierarchy = fields.next();
            let controllers = fields.next();
            let path = fields.next();
            matches!((controllers, path), (Some(""), Some(path)) if cgroup_contains(&control_group, path))
                || matches!((controllers, path), (Some(controllers), Some(path)) if controllers.split(',').any(|controller| controller == "name=systemd") && cgroup_contains(&control_group, path))
        }))
    }

    fn process_route_identity(&self, pid: u32) -> io::Result<Option<String>> {
        process_environment_value(pid, SERVICE_ROUTE_ENVIRONMENT)
    }
}

pub fn sync_startup_with_systemctl(
    entry: &Path,
    service: &Path,
    enabled: bool,
    daemon: &Path,
    systemctl: &Path,
) -> io::Result<()> {
    let (daemon, bootstrap) = startup_commands(daemon);
    sync_startup_commands_with_manager(
        entry,
        service,
        enabled,
        &daemon,
        &bootstrap,
        &SystemctlServiceManager { command: systemctl },
    )
}

#[allow(clippy::too_many_arguments)]
pub fn sync_startup_command(
    entry: &Path,
    service: &Path,
    enabled: bool,
    daemon_executable: &Path,
    daemon_arguments: &[String],
    bootstrap_executable: &Path,
    bootstrap_arguments: &[String],
    systemctl: &Path,
) -> io::Result<()> {
    let daemon = StartupCommand {
        executable: daemon_executable.to_owned(),
        arguments: daemon_arguments.to_owned(),
        identity_files: vec![daemon_executable.to_owned()],
    };
    let bootstrap = StartupCommand {
        executable: bootstrap_executable.to_owned(),
        arguments: bootstrap_arguments.to_owned(),
        identity_files: Vec::new(),
    };
    sync_startup_commands_with_manager(
        entry,
        service,
        enabled,
        &daemon,
        &bootstrap,
        &SystemctlServiceManager { command: systemctl },
    )
}

fn sync_startup_commands_with_manager(
    entry: &Path,
    service: &Path,
    enabled: bool,
    daemon: &StartupCommand,
    bootstrap: &StartupCommand,
    manager: &impl UserServiceManager,
) -> io::Result<()> {
    write_daemon_service(service, daemon)?;
    write_autostart_entry(entry, enabled, &bootstrap.executable, &bootstrap.arguments)?;
    manager.daemon_reload()
}

/// Converges legacy direct launches onto the named user service, then waits
/// until that service owns the AgentDictate IPC endpoint.
pub fn bootstrap_daemon_service(
    runtime_directory: &Path,
    service: &Path,
    daemon: &Path,
) -> anyhow::Result<()> {
    let (daemon, _) = startup_commands(daemon);
    let manager = SystemctlServiceManager {
        command: Path::new("systemctl"),
    };
    let route_identity = prepare_daemon_service_command(service, &daemon, &manager)?;
    bootstrap_daemon_service_with_manager(
        runtime_directory,
        &manager,
        &route_identity,
        DAEMON_STARTUP_TIMEOUT,
    )
}

fn prepare_daemon_service_command(
    service: &Path,
    daemon: &StartupCommand,
    manager: &impl UserServiceManager,
) -> io::Result<String> {
    let route_identity = write_daemon_service(service, daemon)?;
    manager.daemon_reload()?;
    Ok(route_identity)
}

fn bootstrap_daemon_service_with_manager(
    runtime_directory: &Path,
    manager: &impl UserServiceManager,
    expected_route_identity: &str,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    let active = manager.is_active()?;
    let mut endpoint = inspect_daemon(runtime_directory, manager, expected_route_identity)?;
    if active && endpoint.is_none() {
        endpoint = Some(wait_for_endpoint(
            runtime_directory,
            manager,
            expected_route_identity,
            deadline,
        )?);
    }
    match endpoint {
        Some(DaemonEndpoint::Expected(client)) if active && manager.is_active()? => {
            drop(client);
            return Ok(());
        }
        Some(DaemonEndpoint::Expected(client))
        | Some(DaemonEndpoint::Foreign(client))
        | Some(DaemonEndpoint::WrongRoute(client)) => {
            stop_daemon(runtime_directory, client, deadline)?;
            if active {
                manager.restart()?;
            } else {
                manager.start()?;
            }
        }
        None => manager.start()?,
    }
    wait_for_daemon(
        runtime_directory,
        manager,
        expected_route_identity,
        deadline,
    )
}

fn startup_commands(daemon: &Path) -> (StartupCommand, StartupCommand) {
    if let Some(app_image) = std::env::var_os("APPIMAGE") {
        let executable = PathBuf::from(app_image);
        return (
            StartupCommand {
                executable: executable.clone(),
                arguments: vec![SERVICE_ARGUMENT.to_owned()],
                identity_files: vec![executable.clone()],
            },
            StartupCommand {
                executable,
                arguments: vec![APPIMAGE_BOOTSTRAP_ARGUMENT.to_owned()],
                identity_files: Vec::new(),
            },
        );
    }
    if let Some(executable) = std::env::var_os("AGENTDICTATE_AUTOSTART_EXEC") {
        let executable = PathBuf::from(executable);
        let bootstrap_argument = std::env::var("AGENTDICTATE_AUTOSTART_ARG")
            .ok()
            .filter(|argument| !argument.is_empty())
            .unwrap_or_else(|| APPIMAGE_BOOTSTRAP_ARGUMENT.to_owned());
        let service_executable = std::env::var_os("AGENTDICTATE_SERVICE_EXEC")
            .map_or_else(|| executable.clone(), PathBuf::from);
        let service_argument = std::env::var("AGENTDICTATE_SERVICE_ARG")
            .ok()
            .filter(|argument| !argument.is_empty())
            .unwrap_or_else(|| SERVICE_ARGUMENT.to_owned());
        let identity_file = std::env::var_os(SERVICE_IDENTITY_FILE_ENVIRONMENT)
            .map_or_else(|| service_executable.clone(), PathBuf::from);
        return (
            StartupCommand {
                executable: service_executable,
                arguments: vec![service_argument],
                identity_files: vec![identity_file],
            },
            StartupCommand {
                executable,
                arguments: vec![bootstrap_argument],
                identity_files: Vec::new(),
            },
        );
    }
    (
        StartupCommand {
            executable: daemon.to_owned(),
            arguments: vec![SERVICE_ARGUMENT.to_owned()],
            identity_files: vec![daemon.to_owned()],
        },
        StartupCommand {
            executable: daemon.to_owned(),
            arguments: vec![START_SERVICE_ARGUMENT.to_owned()],
            identity_files: Vec::new(),
        },
    )
}

fn write_daemon_service(service: &Path, daemon: &StartupCommand) -> io::Result<String> {
    let route_identity = service_route_identity(daemon);
    let mut command = quote_systemd_exec_value(&daemon.executable.to_string_lossy());
    for argument in &daemon.arguments {
        command.push(' ');
        command.push_str(&quote_systemd_exec_value(argument));
    }
    let contents = format!(
        "[Unit]\n\
Description=AgentDictate background service\n\
PartOf=graphical-session.target\n\
After=graphical-session.target\n\
\n\
[Service]\n\
Type=simple\n\
UMask=0077\n\
Environment={SERVICE_ROUTE_ENVIRONMENT}={route_identity}\n\
ExecStart={command}\n\
Restart=on-failure\n\
RestartSec=1s\n"
    );
    write_atomic(service, &contents)?;
    Ok(route_identity)
}

fn write_autostart_entry(
    entry: &Path,
    enabled: bool,
    executable: &Path,
    arguments: &[String],
) -> io::Result<()> {
    let contents = if enabled {
        let mut command = quote_desktop_exec_value(&executable.to_string_lossy());
        for argument in arguments {
            command.push(' ');
            command.push_str(&quote_desktop_exec_value(argument));
        }
        format!(
            "[Desktop Entry]\n\
Type=Application\n\
Name=AgentDictate background service\n\
Comment=Keep the AgentDictate global shortcut ready\n\
Exec={command}\n\
Icon=agentdictate\n\
Terminal=false\n\
NoDisplay=true\n\
X-GNOME-Autostart-enabled=true\n"
        )
    } else {
        // A Hidden user entry is the freedesktop override for package-level
        // entries installed under /etc/xdg/autostart.
        "[Desktop Entry]\nType=Application\nName=AgentDictate background service\nHidden=true\n"
            .to_owned()
    };
    write_atomic(entry, &contents)
}

fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "startup file has no parent"))?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(AUTOSTART_ENTRY);
    let sequence = STARTUP_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.tmp.{}.{sequence}",
        std::process::id()
    ));
    let result = fs::write(&temporary, contents).and_then(|()| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn service_is_active(systemctl: &Path) -> io::Result<bool> {
    match systemctl_value(systemctl, "ActiveState")?.as_str() {
        "active" | "activating" | "reloading" => Ok(true),
        "inactive" | "failed" | "deactivating" => Ok(false),
        state => Err(io::Error::other(format!(
            "agentdictated.service has unrecognized active state {state:?}"
        ))),
    }
}

enum DaemonEndpoint {
    Expected(IpcClient),
    Foreign(IpcClient),
    WrongRoute(IpcClient),
}

fn inspect_daemon(
    runtime_directory: &Path,
    manager: &impl UserServiceManager,
    expected_route_identity: &str,
) -> anyhow::Result<Option<DaemonEndpoint>> {
    let (client, _) = match IpcClient::connect(runtime_directory) {
        Ok(connection) => connection,
        Err(IpcError::Io(error))
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None);
        }
        Err(error) => {
            return Err(anyhow::anyhow!(
                "could not identify the daemon already holding AgentDictate IPC: {error}"
            ));
        }
    };
    let peer_pid = client.peer_pid()?;
    if !manager.owns_process(peer_pid)? {
        return Ok(Some(DaemonEndpoint::Foreign(client)));
    }
    let actual_route = manager.process_route_identity(peer_pid)?;
    if actual_route.as_deref() == Some(expected_route_identity) {
        Ok(Some(DaemonEndpoint::Expected(client)))
    } else {
        Ok(Some(DaemonEndpoint::WrongRoute(client)))
    }
}

fn stop_daemon(
    runtime_directory: &Path,
    mut client: IpcClient,
    deadline: Instant,
) -> anyhow::Result<()> {
    let socket = runtime_directory.join("agentdictate.sock");
    let socket_metadata = fs::symlink_metadata(&socket)?;
    let socket_identity = (socket_metadata.dev(), socket_metadata.ino());
    let response = client.send(ClientCommand::quit(1))?;
    if let ServerMessageKind::CommandRejected { error, .. } = response.kind {
        anyhow::bail!("AgentDictate daemon refused shutdown: {error}")
    }
    drop(client);
    loop {
        match fs::symlink_metadata(&socket) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
            Ok(metadata) if (metadata.dev(), metadata.ino()) != socket_identity => return Ok(()),
            Ok(_) if Instant::now() >= deadline => {
                anyhow::bail!("AgentDictate daemon did not release its IPC socket")
            }
            Ok(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn wait_for_daemon(
    runtime_directory: &Path,
    manager: &impl UserServiceManager,
    expected_route_identity: &str,
    deadline: Instant,
) -> anyhow::Result<()> {
    loop {
        match inspect_daemon(runtime_directory, manager, expected_route_identity)? {
            Some(DaemonEndpoint::Expected(_)) if manager.is_active()? => return Ok(()),
            Some(DaemonEndpoint::Expected(_)) if Instant::now() >= deadline => {
                anyhow::bail!("AgentDictate IPC is ready but agentdictated.service is not active")
            }
            Some(DaemonEndpoint::Expected(_)) => thread::sleep(Duration::from_millis(10)),
            Some(DaemonEndpoint::Foreign(_)) => {
                anyhow::bail!("a process outside agentdictated.service owns AgentDictate IPC")
            }
            Some(DaemonEndpoint::WrongRoute(_)) => {
                anyhow::bail!("agentdictated.service started the wrong AgentDictate artifact")
            }
            None if Instant::now() >= deadline => {
                anyhow::bail!("AgentDictate service did not become ready")
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn wait_for_endpoint(
    runtime_directory: &Path,
    manager: &impl UserServiceManager,
    expected_route_identity: &str,
    deadline: Instant,
) -> anyhow::Result<DaemonEndpoint> {
    loop {
        if let Some(endpoint) = inspect_daemon(runtime_directory, manager, expected_route_identity)?
        {
            return Ok(endpoint);
        }
        if Instant::now() >= deadline {
            anyhow::bail!("active agentdictated.service did not open AgentDictate IPC")
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn service_route_identity(command: &StartupCommand) -> String {
    let mut digest = Sha256::new();
    digest.update(b"agentdictate-service-route-v1\0");
    hash_bytes(&mut digest, command.executable.as_os_str().as_bytes());
    for argument in &command.arguments {
        hash_bytes(&mut digest, argument.as_bytes());
    }
    for identity_file in &command.identity_files {
        hash_bytes(&mut digest, identity_file.as_os_str().as_bytes());
        match fs::metadata(identity_file) {
            Ok(metadata) => {
                digest.update(b"present\0");
                for value in [
                    metadata.dev(),
                    metadata.ino(),
                    metadata.size(),
                    metadata.mtime() as u64,
                    metadata.mtime_nsec() as u64,
                    metadata.ctime() as u64,
                    metadata.ctime_nsec() as u64,
                ] {
                    digest.update(value.to_le_bytes());
                }
            }
            Err(error) => {
                digest.update(b"missing\0");
                digest.update(error.raw_os_error().unwrap_or_default().to_le_bytes());
            }
        }
    }
    format!("{:x}", digest.finalize())
}

fn hash_bytes(digest: &mut Sha256, value: &[u8]) {
    digest.update(value.len().to_le_bytes());
    digest.update(value);
}

fn systemctl_value(systemctl: &Path, property: &str) -> io::Result<String> {
    let output = Command::new(systemctl)
        .args([
            "--user",
            "show",
            DAEMON_SERVICE_NAME,
            "--property",
            property,
            "--value",
        ])
        .output()?;
    if !output.status.success() {
        return Err(systemctl_error(systemctl, &output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn cgroup_contains(service_group: &str, process_group: &str) -> bool {
    process_group == service_group
        || process_group
            .strip_prefix(service_group)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn process_environment_value(pid: u32, name: &str) -> io::Result<Option<String>> {
    let environment = fs::read(format!("/proc/{pid}/environ"))?;
    let prefix = format!("{name}=");
    Ok(environment
        .split(|byte| *byte == 0)
        .find_map(|entry| entry.strip_prefix(prefix.as_bytes()))
        .map(|value| String::from_utf8_lossy(value).into_owned()))
}

fn run_systemctl(systemctl: &Path, arguments: &[&str]) -> io::Result<()> {
    let output = Command::new(systemctl).args(arguments).output()?;
    if output.status.success() {
        return Ok(());
    }
    Err(systemctl_error(systemctl, &output))
}

fn systemctl_error(systemctl: &Path, output: &std::process::Output) -> io::Error {
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.trim();
    let suffix = if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    };
    io::Error::other(format!(
        "{} exited with {}{suffix}",
        systemctl.display(),
        output.status
    ))
}

fn quote_desktop_exec_value(value: &str) -> String {
    quote_exec_value(value, false)
}

fn quote_systemd_exec_value(value: &str) -> String {
    quote_exec_value(value, true)
}

fn quote_exec_value(value: &str, systemd: bool) -> String {
    if value.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '/' | '.' | '_' | '-')
    }) {
        return value.to_owned();
    }
    let mut escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    if systemd {
        escaped = escaped.replace('$', "$$").replace('%', "%%");
    } else {
        escaped = escaped
            .replace('`', "\\`")
            .replace('$', "\\$")
            .replace('%', "%%");
    }
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
        AppSnapshot, ClientCommandKind, HotkeyReadiness, ServerMessage, Settings, Workflow,
    };
    use agentdictate_runtime::{IpcHandler, IpcServer};
    use tempfile::tempdir;

    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ServiceAction {
        Reload,
        CheckActive,
        Start,
        Restart,
        CheckOwnership,
        ReadRoute,
    }

    struct MockServiceState {
        active: bool,
        owns_process: bool,
        route_identity: Option<String>,
    }

    struct MockServiceManager {
        state: Mutex<MockServiceState>,
        actions: Mutex<Vec<ServiceAction>>,
        start: Option<mpsc::Sender<()>>,
        expected_route_identity: String,
    }

    impl MockServiceManager {
        fn new(
            active: bool,
            start: Option<mpsc::Sender<()>>,
            expected_route_identity: &str,
        ) -> Self {
            Self {
                state: Mutex::new(MockServiceState {
                    active,
                    owns_process: active,
                    route_identity: active.then(|| expected_route_identity.to_owned()),
                }),
                actions: Mutex::new(Vec::new()),
                start,
                expected_route_identity: expected_route_identity.to_owned(),
            }
        }

        fn with_active_route(actual: &str, expected: &str, start: mpsc::Sender<()>) -> Self {
            Self {
                state: Mutex::new(MockServiceState {
                    active: true,
                    owns_process: true,
                    route_identity: Some(actual.to_owned()),
                }),
                actions: Mutex::new(Vec::new()),
                start: Some(start),
                expected_route_identity: expected.to_owned(),
            }
        }

        fn actions(&self) -> Vec<ServiceAction> {
            self.actions.lock().unwrap().clone()
        }
    }

    impl UserServiceManager for MockServiceManager {
        fn daemon_reload(&self) -> io::Result<()> {
            self.actions.lock().unwrap().push(ServiceAction::Reload);
            Ok(())
        }

        fn is_active(&self) -> io::Result<bool> {
            self.actions
                .lock()
                .unwrap()
                .push(ServiceAction::CheckActive);
            Ok(self.state.lock().unwrap().active)
        }

        fn start(&self) -> io::Result<()> {
            self.actions.lock().unwrap().push(ServiceAction::Start);
            self.mark_launched();
            if let Some(start) = &self.start {
                start.send(()).unwrap();
            }
            Ok(())
        }

        fn restart(&self) -> io::Result<()> {
            self.actions.lock().unwrap().push(ServiceAction::Restart);
            self.mark_launched();
            if let Some(start) = &self.start {
                start.send(()).unwrap();
            }
            Ok(())
        }

        fn owns_process(&self, _pid: u32) -> io::Result<bool> {
            self.actions
                .lock()
                .unwrap()
                .push(ServiceAction::CheckOwnership);
            Ok(self.state.lock().unwrap().owns_process)
        }

        fn process_route_identity(&self, _pid: u32) -> io::Result<Option<String>> {
            self.actions.lock().unwrap().push(ServiceAction::ReadRoute);
            Ok(self.state.lock().unwrap().route_identity.clone())
        }
    }

    impl MockServiceManager {
        fn mark_launched(&self) {
            let mut state = self.state.lock().unwrap();
            state.active = true;
            state.owns_process = true;
            state.route_identity = Some(self.expected_route_identity.clone());
        }
    }

    #[test]
    fn startup_files_are_reloaded_after_both_routes_are_written() {
        let directory = tempdir().unwrap();
        let manager = MockServiceManager::new(false, None, "test-route");

        sync_startup_commands_with_manager(
            &directory.path().join("autostart/agentdictate.desktop"),
            &directory.path().join("systemd/agentdictated.service"),
            true,
            &StartupCommand {
                executable: PathBuf::from("/usr/bin/agentdictated"),
                arguments: vec![SERVICE_ARGUMENT.to_owned()],
                identity_files: vec![PathBuf::from("/usr/bin/agentdictated")],
            },
            &StartupCommand {
                executable: PathBuf::from("/usr/bin/agentdictated"),
                arguments: vec![START_SERVICE_ARGUMENT.to_owned()],
                identity_files: Vec::new(),
            },
            &manager,
        )
        .unwrap();

        assert_eq!(manager.actions(), [ServiceAction::Reload]);
    }

    #[test]
    fn concurrent_startup_writes_never_share_a_temporary_file() {
        let directory = tempdir().unwrap();
        let target = directory.path().join("agentdictated.service");
        let writers = (0..16)
            .map(|index| {
                let target = target.clone();
                thread::spawn(move || {
                    let contents = format!("writer-{index}\n").repeat(128);
                    write_atomic(&target, &contents).unwrap();
                    contents
                })
            })
            .collect::<Vec<_>>();
        let complete_outputs = writers
            .into_iter()
            .map(|writer| writer.join().unwrap())
            .collect::<Vec<_>>();

        let result = fs::read_to_string(&target).unwrap();
        assert!(complete_outputs.contains(&result));
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn bootstrap_stops_a_legacy_daemon_before_starting_the_named_service() {
        let directory = tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        let legacy_server = IpcServer::bind(&runtime).unwrap();
        let (quit_sent, quit_received) = mpsc::channel();
        let legacy = thread::spawn(move || {
            legacy_server
                .serve_next(&mut SnapshotHandler {
                    quit: Some(quit_sent),
                    reject_quit: false,
                })
                .unwrap();
        });
        let (start_sent, start_received) = mpsc::channel();
        let manager = MockServiceManager::new(false, Some(start_sent), "test-route");
        let replacement_runtime = runtime.clone();
        let replacement = thread::spawn(move || {
            start_received.recv_timeout(Duration::from_secs(1)).unwrap();
            let server = IpcServer::bind(replacement_runtime).unwrap();
            server
                .serve_next(&mut SnapshotHandler {
                    quit: None,
                    reject_quit: false,
                })
                .unwrap();
        });

        bootstrap_daemon_service_with_manager(
            &runtime,
            &manager,
            "test-route",
            Duration::from_secs(1),
        )
        .unwrap();

        quit_received.recv_timeout(Duration::from_secs(1)).unwrap();
        legacy.join().unwrap();
        replacement.join().unwrap();
        assert_eq!(
            manager.actions(),
            [
                ServiceAction::CheckActive,
                ServiceAction::CheckOwnership,
                ServiceAction::Start,
                ServiceAction::CheckOwnership,
                ServiceAction::ReadRoute,
                ServiceAction::CheckActive,
            ]
        );
    }

    #[test]
    fn bootstrap_never_competes_when_the_legacy_daemon_refuses_shutdown() {
        let directory = tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        let legacy_server = IpcServer::bind(&runtime).unwrap();
        let legacy = thread::spawn(move || {
            legacy_server
                .serve_next(&mut SnapshotHandler {
                    quit: None,
                    reject_quit: true,
                })
                .unwrap();
        });
        let manager = MockServiceManager::new(false, None, "test-route");

        let error = bootstrap_daemon_service_with_manager(
            &runtime,
            &manager,
            "test-route",
            Duration::from_secs(1),
        )
        .unwrap_err();

        legacy.join().unwrap();
        assert!(error.to_string().contains("refused shutdown"));
        assert_eq!(
            manager.actions(),
            [ServiceAction::CheckActive, ServiceAction::CheckOwnership]
        );
    }

    #[test]
    fn repeated_bootstrap_reuses_an_active_named_service() {
        let directory = tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        let server = IpcServer::bind(&runtime).unwrap();
        let service = thread::spawn(move || {
            server
                .serve_next(&mut SnapshotHandler {
                    quit: None,
                    reject_quit: false,
                })
                .unwrap();
        });
        let manager = MockServiceManager::new(true, None, "test-route");

        bootstrap_daemon_service_with_manager(
            &runtime,
            &manager,
            "test-route",
            Duration::from_secs(1),
        )
        .unwrap();

        service.join().unwrap();
        assert_eq!(
            manager.actions(),
            [
                ServiceAction::CheckActive,
                ServiceAction::CheckOwnership,
                ServiceAction::ReadRoute,
                ServiceAction::CheckActive,
            ]
        );
    }

    #[test]
    fn active_service_hands_off_when_the_requested_artifact_changes() {
        let directory = tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        let old_server = IpcServer::bind(&runtime).unwrap();
        let old_service = thread::spawn(move || {
            old_server
                .serve_next(&mut SnapshotHandler {
                    quit: None,
                    reject_quit: false,
                })
                .unwrap();
        });
        let (restart_sent, restart_received) = mpsc::channel();
        let manager = MockServiceManager::with_active_route("old-route", "new-route", restart_sent);
        let replacement_runtime = runtime.clone();
        let replacement = thread::spawn(move || {
            restart_received
                .recv_timeout(Duration::from_secs(1))
                .unwrap();
            let server = IpcServer::bind(replacement_runtime).unwrap();
            server
                .serve_next(&mut SnapshotHandler {
                    quit: None,
                    reject_quit: false,
                })
                .unwrap();
        });

        bootstrap_daemon_service_with_manager(
            &runtime,
            &manager,
            "new-route",
            Duration::from_secs(1),
        )
        .unwrap();

        old_service.join().unwrap();
        replacement.join().unwrap();
        assert!(manager.actions().contains(&ServiceAction::Restart));
    }

    #[test]
    fn concurrent_bootstrap_waits_for_an_active_service_to_bind() {
        let directory = tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        let service_runtime = runtime.clone();
        let service = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            let server = IpcServer::bind(service_runtime).unwrap();
            server
                .serve_next(&mut SnapshotHandler {
                    quit: None,
                    reject_quit: false,
                })
                .unwrap();
        });
        let manager = MockServiceManager::new(true, None, "test-route");

        bootstrap_daemon_service_with_manager(
            &runtime,
            &manager,
            "test-route",
            Duration::from_secs(1),
        )
        .unwrap();

        service.join().unwrap();
        assert!(!manager.actions().contains(&ServiceAction::Restart));
        assert!(!manager.actions().contains(&ServiceAction::Start));
    }

    #[test]
    fn malformed_legacy_handshake_never_starts_a_competing_service() {
        let directory = tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        fs::create_dir_all(&runtime).unwrap();
        let listener = UnixListener::bind(runtime.join("agentdictate.sock")).unwrap();
        let legacy = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"not-json\n").unwrap();
        });
        let manager = MockServiceManager::new(false, None, "new-route");

        let error = bootstrap_daemon_service_with_manager(
            &runtime,
            &manager,
            "new-route",
            Duration::from_secs(1),
        )
        .unwrap_err();

        legacy.join().unwrap();
        assert!(
            error
                .to_string()
                .contains("already holding AgentDictate IPC")
        );
        assert_eq!(manager.actions(), [ServiceAction::CheckActive]);
    }

    #[test]
    fn route_identity_changes_when_the_executable_is_replaced() {
        let directory = tempdir().unwrap();
        let executable = directory.path().join("agentdictated");
        fs::write(&executable, b"first").unwrap();
        let command = StartupCommand {
            executable: executable.clone(),
            arguments: vec![SERVICE_ARGUMENT.to_owned()],
            identity_files: vec![executable.clone()],
        };
        let first = service_route_identity(&command);
        fs::write(&executable, b"a different artifact").unwrap();

        assert_ne!(first, service_route_identity(&command));
    }

    #[test]
    fn service_cgroup_accepts_children_but_not_prefix_collisions() {
        let service = "/user.slice/app.slice/agentdictated.service";
        assert!(cgroup_contains(service, service));
        assert!(cgroup_contains(service, &format!("{service}/worker")));
        assert!(!cgroup_contains(
            service,
            "/user.slice/app.slice/agentdictated.service-old"
        ));
    }

    #[test]
    fn desktop_and_systemd_commands_escape_their_own_expansion_syntax() {
        assert_eq!(quote_desktop_exec_value("cash$ 50%"), "\"cash\\$ 50%%\"");
        assert_eq!(quote_systemd_exec_value("cash$ 50%"), "\"cash$$ 50%%\"");
    }

    struct SnapshotHandler {
        quit: Option<mpsc::Sender<()>>,
        reject_quit: bool,
    }

    impl IpcHandler for SnapshotHandler {
        fn snapshot(&self, request_id: u64) -> ServerMessage {
            ServerMessage::snapshot(
                request_id,
                AppSnapshot {
                    sequence: 0,
                    workflow: Workflow::new().snapshot(),
                    hotkey: HotkeyReadiness::Ready,
                    recoverable_count: 0,
                    last_transcript: None,
                },
                &Settings::default(),
            )
        }

        fn handle(&mut self, command: ClientCommand) -> ServerMessage {
            let ClientCommandKind::Quit { request_id } = command.kind else {
                panic!("bootstrap sent an unexpected command")
            };
            if self.reject_quit {
                return ServerMessage::command_rejected(request_id, "shutdown checkpoint failed");
            }
            if let Some(quit) = self.quit.take() {
                quit.send(()).unwrap();
            }
            self.snapshot(request_id)
        }
    }
}
