use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;
use std::{fs, io};

use agentdictate_core::HotkeyCaptureOutcome;
use agentdictate_runtime::{
    AppSnapshot, ClientCommand, ClientCommandKind, HotkeyReadiness, IpcClient, IpcError,
    IpcHandler, IpcServer, ServerMessage, ServerMessageKind, Settings, Workflow, WorkflowPhase,
    WorkflowSignal,
};
use tempfile::TempDir;

#[derive(Clone)]
struct TestHandler {
    snapshot: Arc<Mutex<AppSnapshot>>,
    settings: Settings,
    workflow: Arc<Mutex<Workflow>>,
}

impl IpcHandler for TestHandler {
    fn snapshot(&self) -> ServerMessage {
        ServerMessage::snapshot(self.snapshot.lock().unwrap().clone(), &self.settings)
    }

    fn handle(&self, command: ClientCommand) -> ServerMessage {
        match command.kind {
            ClientCommandKind::GetSnapshot => self.snapshot(),
            ClientCommandKind::StartRecording { .. } => {
                let mut snapshot = self.snapshot.lock().unwrap();
                let job_id = agentdictate_runtime::JobId::new();
                snapshot.workflow = self
                    .workflow
                    .lock()
                    .unwrap()
                    .apply(WorkflowSignal::StartRequested { job_id })
                    .unwrap();
                ServerMessage::snapshot(snapshot.clone(), &self.settings)
            }
            _ => panic!("test handler received an unexpected command"),
        }
    }
}

#[test]
fn start_recording_round_trip_and_reconnect_snapshot_use_a_private_socket() {
    let directory = TempDir::new().unwrap();
    let runtime_directory = directory.path().join("runtime");
    let workflow = Workflow::new();
    let snapshot = Arc::new(Mutex::new(AppSnapshot {
        workflow: workflow.snapshot(),
        hotkey: HotkeyReadiness::Ready,
        recoverable_count: 2,
        last_transcript: Some("previous words".to_owned()),
    }));
    let handler = TestHandler {
        snapshot: Arc::clone(&snapshot),
        settings: Settings {
            openai_api_key: "must-not-cross-ipc".to_owned(),
            ..Settings::default()
        },
        workflow: Arc::new(Mutex::new(workflow)),
    };
    let server = IpcServer::bind(&runtime_directory).unwrap();
    assert_eq!(server.socket_mode().unwrap(), 0o600);
    let server_thread = thread::spawn(move || {
        server.serve_next(&handler).unwrap();
        server.serve_next(&handler).unwrap();
    });

    let (mut client, initial) = IpcClient::connect(&runtime_directory).unwrap();
    let ServerMessageKind::Snapshot {
        snapshot: initial_snapshot,
        settings,
    } = initial.kind
    else {
        panic!("initial IPC message was not a snapshot")
    };
    assert_eq!(initial_snapshot.recoverable_count, 2);
    assert_eq!(settings.values.openai_api_key, "");
    assert!(settings.has_api_key);

    let response = client.send(ClientCommand::start_recording()).unwrap();
    let ServerMessageKind::Snapshot {
        snapshot: started, ..
    } = response.kind
    else {
        panic!("command response was not a snapshot")
    };
    assert!(matches!(
        started.workflow.phase,
        WorkflowPhase::Starting { .. }
    ));
    drop(client);

    let (reconnected_client, current) = IpcClient::connect(&runtime_directory).unwrap();
    let ServerMessageKind::Snapshot {
        snapshot: reconnected,
        ..
    } = current.kind
    else {
        panic!("reconnect message was not a snapshot")
    };
    assert!(matches!(
        reconnected.workflow.phase,
        WorkflowPhase::Starting { .. }
    ));
    drop(reconnected_client);

    server_thread.join().unwrap();
}

#[test]
fn silent_client_does_not_block_a_second_command_session() {
    let directory = TempDir::new().unwrap();
    let runtime_directory = directory.path().join("runtime");
    let workflow = Workflow::new();
    let snapshot = Arc::new(Mutex::new(AppSnapshot {
        workflow: workflow.snapshot(),
        hotkey: HotkeyReadiness::Ready,
        recoverable_count: 0,
        last_transcript: None,
    }));
    let handler = TestHandler {
        snapshot,
        settings: Settings::default(),
        workflow: Arc::new(Mutex::new(workflow)),
    };
    let server = IpcServer::bind(&runtime_directory).unwrap();
    let accepts = thread::spawn(move || {
        let first = server.serve_next_concurrent(handler.clone()).unwrap();
        let second = server.serve_next_concurrent(handler).unwrap();
        (first, second)
    });

    let (silent, _) = IpcClient::connect(&runtime_directory).unwrap();
    let (mut active, _) = IpcClient::connect(&runtime_directory).unwrap();
    let response = active.send(ClientCommand::start_recording()).unwrap();

    assert!(matches!(response.kind, ServerMessageKind::Snapshot { .. }));
    drop(active);
    drop(silent);
    let (first, second) = accepts.join().unwrap();
    first.join().unwrap().unwrap();
    second.join().unwrap().unwrap();
}

/// Answers shortcut captures only when the test sends an outcome, and
/// reports each capture that starts waiting.
#[derive(Clone)]
struct CapturingHandler {
    waiting: mpsc::Sender<()>,
    outcomes: Arc<Mutex<mpsc::Receiver<HotkeyCaptureOutcome>>>,
}

impl IpcHandler for CapturingHandler {
    fn snapshot(&self) -> ServerMessage {
        let snapshot = AppSnapshot {
            workflow: Workflow::new().snapshot(),
            hotkey: HotkeyReadiness::Ready,
            recoverable_count: 0,
            last_transcript: None,
        };
        ServerMessage::snapshot(snapshot, &Settings::default())
    }

    fn handle(&self, command: ClientCommand) -> ServerMessage {
        match command.kind {
            ClientCommandKind::GetSnapshot => self.snapshot(),
            ClientCommandKind::CaptureHotkey => {
                self.waiting.send(()).unwrap();
                let outcome = self.outcomes.lock().unwrap().recv().unwrap();
                ServerMessage::hotkey_captured(outcome)
            }
            _ => panic!("test handler received an unexpected command"),
        }
    }
}

#[test]
fn a_waiting_shortcut_capture_does_not_block_other_sessions() {
    let directory = TempDir::new().unwrap();
    let runtime_directory = directory.path().join("runtime");
    let (waiting, capture_waiting) = mpsc::channel();
    let (send_outcome, outcomes) = mpsc::channel();
    let handler = CapturingHandler {
        waiting,
        outcomes: Arc::new(Mutex::new(outcomes)),
    };
    let server = IpcServer::bind(&runtime_directory).unwrap();
    let accepts = thread::spawn(move || {
        let first = server.serve_next_concurrent(handler.clone()).unwrap();
        let second = server.serve_next_concurrent(handler).unwrap();
        (first, second)
    });

    let (mut capturing, _) = IpcClient::connect(&runtime_directory).unwrap();
    let capture = thread::spawn(move || {
        capturing
            .send(ClientCommandKind::CaptureHotkey.into())
            .unwrap()
    });
    capture_waiting.recv().unwrap();
    let (mut other, _) = IpcClient::connect(&runtime_directory).unwrap();
    assert!(matches!(
        other
            .send(ClientCommand::new(ClientCommandKind::GetSnapshot))
            .unwrap()
            .kind,
        ServerMessageKind::Snapshot { .. }
    ));

    send_outcome.send(HotkeyCaptureOutcome::TimedOut).unwrap();
    assert!(matches!(
        capture.join().unwrap().kind,
        ServerMessageKind::HotkeyCaptured {
            outcome: HotkeyCaptureOutcome::TimedOut,
        }
    ));
    drop(other);
    let (first, second) = accepts.join().unwrap();
    first.join().unwrap().unwrap();
    second.join().unwrap().unwrap();
}

#[test]
fn second_server_cannot_unlink_an_active_service_socket() {
    let directory = TempDir::new().unwrap();
    let runtime_directory = directory.path().join("runtime");
    let first = IpcServer::bind(&runtime_directory).unwrap();

    let second = IpcServer::bind(&runtime_directory);

    assert!(matches!(second, Err(IpcError::AlreadyRunning { .. })));
    assert_eq!(first.socket_mode().unwrap(), 0o600);
}

#[test]
fn removing_the_socket_cannot_start_a_second_daemon_while_the_first_owns_the_lock() {
    let directory = TempDir::new().unwrap();
    let runtime_directory = directory.path().join("runtime");
    let first = IpcServer::bind(&runtime_directory).unwrap();
    fs::remove_file(runtime_directory.join("agentdictate.sock")).unwrap();

    let second = IpcServer::bind(&runtime_directory);

    assert!(matches!(second, Err(IpcError::AlreadyRunning { .. })));
    drop(first);
}

#[test]
fn binding_never_deletes_a_non_socket_at_the_service_path() {
    let directory = TempDir::new().unwrap();
    let runtime_directory = directory.path().join("runtime");
    fs::create_dir(&runtime_directory).unwrap();
    let occupied_path = runtime_directory.join("agentdictate.sock");
    fs::write(&occupied_path, "keep this file").unwrap();

    let result = IpcServer::bind(&runtime_directory);

    assert!(
        matches!(result, Err(IpcError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists)
    );
    assert_eq!(fs::read_to_string(occupied_path).unwrap(), "keep this file");
}

#[test]
fn dropping_a_server_after_its_socket_disappears_allows_a_clean_rebind() {
    let directory = TempDir::new().unwrap();
    let runtime_directory = directory.path().join("runtime");
    let first = IpcServer::bind(&runtime_directory).unwrap();
    fs::remove_file(runtime_directory.join("agentdictate.sock")).unwrap();
    drop(first);

    let replacement = IpcServer::bind(&runtime_directory).unwrap();

    assert_eq!(replacement.socket_mode().unwrap(), 0o600);
}

#[test]
fn one_connected_ui_can_send_multiple_commands_without_reconnecting() {
    let directory = TempDir::new().unwrap();
    let runtime_directory = directory.path().join("runtime");
    let workflow = Workflow::new();
    let snapshot = Arc::new(Mutex::new(AppSnapshot {
        workflow: workflow.snapshot(),
        hotkey: HotkeyReadiness::Ready,
        recoverable_count: 0,
        last_transcript: None,
    }));
    let handler = TestHandler {
        snapshot,
        settings: Settings::default(),
        workflow: Arc::new(Mutex::new(workflow)),
    };
    let server = IpcServer::bind(&runtime_directory).unwrap();
    let server_thread = thread::spawn(move || server.serve_next(&handler).unwrap());
    let (mut client, _) = IpcClient::connect(&runtime_directory).unwrap();

    let first = client
        .send(ClientCommand::new(ClientCommandKind::GetSnapshot))
        .unwrap();
    let second = client.send(ClientCommand::start_recording()).unwrap();

    assert!(matches!(
        first.kind,
        ServerMessageKind::Snapshot { snapshot, .. }
            if snapshot.workflow.phase == WorkflowPhase::Ready
    ));
    assert!(matches!(
        second.kind,
        ServerMessageKind::Snapshot { snapshot, .. }
            if matches!(snapshot.workflow.phase, WorkflowPhase::Starting { .. })
    ));
    drop(client);
    server_thread.join().unwrap();
}

#[test]
fn idle_session_is_closed_after_the_read_timeout() {
    let directory = TempDir::new().unwrap();
    let runtime_directory = directory.path().join("runtime");
    let workflow = Workflow::new();
    let handler = TestHandler {
        snapshot: Arc::new(Mutex::new(AppSnapshot {
            workflow: workflow.snapshot(),
            hotkey: HotkeyReadiness::Ready,
            recoverable_count: 0,
            last_transcript: None,
        })),
        settings: Settings::default(),
        workflow: Arc::new(Mutex::new(workflow)),
    };
    let server = IpcServer::bind(&runtime_directory)
        .unwrap()
        .with_session_timeout(Duration::from_millis(100));
    let accepts = thread::spawn(move || server.serve_next_concurrent(handler).unwrap());
    let (mut silent, _) = IpcClient::connect(&runtime_directory).unwrap();

    let session = accepts.join().unwrap();

    session.join().unwrap().unwrap();
    assert!(
        silent
            .send(ClientCommand::new(ClientCommandKind::GetSnapshot))
            .is_err()
    );
}
