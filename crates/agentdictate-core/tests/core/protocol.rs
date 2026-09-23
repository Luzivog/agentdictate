use agentdictate_core::{
    AppSnapshot, ClientCommand, ClientCommandKind, HistoryPageCursor, HistoryPageSnapshot,
    HotkeyReadiness, JobId, ServerMessage, ServerMessageKind, Settings, Workflow,
};

#[test]
fn client_commands_have_a_versioned_stable_wire_shape() {
    assert_eq!(
        serde_json::to_string(&ClientCommand::start_recording()).unwrap(),
        r#"{"protocol_version":12,"command":"start_recording"}"#
    );
    assert_eq!(
        serde_json::to_string(&ClientCommand::new(ClientCommandKind::StopRecording)).unwrap(),
        r#"{"protocol_version":12,"command":"stop_recording"}"#
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
        r#"{"protocol_version":12,"message":"command_rejected","error":"microphone unavailable"}"#
    );
}

#[test]
fn commands_round_trip_through_the_wire() {
    let job_id = JobId::new();
    let commands = [
        ClientCommandKind::GetSnapshot,
        ClientCommandKind::GetWorkspace,
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
fn history_page_requests_are_bounded_and_typed_on_the_wire() {
    let command = ClientCommand::get_history_page(
        "database migration",
        20,
        Some(HistoryPageCursor::new("opaque-page-2")),
    );
    let wire = serde_json::to_string(&command).unwrap();

    assert_eq!(
        wire,
        r#"{"protocol_version":12,"command":"get_history_page","request":{"search":"database migration","page_size":20,"after":"opaque-page-2"}}"#
    );
    assert_eq!(
        serde_json::from_str::<ClientCommand>(&wire).unwrap(),
        command
    );
}

#[test]
fn history_page_responses_round_trip_independently_from_the_workspace() {
    let page = HistoryPageSnapshot {
        search: "needle".into(),
        total_matches: 31,
        cursor_restarted: false,
        next_cursor: Some(HistoryPageCursor::new("opaque-page-2")),
        rows: Vec::new(),
    };
    let message = ServerMessage::history_page(page.clone());
    let wire = serde_json::to_string(&message).unwrap();

    assert!(matches!(
        serde_json::from_str::<ServerMessage>(&wire).unwrap().kind,
        ServerMessageKind::HistoryPage { page: decoded } if *decoded == page
    ));
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
            last_transcript: Some("safe transcript".into()),
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
