use agentdictate_core::{
    AppSnapshot, ClientCommand, ClientCommandKind, HistoryPageCursor, HistoryPageRequest,
    HistoryPageSnapshot, HistorySnapshot, HotkeyReadiness, JobId, ServerMessage, ServerMessageKind,
    Settings, Workflow, WorkspaceSnapshot,
};

#[test]
fn client_commands_have_a_versioned_stable_wire_shape() {
    let wire = serde_json::to_string(&ClientCommand::start_recording(7)).unwrap();

    assert_eq!(
        wire,
        r#"{"protocol_version":3,"command":"start_recording","request_id":7}"#
    );
}

#[test]
fn a_default_history_page_requests_twenty_transcripts() {
    assert_eq!(HistoryPageRequest::default().page_size, 20);
}

#[test]
fn rejected_commands_return_a_correlated_error_instead_of_looking_successful() {
    let message = ServerMessage::command_rejected(19, "microphone unavailable");

    assert!(matches!(
        message.kind,
        ServerMessageKind::CommandRejected { request_id: 19, ref error }
            if error == "microphone unavailable"
    ));
    assert_eq!(
        serde_json::to_string(&message).unwrap(),
        r#"{"protocol_version":3,"message":"command_rejected","request_id":19,"error":"microphone unavailable"}"#
    );
}

#[test]
fn lifecycle_commands_round_trip_with_request_identity() {
    let job_id = JobId::new();
    let commands = [
        ClientCommand::get_snapshot(1),
        ClientCommand::stop_recording(2),
        ClientCommand::cancel(3),
        ClientCommand::recorder_exited(8, job_id),
        ClientCommand::retry_transcription(4, job_id),
        ClientCommand::retry_delivery(5, job_id),
        ClientCommand::delete_recovery(6, job_id),
        ClientCommand::get_workspace(9),
        ClientCommand::refresh_model_catalog(10),
        ClientCommand::quit(7),
    ];

    for command in commands {
        let wire = serde_json::to_string(&command).unwrap();
        let decoded: ClientCommand = serde_json::from_str(&wire).unwrap();
        assert_eq!(decoded, command);
    }
}

#[test]
fn history_page_requests_are_bounded_and_typed_on_the_wire() {
    let command = ClientCommand::get_history_page(
        12,
        "database migration",
        20,
        Some(HistoryPageCursor::new("opaque-page-2")),
    );
    let wire = serde_json::to_string(&command).unwrap();

    assert_eq!(
        wire,
        r#"{"protocol_version":3,"command":"get_history_page","request_id":12,"request":{"search":"database migration","page_size":20,"after":"opaque-page-2"}}"#
    );
    assert_eq!(
        serde_json::from_str::<ClientCommand>(&wire).unwrap(),
        command
    );
}

#[test]
fn history_page_requests_read_the_previous_limit_field_during_upgrade() {
    let legacy = r#"{"protocol_version":2,"command":"get_history_page","request_id":12,"request":{"search":"database migration","limit":20}}"#;

    let decoded: ClientCommand = serde_json::from_str(legacy).unwrap();
    assert!(matches!(
        decoded.kind,
        ClientCommandKind::GetHistoryPage {
            request_id: 12,
            request,
        } if request.search == "database migration"
            && request.page_size == 20
            && request.after.is_none()
    ));
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
    let message = ServerMessage::history_page(13, page.clone());
    let wire = serde_json::to_string(&message).unwrap();

    assert!(matches!(
        serde_json::from_str::<ServerMessage>(&wire).unwrap().kind,
        ServerMessageKind::HistoryPage {
            request_id: 13,
            page: decoded,
        } if *decoded == page
    ));
}

#[test]
fn history_rows_name_display_content_as_a_preview_and_read_legacy_payloads() {
    let legacy = serde_json::json!({
        "id": 41,
        "created_at": "2026-08-19T09:30:00Z",
        "final_text": "legacy transcript preview",
        "word_count": 3,
        "duration_seconds": 1.5
    });

    let row: HistorySnapshot = serde_json::from_value(legacy).unwrap();
    assert_eq!(row.preview_text, "legacy transcript preview");

    let current = serde_json::to_value(row).unwrap();
    assert_eq!(current["preview_text"], "legacy transcript preview");
    assert!(current.get("final_text").is_none());
}

#[test]
fn workspace_history_cursor_round_trips_and_defaults_for_legacy_snapshots() {
    let mut legacy = serde_json::to_value(WorkspaceSnapshot::default()).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("history_next_cursor");
    let decoded: WorkspaceSnapshot = serde_json::from_value(legacy).unwrap();
    assert_eq!(decoded.history_next_cursor, None);

    let workspace = WorkspaceSnapshot {
        history_next_cursor: Some(HistoryPageCursor::new("workspace-page-2")),
        ..WorkspaceSnapshot::default()
    };
    let decoded: WorkspaceSnapshot =
        serde_json::from_str(&serde_json::to_string(&workspace).unwrap()).unwrap();
    assert_eq!(decoded.history_next_cursor, workspace.history_next_cursor);
}

#[test]
fn workspace_mutations_are_typed_and_round_trip() {
    let rule = agentdictate_core::ReplacementRule {
        id: None,
        source_phrase: "kube cuddle".into(),
        replacement_phrase: "kubectl".into(),
        enabled: true,
        case_sensitive: false,
        whole_word_only: true,
    };
    let commands = [
        ClientCommand::create_replacement(20, rule.clone()),
        ClientCommand::update_replacement(
            21,
            agentdictate_core::ReplacementRule {
                id: Some(4),
                ..rule
            },
        ),
        ClientCommand::delete_replacement(22, 4),
        ClientCommand::delete_history(23, 7),
        ClientCommand::clear_history(24),
        ClientCommand::copy_transcript(25, 7),
    ];

    for command in commands {
        let wire = serde_json::to_string(&command).unwrap();
        assert_eq!(
            serde_json::from_str::<ClientCommand>(&wire).unwrap(),
            command
        );
    }
}

#[test]
fn snapshot_messages_round_trip_without_secret_settings() {
    let settings = Settings {
        openai_api_key: "sk-must-not-cross-the-seam".into(),
        ..Settings::default()
    };
    let message = ServerMessage::snapshot(
        9,
        AppSnapshot {
            sequence: 42,
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
fn ordinary_settings_updates_cannot_overwrite_or_echo_the_api_key() {
    let settings = Settings {
        openai_api_key: "sk-existing-secret".into(),
        hotkey: "F9".into(),
        ..Settings::default()
    };

    let command = ClientCommand::update_settings(10, &settings);
    let wire = serde_json::to_string(&command).unwrap();

    assert!(wire.contains("\"hotkey\":\"F9\""));
    assert!(!wire.contains("sk-existing-secret"));
}

#[test]
fn api_key_changes_use_a_dedicated_command() {
    let command = ClientCommand::set_api_key(11, "sk-replacement");
    let wire = serde_json::to_string(&command).unwrap();
    let decoded: ClientCommand = serde_json::from_str(&wire).unwrap();

    assert_eq!(decoded, command);
    assert!(wire.contains("\"command\":\"set_api_key\""));
    assert!(!format!("{command:?}").contains("sk-replacement"));
}
