use std::{
    io,
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, channel, sync_channel},
    thread::JoinHandle,
    time::Duration,
};

use agentdictate_runtime::{DeliveryGate, DeliveryGateError};
use agentdictate_ui::{LogicalRect, OverlayState};
use thiserror::Error;

use super::{
    child::OverlayChild,
    protocol::{OverlayHelperStatus, OverlayUpdate},
};

const AUTOMATIC_RESTART_LIMIT_PER_UPDATE: u8 = 1;
const OVERLAY_READY_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound the daemon waits for the helper to exit after dismissal. The
/// dismissal ack now includes the helper's fade-out, so this must comfortably
/// exceed the UI crate's `OVERLAY_FADE_HOLD` plus process teardown; the
/// relation is asserted by the overlay lifecycle tests.
#[doc(hidden)]
pub const OVERLAY_TEARDOWN_TIMEOUT: Duration = Duration::from_secs(2);

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

pub(super) enum OverlayCommand {
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

pub(super) enum PresenterEvent {
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
