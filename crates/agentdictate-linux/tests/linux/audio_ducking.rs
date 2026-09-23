use crate::support;

use std::time::{Duration, Instant};

use agentdictate_core::Settings;
use agentdictate_linux::{
    audio_ducking::{PlaybackDucker, SystemPactl},
    command::{PlatformExecutable, PlatformTool},
};
use support::TestDirectory;

#[test]
fn a_hung_pactl_skips_ducking_within_its_deadline() {
    let directory = TestDirectory::new();
    let calls = directory.path().join("calls");
    let pactl = directory.executable(
        "pactl",
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec sleep 30\n",
            calls.display()
        ),
    );
    let mut ducker = PlaybackDucker::new(SystemPactl::at(PlatformExecutable::at(
        PlatformTool::Pactl,
        pactl,
    )));
    let started = Instant::now();

    ducker.duck(&Settings {
        audio_ducking_enabled: true,
        ..Settings::default()
    });
    ducker.restore();
    drop(ducker);

    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(
        std::fs::read_to_string(calls).unwrap(),
        "get-default-sink\n",
        "a hung snapshot must skip ducking instead of writing a volume"
    );
}
