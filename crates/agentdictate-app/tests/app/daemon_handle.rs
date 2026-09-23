use std::{
    path::Path,
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};

use agentdictate_app::{
    AgentProcess, AppPaths, CapturedRecording, Daemon, DaemonDeliverer, DaemonHandle,
    FinishingEncode, RecordingController, Transcriber, Trigger, TriggerOutcome,
};
use agentdictate_core::{
    ClientCommand, ClientCommandKind, JobStage, ServerMessageKind, SettingChange, Settings,
    WorkflowPhase,
};
use agentdictate_linux::hotkey::HotkeySignal;
use agentdictate_runtime::{
    Deliverer, DeliveryDisposition, DeliveryMethod, ExternalError, IpcHandler, Recorder,
    RecordingJob, Runtime, Transcript,
};
use tempfile::tempdir;

/// Holds every transcription until the test opens it.
#[derive(Clone, Default)]
struct Gate(Arc<(Mutex<GateState>, Condvar)>);

#[derive(Default)]
struct GateState {
    entered: bool,
    open: bool,
}

impl Gate {
    fn pass(&self) {
        let (state, changed) = &*self.0;
        let mut state = state.lock().unwrap();
        state.entered = true;
        changed.notify_all();
        let _ = changed
            .wait_timeout_while(state, Duration::from_secs(10), |state| !state.open)
            .unwrap();
    }

    fn wait_until_entered(&self) {
        let (state, changed) = &*self.0;
        let (state, _) = changed
            .wait_timeout_while(state.lock().unwrap(), Duration::from_secs(5), |state| {
                !state.entered
            })
            .unwrap();
        assert!(state.entered, "the transcription never started");
    }

    fn open(&self) {
        let (state, changed) = &*self.0;
        state.lock().unwrap().open = true;
        changed.notify_all();
    }
}

#[derive(Clone, Default)]
struct GatedTranscriber {
    gate: Gate,
}

impl Transcriber for GatedTranscriber {
    fn transcribe(
        &mut self,
        _job: &RecordingJob,
        _: Option<FinishingEncode>,
    ) -> Result<Transcript, ExternalError> {
        self.gate.pass();
        Ok(Transcript {
            text: "Gated words.".into(),
            model: "gpt-transcribe".into(),
        })
    }
}

#[derive(Clone)]
struct PanickingTranscriber;

impl Transcriber for PanickingTranscriber {
    fn transcribe(
        &mut self,
        _job: &RecordingJob,
        _: Option<FinishingEncode>,
    ) -> Result<Transcript, ExternalError> {
        panic!("transcriber bug")
    }
}

struct FileRecorder;

impl Recorder for FileRecorder {
    fn start(&mut self, job: &RecordingJob) -> Result<(), ExternalError> {
        std::fs::write(&job.audio_path, b"RIFFaudio").unwrap();
        Ok(())
    }
}

impl RecordingController for FileRecorder {
    fn finish(&mut self, _job: &RecordingJob) -> Result<CapturedRecording, ExternalError> {
        Ok(CapturedRecording {
            duration_seconds: 3.0,
            encoding: None,
        })
    }
}

#[derive(Default)]
struct RecordedDelivery {
    methods: Vec<DeliveryMethod>,
}

impl Deliverer for RecordedDelivery {
    fn deliver(
        &mut self,
        _job: &RecordingJob,
        method: DeliveryMethod,
    ) -> Result<DeliveryDisposition, ExternalError> {
        self.methods.push(method);
        Ok(DeliveryDisposition::Submitted {
            copied_to_clipboard: true,
            paste_triggered: method == DeliveryMethod::Paste,
            consumed: method == DeliveryMethod::Paste,
        })
    }
}

impl DaemonDeliverer for RecordedDelivery {
    fn copy_text(&mut self, _text: &str) -> Result<(), ExternalError> {
        Ok(())
    }
}

type TestHandle<T> = DaemonHandle<FileRecorder, T, RecordedDelivery>;

fn handle_with<T: Transcriber>(root: &Path, transcriber: T) -> (TestHandle<T>, AppPaths) {
    let paths = AppPaths::isolated(root);
    paths.ensure_directories().unwrap();
    let daemon = Daemon::new(
        Runtime::open(&paths.database_file).unwrap(),
        Settings::default(),
        paths.clone(),
        FileRecorder,
        transcriber,
        RecordedDelivery::default(),
    );
    let handle = DaemonHandle::new(
        AgentProcess::from_parts(daemon, &paths),
        paths.runtime.clone(),
    );
    (handle, paths)
}

fn phase<T: Transcriber>(handle: &TestHandle<T>) -> WorkflowPhase {
    handle.with_process(|process| process.daemon().phase())
}

fn deliveries<T: Transcriber>(handle: &TestHandle<T>) -> Vec<DeliveryMethod> {
    handle.with_process(|process| process.daemon().deliverer().methods.clone())
}

fn wait_for<T: Transcriber>(handle: &TestHandle<T>, done: impl Fn(WorkflowPhase) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(phase(handle)) {
        assert!(Instant::now() < deadline, "still {:?}", phase(handle));
        thread::sleep(Duration::from_millis(5));
    }
}

/// Starts and stops a dictation through IPC, as `agentdictate` would.
fn record_and_stop<T: Transcriber>(handle: &TestHandle<T>) {
    for command in [
        ClientCommand::start_recording(),
        ClientCommandKind::StopRecording.into(),
    ] {
        let response = handle.handle(command);
        assert!(
            matches!(response.kind, ServerMessageKind::Snapshot { .. }),
            "{response:?}"
        );
    }
}

#[test]
fn settings_and_snapshots_are_served_while_a_transcription_is_blocked() {
    let directory = tempdir().unwrap();
    let transcriber = GatedTranscriber::default();
    let gate = transcriber.gate.clone();
    let (handle, _paths) = handle_with(directory.path(), transcriber);
    record_and_stop(&handle);
    gate.wait_until_entered();

    let started = Instant::now();
    let saved = handle.handle(ClientCommand::change_setting(
        SettingChange::PreserveTempAudio(true),
    ));
    let snapshot = handle.snapshot();

    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(matches!(saved.kind, ServerMessageKind::Snapshot { .. }));
    assert!(matches!(
        snapshot.kind,
        ServerMessageKind::Snapshot { snapshot, .. }
            if matches!(snapshot.workflow.phase, WorkflowPhase::Processing { .. })
    ));
    gate.open();
    wait_for(&handle, |phase| phase == WorkflowPhase::Ready);
    assert_eq!(deliveries(&handle), [DeliveryMethod::Paste]);
}

#[test]
fn presses_during_processing_are_ignored_atomically() {
    let directory = tempdir().unwrap();
    let transcriber = GatedTranscriber::default();
    let gate = transcriber.gate.clone();
    let (handle, paths) = handle_with(directory.path(), transcriber);
    let press = Trigger::Hotkey(HotkeySignal::Pressed);
    let TriggerOutcome::Started {
        job_id,
        toggle: true,
    } = handle.trigger(press)
    else {
        panic!("a toggle press starts a recording");
    };
    assert_eq!(handle.trigger(press), TriggerOutcome::Stopped { job_id });
    gate.wait_until_entered();

    for trigger in [
        press,
        Trigger::TrayToggle,
        Trigger::TrayStartLiteral,
        Trigger::Hotkey(HotkeySignal::Cancelled),
    ] {
        assert!(
            matches!(
                handle.trigger(trigger),
                TriggerOutcome::Ignored {
                    phase: WorkflowPhase::Processing { .. }
                }
            ),
            "{trigger:?}"
        );
    }

    let jobs: i64 = rusqlite::Connection::open(&paths.database_file)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM dictation_jobs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(jobs, 1);
    gate.open();
    wait_for(&handle, |phase| phase == WorkflowPhase::Ready);
    assert_eq!(deliveries(&handle), [DeliveryMethod::Paste]);
}

#[test]
fn cancel_during_processing_detaches_and_allows_a_new_recording() {
    let directory = tempdir().unwrap();
    let transcriber = GatedTranscriber::default();
    let gate = transcriber.gate.clone();
    let (handle, paths) = handle_with(directory.path(), transcriber);
    record_and_stop(&handle);
    gate.wait_until_entered();

    handle.handle(ClientCommandKind::Cancel.into());
    assert_eq!(phase(&handle), WorkflowPhase::Ready);
    handle.handle(ClientCommand::start_recording());
    let WorkflowPhase::Recording { job_id: next } = phase(&handle) else {
        panic!("a new recording starts at once");
    };
    gate.open();

    let deadline = Instant::now() + Duration::from_secs(5);
    let recovery = loop {
        let recoveries = Runtime::open_observer(&paths.database_file)
            .unwrap()
            .recoveries()
            .unwrap();
        if let Some(recovery) = recoveries.into_iter().next() {
            break recovery;
        }
        assert!(
            Instant::now() < deadline,
            "the cancelled result never arrived"
        );
        thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(recovery.stage, JobStage::ReadyToDeliver);
    assert_eq!(recovery.final_text, "Gated words.");
    assert!(deliveries(&handle).is_empty());
    assert_eq!(phase(&handle), WorkflowPhase::Recording { job_id: next });
}

#[test]
fn quit_waits_for_in_flight_processing_within_the_grace() {
    let directory = tempdir().unwrap();
    let transcriber = GatedTranscriber::default();
    let gate = transcriber.gate.clone();
    let (handle, _paths) = handle_with(directory.path(), transcriber);
    record_and_stop(&handle);
    gate.wait_until_entered();
    let opener = {
        let gate = gate.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            gate.open();
        })
    };

    handle.quit().unwrap();

    assert_eq!(deliveries(&handle), [DeliveryMethod::Paste]);
    assert!(handle.should_quit());
    opener.join().unwrap();
}

#[test]
fn quit_leaves_a_transcription_that_outlasts_the_grace_for_recovery() {
    let directory = tempdir().unwrap();
    let transcriber = GatedTranscriber::default();
    let gate = transcriber.gate.clone();
    let (handle, paths) = handle_with(directory.path(), transcriber);
    record_and_stop(&handle);
    gate.wait_until_entered();
    let WorkflowPhase::Processing { job_id, .. } = phase(&handle) else {
        panic!("the stop hands the job to processing");
    };
    let started = Instant::now();

    handle.quit().unwrap();

    assert!(started.elapsed() >= Duration::from_millis(2900));
    assert!(started.elapsed() < Duration::from_secs(6));
    let observer = Runtime::open_observer(&paths.database_file).unwrap();
    assert_eq!(
        observer.job(job_id).unwrap().unwrap().stage,
        JobStage::Transcribing
    );
    assert!(deliveries(&handle).is_empty());
    gate.open();
}

#[test]
fn processing_panic_is_reported_as_a_failed_job() {
    let directory = tempdir().unwrap();
    let (handle, paths) = handle_with(directory.path(), PanickingTranscriber);
    record_and_stop(&handle);

    wait_for(&handle, |phase| {
        matches!(
            phase,
            WorkflowPhase::NeedsAttention {
                at: JobStage::Failed,
                ..
            }
        )
    });

    let recovery = Runtime::open_observer(&paths.database_file)
        .unwrap()
        .recoveries()
        .unwrap()
        .remove(0);
    assert_eq!(recovery.stage, JobStage::Failed);
    assert!(
        recovery
            .error_message
            .is_some_and(|message| message.contains("stopped unexpectedly"))
    );
    handle.handle(ClientCommand::start_recording());
    assert!(matches!(phase(&handle), WorkflowPhase::Recording { .. }));
}
