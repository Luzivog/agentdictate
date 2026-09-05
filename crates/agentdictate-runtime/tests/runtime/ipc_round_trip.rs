use std::sync::{Arc, Mutex};
use std::thread;
use std::{fs, io};

use agentdictate_runtime::{
    AppSnapshot, ClientCommand, ClientCommandKind, HotkeyReadiness, IpcClient, IpcError,
    IpcHandler, IpcServer, ServerMessage, ServerMessageKind, Settings, Workflow, WorkflowPhase,
    WorkflowSignal,
};
use tempfile::TempDir;

struct TestHandler {
    snapshot: Arc<Mutex<AppSnapshot>>,
    settings: Settings,
    workflow: Workflow,
}

impl IpcHandler for TestHandler {
    fn snapshot(&self, request_id: u64) -> ServerMessage {
        ServerMessage::snapshot(
            request_id,
            self.snapshot.lock().unwrap().clone(),
            &self.settings,
        )
    }

    fn handle(&mut self, command: ClientCommand) -> ServerMessage {
        match command.kind {
            ClientCommandKind::GetSnapshot { request_id } => self.snapshot(request_id),
            ClientCommandKind::StartRecording { request_id, .. } => {
                let mut snapshot = self.snapshot.lock().unwrap();
                let job_id = agentdictate_runtime::JobId::new();
                snapshot.workflow = self
                    .workflow
                    .apply(WorkflowSignal::StartRequested { job_id })
                    .unwrap();
                snapshot.sequence += 1;
                ServerMessage::snapshot(request_id, snapshot.clone(), &self.settings)
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
        sequence: 5,
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
        workflow,
    };
    let server = IpcServer::bind(&runtime_directory).unwrap();
    assert_eq!(server.socket_mode().unwrap(), 0o600);
    let server_thread = thread::spawn(move || {
        let mut handler = handler;
        server.serve_next(&mut handler).unwrap();
        server.serve_next(&mut handler).unwrap();
    });

    let (mut client, initial) = IpcClient::connect(&runtime_directory).unwrap();
    let ServerMessageKind::Snapshot {
        request_id,
        snapshot: initial_snapshot,
        settings,
    } = initial.kind
    else {
        panic!("initial IPC message was not a snapshot")
    };
    assert_eq!(request_id, 0);
    assert_eq!(initial_snapshot.sequence, 5);
    assert_eq!(settings.values.openai_api_key, "");
    assert!(settings.has_api_key);

    let response = client.send(ClientCommand::start_recording(42)).unwrap();
    let ServerMessageKind::Snapshot {
        request_id,
        snapshot: started,
        ..
    } = response.kind
    else {
        panic!("command response was not a snapshot")
    };
    assert_eq!(request_id, 42);
    assert_eq!(started.sequence, 6);
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
    assert_eq!(reconnected.sequence, 6);
    assert!(matches!(
        reconnected.workflow.phase,
        WorkflowPhase::Starting { .. }
    ));
    drop(reconnected_client);

    server_thread.join().unwrap();
}

#[test]
fn client_reads_the_kernel_authenticated_server_pid() {
    let directory = tempfile::tempdir().unwrap();
    let runtime_directory = directory.path().join("runtime");
    let workflow = Workflow::new();
    let snapshot = Arc::new(Mutex::new(AppSnapshot {
        sequence: 0,
        workflow: workflow.snapshot(),
        hotkey: HotkeyReadiness::Ready,
        recoverable_count: 0,
        last_transcript: None,
    }));
    let mut handler = TestHandler {
        snapshot,
        settings: Settings::default(),
        workflow,
    };
    let server = IpcServer::bind(&runtime_directory).unwrap();
    let service = std::thread::spawn(move || {
        server.serve_next(&mut handler).unwrap();
    });

    let (client, _) = IpcClient::connect(&runtime_directory).unwrap();
    assert_eq!(client.peer_pid().unwrap(), std::process::id());
    drop(client);
    service.join().unwrap();
}

#[test]
fn silent_client_does_not_block_a_second_command_session() {
    let directory = TempDir::new().unwrap();
    let runtime_directory = directory.path().join("runtime");
    let workflow = Workflow::new();
    let snapshot = Arc::new(Mutex::new(AppSnapshot {
        sequence: 1,
        workflow: workflow.snapshot(),
        hotkey: HotkeyReadiness::Ready,
        recoverable_count: 0,
        last_transcript: None,
    }));
    let handler = Arc::new(Mutex::new(TestHandler {
        snapshot,
        settings: Settings::default(),
        workflow,
    }));
    let server = IpcServer::bind(&runtime_directory).unwrap();
    let server_handler = Arc::clone(&handler);
    let accepts = thread::spawn(move || {
        let first = server
            .serve_next_concurrent(Arc::clone(&server_handler))
            .unwrap();
        let second = server.serve_next_concurrent(server_handler).unwrap();
        (first, second)
    });

    let (silent, _) = IpcClient::connect(&runtime_directory).unwrap();
    let (mut active, _) = IpcClient::connect(&runtime_directory).unwrap();
    let response = active.send(ClientCommand::start_recording(77)).unwrap();

    assert!(matches!(
        response.kind,
        ServerMessageKind::Snapshot { request_id: 77, .. }
    ));
    drop(active);
    drop(silent);
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
        sequence: 9,
        workflow: workflow.snapshot(),
        hotkey: HotkeyReadiness::Ready,
        recoverable_count: 0,
        last_transcript: None,
    }));
    let mut handler = TestHandler {
        snapshot,
        settings: Settings::default(),
        workflow,
    };
    let server = IpcServer::bind(&runtime_directory).unwrap();
    let server_thread = thread::spawn(move || server.serve_next(&mut handler).unwrap());
    let (mut client, _) = IpcClient::connect(&runtime_directory).unwrap();

    let first = client.send(ClientCommand::get_snapshot(91)).unwrap();
    let second = client.send(ClientCommand::start_recording(92)).unwrap();

    assert!(matches!(
        first.kind,
        ServerMessageKind::Snapshot { request_id: 91, .. }
    ));
    assert!(matches!(
        second.kind,
        ServerMessageKind::Snapshot { request_id: 92, .. }
    ));
    drop(client);
    server_thread.join().unwrap();
}
