use std::{
    io::{self, Read, Write},
    os::fd::AsRawFd,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, channel, sync_channel},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use agentdictate_core::WorkflowSnapshot;
use agentdictate_runtime::{DeliveryGate, DeliveryGateError};
use agentdictate_ui::{
    ActiveRecordingPresentation, LogicalRect, OverlayPresentation, OverlayState,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(feature = "desktop")]
use agentdictate_ui::run_recording_overlay_with_ready;
#[cfg(feature = "desktop")]
use std::io::{BufRead, BufReader};
#[cfg(feature = "desktop")]
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

const OVERLAY_HELPER_ARGUMENT: &str = "--overlay-helper";
const OVERLAY_WORK_AREA: &str = "AGENTDICTATE_OVERLAY_WORK_AREA";
const AUTOMATIC_RESTART_LIMIT_PER_UPDATE: u8 = 1;
const OVERLAY_READY_TIMEOUT: Duration = Duration::from_secs(5);
const OVERLAY_TEARDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_OVERLAY_STATUS_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum OverlayHelperStatus {
    Ready,
    Error { message: String },
}

/// Serializable recording metadata for the private daemon-to-overlay pipe.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActiveRecordingUpdate {
    pub audio_path: PathBuf,
    pub started_at_unix_millis: i64,
}

/// Event-driven status update consumed by the short-lived overlay helper.
///
/// This is intentionally separate from the public IPC `AppSnapshot`: only the
/// helper receives the temporary audio path that it samples on its own ticks.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OverlayUpdate {
    pub workflow: WorkflowSnapshot,
    pub active_recording: Option<ActiveRecordingUpdate>,
}

impl OverlayUpdate {
    pub fn presentation(&self) -> OverlayPresentation {
        OverlayPresentation {
            workflow: self.workflow,
            active_recording: self.active_recording.as_ref().map(|recording| {
                ActiveRecordingPresentation {
                    audio_path: recording.audio_path.clone(),
                    started_at_unix_millis: recording.started_at_unix_millis,
                }
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayProcessAction {
    StayHeadless,
    Launch,
    Update,
    Stop,
}

#[derive(Debug, Error)]
pub enum OverlayTeardownError {
    #[error("recording overlay presenter is unavailable")]
    PresenterUnavailable,
    #[error("recording overlay helper did not exit before the teardown deadline")]
    TimedOut,
    #[error("recording overlay helper exit could not be confirmed: {0}")]
    ExitObservation(#[source] io::Error),
}

#[derive(Clone)]
pub struct OverlayController {
    commands: Sender<OverlayCommand>,
}

impl OverlayController {
    pub fn update(&self, update: OverlayUpdate) {
        let _ = self.commands.send(OverlayCommand::Update(update));
    }

    pub fn dismiss_and_wait(&self) -> Result<(), OverlayTeardownError> {
        let (reply, acknowledgment) = sync_channel(1);
        self.commands
            .send(OverlayCommand::Dismiss { reply })
            .map_err(|_| OverlayTeardownError::PresenterUnavailable)?;
        match acknowledgment.recv_timeout(OVERLAY_TEARDOWN_TIMEOUT) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                let _ = self.commands.send(OverlayCommand::ForceDismiss);
                Err(OverlayTeardownError::TimedOut)
            }
            Err(RecvTimeoutError::Disconnected) => Err(OverlayTeardownError::PresenterUnavailable),
        }
    }
}

impl DeliveryGate for OverlayController {
    fn confirm_ready(&mut self) -> Result<(), DeliveryGateError> {
        self.dismiss_and_wait()
            .map_err(|error| DeliveryGateError::new(error.to_string()))
    }
}

enum OverlayCommand {
    Update(OverlayUpdate),
    Dismiss {
        reply: SyncSender<Result<(), OverlayTeardownError>>,
    },
    ForceDismiss,
}

/// Tracks whether the transient notification helper exists. The daemon itself
/// never creates a GPUI application or window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OverlayProcessState {
    running: bool,
}

impl OverlayProcessState {
    pub fn transition(&mut self, update: &OverlayUpdate) -> OverlayProcessAction {
        let visible = OverlayState::from(update.workflow).is_visible();
        match (self.running, visible) {
            (false, false) => OverlayProcessAction::StayHeadless,
            (false, true) => {
                self.running = true;
                OverlayProcessAction::Launch
            }
            (true, true) => OverlayProcessAction::Update,
            (true, false) => {
                self.running = false;
                OverlayProcessAction::Stop
            }
        }
    }

    pub fn mark_stopped(&mut self) {
        self.running = false;
    }
}

pub fn start_overlay_presenter(
    executable: PathBuf,
    work_area: Option<LogicalRect>,
) -> io::Result<(OverlayController, JoinHandle<()>)> {
    start_overlay_presenter_with_timeout(executable, work_area, OVERLAY_READY_TIMEOUT)
}

#[doc(hidden)]
pub fn start_overlay_presenter_with_timeout(
    executable: PathBuf,
    work_area: Option<LogicalRect>,
    ready_timeout: Duration,
) -> io::Result<(OverlayController, JoinHandle<()>)> {
    let (commands, receiver) = channel();
    let controller = OverlayController { commands };
    let presenter = std::thread::Builder::new()
        .name("agentdictate-overlay-presenter".into())
        .spawn(move || {
            overlay_presenter_loop(&executable, receiver, work_area, ready_timeout);
        })?;
    Ok((controller, presenter))
}

pub fn is_overlay_helper_argument(argument: Option<&str>) -> bool {
    argument == Some(OVERLAY_HELPER_ARGUMENT)
}

pub fn overlay_work_area_from_environment() -> Option<LogicalRect> {
    std::env::var(OVERLAY_WORK_AREA)
        .ok()
        .as_deref()
        .and_then(parse_work_area)
}

#[cfg(feature = "desktop")]
pub fn run_overlay_helper() -> anyhow::Result<()> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("agentdictate-overlay-input".into())
        .spawn(move || {
            let input = std::io::stdin();
            for line in BufReader::new(input).lines() {
                let update = match line {
                    Ok(line) => match serde_json::from_str::<OverlayUpdate>(&line) {
                        Ok(update) => update,
                        Err(error) => {
                            tracing::error!(%error, "invalid overlay snapshot");
                            return;
                        }
                    },
                    Err(error) => {
                        tracing::error!(%error, "could not read overlay snapshot");
                        return;
                    }
                };
                if sender.send(update.presentation()).is_err() {
                    return;
                }
            }
        })?;
    let initial = receiver
        .recv()
        .map_err(|_| anyhow::anyhow!("overlay helper received no initial update"))?;
    let ready = Arc::new(AtomicBool::new(false));
    let ready_callback = Arc::clone(&ready);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_recording_overlay_with_ready(
            initial,
            receiver,
            overlay_work_area_from_environment(),
            move || {
                write_overlay_helper_status(&OverlayHelperStatus::Ready)
                    .expect("overlay helper readiness should be writable");
                ready_callback.store(true, Ordering::Release);
            },
        );
    }));
    if let Err(payload) = result {
        let message = panic_message(payload.as_ref());
        if !ready.load(Ordering::Acquire) {
            let _ = write_overlay_helper_status(&OverlayHelperStatus::Error {
                message: message.clone(),
            });
        }
        anyhow::bail!("recording overlay panicked: {message}")
    }
    Ok(())
}

#[cfg(feature = "desktop")]
fn write_overlay_helper_status(status: &OverlayHelperStatus) -> io::Result<()> {
    let output = std::io::stdout();
    let mut output = output.lock();
    serde_json::to_writer(&mut output, status).map_err(io::Error::other)?;
    output.write_all(b"\n")?;
    output.flush()
}

#[cfg(feature = "desktop")]
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&str>()
                .map(|message| (*message).to_owned())
        })
        .unwrap_or_else(|| "unknown panic".to_owned())
}

fn overlay_presenter_loop(
    executable: &Path,
    commands: Receiver<OverlayCommand>,
    work_area: Option<LogicalRect>,
    ready_timeout: Duration,
) {
    let (events, event_receiver) = channel();
    let update_events = events.clone();
    let Ok(update_forwarder) = std::thread::Builder::new()
        .name("agentdictate-overlay-updates".into())
        .spawn(move || {
            while let Ok(command) = commands.recv() {
                if update_events
                    .send(PresenterEvent::Command(command))
                    .is_err()
                {
                    return;
                }
            }
            let _ = update_events.send(PresenterEvent::UpdatesClosed);
        })
    else {
        tracing::warn!("recording overlay update forwarder could not start");
        return;
    };

    let mut supervisor =
        OverlaySupervisor::new(executable, work_area, ready_timeout, events.clone());
    while let Ok(event) = event_receiver.recv() {
        match event {
            PresenterEvent::Command(command) => supervisor.handle_command(command),
            PresenterEvent::HelperStatus { generation, status } => {
                supervisor.handle_helper_status(generation, status);
            }
            PresenterEvent::HelperExited { generation, result } => {
                supervisor.handle_helper_exit(generation, result);
            }
            PresenterEvent::UpdatesClosed => {
                supervisor.shutdown();
                break;
            }
        }
    }
    let _ = update_forwarder.join();
}

enum PresenterEvent {
    Command(OverlayCommand),
    HelperStatus {
        generation: u64,
        status: Result<OverlayHelperStatus, String>,
    },
    HelperExited {
        generation: u64,
        result: io::Result<()>,
    },
    UpdatesClosed,
}

struct OverlaySupervisor<'a> {
    executable: &'a Path,
    work_area: Option<LogicalRect>,
    ready_timeout: Duration,
    events: Sender<PresenterEvent>,
    lifecycle: OverlayProcessState,
    helper: Option<OverlayChild>,
    last_visible_update: Option<OverlayUpdate>,
    remaining_restarts: u8,
    next_generation: u64,
    pending_dismissal: Option<PendingDismissal>,
}

struct PendingDismissal {
    generation: u64,
    reply: SyncSender<Result<(), OverlayTeardownError>>,
}

impl<'a> OverlaySupervisor<'a> {
    fn new(
        executable: &'a Path,
        work_area: Option<LogicalRect>,
        ready_timeout: Duration,
        events: Sender<PresenterEvent>,
    ) -> Self {
        Self {
            executable,
            work_area,
            ready_timeout,
            events,
            lifecycle: OverlayProcessState::default(),
            helper: None,
            last_visible_update: None,
            remaining_restarts: 0,
            next_generation: 0,
            pending_dismissal: None,
        }
    }

    fn handle_command(&mut self, command: OverlayCommand) {
        match command {
            OverlayCommand::Update(update) => self.handle_update(update),
            OverlayCommand::Dismiss { reply } => self.dismiss_and_wait(reply),
            OverlayCommand::ForceDismiss => self.force_dismissal(),
        }
    }

    fn dismiss_and_wait(&mut self, reply: SyncSender<Result<(), OverlayTeardownError>>) {
        self.last_visible_update = None;
        self.remaining_restarts = 0;
        self.lifecycle.mark_stopped();
        let Some(child) = self.helper.as_mut() else {
            let _ = reply.send(Ok(()));
            return;
        };
        let generation = child.generation();
        child.finish();
        self.pending_dismissal = Some(PendingDismissal { generation, reply });
    }

    fn force_dismissal(&mut self) {
        let Some(pending) = self.pending_dismissal.as_ref() else {
            return;
        };
        if let Some(child) = self.helper.as_mut()
            && child.generation() == pending.generation
        {
            child.terminate();
        }
    }

    fn handle_update(&mut self, update: OverlayUpdate) {
        if OverlayState::from(update.workflow).is_visible() {
            self.last_visible_update = Some(update.clone());
            self.remaining_restarts = AUTOMATIC_RESTART_LIMIT_PER_UPDATE;
        } else {
            // Clear this before closing the helper so a normal hidden
            // transition is ineligible for restart when its exit arrives.
            self.last_visible_update = None;
            self.remaining_restarts = 0;
        }

        match self.lifecycle.transition(&update) {
            OverlayProcessAction::StayHeadless => {}
            OverlayProcessAction::Launch => {
                self.launch(&update, "recording overlay helper could not start");
            }
            OverlayProcessAction::Update => {
                let send_result = self
                    .helper
                    .as_mut()
                    .ok_or_else(|| io::Error::other("overlay helper is missing"))
                    .and_then(|child| child.send(&update));
                if let Err(error) = send_result {
                    tracing::warn!(%error, "recording overlay helper disconnected");
                    if let Some(mut child) = self.helper.take() {
                        child.terminate();
                    }
                    self.lifecycle.mark_stopped();
                    if self.lifecycle.transition(&update) == OverlayProcessAction::Launch {
                        self.launch(&update, "recording overlay helper could not recover");
                    }
                }
            }
            OverlayProcessAction::Stop => {
                if let Some(mut child) = self.helper.take() {
                    let _ = child.send(&update);
                    child.finish();
                }
            }
        }
    }

    fn handle_helper_status(
        &mut self,
        generation: u64,
        status: Result<OverlayHelperStatus, String>,
    ) {
        if self
            .pending_dismissal
            .as_ref()
            .is_some_and(|pending| pending.generation == generation)
        {
            return;
        }
        let Some(child) = self
            .helper
            .as_mut()
            .filter(|child| child.generation() == generation)
        else {
            return;
        };
        match status {
            Ok(OverlayHelperStatus::Ready) => {
                child.mark_ready();
                tracing::info!(generation, "recording overlay helper is ready");
            }
            Ok(OverlayHelperStatus::Error { message }) => {
                tracing::warn!(generation, %message, "recording overlay helper failed before readiness");
                child.terminate();
            }
            Err(error) => {
                tracing::warn!(generation, %error, "recording overlay helper failed before readiness");
                child.terminate();
            }
        }
    }

    fn handle_helper_exit(&mut self, generation: u64, result: io::Result<()>) {
        if self
            .pending_dismissal
            .as_ref()
            .is_some_and(|pending| pending.generation == generation)
        {
            let pending = self
                .pending_dismissal
                .take()
                .expect("matching pending dismissal must exist");
            if self.helper.as_ref().map(OverlayChild::generation) == Some(generation) {
                self.helper.take();
                self.lifecycle.mark_stopped();
            }
            let acknowledgment = result.map_err(OverlayTeardownError::ExitObservation);
            let _ = pending.reply.send(acknowledgment);
            return;
        }
        if self.helper.as_ref().map(OverlayChild::generation) != Some(generation) {
            return;
        }
        let was_ready = self.helper.take().is_some_and(|child| child.is_ready());
        self.lifecycle.mark_stopped();
        if let Err(error) = result {
            tracing::warn!(%error, "recording overlay helper exit could not be observed");
        } else if !was_ready {
            tracing::warn!("recording overlay helper exited before readiness");
        } else {
            tracing::warn!("recording overlay helper exited unexpectedly");
        }

        if let Some(update) = self.last_visible_update.clone() {
            if self.remaining_restarts == 0 {
                tracing::warn!(
                    "recording overlay helper restart budget exhausted; waiting for a new update"
                );
                return;
            }
            self.remaining_restarts -= 1;
            if self.lifecycle.transition(&update) == OverlayProcessAction::Launch {
                self.launch(&update, "recording overlay helper could not relaunch");
            }
        }
    }

    fn launch(&mut self, update: &OverlayUpdate, failure_message: &'static str) {
        self.next_generation = self.next_generation.wrapping_add(1);
        match OverlayChild::launch(
            self.executable,
            self.work_area,
            self.ready_timeout,
            self.next_generation,
            self.events.clone(),
        ) {
            Ok(mut child) => {
                if let Err(error) = child.send(update) {
                    // Keep the generation installed so its kernel exit event
                    // drives the same recovery path as a later disconnect.
                    tracing::warn!(%error, "recording overlay helper rejected its initial update");
                }
                self.helper = Some(child);
            }
            Err(error) => {
                self.lifecycle.mark_stopped();
                tracing::warn!(%error, "{failure_message}");
            }
        }
    }

    fn shutdown(&mut self) {
        self.last_visible_update = None;
        self.remaining_restarts = 0;
        self.lifecycle.mark_stopped();
        if let Some(mut child) = self.helper.take() {
            child.finish();
        }
        if let Some(pending) = self.pending_dismissal.take() {
            let _ = pending
                .reply
                .send(Err(OverlayTeardownError::PresenterUnavailable));
        }
    }
}

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

struct OverlayChild {
    child: Child,
    input: Option<ChildStdin>,
    generation: u64,
    ready: bool,
}

impl OverlayChild {
    fn launch(
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

    fn generation(&self) -> u64 {
        self.generation
    }

    fn mark_ready(&mut self) {
        self.ready = true;
    }

    fn is_ready(&self) -> bool {
        self.ready
    }

    fn send(&mut self, update: &OverlayUpdate) -> io::Result<()> {
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "overlay input is closed"))?;
        serde_json::to_writer(&mut *input, update).map_err(io::Error::other)?;
        input.write_all(b"\n")?;
        input.flush()
    }

    fn finish(&mut self) {
        drop(self.input.take());
    }

    fn terminate(&mut self) {
        drop(self.input.take());
        let _ = self.child.kill();
    }
}

fn format_work_area(work_area: LogicalRect) -> String {
    format!(
        "{},{},{},{}",
        work_area.x, work_area.y, work_area.width, work_area.height
    )
}

fn parse_work_area(value: &str) -> Option<LogicalRect> {
    let mut values = value.split(',');
    let x = values.next()?.parse().ok()?;
    let y = values.next()?.parse().ok()?;
    let width = values.next()?.parse().ok()?;
    let height = values.next()?.parse().ok()?;
    values
        .next()
        .is_none()
        .then(|| LogicalRect::new(x, y, width, height))
}
