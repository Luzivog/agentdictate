use crate::support;

use std::{
    env, fs,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    process::Command,
    thread,
    time::{Duration, Instant},
};

use agentdictate_linux::{
    command::SystemCommandRunner,
    recorder::{PwRecordRecorder, RecorderError, RecordingStatus},
};
use support::TestDirectory;

#[test]
fn recorder_becomes_ready_only_after_audio_bytes_exist_and_finalizes_on_stop() {
    let directory = TestDirectory::new();
    let fake_pw_record = directory.executable(
        "pw-record",
        concat!(
            "#!/bin/sh\n",
            "for output do :; done\n",
            "printf 'RIFF1234567890123456789012345678901234567890audio' > \"$output\"\n",
            "exec tail -f /dev/null\n",
        ),
    );
    let output = directory.path().join("recording.wav");
    let recorder = PwRecordRecorder::new(SystemCommandRunner, fake_pw_record);

    let mut recording = recorder
        .start(&output, Instant::now() + Duration::from_secs(2))
        .expect("first audio makes the recorder ready");
    assert!(matches!(
        recording.status().expect("recorder status"),
        RecordingStatus::Capturing { bytes } if bytes > 44
    ));

    let artifact = recording
        .stop(Instant::now() + Duration::from_secs(2))
        .expect("recording finalizes");
    assert_eq!(artifact.path, output);
    assert!(artifact.bytes > 44);
}

#[test]
fn stale_audio_at_the_output_path_never_satisfies_new_capture_readiness() {
    let directory = TestDirectory::new();
    let fake_pw_record = directory.executable("pw-record", "#!/bin/sh\nexec tail -f /dev/null\n");
    let output = directory.path().join("recording.wav");
    fs::write(
        &output,
        b"RIFF1234567890123456789012345678901234567890stale",
    )
    .expect("stale recording fixture");
    let recorder = PwRecordRecorder::new(SystemCommandRunner, fake_pw_record);

    let error = recorder
        .start(&output, Instant::now() + Duration::from_millis(20))
        .expect_err("old samples cannot make a new recorder ready");

    assert!(matches!(error, RecorderError::ReadinessDeadline));
}

#[test]
fn pidfd_exit_observer_wakes_without_consuming_the_child_needed_by_stop() {
    let directory = TestDirectory::new();
    let fake_pw_record = directory.executable(
        "pw-record",
        concat!(
            "#!/bin/sh\n",
            "for output do :; done\n",
            "printf 'RIFF1234567890123456789012345678901234567890audio' > \"$output\"\n",
            "exec tail -f /dev/null\n",
        ),
    );
    let output = directory.path().join("observed.wav");
    let recorder = PwRecordRecorder::new(SystemCommandRunner, fake_pw_record);
    let recording = recorder
        .start(&output, Instant::now() + Duration::from_secs(2))
        .expect("recorder starts");
    let observer = recording.exit_observer().expect("pidfd is available");
    assert!(observer.process_id() > 0);
    let watcher = thread::spawn(move || observer.wait());

    let artifact = recording
        .stop(Instant::now() + Duration::from_secs(2))
        .expect("stop still owns and finalizes the child");

    watcher
        .join()
        .expect("watcher thread joins")
        .expect("pidfd reports process exit");
    assert_eq!(artifact.path, output);
}

#[test]
fn dropping_a_live_recording_gives_sigint_time_to_finalize() {
    let directory = TestDirectory::new();
    let finalized = directory.path().join("finalized.bin");
    let fake_pw_record = directory.executable(
        "pw-record",
        &format!(
            concat!(
                "#!/bin/sh\n",
                "for output do :; done\n",
                "trap 'head -c 65536 /dev/zero > \"{}\"; exit 0' INT\n",
                "printf 'RIFF1234567890123456789012345678901234567890audio' > \"$output\"\n",
                "while :; do :; done\n",
            ),
            finalized.display(),
        ),
    );
    let output = directory.path().join("dropped.wav");
    let recorder = PwRecordRecorder::new(SystemCommandRunner, fake_pw_record);

    let recording = recorder
        .start(&output, Instant::now() + Duration::from_secs(2))
        .expect("recorder starts");
    drop(recording);

    assert_eq!(
        fs::metadata(finalized)
            .expect("SIGINT finalizer completed")
            .len(),
        65_536
    );
}

#[test]
fn pdeathsig_helper_process() {
    let Some(program) = env::var_os("AGENTDICTATE_PDEATH_PROGRAM") else {
        return;
    };
    let output = env::var_os("AGENTDICTATE_PDEATH_OUTPUT").expect("helper output path");
    let recorder = PwRecordRecorder::new(SystemCommandRunner, program);
    let recording = recorder
        .start(output.as_ref(), Instant::now() + Duration::from_secs(2))
        .expect("helper recorder starts");
    std::mem::forget(recording);
    // SAFETY: this deliberately models abrupt daemon loss without running
    // destructors, which is the behavior PR_SET_PDEATHSIG must cover.
    unsafe { libc::_exit(0) }
}

#[test]
fn abrupt_owner_death_sends_sigint_to_the_recorder_process_group() {
    let directory = TestDirectory::new();
    let child_pid = directory.path().join("child.pid");
    let fake_pw_record = directory.executable(
        "pw-record",
        &format!(
            concat!(
                "#!/bin/sh\n",
                "for output do :; done\n",
                "printf '%s' $$ > '{}'\n",
                "printf 'RIFF1234567890123456789012345678901234567890audio' > \"$output\"\n",
                "exec tail -f /dev/null\n",
            ),
            child_pid.display(),
        ),
    );
    let output = directory.path().join("abrupt.wav");

    let status = Command::new(env::current_exe().expect("current test executable"))
        .args([
            "--exact",
            "recorder::pdeathsig_helper_process",
            "--nocapture",
        ])
        .env("AGENTDICTATE_PDEATH_PROGRAM", &fake_pw_record)
        .env("AGENTDICTATE_PDEATH_OUTPUT", &output)
        .status()
        .expect("helper subprocess runs");
    assert!(status.success());
    let process_id = fs::read_to_string(&child_pid)
        .expect("recorder wrote its pid")
        .parse::<i32>()
        .expect("recorder pid is numeric");

    let exited = wait_for_process_exit(process_id, Duration::from_secs(2));
    if !exited {
        // SAFETY: cleanup is limited to the explicit fake recorder process
        // group created by this test.
        unsafe { libc::kill(-process_id, libc::SIGKILL) };
    }
    assert!(exited, "recorder survived abrupt owner process death");
}

fn wait_for_process_exit(process_id: i32, timeout: Duration) -> bool {
    // SAFETY: pidfd_open takes integer arguments and returns a new descriptor.
    let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, process_id, 0) };
    if descriptor < 0 {
        return io_error_is_process_gone();
    }
    let Ok(descriptor) = i32::try_from(descriptor) else {
        return false;
    };
    // SAFETY: ownership of the fresh pidfd transfers exactly once.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    let mut poll = libc::pollfd {
        fd: descriptor.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let timeout_millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: `poll` receives one initialized pollfd for the call duration.
    unsafe { libc::poll(&mut poll, 1, timeout_millis) > 0 }
}

fn io_error_is_process_gone() -> bool {
    std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}
