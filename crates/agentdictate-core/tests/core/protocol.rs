use agentdictate_core::{
    AppSnapshot, ClientCommand, ClientCommandKind, HotkeyReadiness, JobId, ServerMessage,
    ServerMessageKind, Settings, Workflow,
};

#[test]
fn client_commands_have_a_versioned_stable_wire_shape() {
    assert_eq!(
        serde_json::to_string(&ClientCommand::start_recording()).unwrap(),
        r#"{"protocol_version":15,"command":"start_recording"}"#
    );
    assert_eq!(
        serde_json::to_string(&ClientCommand::new(ClientCommandKind::StopRecording)).unwrap(),
        r#"{"protocol_version":15,"command":"stop_recording"}"#
    );
}

#[test]
fn rejected_commands_return_an_error_instead_of_looking_successful() {
    let message = ServerMessage::command_rejected("microphone unavailable");

    assert!(matches!(
        message.kind,
        ServerMessageKind::CommandRejected { ref error } if error == "microphone unavailable"
    ));
    assert_eq!(
        serde_json::to_string(&message).unwrap(),
        r#"{"protocol_version":15,"message":"command_rejected","error":"microphone unavailable"}"#
    );
}

#[test]
fn commands_round_trip_through_the_wire() {
    let job_id = JobId::new();
    let commands = [
        ClientCommandKind::GetSnapshot,
        ClientCommandKind::StopRecording,
        ClientCommandKind::Cancel,
        ClientCommandKind::RetryTranscription { job_id },
        ClientCommandKind::RetryDelivery { job_id },
        ClientCommandKind::DeleteRecovery { job_id },
        ClientCommandKind::DeleteHistory { id: 7 },
        ClientCommandKind::ClearHistory,
        ClientCommandKind::CopyTranscript { id: 7 },
        ClientCommandKind::CaptureHotkey,
        ClientCommandKind::CancelHotkeyCapture,
        ClientCommandKind::Quit,
    ]
    .map(ClientCommand::new);

    for command in commands {
        let wire = serde_json::to_string(&command).unwrap();
        let decoded: ClientCommand = serde_json::from_str(&wire).unwrap();
        assert_eq!(decoded, command);
    }
}

#[test]
fn snapshot_messages_round_trip_without_secret_settings() {
    let settings = Settings {
        openai_api_key: "sk-must-not-cross-the-seam".into(),
        ..Settings::default()
    };
    let message = ServerMessage::snapshot(
        AppSnapshot {
            workflow: Workflow::new().snapshot(),
            hotkey: HotkeyReadiness::Ready,
            recoverable_count: 3,
            overlay_unavailable: true,
            history_set_aside: Some("/tmp/agentdictate.sqlite.corrupt-1".into()),
        },
        &settings,
    );

    let wire = serde_json::to_string(&message).unwrap();
    let decoded: ServerMessage = serde_json::from_str(&wire).unwrap();

    assert_eq!(decoded, message);
    assert!(!wire.contains("sk-must-not-cross-the-seam"));
}

#[test]
fn api_key_changes_use_a_dedicated_command() {
    let command = ClientCommand::set_api_key("sk-replacement");
    let wire = serde_json::to_string(&command).unwrap();
    let decoded: ClientCommand = serde_json::from_str(&wire).unwrap();

    assert_eq!(decoded, command);
    assert!(wire.contains("\"command\":\"set_api_key\""));
    assert!(!format!("{command:?}").contains("sk-replacement"));
}
