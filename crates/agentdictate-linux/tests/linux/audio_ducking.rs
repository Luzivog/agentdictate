use crate::support;

use std::time::{Duration, Instant};

use agentdictate_core::Settings;
use agentdictate_linux::{
    audio_ducking::{PlaybackDucker, SystemPactl},
    command::{PlatformExecutable, PlatformTool},
};
use support::TestDirectory;

#[test]
fn a_hung_pactl_neither_delays_the_recording_nor_ducks() {
    let directory = TestDirectory::new();
    let calls = directory.path().join("calls");
    let pactl = directory.executable(
        "pactl",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec sleep 30\n",
            calls.display()
        ),
    );
    let mut ducker = PlaybackDucker::open(
        SystemPactl::at(PlatformExecutable::at(PlatformTool::Pactl, pactl)),
        directory.path().join("ducking.json"),
    );
    let started = Instant::now();

    ducker.duck(&Settings {
        audio_ducking_enabled: true,
        ..Settings::default()
    });

    // The snapshot hangs for its whole one-second deadline on the worker.
    assert!(started.elapsed() < Duration::from_millis(500));
    while !calls.exists() {
        std::thread::sleep(Duration::from_millis(2));
    }
    ducker.restore();
    drop(ducker);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(
        std::fs::read_to_string(calls).unwrap(),
        "get-default-sink\n",
        "a hung snapshot must skip ducking instead of writing a volume"
    );
}
