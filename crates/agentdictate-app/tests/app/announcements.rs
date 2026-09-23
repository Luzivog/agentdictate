//! How the daemon announces a dictation that ends without a paste: on the
//! overlay and as a desktop notification, through fake presenters.

use std::{
    collections::VecDeque,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, Sender, channel},
    time::{Duration, Instant},
};

use agentdictate_app::{
    AppPaths, Daemon, FinishingEncode, Notification, NotificationAction, NotificationBus, Notifier,
    OverlayController, OverlayUpdate, Transcriber, TranscriptionCompletion,
    start_overlay_presenter,
};
use agentdictate_core::{DictationNotice, FailureKind, JobId, JobStage, Settings, WorkflowPhase};
use agentdictate_runtime::{
    Deliverer, DeliveryDisposition, DeliveryMethod, ExternalError, ObservedFocus, RecordingJob,
    Runtime, Transcript,
};
use tempfile::tempdir;

use super::support::{InspectingRecorder, finish};

/// Fails every transcription with one kind of failure.
#[derive(Clone)]
struct FailingTranscriber(FailureKind);

impl Transcriber for FailingTranscriber {
    fn transcribe(
        &mut self,
        _job: &RecordingJob,
        _encode: Option<FinishingEncode>,
    ) -> Result<Transcript, ExternalError> {
        Err(ExternalError::of_kind(self.0, "raw service error"))
    }
}

/// Hears nothing in every recording.
#[derive(Clone)]
struct QuietTranscriber;

impl Transcriber for QuietTranscriber {
    fn transcribe(
        &mut self,
        _job: &RecordingJob,
        _encode: Option<FinishingEncode>,
    ) -> Result<Transcript, ExternalError> {
        Err(ExternalError::NoSpeech)
    }
}

struct SubmittedDelivery;

impl Deliverer for SubmittedDelivery {
    fn deliver(
        &mut self,
        _job: &RecordingJob,
        method: DeliveryMethod,
    ) -> Result<DeliveryDisposition, ExternalError> {
        Ok(DeliveryDisposition::Submitted {
            copied_to_clipboard: true,
            paste_triggered: method == DeliveryMethod::Paste,
            consumed: method == DeliveryMethod::Paste,
        })
    }
}

/// Hands every notification it is asked to show to the test.
struct ForwardingBus(Sender<Notification>);

impl NotificationBus for ForwardingBus {
    fn show(&mut self, notification: &Notification, replaces: u32) -> Result<u32, String> {
        self.0
            .send(notification.clone())
            .map_err(|error| error.to_string())?;
        Ok(replaces + 1)
    }
}

/// A notifier whose notifications arrive on the returned receiver. It ends,
/// disconnecting the receiver, once every clone of the notifier is dropped.
fn forwarding_notifier() -> (Notifier, Receiver<Notification>) {
    let (shown, notifications) = channel();
    let (actions, _clicked) = channel::<NotificationAction>();
    (
        Notifier::start(ForwardingBus(shown), actions).unwrap(),
        notifications,
    )
}

/// An overlay presenter whose fake helper writes each update it receives
/// to `received`.
struct FakeOverlay {
    received: PathBuf,
}

impl FakeOverlay {
    fn start(directory: &Path) -> (OverlayController, Self) {
        let executable = directory.join("overlay-helper");
        let received = directory.join("received");
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '{{\"status\":\"frame_submitted\"}}\\n'\nwhile IFS= read -r line; do printf '%s\\n' \"$line\" >> '{}'; done\n",
                received.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (overlay, _presenter) = start_overlay_presenter(executable).unwrap();
        (overlay, Self { received })
    }

    /// The notices the helper received before the start of `next_job`. The
    /// helper reads its updates in order, so none can still be on the way.
    fn notices_before(&self, next_job: JobId) -> Vec<DictationNotice> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let updates = std::fs::read_to_string(&self.received)
                .unwrap_or_default()
                .lines()
                .map(|line| serde_json::from_str::<OverlayUpdate>(line).unwrap())
                .collect::<Vec<_>>();
            if let Some(next) = updates.iter().position(|update| {
                update.workflow.phase == WorkflowPhase::Starting { job_id: next_job }
            }) {
                return updates[..next]
                    .iter()
                    .filter_map(|update| update.notice)
                    .collect();
            }
            assert!(
                Instant::now() < deadline,
                "the helper never saw the next start"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Reads the focus it is scripted to, one reading per call, and reports
/// whether the target took its paste.
struct ScriptedDelivery {
    focus: VecDeque<ObservedFocus>,
    consumed: bool,
    methods: Vec<DeliveryMethod>,
}

impl Deliverer for ScriptedDelivery {
    fn observe_focus(&mut self) -> ObservedFocus {
        self.focus.pop_front().unwrap_or(ObservedFocus::Unknown)
    }

    fn deliver(
        &mut self,
        _job: &RecordingJob,
        method: DeliveryMethod,
    ) -> Result<DeliveryDisposition, ExternalError> {
        self.methods.push(method);
        Ok(DeliveryDisposition::Submitted {
            copied_to_clipboard: true,
            paste_triggered: method == DeliveryMethod::Paste,
            consumed: method == DeliveryMethod::Paste && self.consumed,
        })
    }
}

fn daemon_with<T: Transcriber, D: Deliverer>(
    paths: &AppPaths,
    transcriber: T,
    deliverer: D,
) -> Daemon<InspectingRecorder, T, D> {
    std::fs::create_dir_all(paths.database_file.parent().unwrap()).unwrap();
    Daemon::new(
        Runtime::open(&paths.database_file).unwrap(),
        Settings::default(),
        paths.clone(),
        InspectingRecorder {
            database: paths.database_file.clone(),
            started_after_checkpoint: false,
        },
        transcriber,
        deliverer,
    )
}

#[test]
fn a_failed_dictation_is_announced_once_and_a_retry_from_the_window_is_not() {
    let directory = tempdir().unwrap();
    let paths = AppPaths::isolated(directory.path());
    let (overlay, fake_overlay) = FakeOverlay::start(directory.path());
    let (notifier, notifications) = forwarding_notifier();
    let mut daemon = daemon_with(
        &paths,
        FailingTranscriber(FailureKind::Offline),
        SubmittedDelivery,
    );
    daemon.set_overlay_controller(overlay);
    daemon.set_notifier(notifier);
    let started = daemon.start_recording().unwrap();

    finish(&mut daemon);
    // The settings window reports its own retry.
    let retry = daemon.retry_transcription(started.id).unwrap();
    daemon.complete_transcription(retry.run()).unwrap();
    let next = daemon.start_recording().unwrap();

    let failed = DictationNotice::Failed {
        failure: FailureKind::Offline,
    };
    assert_eq!(fake_overlay.notices_before(next.id), [failed]);
    drop(daemon);
    let shown = notifications.iter().collect::<Vec<_>>();
    assert_eq!(shown, [Notification::for_notice(failed, started.id)]);
    assert_eq!(shown[0].summary, "Couldn't reach OpenAI");
}

#[test]
fn a_quiet_recording_is_announced_as_nothing_heard() {
    let directory = tempdir().unwrap();
    let paths = AppPaths::isolated(directory.path());
    let (notifier, notifications) = forwarding_notifier();
    let mut daemon = daemon_with(&paths, QuietTranscriber, SubmittedDelivery);
    daemon.set_notifier(notifier);
    let started = daemon.start_recording().unwrap();

    finish(&mut daemon);
    drop(daemon);

    assert_eq!(
        notifications.iter().collect::<Vec<_>>(),
        [Notification::for_notice(
            DictationNotice::NothingHeard,
            started.id
        )]
    );
}

#[test]
fn paste_again_of_a_deleted_dictation_says_it_cannot() {
    let directory = tempdir().unwrap();
    let paths = AppPaths::isolated(directory.path());
    let (notifier, notifications) = forwarding_notifier();
    let mut daemon = daemon_with(&paths, super::support::FixedTranscriber, SubmittedDelivery);
    daemon.set_notifier(notifier);
    let started = daemon.start_recording().unwrap();
    finish(&mut daemon);
    daemon.clear_history().unwrap();

    assert!(daemon.paste_dictation(started.id).is_err());
    drop(daemon);

    assert_eq!(
        notifications.iter().collect::<Vec<_>>(),
        [Notification::for_notice(
            DictationNotice::PasteUnavailable,
            started.id
        )]
    );
}

/// A paste is sent once, into the window the dictation was stopped in, and
/// only a paste its target took ends silently. Otherwise the text is on the
/// clipboard, and the user is told to press Ctrl+V.
#[test]
fn a_dictation_is_pasted_once_and_says_copied_whenever_the_paste_is_not_confirmed() {
    use ObservedFocus::X11;
    let prompt = Duration::ZERO;
    let late = Duration::from_secs(9);
    for (focus, consumed, waited, method, notice) in [
        ([X11(7), X11(7)], true, prompt, DeliveryMethod::Paste, None),
        // The target never requested the text, so the paste may not have
        // landed; it is never sent again.
        (
            [X11(7), X11(7)],
            false,
            prompt,
            DeliveryMethod::Paste,
            Some(DictationNotice::Copied),
        ),
        (
            [X11(7), X11(9)],
            true,
            prompt,
            DeliveryMethod::CopyOnly,
            Some(DictationNotice::Copied),
        ),
        (
            [X11(7), X11(7)],
            true,
            late,
            DeliveryMethod::CopyOnly,
            Some(DictationNotice::Copied),
        ),
    ] {
        let directory = tempdir().unwrap();
        let paths = AppPaths::isolated(directory.path());
        let (notifier, notifications) = forwarding_notifier();
        let mut daemon = daemon_with(
            &paths,
            super::support::FixedTranscriber,
            ScriptedDelivery {
                focus: focus.into(),
                consumed,
                methods: Vec::new(),
            },
        );
        daemon.set_notifier(notifier);
        let started = daemon.start_recording().unwrap();
        let ticket = daemon.stop_recording().unwrap();
        let completion = TranscriptionCompletion {
            finished_at: Instant::now() + waited,
            ..ticket.run()
        };

        let delivered = daemon.complete_transcription(completion).unwrap();

        assert_eq!(delivered.stage, JobStage::Delivered);
        assert_eq!(daemon.deliverer().methods, [method], "{focus:?} {consumed}");
        drop(daemon);
        assert_eq!(
            notifications.iter().collect::<Vec<_>>(),
            notice
                .map(|notice| Notification::for_notice(notice, started.id))
                .into_iter()
                .collect::<Vec<_>>(),
            "{focus:?} {consumed} {waited:?}"
        );
    }
}
