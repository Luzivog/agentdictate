use std::path::Path;

use agentdictate_core::{
    ApiKeyCheck, AppSnapshot, ClientCommand, ClientCommandKind, DesktopReadiness, DictationMode,
    ExposedInput, FailureKind, HotkeyCaptureOutcome, HotkeyReadiness, JobId, JobStage,
    KeepTranscripts, MicrophoneCheck, MissingTool, PROTOCOL_VERSION, PasteShortcut,
    ProcessingStage, Readiness, RecordingMode, ServerMessage, ServerMessageKind, SettingChange,
    Settings, WorkflowPhase, WorkflowSnapshot, parse_vocabulary,
};

/// Every wire shape, as `wire_samples` serializes them, saved with the
/// protocol version they belong to.
const GOLDEN_FILE: &str = "tests/core/protocol.json";

/// Set to save the samples as the new version's shapes, after a bump.
const SAVE_VARIABLE: &str = "AGENTDICTATE_SAVE_PROTOCOL";

/// A wire change needs a new `PROTOCOL_VERSION`, or a window and a daemon
/// of different builds misread each other instead of saying one is
/// outdated. The golden file holds the shapes of the current version: a
/// change under the same version fails, and after a bump the new shapes are
/// saved with `AGENTDICTATE_SAVE_PROTOCOL=1`.
#[test]
fn every_wire_change_comes_with_a_new_protocol_version() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN_FILE);
    let current = serde_json::json!({
        "protocol_version": PROTOCOL_VERSION,
        "samples": wire_samples(),
    });
    let golden: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    if golden == current {
        return;
    }
    assert_ne!(
        golden["protocol_version"], current["protocol_version"],
        "The IPC wire format changed without a new protocol version. Bump PROTOCOL_VERSION, \
         then run this test with {SAVE_VARIABLE}=1 to save the new shapes."
    );
    assert!(
        std::env::var_os(SAVE_VARIABLE).is_some(),
        "PROTOCOL_VERSION changed. Run this test with {SAVE_VARIABLE}=1 to save its shapes."
    );
    let saved = serde_json::to_string_pretty(&current).unwrap() + "\n";
    std::fs::write(&path, saved).unwrap();
}

#[test]
fn every_message_reads_back_from_the_wire() {
    fn round_trips<T>(samples: Vec<T>)
    where
        T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        for sample in samples {
            let wire = serde_json::to_string(&sample).unwrap();
            assert_eq!(serde_json::from_str::<T>(&wire).unwrap(), sample);
        }
    }
    let samples = wire_samples();
    round_trips(vec![samples.envelopes.0]);
    round_trips(vec![samples.envelopes.1]);
    round_trips(samples.commands);
    round_trips(samples.replies);
    round_trips(samples.workflow_phases);
    round_trips(samples.readiness);
}

#[test]
fn snapshot_messages_never_carry_the_api_key() {
    let settings = Settings {
        openai_api_key: "sk-must-not-cross-the-seam".into(),
        ..Settings::default()
    };
    let message = ServerMessage::snapshot(snapshot(), &settings);

    let wire = serde_json::to_string(&message).unwrap();

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

/// One sample of every command and reply, covering each variant of the
/// enums they carry. Only the envelopes carry the version, so a bump
/// changes a few lines of the golden file.
#[derive(serde::Serialize)]
struct WireSamples {
    envelopes: (ClientCommand, ServerMessage),
    commands: Vec<ClientCommandKind>,
    replies: Vec<ServerMessageKind>,
    workflow_phases: Vec<WorkflowPhase>,
    readiness: Vec<Readiness>,
}

fn wire_samples() -> WireSamples {
    let job_id = sample_job_id();
    let mut commands = vec![
        ClientCommandKind::GetSnapshot,
        ClientCommandKind::StartRecording { mode: None },
        ClientCommandKind::StartRecording {
            mode: Some(DictationMode::Literal),
        },
        ClientCommandKind::StopRecording,
        ClientCommandKind::Cancel,
        ClientCommandKind::RetryTranscription { job_id },
        ClientCommandKind::PasteLast,
        ClientCommandKind::RetryDelivery { job_id },
        ClientCommandKind::DeleteRecovery { job_id },
        ClientCommandKind::DeleteHistory { id: 7 },
        ClientCommandKind::ClearHistory,
        ClientCommandKind::CopyTranscript { id: 7 },
        ClientCommandKind::CaptureHotkey,
        ClientCommandKind::CancelHotkeyCapture,
        ClientCommand::set_api_key("sk-sample").kind,
        ClientCommand::check_api_key(None).kind,
        ClientCommand::check_api_key(Some("sk-pasted".to_owned())).kind,
        ClientCommandKind::TestMicrophone,
        ClientCommandKind::Quit,
    ];
    commands.extend(
        [
            SettingChange::Language("fr".into()),
            SettingChange::Hotkey("Ctrl+Alt+D".parse().unwrap()),
            SettingChange::RecordingMode(RecordingMode::Hold),
            SettingChange::AudioDuckingEnabled(false),
            SettingChange::KeepTranscripts(KeepTranscripts::Days30),
            SettingChange::StartOnLogin(false),
            SettingChange::TranscriptionPrompt("Rust and GPUI".into()),
            SettingChange::DictationMode(DictationMode::Literal),
            SettingChange::PasteShortcut(PasteShortcut::Terminal),
            SettingChange::MaxRecordingSeconds(600),
            SettingChange::AudioDuckingVolumePercent(20),
            SettingChange::PreserveTempAudio(true),
            SettingChange::Vocabulary(parse_vocabulary("kubectl = cube control").unwrap()),
        ]
        .map(|change| ClientCommandKind::ChangeSetting { change }),
    );
    let mut replies = vec![ServerMessage::snapshot(snapshot(), &sample_settings()).kind];
    replies.extend(
        [
            HotkeyCaptureOutcome::Captured {
                hotkey: "Ctrl+Space".parse().unwrap(),
            },
            HotkeyCaptureOutcome::Cancelled,
            HotkeyCaptureOutcome::TimedOut,
        ]
        .map(|outcome| ServerMessage::hotkey_captured(outcome).kind),
    );
    replies.extend(
        [
            ApiKeyCheck::Works,
            ApiKeyCheck::Rejected,
            ApiKeyCheck::Unreachable,
        ]
        .map(|outcome| ServerMessage::api_key_checked(outcome).kind),
    );
    replies.push(ServerMessage::microphone_level(42).kind);
    replies.extend(
        [MicrophoneCheck::Heard, MicrophoneCheck::Silent]
            .map(|outcome| ServerMessage::microphone_tested(outcome).kind),
    );
    replies.push(ServerMessage::command_rejected("microphone unavailable").kind);
    let workflow_phases = vec![
        WorkflowPhase::Ready,
        WorkflowPhase::Starting { job_id },
        WorkflowPhase::Recording { job_id },
        WorkflowPhase::Stopping { job_id },
        WorkflowPhase::Processing {
            job_id,
            stage: ProcessingStage::Transcribing,
        },
        WorkflowPhase::NeedsAttention {
            job_id,
            at: JobStage::Failed,
            failure: FailureKind::Offline,
        },
    ];
    let readiness = vec![
        Readiness::default(),
        Readiness {
            shortcut: HotkeyReadiness::Ready,
            ..Readiness::default()
        },
        Readiness {
            shortcut: HotkeyReadiness::Unavailable {
                message: "no keyboard access".into(),
            },
            transcription_key: false,
            desktop: DesktopReadiness {
                paste_access: false,
                exposed_input: Some(ExposedInput {
                    rule: Some("/etc/udev/rules.d/99-open-input.rules".into()),
                }),
                missing_tools: MissingTool::ALL.to_vec(),
            },
        },
    ];
    commands.iter().for_each(sampled_command);
    replies.iter().for_each(sampled_reply);
    workflow_phases.iter().for_each(sampled_phase);
    readiness
        .iter()
        .for_each(|readiness| sampled_shortcut(&readiness.shortcut));
    WireSamples {
        envelopes: (
            ClientCommand::new(ClientCommandKind::GetSnapshot),
            ServerMessage::command_rejected("microphone unavailable"),
        ),
        commands,
        replies,
        workflow_phases,
        readiness,
    }
}

/// Fails to compile when a command or setting gains a variant, until
/// `wire_samples` has a sample of it.
fn sampled_command(command: &ClientCommandKind) {
    match command {
        ClientCommandKind::GetSnapshot
        | ClientCommandKind::StartRecording { .. }
        | ClientCommandKind::StopRecording
        | ClientCommandKind::Cancel
        | ClientCommandKind::RetryTranscription { .. }
        | ClientCommandKind::PasteLast
        | ClientCommandKind::RetryDelivery { .. }
        | ClientCommandKind::DeleteRecovery { .. }
        | ClientCommandKind::DeleteHistory { .. }
        | ClientCommandKind::ClearHistory
        | ClientCommandKind::CopyTranscript { .. }
        | ClientCommandKind::CaptureHotkey
        | ClientCommandKind::CancelHotkeyCapture
        | ClientCommandKind::SetApiKey { .. }
        | ClientCommandKind::CheckApiKey { .. }
        | ClientCommandKind::TestMicrophone
        | ClientCommandKind::Quit => {}
        ClientCommandKind::ChangeSetting { change } => match change {
            SettingChange::Language(_)
            | SettingChange::Hotkey(_)
            | SettingChange::RecordingMode(_)
            | SettingChange::AudioDuckingEnabled(_)
            | SettingChange::KeepTranscripts(_)
            | SettingChange::StartOnLogin(_)
            | SettingChange::TranscriptionPrompt(_)
            | SettingChange::DictationMode(_)
            | SettingChange::PasteShortcut(_)
            | SettingChange::MaxRecordingSeconds(_)
            | SettingChange::AudioDuckingVolumePercent(_)
            | SettingChange::PreserveTempAudio(_)
            | SettingChange::Vocabulary(_) => {}
        },
    }
}

/// Fails to compile when a reply, or an outcome it carries, gains a
/// variant, until `wire_samples` has a sample of it.
fn sampled_reply(reply: &ServerMessageKind) {
    match reply {
        ServerMessageKind::Snapshot { .. }
        | ServerMessageKind::MicrophoneLevel { .. }
        | ServerMessageKind::CommandRejected { .. } => {}
        ServerMessageKind::HotkeyCaptured { outcome } => match outcome {
            HotkeyCaptureOutcome::Captured { .. }
            | HotkeyCaptureOutcome::Cancelled
            | HotkeyCaptureOutcome::TimedOut => {}
        },
        ServerMessageKind::ApiKeyChecked { outcome } => match outcome {
            ApiKeyCheck::Works | ApiKeyCheck::Rejected | ApiKeyCheck::Unreachable => {}
        },
        ServerMessageKind::MicrophoneTested { outcome } => match outcome {
            MicrophoneCheck::Heard | MicrophoneCheck::Silent => {}
        },
    }
}

/// Fails to compile when the workflow gains a phase, until `wire_samples`
/// has a sample of it.
fn sampled_phase(phase: &WorkflowPhase) {
    match phase {
        WorkflowPhase::Ready
        | WorkflowPhase::Starting { .. }
        | WorkflowPhase::Recording { .. }
        | WorkflowPhase::Stopping { .. }
        | WorkflowPhase::Processing { .. }
        | WorkflowPhase::NeedsAttention { .. } => {}
    }
}

/// Fails to compile when the shortcut gains a state, until `wire_samples`
/// has a sample of it.
fn sampled_shortcut(shortcut: &HotkeyReadiness) {
    match shortcut {
        HotkeyReadiness::Starting
        | HotkeyReadiness::Ready
        | HotkeyReadiness::Unavailable { .. } => {}
    }
}

fn sample_job_id() -> JobId {
    "00000000-0000-4000-8000-000000000001".parse().unwrap()
}

/// Settings spelled out, so a changed default is not a wire change.
fn sample_settings() -> Settings {
    Settings {
        openai_api_key: "sk-never-sent".into(),
        transcription_model: "gpt-transcribe".into(),
        language: "en".into(),
        transcription_prompt: "Rust and GPUI".into(),
        vocabulary: parse_vocabulary("kubectl = cube control").unwrap(),
        dictation_mode: DictationMode::Dictate,
        hotkey: "Ctrl+Space".parse().unwrap(),
        recording_mode: RecordingMode::Toggle,
        max_recording_seconds: 300,
        audio_ducking_enabled: true,
        audio_ducking_volume_percent: 15,
        audio_ducking_fade_out_ms: 600,
        audio_ducking_fade_in_ms: 600,
        start_on_login: true,
        show_tray_icon: true,
        preserve_temp_audio: false,
        keep_transcripts: KeepTranscripts::Forever,
        paste_shortcut: PasteShortcut::Automatic,
    }
}

fn snapshot() -> AppSnapshot {
    AppSnapshot {
        workflow: WorkflowSnapshot {
            phase: WorkflowPhase::Ready,
        },
        readiness: Readiness::default(),
        recoverable_count: 3,
        overlay_unavailable: true,
        history_set_aside: Some("/data/agentdictate.sqlite.corrupt-1".into()),
    }
}
