use std::io::{BufRead, BufReader};
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

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
        NativeHotkeyRetryWatcher,
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
    let (overlay_sender, overlay_receiver) = std::sync::mpsc::channel();
    process.set_overlay_sender(overlay_sender);
    start_hotkey_listener(&mut process, &runtime)?;
    let _maintenance_thread = match process.start_post_listener_maintenance() {
        Ok(thread) => Some(thread),
        Err(error) => {
            tracing::warn!(%error, "could not start nonessential maintenance");
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
    let overlay_presenter = match std::env::current_exe()
        .map_err(anyhow::Error::from)
        .and_then(|executable| {
            start_overlay_presenter(executable, overlay_receiver, detect_primary_work_area())
                .map_err(anyhow::Error::from)
        }) {
        Ok(thread) => Some(thread),
        Err(error) => {
            tracing::warn!(%error, "recording overlay is unavailable; dictation will continue");
            None
        }
    };
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
                event: NativeHotkeyEvent::Signal(signal),
            } if event_generation == generation => {
                let mode = recording_mode
                    .read()
                    .map_or_else(|_| "toggle".to_owned(), |mode| mode.clone());
                if let Some(signal) = gate.accept(&mode, signal) {
                    spawn_hotkey_action(runtime, &mode, signal, events.clone());
                }
            }
            DispatchLoopEvent::Native {
                generation: event_generation,
                event: NativeHotkeyEvent::Status(status),
            } if event_generation == generation => {
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
            DispatchLoopEvent::ActionFinished => {
                if let Some(signal) = gate.complete() {
                    let mode = recording_mode
                        .read()
                        .map_or_else(|_| "toggle".to_owned(), |mode| mode.clone());
                    spawn_hotkey_action(runtime, &mode, signal, events.clone());
                }
            }
            DispatchLoopEvent::ListenerClosed {
                generation: closed_generation,
            } if closed_generation == generation => {
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
    ActionFinished,
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

#[derive(Default)]
struct HotkeyDispatchGate {
    in_flight: bool,
    pending_hold_terminal: Option<HotkeySignal>,
}

impl HotkeyDispatchGate {
    fn accept(&mut self, mode: &str, signal: HotkeySignal) -> Option<HotkeySignal> {
        if mode != "hold" && signal == HotkeySignal::Released {
            return None;
        }
        if !self.in_flight {
            self.in_flight = true;
            return Some(signal);
        }
        if mode == "hold"
            && matches!(signal, HotkeySignal::Released | HotkeySignal::Cancelled)
            && (signal == HotkeySignal::Cancelled
                || self.pending_hold_terminal != Some(HotkeySignal::Cancelled))
        {
            self.pending_hold_terminal = Some(signal);
        }
        None
    }

    fn complete(&mut self) -> Option<HotkeySignal> {
        self.in_flight = false;
        let pending = self.pending_hold_terminal.take();
        if pending.is_some() {
            self.in_flight = true;
        }
        pending
    }
}

fn spawn_hotkey_action(
    runtime: &Path,
    mode: &str,
    signal: HotkeySignal,
    events: std::sync::mpsc::Sender<DispatchLoopEvent>,
) {
    let runtime = runtime.to_owned();
    let mode = mode.to_owned();
    std::thread::Builder::new()
        .name("agentdictate-hotkey-action".into())
        .spawn(move || {
            if let Err(error) = dispatch_hotkey(&runtime, &mode, signal) {
                tracing::error!(%error, "hotkey action failed");
            }
            let _ = events.send(DispatchLoopEvent::ActionFinished);
        })
        .expect("hotkey action worker should start");
}

fn dispatch_hotkey(runtime: &Path, mode: &str, signal: HotkeySignal) -> anyhow::Result<()> {
    let (mut client, initial) = IpcClient::connect(runtime)?;
    let ServerMessageKind::Snapshot { snapshot, .. } = initial.kind else {
        anyhow::bail!("daemon did not provide a hotkey snapshot")
    };
    let request_id = REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let Some(command) = command_for_hotkey(mode, signal, snapshot.workflow.phase, request_id)
    else {
        return Ok(());
    };
    let starts_recording = matches!(&command.kind, ClientCommandKind::StartRecording { .. });
    let response = client.send(command)?;
    match response.kind {
        ServerMessageKind::CommandRejected { error, .. } => anyhow::bail!(error),
        ServerMessageKind::Snapshot {
            snapshot, settings, ..
        } if starts_recording && settings.values.max_recording_seconds > 0 => {
            if let WorkflowPhase::Recording { job_id } = snapshot.workflow.phase {
                spawn_maximum_duration_stop(
                    runtime.to_owned(),
                    job_id,
                    settings.values.max_recording_seconds,
                );
            }
        }
        ServerMessageKind::Snapshot { .. }
        | ServerMessageKind::Workspace { .. }
        | ServerMessageKind::HistoryPage { .. } => {}
    }
    Ok(())
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
    use super::*;

    #[test]
    fn toggle_press_during_processing_is_discarded_not_replayed() {
        let mut gate = HotkeyDispatchGate::default();
        assert_eq!(
            gate.accept("toggle", HotkeySignal::Pressed),
            Some(HotkeySignal::Pressed)
        );
        assert_eq!(gate.accept("toggle", HotkeySignal::Pressed), None);
        assert_eq!(gate.complete(), None);
    }

    #[test]
    fn hold_release_during_start_is_coalesced_once() {
        let mut gate = HotkeyDispatchGate::default();
        assert_eq!(
            gate.accept("hold", HotkeySignal::Pressed),
            Some(HotkeySignal::Pressed)
        );
        assert_eq!(gate.accept("hold", HotkeySignal::Released), None);
        assert_eq!(gate.accept("hold", HotkeySignal::Released), None);
        assert_eq!(gate.complete(), Some(HotkeySignal::Released));
        assert_eq!(gate.complete(), None);
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
