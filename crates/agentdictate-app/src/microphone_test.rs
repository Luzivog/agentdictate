//! The Setup screen's microphone test: listens for a few seconds, reports
//! how loud the microphone is as it goes, and keeps nothing it hears. No
//! audio is uploaded, and no dictation or Recovery item is made.

use std::{
    fs::{self, File},
    io::Read,
    path::PathBuf,
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

use agentdictate_core::MicrophoneCheck;
use agentdictate_linux::{
    command::SystemCommandRunner,
    recorder::{PwRecordRecorder, RecordingStatus},
    wav::data_start,
};

/// How long a microphone test listens.
pub(crate) const MICROPHONE_TEST_DURATION: Duration = Duration::from_secs(3);
/// The test's audio file in the runtime directory, which lives in memory.
pub(crate) const MICROPHONE_TEST_FILE: &str = "microphone-test.wav";
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the test reads what the recorder has written.
const READ_INTERVAL: Duration = Duration::from_millis(20);
/// Samples in one level reading: 50 ms at 16 kHz.
const READING_SAMPLES: usize = 800;
/// The level meter's range, in dBFS: 0 at a quiet room, 100 at a raised voice.
const LEVEL_FLOOR_DBFS: f64 = -60.0;
const LEVEL_CEILING_DBFS: f64 = -10.0;
/// A reading at least this loud, about -45 dBFS, sounds like speech.
const HEARD_LEVEL: u8 = 30;
/// Speech lasts at least this many readings (150 ms); a click does not.
const HEARD_READINGS: usize = 3;

/// The microphone through `pw-record`, as a dictation records it. Its audio
/// goes to a file in the runtime directory whose name is removed as soon as
/// recording starts, so nothing it hears outlasts the test, even if the
/// daemon dies during it.
pub(crate) struct Microphone {
    recorder: PwRecordRecorder,
    file: PathBuf,
    /// One test at a time: they share `file`.
    busy: Mutex<()>,
}

impl Microphone {
    /// The microphone recorded by `program`, through `file`.
    pub(crate) fn new(program: impl Into<PathBuf>, file: PathBuf) -> Self {
        Self {
            recorder: PwRecordRecorder::new(SystemCommandRunner, program),
            file,
            busy: Mutex::new(()),
        }
    }

    /// Listens for `duration`, or until the recorder stops, and calls
    /// `report` with the level of each 50 ms heard, from 0 to 100. Stops
    /// early once `report` returns false because no one is watching.
    pub(crate) fn test(
        &self,
        duration: Duration,
        mut report: impl FnMut(u8) -> bool,
    ) -> anyhow::Result<MicrophoneCheck> {
        let mut reading = Vec::with_capacity(READING_SAMPLES);
        let mut loud_readings = 0;
        self.listen(duration, |samples| {
            for &sample in samples {
                reading.push(sample);
                if reading.len() == READING_SAMPLES {
                    let level = level(&reading);
                    reading.clear();
                    if level >= HEARD_LEVEL {
                        loud_readings += 1;
                    }
                    if !report(level) {
                        return false;
                    }
                }
            }
            true
        })?;
        Ok(if loud_readings >= HEARD_READINGS {
            MicrophoneCheck::Heard
        } else {
            MicrophoneCheck::Silent
        })
    }

    /// Hands the 16 kHz mono samples to `samples` as the recorder writes
    /// them, until `duration` passes, the recorder stops, or `samples`
    /// returns false. The recorder is stopped when this returns.
    fn listen(
        &self,
        duration: Duration,
        mut samples: impl FnMut(&[i16]) -> bool,
    ) -> anyhow::Result<()> {
        let _busy = self.busy.lock().unwrap_or_else(PoisonError::into_inner);
        let mut recording = self
            .recorder
            .start(&self.file, Instant::now() + START_TIMEOUT)?;
        let opened = File::open(&self.file);
        // The recorder keeps writing to the file it opened.
        fs::remove_file(&self.file)?;
        let mut audio = opened?;
        // Leaves the file at its first sample.
        data_start(&mut audio)?;
        let deadline = Instant::now() + duration;
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        while Instant::now() < deadline {
            let read = audio.read(&mut buffer)?;
            bytes.extend_from_slice(&buffer[..read]);
            let whole = bytes.len() - bytes.len() % 2;
            if whole > 0 {
                let block: Vec<i16> = bytes
                    .drain(..whole)
                    .as_slice()
                    .chunks_exact(2)
                    .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                if !samples(&block) {
                    break;
                }
            }
            if read == 0 {
                if matches!(recording.status()?, RecordingStatus::Exited { .. }) {
                    break;
                }
                std::thread::sleep(READ_INTERVAL);
            }
        }
        Ok(())
    }
}

/// How loud `samples` are, from 0 for a quiet room to 100 for a raised
/// voice, on a decibel scale like a sound settings meter.
fn level(samples: &[i16]) -> u8 {
    if samples.is_empty() {
        return 0;
    }
    let energy = samples
        .iter()
        .map(|&sample| f64::from(sample).powi(2))
        .sum::<f64>()
        / samples.len() as f64;
    let dbfs = 10.0 * energy.max(1.0).log10() - 20.0 * 32_768_f64.log10();
    let share = (dbfs - LEVEL_FLOOR_DBFS) / (LEVEL_CEILING_DBFS - LEVEL_FLOOR_DBFS);
    (share * 100.0).clamp(0.0, 100.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_meter_reads_a_quiet_room_low_and_a_voice_high() {
        let steady = |amplitude: i16| vec![amplitude; READING_SAMPLES];

        // The transcription check's near-silence: RMS 32, peaks up to 128.
        assert_eq!(level(&steady(32)), 0);
        assert!(level(&steady(128)) < HEARD_LEVEL);
        // Conversational speech, about -26 dBFS.
        assert!(level(&steady(1_600)) > 60);
        assert_eq!(level(&steady(i16::MAX)), 100);
    }
}
