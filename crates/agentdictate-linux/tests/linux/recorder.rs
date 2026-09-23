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

/// A 16 kHz mono PCM16 WAV header as a printf format, the way pw-record
/// starts its file.
const WAV_HEADER: &str = r"RIFF\000\000\000\000WAVEfmt \020\000\000\000\001\000\001\000\200\076\000\000\000\175\000\000\002\000\020\000data\000\000\000\000";

/// A fake pw-record that writes a header plus `samples`, then records forever.
fn fake_recorder(directory: &TestDirectory, samples: &str) -> std::path::PathBuf {
    directory.executable(
        "pw-record",
        &format!(
            "#!/bin/sh\nfor output do :; done\nprintf '{WAV_HEADER}{samples}' > \"$output\"\nexec tail -f /dev/null\n"
        ),
    )
}

#[test]
fn recorder_becomes_ready_only_after_audio_bytes_exist_and_finalizes_on_stop() {
    let directory = TestDirectory::new();
    let fake_pw_record = fake_recorder(&directory, "audio");
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
fn a_header_longer_than_44_bytes_is_not_mistaken_for_audio() {
    let directory = TestDirectory::new();
    // pw-record may add chunks before `data`; this header is 56 bytes.
    let header = WAV_HEADER.replace("data", r"LIST\004\000\000\000abcddata");
    let fake_pw_record = directory.executable(
        "pw-record",
        &format!(
            "#!/bin/sh\nfor output do :; done\nprintf '{header}' > \"$output\"\nexec tail -f /dev/null\n"
        ),
    );
    let output = directory.path().join("recording.wav");
    let recorder = PwRecordRecorder::new(SystemCommandRunner, fake_pw_record);

    let error = recorder
        .start(&output, Instant::now() + Duration::from_millis(200))
        .expect_err("a header without samples is not a started recording");

    assert!(matches!(error, RecorderError::ReadinessDeadline));
    assert_eq!(fs::metadata(&output).unwrap().len(), 56);
}

#[test]
fn stale_audio_at_the_output_path_never_satisfies_new_capture_readiness() {
    let directory = TestDirectory::new();
    let fake_pw_record = directory.executable("pw-record", "#!/bin/sh\nexec tail -f /dev/null\n");
    let output = directory.path().join("recording.wav");
    let status = Command::new("sh")
        .args([
            "-c",
            &format!("printf '{WAV_HEADER}stale' > \"$0\""),
            output.to_str().unwrap(),
        ])
        .status()
        .expect("stale recording fixture");
    assert!(status.success() && fs::metadata(&output).unwrap().len() > 44);
    let recorder = PwRecordRecorder::new(SystemCommandRunner, fake_pw_record);

    let error = recorder
        .start(&output, Instant::now() + Duration::from_millis(20))
        .expect_err("old samples cannot make a new recorder ready");

    assert!(matches!(error, RecorderError::ReadinessDeadline));
}

#[test]
fn an_exited_recorder_is_reported_by_status_and_still_finalizes() {
    let directory = TestDirectory::new();
    let fake_pw_record = directory.executable(
        "pw-record",
        &format!(
            "#!/bin/sh\nfor output do :; done\nprintf '{WAV_HEADER}audio' > \"$output\"\nsleep 0.1\n"
        ),
    );
    let output = directory.path().join("exited.wav");
    let recorder = PwRecordRecorder::new(SystemCommandRunner, fake_pw_record);
    let mut recording = recorder
        .start(&output, Instant::now() + Duration::from_secs(2))
        .expect("recorder starts");
    let deadline = Instant::now() + Duration::from_secs(2);
    while !matches!(
        recording.status().expect("recorder status"),
        RecordingStatus::Exited { .. }
    ) {
        assert!(Instant::now() < deadline, "the recorder never exited");
        thread::sleep(Duration::from_millis(5));
    }

    let artifact = recording
        .stop(Instant::now() + Duration::from_secs(2))
        .expect("an exited recorder still finalizes");

    assert_eq!(artifact.path, output);
    assert!(artifact.bytes > 44);
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
                "printf '{}audio' > \"$output\"\n",
                "while :; do :; done\n",
            ),
            finalized.display(),
            WAV_HEADER,
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
                "printf '{}audio' > \"$output\"\n",
                "exec tail -f /dev/null\n",
            ),
            child_pid.display(),
            WAV_HEADER,
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
