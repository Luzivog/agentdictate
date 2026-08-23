use std::io::{BufRead, BufReader};
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use agentdictate_app::{
    AgentProcess, AppPaths, HotkeyReconfigurer, OverlayUpdate, command_for_hotkey,
    detect_primary_work_area, init_file_logging, is_overlay_helper_argument,
    overlay_work_area_from_environment, settings_executable_for_current_process,
    start_overlay_presenter, start_system_tray,
};
use agentdictate_core::{
    AppSnapshot, ClientCommand, ClientCommandKind, HotkeyReadiness, JobId, ServerMessageKind,
    WorkflowPhase,
};
use agentdictate_linux::{
    hotkey::{HotkeyListenerStatus, HotkeySignal, HotkeySpec},
    native_hotkey::{
        NativeHotkeyControl, NativeHotkeyEvent, NativeHotkeyListener, NativeHotkeyReadiness,
        NativeHotkeyRetryWatcher, NativeHotkeySignal, NativeHotkeySignalTrigger,
    },
};
use agentdictate_runtime::{IpcClient, IpcServer};
use agentdictate_ui::run_recording_overlay;

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn main() -> anyhow::Result<()> {
    let overlay_helper = is_overlay_helper_argument(std::env::args().nth(1).as_deref());
    let paths = AppPaths::from_environment()?;
    let _log_guard = init_file_logging(&paths.logs, "agentdictated.log")?;
    if overlay_helper {
        tracing::info!("transient recording overlay starting");
        return run_overlay_helper();
    }
    tracing::info!("native daemon starting");
    let runtime = paths.runtime.clone();
    let server = IpcServer::bind(&paths.runtime)?;
    let mut process = AgentProcess::open(paths)?;
    let overlay_presenter = match std::env::current_exe()
        .map_err(anyhow::Error::from)
        .and_then(|executable| {
            start_overlay_presenter(executable, detect_primary_work_area())
                .map_err(anyhow::Error::from)
        }) {
        Ok((controller, thread)) => {
            process.set_overlay_controller(controller);
            Some(thread)
        }
        Err(error) => {
            tracing::warn!(%error, "recording overlay is unavailable; dictation will continue");
            None
        }
    };
    start_hotkey_listener(&mut process, &runtime)?;
    let _maintenance_thread = match process.start_post_listener_maintenance() {
        Ok(thread) => Some(thread),
        Err(error) => {
            tracing::warn!(%error, "could not start nonessential maintenance");
            None
        }
    };
    let _chatgpt_dictation_importer = match process.start_chatgpt_dictation_importer() {
        Ok(thread) => Some(thread),
        Err(error) => {
            tracing::warn!(%error, "could not start ChatGPT dictation usage importer");
            None
        }
    };
    let show_tray_icon = process.show_tray_icon();
    let process = Arc::new(Mutex::new(process));
    let ipc_thread = std::thread::Builder::new()
        .name("agentdictate-ipc".into())
        .spawn(move || {
            loop {
                match process.lock() {
                    Ok(process) if process.should_quit() => break,
                    Ok(_) => {}
                    Err(_) => {
                        tracing::error!("daemon process lock is poisoned");
                        break;
                    }
                }
                if let Err(error) = server.serve_next_concurrent(Arc::clone(&process)) {
                    tracing::warn!(%error, "could not accept IPC session");
                }
            }
        })?;
    if let Err(error) = start_signal_listener(runtime.clone()) {
        tracing::warn!(%error, "signal listener is unavailable; daemon will continue");
    }
    let _tray_handle = if show_tray_icon {
        match settings_executable_for_current_process() {
            Ok(executable) => match start_system_tray(runtime, executable) {
                Ok(handle) => Some(handle),
                Err(error) => {
                    tracing::warn!(%error, "native tray is unavailable; daemon will continue");
                    None
                }
            },
            Err(error) => {
                tracing::warn!(%error, "settings launcher is unavailable; tray will stay hidden");
                None
            }
        }
    } else {
        None
    };
    ipc_thread
        .join()
        .map_err(|_| anyhow::anyhow!("daemon IPC thread panicked"))?;
    if let Some(thread) = overlay_presenter {
        thread
            .join()
            .map_err(|_| anyhow::anyhow!("overlay presenter thread panicked"))?;
    }
    Ok(())
}

fn run_overlay_helper() -> anyhow::Result<()> {
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
    run_recording_overlay(initial, receiver, overlay_work_area_from_environment());
    Ok(())
}

fn start_signal_listener(runtime: std::path::PathBuf) -> anyhow::Result<()> {
    let mut signals = signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
    ])?;
    std::thread::Builder::new()
        .name("agentdictate-signals".into())
        .spawn(move || {
            if signals.forever().next().is_some()
                && let Err(error) = request_shutdown(&runtime)
            {
                tracing::error!(%error, "graceful shutdown request failed");
            }
        })?;
    Ok(())
}

fn request_shutdown(runtime: &Path) -> anyhow::Result<()> {
    let (mut client, _) = IpcClient::connect(runtime)?;
    let request_id = REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let response = client.send(agentdictate_core::ClientCommand::quit(request_id))?;
    if let ServerMessageKind::CommandRejected { error, .. } = response.kind {
        anyhow::bail!(error)
    }
    Ok(())
}

fn start_hotkey_listener(process: &mut AgentProcess, runtime: &Path) -> anyhow::Result<()> {
    let (events, incoming) = std::sync::mpsc::channel();
    let (status_updates, status_receiver) = std::sync::mpsc::channel();
    let status_runtime = runtime.to_owned();
    std::thread::Builder::new()
        .name("agentdictate-hotkey-status".into())
        .spawn(move || {
            while let Ok(readiness) = status_receiver.recv() {
                if let Err(error) = update_hotkey_status(&status_runtime, readiness) {
                    tracing::error!(%error, "could not publish hotkey status");
                }
            }
        })?;
    let recording_mode = Arc::new(RwLock::new(process.recording_mode().to_owned()));
    process.set_recording_mode_control(Arc::clone(&recording_mode));
    process.set_hotkey_reconfigurer(Arc::new(SupervisedHotkeyControl {
        events: events.clone(),
    }));

    let generation = 1_u64;
    let (current_spec, active_listener) = match HotkeySpec::from_str(process.hotkey()) {
        Ok(spec) => match NativeHotkeyListener::start(spec.clone()) {
            Ok(listener) => {
                log_initial_hotkey_readiness(generation, listener.readiness());
                process.set_hotkey_readiness(readiness_from_initial(listener.readiness()));
                let active = activate_listener(listener, generation, events.clone())?;
                (Some(spec), Some(active))
            }
            Err(error) => {
                process.set_hotkey_readiness(HotkeyReadiness::Unavailable {
                    message: error.to_string(),
                });
                schedule_environment_retry(generation, events.clone());
                (Some(spec), None)
            }
        },
        Err(error) => {
            process.set_hotkey_readiness(HotkeyReadiness::Unavailable {
                message: format!("Invalid hotkey: {error}"),
            });
            tracing::error!(%error, hotkey = process.hotkey(), "invalid configured hotkey");
            (None, None)
        }
    };
    let runtime = runtime.to_owned();
    std::thread::Builder::new()
        .name("agentdictate-hotkey-dispatch".into())
        .spawn(move || {
            hotkey_dispatch_loop(HotkeyDispatchLoop {
                runtime,
                recording_mode,
                events,
                incoming,
                current_spec,
                active_listener,
                generation,
                status_updates,
            });
        })?;
    Ok(())
}

struct SupervisedHotkeyControl {
    events: std::sync::mpsc::Sender<DispatchLoopEvent>,
}

impl HotkeyReconfigurer for SupervisedHotkeyControl {
    fn reconfigure(&self, spec: HotkeySpec) -> anyhow::Result<()> {
        let (response_sender, response) = std::sync::mpsc::sync_channel(1);
        self.events
            .send(DispatchLoopEvent::Reconfigure {
                spec,
                response: response_sender,
            })
            .map_err(|_| anyhow::anyhow!("hotkey supervisor has stopped"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("hotkey supervisor stopped before responding"))?
            .map_err(anyhow::Error::msg)
    }
}

struct ActiveHotkeyListener {
    control: NativeHotkeyControl,
}

fn activate_listener(
    listener: NativeHotkeyListener,
    generation: u64,
    events: std::sync::mpsc::Sender<DispatchLoopEvent>,
) -> anyhow::Result<ActiveHotkeyListener> {
    let control = listener.control_handle();
    std::thread::Builder::new()
        .name("agentdictate-hotkey-events".into())
        .spawn(move || {
            loop {
                match listener.recv() {
                    Ok(event) => {
                        if events
                            .send(DispatchLoopEvent::Native { generation, event })
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(_) => {
                        let _ = events.send(DispatchLoopEvent::ListenerClosed { generation });
                        return;
                    }
                }
            }
        })?;
    Ok(ActiveHotkeyListener { control })
}

fn readiness_from_initial(readiness: &NativeHotkeyReadiness) -> HotkeyReadiness {
    match readiness.status {
        HotkeyListenerStatus::Ready { .. } => HotkeyReadiness::Ready,
        HotkeyListenerStatus::Starting => HotkeyReadiness::Starting,
        HotkeyListenerStatus::Unavailable { .. } => {
            let message = readiness.failed_devices.first().map_or(
                "No readable keyboard input device was found".to_owned(),
                |failure| {
                    format!(
                        "Could not read {}: {}",
                        failure.path.display(),
                        failure.message
                    )
                },
            );
            HotkeyReadiness::Unavailable { message }
        }
    }
}

fn log_initial_hotkey_readiness(generation: u64, readiness: &NativeHotkeyReadiness) {
    tracing::info!(
        listener_generation = generation,
        status = ?readiness.status,
        discovered_devices = readiness.discovered_devices,
        failed_devices = readiness.failed_devices.len(),
        "hotkey listener initialized"
    );
}

fn schedule_environment_retry(generation: u64, events: std::sync::mpsc::Sender<DispatchLoopEvent>) {
    let watcher = match NativeHotkeyRetryWatcher::new() {
        Ok(watcher) => watcher,
        Err(error) => {
            tracing::error!(%error, "could not watch input environment for hotkey recovery");
            return;
        }
    };
    let result = std::thread::Builder::new()
        .name("agentdictate-hotkey-recovery".into())
        .spawn(move || match watcher.wait() {
            Ok(()) => {
                let _ = events.send(DispatchLoopEvent::EnvironmentChanged { generation });
            }
            Err(error) => tracing::error!(%error, "hotkey recovery watch failed"),
        });
    if let Err(error) = result {
        tracing::error!(%error, "could not start hotkey recovery watcher");
    }
}

struct HotkeyDispatchLoop {
    runtime: std::path::PathBuf,
    recording_mode: Arc<RwLock<String>>,
    events: std::sync::mpsc::Sender<DispatchLoopEvent>,
    incoming: std::sync::mpsc::Receiver<DispatchLoopEvent>,
    current_spec: Option<HotkeySpec>,
    active_listener: Option<ActiveHotkeyListener>,
    generation: u64,
    status_updates: std::sync::mpsc::Sender<HotkeyReadiness>,
}

fn hotkey_dispatch_loop(state: HotkeyDispatchLoop) {
    let HotkeyDispatchLoop {
        runtime,
        recording_mode,
        events,
        incoming,
        mut current_spec,
        mut active_listener,
        mut generation,
        status_updates,
    } = state;
    let runtime = runtime.as_path();
    let mut gate = HotkeyDispatchGate::default();
    while let Ok(event) = incoming.recv() {
        match event {
            DispatchLoopEvent::Native {
                generation: event_generation,
                event: NativeHotkeyEvent::Signal(event),
            } if event_generation == generation => {
                let mode = recording_mode
                    .read()
                    .map_or_else(|_| "toggle".to_owned(), |mode| mode.clone());
                match gate.accept(&mode, &event) {
                    Ok(()) => {
                        log_hotkey_decision(
                            event_generation,
                            &mode,
                            &event,
                            "dispatch",
                            "accepted",
                            None,
                        );
                        spawn_hotkey_action(runtime, &mode, event, events.clone());
                    }
                    Err(reason) => {
                        let disposition = if reason == HotkeyIgnoreReason::TerminalQueued {
                            "queued"
                        } else {
                            "ignored"
                        };
                        log_hotkey_decision(
                            event_generation,
                            &mode,
                            &event,
                            disposition,
                            reason.label(),
                            reason.guard_remaining(),
                        );
                    }
                }
            }
            DispatchLoopEvent::Native {
                generation: event_generation,
                event: NativeHotkeyEvent::Status(status),
            } if event_generation == generation => {
                tracing::info!(
                    listener_generation = event_generation,
                    ?status,
                    "hotkey listener status changed"
                );
                let readiness = match status {
                    HotkeyListenerStatus::Starting => HotkeyReadiness::Starting,
                    HotkeyListenerStatus::Ready { .. } => HotkeyReadiness::Ready,
                    HotkeyListenerStatus::Unavailable { .. } => HotkeyReadiness::Unavailable {
                        message: "No readable keyboard input device is currently available"
                            .to_owned(),
                    },
                };
                publish_hotkey_status(&status_updates, readiness);
            }
            DispatchLoopEvent::Native {
                generation: event_generation,
                event: NativeHotkeyEvent::DeviceError(error),
            } if event_generation == generation => {
                tracing::warn!(
                    path = %error.path.display(),
                    error = %error.message,
                    "lost keyboard device"
                );
            }
            DispatchLoopEvent::Native {
                generation: event_generation,
                event: NativeHotkeyEvent::DiscoveryError(error),
            } if event_generation == generation => {
                tracing::error!(%error, "keyboard discovery failed");
                publish_hotkey_status(
                    &status_updates,
                    HotkeyReadiness::Unavailable {
                        message: format!("Keyboard discovery failed: {error}"),
                    },
                );
            }
            DispatchLoopEvent::Native {
                generation: event_generation,
                event: NativeHotkeyEvent::Reconfigured { hotkey },
            } if event_generation == generation => {
                tracing::info!(%hotkey, "hotkey reconfigured");
            }
            DispatchLoopEvent::Native {
                generation: event_generation,
                event: NativeHotkeyEvent::ReconfigurationRejected { hotkey, reason },
            } if event_generation == generation => {
                tracing::error!(%hotkey, %reason, "hotkey reconfiguration rejected");
            }
            DispatchLoopEvent::Native {
                generation: event_generation,
                event: NativeHotkeyEvent::ControlError(error),
            } if event_generation == generation => {
                tracing::error!(%error, "hotkey listener control failed");
            }
            DispatchLoopEvent::ActionFinished(completion) => {
                if let Some(event) = gate.complete(completion) {
                    let mode = recording_mode
                        .read()
                        .map_or_else(|_| "toggle".to_owned(), |mode| mode.clone());
                    spawn_hotkey_action(runtime, &mode, event, events.clone());
                }
            }
            DispatchLoopEvent::ListenerClosed {
                generation: closed_generation,
            } if closed_generation == generation => {
                tracing::warn!(
                    listener_generation = closed_generation,
                    "hotkey listener closed"
                );
                active_listener = None;
                publish_hotkey_status(&status_updates, listener_closed_readiness());
                schedule_environment_retry(generation, events.clone());
            }
            DispatchLoopEvent::EnvironmentChanged {
                generation: retry_generation,
            } if retry_generation == generation && active_listener.is_none() => {
                let Some(spec) = current_spec.clone() else {
                    continue;
                };
                publish_hotkey_status(&status_updates, HotkeyReadiness::Starting);
                generation += 1;
                match NativeHotkeyListener::start(spec) {
                    Ok(listener) => {
                        log_initial_hotkey_readiness(generation, listener.readiness());
                        let readiness = readiness_from_initial(listener.readiness());
                        match activate_listener(listener, generation, events.clone()) {
                            Ok(listener) => {
                                active_listener = Some(listener);
                                publish_hotkey_status(&status_updates, readiness);
                            }
                            Err(error) => {
                                tracing::error!(%error, "could not bridge recovered hotkey listener");
                                publish_hotkey_status(
                                    &status_updates,
                                    HotkeyReadiness::Unavailable {
                                        message: error.to_string(),
                                    },
                                );
                                schedule_environment_retry(generation, events.clone());
                            }
                        }
                    }
                    Err(error) => {
                        tracing::error!(%error, "hotkey listener recovery failed");
                        publish_hotkey_status(
                            &status_updates,
                            HotkeyReadiness::Unavailable {
                                message: error.to_string(),
                            },
                        );
                        schedule_environment_retry(generation, events.clone());
                    }
                }
            }
            DispatchLoopEvent::Reconfigure { spec, response } => {
                let result = active_listener.as_ref().map_or_else(
                    || Err("hotkey listener is not currently available".to_owned()),
                    |listener| {
                        listener
                            .control
                            .reconfigure(spec.clone())
                            .map_err(|error| error.to_string())
                    },
                );
                if result.is_ok() {
                    current_spec = Some(spec);
                }
                let _ = response.send(result);
            }
            DispatchLoopEvent::Native { .. }
            | DispatchLoopEvent::ListenerClosed { .. }
            | DispatchLoopEvent::EnvironmentChanged { .. } => {}
        }
    }
}

fn publish_hotkey_status(
    status_updates: &std::sync::mpsc::Sender<HotkeyReadiness>,
    readiness: HotkeyReadiness,
) {
    if status_updates.send(readiness).is_err() {
        tracing::error!("hotkey status publisher has stopped");
    }
}

fn log_hotkey_decision(
    listener_generation: u64,
    mode: &str,
    event: &NativeHotkeySignal,
    disposition: &str,
    reason: &str,
    guard_remaining: Option<Duration>,
) {
    let event_age = Instant::now().saturating_duration_since(event.observed_at);
    let (trigger, key_code, key_state) = match event.trigger {
        NativeHotkeySignalTrigger::Input(input) => ("input", Some(input.code), Some(input.state)),
        NativeHotkeySignalTrigger::DeviceDisconnected => ("device_disconnected", None, None),
    };
    tracing::info!(
        listener_generation,
        mode,
        signal = ?event.signal,
        disposition,
        reason,
        device_id = event.device.id,
        device_path = %event.device.path.display(),
        device_name = %event.device.name,
        trigger,
        ?key_code,
        ?key_state,
        event_age_micros = event_age.as_micros(),
        guard_remaining_millis = guard_remaining.map(|remaining| remaining.as_millis()),
        "hotkey signal evaluated"
    );
}

fn listener_closed_readiness() -> HotkeyReadiness {
    HotkeyReadiness::Unavailable {
        message: "Global shortcut listener stopped; waiting for the input environment to change"
            .to_owned(),
    }
}

enum DispatchLoopEvent {
    Native {
        generation: u64,
        event: NativeHotkeyEvent,
    },
    ActionFinished(HotkeyActionCompletion),
    ListenerClosed {
        generation: u64,
    },
    EnvironmentChanged {
        generation: u64,
    },
    Reconfigure {
        spec: HotkeySpec,
        response: std::sync::mpsc::SyncSender<Result<(), String>>,
    },
}

const TOGGLE_REARM_DELAY: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HotkeyActionOutcome {
    ToggleRecordingStarted,
    Other,
}

#[derive(Clone, Copy, Debug)]
struct HotkeyActionCompletion {
    outcome: HotkeyActionOutcome,
    completed_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HotkeyIgnoreReason {
    ToggleRelease,
    ToggleRearming { remaining: Duration },
    ActionInFlight,
    TerminalQueued,
}

impl HotkeyIgnoreReason {
    const fn label(self) -> &'static str {
        match self {
            Self::ToggleRelease => "toggle_release",
            Self::ToggleRearming { .. } => "toggle_rearming",
            Self::ActionInFlight => "action_in_flight",
            Self::TerminalQueued => "terminal_queued",
        }
    }

    const fn guard_remaining(self) -> Option<Duration> {
        match self {
            Self::ToggleRearming { remaining } => Some(remaining),
            Self::ToggleRelease | Self::ActionInFlight | Self::TerminalQueued => None,
        }
    }
}

#[derive(Default)]
struct HotkeyDispatchGate {
    in_flight: bool,
    pending_terminal: Option<NativeHotkeySignal>,
    toggle_rearm_at: Option<Instant>,
}

impl HotkeyDispatchGate {
    fn accept(&mut self, mode: &str, event: &NativeHotkeySignal) -> Result<(), HotkeyIgnoreReason> {
        if mode != "hold" && event.signal == HotkeySignal::Released {
            return Err(HotkeyIgnoreReason::ToggleRelease);
        }
        if mode != "hold"
            && event.signal == HotkeySignal::Pressed
            && let Some(rearm_at) = self.toggle_rearm_at
            && event.observed_at < rearm_at
        {
            return Err(HotkeyIgnoreReason::ToggleRearming {
                remaining: rearm_at.saturating_duration_since(event.observed_at),
            });
        }
        if !self.in_flight {
            self.in_flight = true;
            return Ok(());
        }
        let should_queue_terminal = match event.signal {
            HotkeySignal::Cancelled => true,
            HotkeySignal::Released if mode == "hold" => self
                .pending_terminal
                .as_ref()
                .is_none_or(|pending| pending.signal != HotkeySignal::Cancelled),
            HotkeySignal::Pressed | HotkeySignal::Released => false,
        };
        if should_queue_terminal {
            self.pending_terminal = Some(event.clone());
            return Err(HotkeyIgnoreReason::TerminalQueued);
        }
        Err(HotkeyIgnoreReason::ActionInFlight)
    }

    fn complete(&mut self, completion: HotkeyActionCompletion) -> Option<NativeHotkeySignal> {
        self.in_flight = false;
        if completion.outcome == HotkeyActionOutcome::ToggleRecordingStarted {
            self.toggle_rearm_at = Some(completion.completed_at + TOGGLE_REARM_DELAY);
        }
        let pending = self.pending_terminal.take();
        if pending.is_some() {
            self.in_flight = true;
        }
        pending
    }
}

fn spawn_hotkey_action(
    runtime: &Path,
    mode: &str,
    event: NativeHotkeySignal,
    events: std::sync::mpsc::Sender<DispatchLoopEvent>,
) {
    let runtime = runtime.to_owned();
    let mode = mode.to_owned();
    std::thread::Builder::new()
        .name("agentdictate-hotkey-action".into())
        .spawn(move || {
            let outcome = match dispatch_hotkey(&runtime, &mode, &event) {
                Ok(outcome) => outcome,
                Err(error) => {
                    tracing::error!(
                        %error,
                        signal = ?event.signal,
                        device_id = event.device.id,
                        device_path = %event.device.path.display(),
                        device_name = %event.device.name,
                        "hotkey action failed"
                    );
                    HotkeyActionOutcome::Other
                }
            };
            let _ = events.send(DispatchLoopEvent::ActionFinished(HotkeyActionCompletion {
                outcome,
                completed_at: Instant::now(),
            }));
        })
        .expect("hotkey action worker should start");
}

fn dispatch_hotkey(
    runtime: &Path,
    mode: &str,
    event: &NativeHotkeySignal,
) -> anyhow::Result<HotkeyActionOutcome> {
    let (mut client, initial) = IpcClient::connect(runtime)?;
    let ServerMessageKind::Snapshot { snapshot, .. } = initial.kind else {
        anyhow::bail!("daemon did not provide a hotkey snapshot")
    };
    let request_id = REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let phase = snapshot.workflow.phase;
    let Some(command) = command_for_hotkey(mode, event.signal, phase, request_id) else {
        tracing::info!(
            request_id,
            mode,
            signal = ?event.signal,
            ?phase,
            device_id = event.device.id,
            device_path = %event.device.path.display(),
            "hotkey signal produced no command"
        );
        return Ok(HotkeyActionOutcome::Other);
    };
    let starts_recording = matches!(&command.kind, ClientCommandKind::StartRecording { .. });
    let action = match &command.kind {
        ClientCommandKind::StartRecording { .. } => "start_recording",
        ClientCommandKind::StopRecording { .. } => "stop_recording",
        ClientCommandKind::Cancel { .. } => "cancel",
        _ => "unexpected",
    };
    tracing::info!(
        request_id,
        mode,
        action,
        signal = ?event.signal,
        ?phase,
        device_id = event.device.id,
        device_path = %event.device.path.display(),
        device_name = %event.device.name,
        "dispatching hotkey command"
    );
    let response = client.send(command)?;
    let toggle_recording_started = match response.kind {
        ServerMessageKind::CommandRejected { error, .. } => anyhow::bail!(error),
        ServerMessageKind::Snapshot {
            snapshot, settings, ..
        } => {
            let recording_job = match snapshot.workflow.phase {
                WorkflowPhase::Recording { job_id } if starts_recording => Some(job_id),
                _ => None,
            };
            tracing::info!(
                request_id,
                action,
                resulting_phase = ?snapshot.workflow.phase,
                "hotkey command completed"
            );
            if settings.values.max_recording_seconds > 0
                && let Some(job_id) = recording_job
            {
                spawn_maximum_duration_stop(
                    runtime.to_owned(),
                    job_id,
                    settings.values.max_recording_seconds,
                );
            }
            mode != "hold" && recording_job.is_some()
        }
        ServerMessageKind::Workspace { .. } | ServerMessageKind::HistoryPage { .. } => false,
    };
    Ok(if toggle_recording_started {
        HotkeyActionOutcome::ToggleRecordingStarted
    } else {
        HotkeyActionOutcome::Other
    })
}

fn spawn_maximum_duration_stop(runtime: std::path::PathBuf, job_id: JobId, seconds: u32) {
    std::thread::Builder::new()
        .name("agentdictate-maximum-duration".into())
        .spawn(move || {
            std::thread::park_timeout(Duration::from_secs(u64::from(seconds)));
            let result = (|| -> anyhow::Result<()> {
                let (mut client, initial) = IpcClient::connect(&runtime)?;
                let ServerMessageKind::Snapshot { snapshot, .. } = initial.kind else {
                    return Ok(());
                };
                let request_id = REQUEST_ID.fetch_add(1, Ordering::Relaxed);
                let Some(command) = maximum_duration_command(job_id, &snapshot, request_id) else {
                    return Ok(());
                };
                if let ServerMessageKind::CommandRejected { error, .. } = client.send(command)?.kind
                {
                    anyhow::bail!(error);
                }
                Ok(())
            })();
            if let Err(error) = result {
                tracing::error!(job_id = %job_id, %error, "maximum-duration stop failed");
            }
        })
        .expect("maximum duration worker should start");
}

fn maximum_duration_command(
    expected_job: JobId,
    snapshot: &AppSnapshot,
    request_id: u64,
) -> Option<ClientCommand> {
    matches!(
        snapshot.workflow.phase,
        WorkflowPhase::Starting { job_id } | WorkflowPhase::Recording { job_id }
            if job_id == expected_job
    )
    .then(|| ClientCommand::stop_recording(request_id))
}

fn update_hotkey_status(runtime: &Path, readiness: HotkeyReadiness) -> anyhow::Result<()> {
    let (mut client, _) = IpcClient::connect(runtime)?;
    let request_id = REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let response = client.send(agentdictate_core::ClientCommand::hotkey_status_changed(
        request_id, readiness,
    ))?;
    if let ServerMessageKind::CommandRejected { error, .. } = response.kind {
        anyhow::bail!(error)
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use agentdictate_linux::{
        hotkey::{KEY_ESC, KEY_SPACE, KeyInput, KeyState},
        native_hotkey::{NativeHotkeyDevice, NativeHotkeySignalTrigger},
    };

    use super::*;

    fn hotkey_event(signal: HotkeySignal, observed_at: Instant) -> NativeHotkeySignal {
        let input = match signal {
            HotkeySignal::Pressed => KeyInput::new(KEY_SPACE, KeyState::Pressed),
            HotkeySignal::Released => KeyInput::new(KEY_SPACE, KeyState::Released),
            HotkeySignal::Cancelled => KeyInput::new(KEY_ESC, KeyState::Pressed),
        };
        NativeHotkeySignal {
            signal,
            device: NativeHotkeyDevice {
                id: 20,
                path: "/dev/input/event20".into(),
                name: "Test keyboard".into(),
            },
            trigger: NativeHotkeySignalTrigger::Input(input),
            observed_at,
        }
    }

    fn completion(outcome: HotkeyActionOutcome, completed_at: Instant) -> HotkeyActionCompletion {
        HotkeyActionCompletion {
            outcome,
            completed_at,
        }
    }

    #[test]
    fn toggle_press_during_processing_is_discarded_not_replayed() {
        let started_at = Instant::now();
        let mut gate = HotkeyDispatchGate::default();
        assert!(
            gate.accept("toggle", &hotkey_event(HotkeySignal::Pressed, started_at))
                .is_ok()
        );
        assert!(matches!(
            gate.accept(
                "toggle",
                &hotkey_event(
                    HotkeySignal::Pressed,
                    started_at + Duration::from_millis(50)
                )
            ),
            Err(HotkeyIgnoreReason::ActionInFlight)
        ));
        assert!(
            gate.complete(completion(
                HotkeyActionOutcome::Other,
                started_at + Duration::from_millis(100)
            ))
            .is_none()
        );
    }

    #[test]
    fn toggle_reactivation_immediately_after_start_is_ignored_until_rearmed() {
        let started_at = Instant::now();
        let completed_at = started_at + Duration::from_millis(100);
        let mut gate = HotkeyDispatchGate::default();
        assert!(
            gate.accept("toggle", &hotkey_event(HotkeySignal::Pressed, started_at))
                .is_ok()
        );
        assert!(
            gate.complete(completion(
                HotkeyActionOutcome::ToggleRecordingStarted,
                completed_at
            ))
            .is_none()
        );
        assert!(matches!(
            gate.accept(
                "toggle",
                &hotkey_event(
                    HotkeySignal::Released,
                    completed_at + Duration::from_millis(10)
                )
            ),
            Err(HotkeyIgnoreReason::ToggleRelease)
        ));
        assert!(matches!(
            gate.accept(
                "toggle",
                &hotkey_event(
                    HotkeySignal::Pressed,
                    completed_at + Duration::from_millis(31)
                )
            ),
            Err(HotkeyIgnoreReason::ToggleRearming { .. })
        ));
        assert!(
            gate.accept(
                "toggle",
                &hotkey_event(HotkeySignal::Pressed, completed_at + TOGGLE_REARM_DELAY)
            )
            .is_ok()
        );
    }

    #[test]
    fn toggle_cancel_bypasses_rearm_delay() {
        let started_at = Instant::now();
        let completed_at = started_at + Duration::from_millis(100);
        let mut gate = HotkeyDispatchGate::default();
        assert!(
            gate.accept("toggle", &hotkey_event(HotkeySignal::Pressed, started_at))
                .is_ok()
        );
        gate.complete(completion(
            HotkeyActionOutcome::ToggleRecordingStarted,
            completed_at,
        ));

        assert!(
            gate.accept(
                "toggle",
                &hotkey_event(
                    HotkeySignal::Cancelled,
                    completed_at + Duration::from_millis(31)
                )
            )
            .is_ok()
        );
    }

    #[test]
    fn toggle_cancel_during_start_is_queued() {
        let started_at = Instant::now();
        let completed_at = started_at + Duration::from_millis(100);
        let mut gate = HotkeyDispatchGate::default();
        assert!(
            gate.accept("toggle", &hotkey_event(HotkeySignal::Pressed, started_at))
                .is_ok()
        );
        assert_eq!(
            gate.accept(
                "toggle",
                &hotkey_event(
                    HotkeySignal::Cancelled,
                    started_at + Duration::from_millis(50)
                )
            ),
            Err(HotkeyIgnoreReason::TerminalQueued)
        );

        assert_eq!(
            gate.complete(completion(
                HotkeyActionOutcome::ToggleRecordingStarted,
                completed_at
            ))
            .map(|event| event.signal),
            Some(HotkeySignal::Cancelled)
        );
    }

    #[test]
    fn unsuccessful_toggle_start_does_not_arm_reactivation_delay() {
        let started_at = Instant::now();
        let completed_at = started_at + Duration::from_millis(100);
        let mut gate = HotkeyDispatchGate::default();
        assert!(
            gate.accept("toggle", &hotkey_event(HotkeySignal::Pressed, started_at))
                .is_ok()
        );
        gate.complete(completion(HotkeyActionOutcome::Other, completed_at));

        assert!(
            gate.accept(
                "toggle",
                &hotkey_event(
                    HotkeySignal::Pressed,
                    completed_at + Duration::from_millis(31)
                )
            )
            .is_ok()
        );
    }

    #[test]
    fn hold_release_during_start_is_coalesced_once() {
        let started_at = Instant::now();
        let mut gate = HotkeyDispatchGate::default();
        assert!(
            gate.accept("hold", &hotkey_event(HotkeySignal::Pressed, started_at))
                .is_ok()
        );
        assert!(matches!(
            gate.accept(
                "hold",
                &hotkey_event(
                    HotkeySignal::Released,
                    started_at + Duration::from_millis(50)
                )
            ),
            Err(HotkeyIgnoreReason::TerminalQueued)
        ));
        assert!(matches!(
            gate.accept(
                "hold",
                &hotkey_event(
                    HotkeySignal::Released,
                    started_at + Duration::from_millis(60)
                )
            ),
            Err(HotkeyIgnoreReason::TerminalQueued)
        ));
        assert_eq!(
            gate.complete(completion(
                HotkeyActionOutcome::Other,
                started_at + Duration::from_millis(100)
            ))
            .map(|event| event.signal),
            Some(HotkeySignal::Released)
        );
        assert!(
            gate.complete(completion(
                HotkeyActionOutcome::Other,
                started_at + Duration::from_millis(120)
            ))
            .is_none()
        );
    }

    #[test]
    fn maximum_duration_stop_never_targets_a_later_recording() {
        let expected = JobId::new();
        let later = JobId::new();
        let snapshot = AppSnapshot {
            sequence: 1,
            workflow: agentdictate_core::WorkflowSnapshot {
                phase: WorkflowPhase::Recording { job_id: later },
            },
            hotkey: HotkeyReadiness::Ready,
            recoverable_count: 0,
            last_transcript: None,
        };

        assert!(maximum_duration_command(expected, &snapshot, 9).is_none());
    }

    #[test]
    fn listener_closure_is_never_left_reported_as_ready() {
        assert!(matches!(
            listener_closed_readiness(),
            HotkeyReadiness::Unavailable { message }
                if message.contains("waiting for the input environment to change")
        ));
    }
}
