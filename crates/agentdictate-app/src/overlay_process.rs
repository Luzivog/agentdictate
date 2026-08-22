use std::{
    io::{self, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, channel, sync_channel},
    thread::JoinHandle,
    time::Duration,
};

use agentdictate_core::WorkflowSnapshot;
use agentdictate_runtime::{DeliveryGate, DeliveryGateError};
use agentdictate_ui::{
    ActiveRecordingPresentation, LogicalRect, OverlayPresentation, OverlayState,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const OVERLAY_HELPER_ARGUMENT: &str = "--overlay-helper";
const OVERLAY_WORK_AREA: &str = "AGENTDICTATE_OVERLAY_WORK_AREA";
const AUTOMATIC_RESTART_LIMIT_PER_UPDATE: u8 = 1;
const OVERLAY_TEARDOWN_TIMEOUT: Duration = Duration::from_secs(2);

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
    let (commands, receiver) = channel();
    let controller = OverlayController { commands };
    let presenter = std::thread::Builder::new()
        .name("agentdictate-overlay-presenter".into())
        .spawn(move || overlay_presenter_loop(&executable, receiver, work_area))?;
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

fn overlay_presenter_loop(
    executable: &Path,
    commands: Receiver<OverlayCommand>,
    work_area: Option<LogicalRect>,
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

    let mut supervisor = OverlaySupervisor::new(executable, work_area, events.clone());
    while let Ok(event) = event_receiver.recv() {
        match event {
            PresenterEvent::Command(command) => supervisor.handle_command(command),
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
    HelperExited {
        generation: u64,
        result: io::Result<()>,
    },
    UpdatesClosed,
}

struct OverlaySupervisor<'a> {
    executable: &'a Path,
    work_area: Option<LogicalRect>,
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
        events: Sender<PresenterEvent>,
    ) -> Self {
        Self {
            executable,
            work_area,
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
        self.helper.take();
        self.lifecycle.mark_stopped();
        if let Err(error) = result {
            tracing::warn!(%error, "recording overlay helper exit could not be observed");
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

struct OverlayChild {
    child: Child,
    input: Option<ChildStdin>,
    generation: u64,
}

impl OverlayChild {
    fn launch(
        executable: &Path,
        work_area: Option<LogicalRect>,
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
            .stdout(Stdio::null())
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
        let process_id = child.id();
        if let Err(error) = std::thread::Builder::new()
            .name("agentdictate-overlay-exit".into())
            .spawn(move || {
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
        })
    }

    fn generation(&self) -> u64 {
        self.generation
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
