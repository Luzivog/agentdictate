//! The daemon's in-process entry point, shared by every thread.
//!
//! Lock rules:
//! 1. Work done while holding the process lock is bounded; network I/O never
//!    runs under it. Transcription runs from a `ProcessingTicket` after the
//!    lock is released.
//! 2. The hotkey dispatch loop and the recorder owner thread never wait for
//!    the lock: saving settings holds it while it waits for the dispatch loop,
//!    and stopping a recording holds it while it waits for the recorder
//!    owner. They read `DaemonStatus` and send events through channels.
//! 3. Processing threads take the lock only to complete their job. Nothing
//!    that holds the lock waits for a processing thread, except `quit`, which
//!    waits on a condition variable and so releases the lock meanwhile.
//! 4. A poisoned lock ends the process with `EXIT_LOCK_POISONED`, so systemd
//!    restarts the daemon. The recorder's parent-death signal stops any
//!    recording, and startup reconciliation keeps its audio.

use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use agentdictate_core::{
    ClientCommand, ClientCommandKind, DictationMode, JobId, ProcessingStage, RecordingMode,
    ServerMessage, WorkflowPhase,
};
use agentdictate_linux::hotkey::HotkeySignal;
use agentdictate_runtime::{IpcClient, IpcHandler, RecordingJob};

use crate::daemon::copied;
use crate::process::{Followup, HOTKEY_CAPTURE_TIMEOUT, Reply};
use crate::{
    AgentProcess, DaemonDeliverer, DaemonError, DaemonStatus, ProcessingTicket,
    ProductionTranscriber, RecorderEvent, RecordingController, SystemDeliverer,
    SystemRecordingController, Transcriber, TranscriptionCompletion,
};

/// The exit status after the process lock was poisoned by a panic.
const EXIT_LOCK_POISONED: i32 = 70;
/// How long `quit` waits for a transcription in progress to be delivered.
/// A job still transcribing after that is recovered at the next start.
const SHUTDOWN_PROCESSING_GRACE: Duration = Duration::from_secs(3);

/// User intent from inside the daemon process: the hotkey and the tray.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Trigger {
    Hotkey(HotkeySignal),
    TrayToggle,
    TrayStartLiteral,
    TrayCancel,
}

/// What a trigger asks the daemon to do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleAction {
    /// Start a recording, optionally in another dictation mode.
    Start(Option<DictationMode>),
    Stop,
    /// Discard the recording, as Esc does.
    Discard,
    /// Stop waiting for the transcription; its result goes to Recovery.
    CancelProcessing,
}

/// What a trigger did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TriggerOutcome {
    /// `toggle` is set in toggle mode, where the press that started the
    /// recording must not stop it again at once.
    Started {
        job_id: JobId,
        toggle: bool,
    },
    Stopped {
        job_id: JobId,
    },
    Discarded {
        job_id: JobId,
    },
    ProcessingCancelled {
        job_id: JobId,
    },
    /// Nothing to do in this phase.
    Ignored {
        phase: WorkflowPhase,
    },
    Failed(String),
}

/// What `trigger` does in each workflow phase. A dictation waiting in
/// Recovery never blocks a new one. Presses while a dictation stops or
/// transcribes do nothing and are never replayed later, and Esc only
/// cancels a recording: a transcription is cancelled only from the tray or
/// `agentdictate cancel`.
#[must_use]
pub const fn lifecycle_action(
    trigger: Trigger,
    mode: RecordingMode,
    phase: WorkflowPhase,
) -> Option<LifecycleAction> {
    let idle = matches!(
        phase,
        WorkflowPhase::Ready | WorkflowPhase::NeedsAttention { .. }
    );
    let recording = matches!(
        phase,
        WorkflowPhase::Starting { .. } | WorkflowPhase::Recording { .. }
    );
    let transcribing = matches!(
        phase,
        WorkflowPhase::Processing {
            stage: ProcessingStage::Transcribing,
            ..
        }
    );
    match (trigger, mode) {
        (Trigger::Hotkey(HotkeySignal::Pressed), _) | (Trigger::TrayToggle, _) if idle => {
            Some(LifecycleAction::Start(None))
        }
        (Trigger::TrayStartLiteral, _) if idle => {
            Some(LifecycleAction::Start(Some(DictationMode::Literal)))
        }
        (Trigger::Hotkey(HotkeySignal::Pressed), RecordingMode::Toggle)
        | (Trigger::Hotkey(HotkeySignal::Released), RecordingMode::Hold)
        | (Trigger::TrayToggle, _)
            if recording =>
        {
            Some(LifecycleAction::Stop)
        }
        (Trigger::Hotkey(HotkeySignal::Cancelled) | Trigger::TrayCancel, _) if recording => {
            Some(LifecycleAction::Discard)
        }
        (Trigger::TrayCancel, _) if transcribing => Some(LifecycleAction::CancelProcessing),
        _ => None,
    }
}

/// Shares the daemon between threads. Clone it freely; every method holds
/// the process lock only for bounded work.
pub struct DaemonHandle<
    R = SystemRecordingController,
    T = ProductionTranscriber,
    D = SystemDeliverer,
> {
    shared: Arc<Shared<R, T, D>>,
}

struct Shared<R, T, D> {
    process: Mutex<AgentProcess<R, T, D>>,
    status: Arc<DaemonStatus>,
    /// Notified after every completed transcription, for `quit`.
    processing_settled: Condvar,
    /// Set when shutdown begins; commands that would start work are refused.
    quitting: AtomicBool,
    /// Set once shutdown is done; the IPC accept loop then ends.
    should_quit: AtomicBool,
    runtime_directory: PathBuf,
}

impl<R, T, D> Clone for DaemonHandle<R, T, D> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<R, T, D> DaemonHandle<R, T, D>
where
    R: RecordingController + Send + 'static,
    T: Transcriber,
    D: DaemonDeliverer + Send + 'static,
{
    /// `runtime_directory` holds the IPC socket that `quit` wakes.
    #[must_use]
    pub fn new(process: AgentProcess<R, T, D>, runtime_directory: PathBuf) -> Self {
        Self {
            shared: Arc::new(Shared {
                status: process.daemon().status(),
                process: Mutex::new(process),
                processing_settled: Condvar::new(),
                quitting: AtomicBool::new(false),
                should_quit: AtomicBool::new(false),
                runtime_directory,
            }),
        }
    }

    /// State readable without the process lock.
    #[must_use]
    pub fn status(&self) -> Arc<DaemonStatus> {
        Arc::clone(&self.shared.status)
    }

    /// Acts on user intent. The phase is read and acted on under one lock,
    /// so a press can never act on a phase that changed meanwhile. A stopped
    /// recording is transcribed on its own thread.
    pub fn trigger(&self, trigger: Trigger) -> TriggerOutcome {
        if self.shared.quitting.load(Ordering::Acquire) {
            return TriggerOutcome::Failed(DaemonError::ShuttingDown.to_string());
        }
        let (outcome, ticket) = {
            let mut process = self.lock();
            let mode = process.recording_mode();
            let phase = process.daemon().phase();
            let Some(action) = lifecycle_action(trigger, mode, phase) else {
                return TriggerOutcome::Ignored { phase };
            };
            let daemon = process.daemon_mut();
            let acted = match action {
                LifecycleAction::Start(dictation_mode) => {
                    daemon.start_recording_in_mode(dictation_mode).map(|job| {
                        let toggle = mode == RecordingMode::Toggle;
                        (
                            TriggerOutcome::Started {
                                job_id: job.id,
                                toggle,
                            },
                            None,
                        )
                    })
                }
                LifecycleAction::Stop => daemon.stop_recording().map(|ticket| {
                    let job_id = ticket.job_id();
                    (TriggerOutcome::Stopped { job_id }, Some(ticket))
                }),
                LifecycleAction::Discard => daemon
                    .discard_recording()
                    .map(|job| (TriggerOutcome::Discarded { job_id: job.id }, None)),
                LifecycleAction::CancelProcessing => daemon
                    .cancel_processing()
                    .map(|job_id| (TriggerOutcome::ProcessingCancelled { job_id }, None)),
            };
            acted.unwrap_or_else(|error| (TriggerOutcome::Failed(error.to_string()), None))
        };
        if let Some(ticket) = ticket {
            self.spawn_processing(ticket);
        }
        outcome
    }

    /// "Try again" on a failure notification: transcribes the job again on
    /// its own thread; the result is copied and announced.
    pub fn try_again(&self, job_id: JobId) -> Result<(), DaemonError> {
        if self.shared.quitting.load(Ordering::Acquire) {
            return Err(DaemonError::ShuttingDown);
        }
        let ticket = self.lock().daemon_mut().try_again(job_id)?;
        self.spawn_processing(ticket);
        Ok(())
    }

    /// Runs `f` under the process lock, for composition and tests.
    pub fn with_process<O>(&self, f: impl FnOnce(&mut AgentProcess<R, T, D>) -> O) -> O {
        f(&mut self.lock())
    }

    #[must_use]
    pub fn should_quit(&self) -> bool {
        self.shared.should_quit.load(Ordering::Acquire)
    }

    /// Shuts down gracefully: an active recording is preserved for Recovery,
    /// and a transcription in progress gets `SHUTDOWN_PROCESSING_GRACE` to be
    /// delivered. Then the IPC accept loop is woken to end.
    pub fn quit(&self) -> Result<(), DaemonError> {
        self.shared.quitting.store(true, Ordering::Release);
        let mut process = self.lock();
        process.daemon_mut().shutdown()?;
        let (process, waited) = self
            .shared
            .processing_settled
            .wait_timeout_while(process, SHUTDOWN_PROCESSING_GRACE, |process| {
                process.daemon().is_processing()
            })
            .unwrap_or_else(|_| exit_poisoned());
        if waited.timed_out() {
            tracing::warn!(
                "quitting before the transcription in progress finished; it will be recovered at the next start"
            );
        }
        drop(process);
        self.shared.should_quit.store(true, Ordering::Release);
        // Fails harmlessly when nothing is listening.
        let _ = IpcClient::wake(&self.shared.runtime_directory);
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, AgentProcess<R, T, D>> {
        self.shared
            .process
            .lock()
            .unwrap_or_else(|_| exit_poisoned())
    }

    /// Applies the recorder's events as they arrive, on their own thread: the
    /// recorder owner must never wait for the process lock.
    pub fn forward_recorder_events(
        &self,
        events: Receiver<RecorderEvent>,
    ) -> std::io::Result<JoinHandle<()>> {
        let handle = self.clone();
        std::thread::Builder::new()
            .name("agentdictate-recorder-events".into())
            .spawn(move || {
                for event in events {
                    handle.recorder_event(event);
                }
            })
    }

    pub fn recorder_event(&self, event: RecorderEvent) {
        let handled = self.lock().daemon_mut().recorder_event(event);
        match handled {
            Ok(Some(ticket)) => self.spawn_processing(ticket),
            Ok(None) => {}
            Err(error) => tracing::error!(?event, %error, "could not act on the recorder event"),
        }
    }

    /// Transcribes on a new thread and delivers the result when it arrives.
    fn spawn_processing(&self, ticket: ProcessingTicket<T>) {
        let job_id = ticket.job_id();
        let handle = self.clone();
        let spawned = std::thread::Builder::new()
            .name("agentdictate-processing".into())
            .spawn(move || {
                let _ = handle.process(ticket);
            });
        if let Err(error) = spawned {
            // The ticket was dropped with the closure; the job must still
            // leave `transcribing`.
            tracing::error!(%job_id, %error, "could not start transcription");
            let _ = self.complete(TranscriptionCompletion::failed(
                job_id,
                format!("could not start transcription: {error}; audio is saved"),
            ));
        }
    }

    /// Transcribes on this thread, without the lock, then completes the job.
    /// A panic counts as a failed transcription, so the job never stays in
    /// flight.
    fn process(&self, ticket: ProcessingTicket<T>) -> Result<RecordingJob, DaemonError> {
        let job_id = ticket.job_id();
        let completion =
            panic::catch_unwind(AssertUnwindSafe(|| ticket.run())).unwrap_or_else(|_| {
                tracing::error!(%job_id, "transcription panicked");
                TranscriptionCompletion::failed(
                    job_id,
                    "transcription stopped unexpectedly; audio is saved",
                )
            });
        self.complete(completion)
    }

    fn complete(&self, completion: TranscriptionCompletion) -> Result<RecordingJob, DaemonError> {
        let job_id = completion.job_id;
        let result = self.lock().daemon_mut().complete_transcription(completion);
        self.shared.processing_settled.notify_all();
        if let Err(error) = &result {
            tracing::warn!(%job_id, %error, "dictation did not complete");
        }
        result
    }
}

impl<R, T, D> IpcHandler for DaemonHandle<R, T, D>
where
    R: RecordingController + Send + 'static,
    T: Transcriber,
    D: DaemonDeliverer + Send + 'static,
{
    fn snapshot(&self) -> ServerMessage {
        self.lock().render(Reply::Snapshot)
    }

    fn handle(&self, command: ClientCommand) -> ServerMessage {
        let starts_work = matches!(
            command.kind,
            ClientCommandKind::StartRecording { .. }
                | ClientCommandKind::RetryTranscription { .. }
                | ClientCommandKind::RetryDelivery { .. }
        );
        if starts_work && self.shared.quitting.load(Ordering::Acquire) {
            return ServerMessage::command_rejected(DaemonError::ShuttingDown.to_string());
        }
        let (reply, followup) = self.lock().handle_locked(command.kind);
        let reply = match followup {
            Followup::None => reply,
            Followup::Process(ticket) => {
                self.spawn_processing(ticket);
                reply
            }
            Followup::ProcessThenReply(ticket) => match self.process(ticket).and_then(copied) {
                Ok(_) => reply,
                Err(error) => Reply::Rejected(error.to_string()),
            },
            Followup::CaptureHotkey(control) => match control.capture(HOTKEY_CAPTURE_TIMEOUT) {
                Ok(outcome) => Reply::HotkeyCaptured(outcome),
                Err(error) => Reply::Rejected(error.to_string()),
            },
            Followup::Quit => match self.quit() {
                Ok(()) => reply,
                Err(error) => Reply::Rejected(error.to_string()),
            },
        };
        self.lock().render(reply)
    }
}

fn exit_poisoned() -> ! {
    tracing::error!("the daemon lock was poisoned by a panic; exiting so the service restarts");
    std::process::exit(EXIT_LOCK_POISONED)
}
