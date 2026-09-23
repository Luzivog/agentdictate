//! WebM/Opus encoding of recordings for upload.
//!
//! An `OpusEncoder` runs beside each recording: one ffmpeg process fed from
//! the growing WAV, so at stop only the last moments are left to encode. The
//! WAV stays the source of truth. When that encode fails, `encode_file`
//! encodes the saved WAV instead, as it does for every Recovery retry.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use agentdictate_linux::command::{
    AvailabilityDiagnostic, PlatformCapability, PlatformCommandError, PlatformExecutable,
    PlatformTool, SystemCommandRunner,
};
use agentdictate_linux::wav;

/// 32 kbps Opus reduces the 256 kbps PCM payload by roughly 8x before container
/// overhead. Recognition quality still depends on the audio and selected model.
const UPLOAD_OPUS_BITRATE: &str = "32k";

/// The output of every upload encode: 16 kHz mono Opus in WebM, on stdout.
/// `-application voip` keeps libopus in its speech-optimized mode.
const OUTPUT_ARGUMENTS: [&str; 13] = [
    "-ac",
    "1",
    "-ar",
    "16000",
    "-c:a",
    "libopus",
    "-b:a",
    UPLOAD_OPUS_BITRATE,
    "-application",
    "voip",
    "-f",
    "webm",
    "pipe:1",
];

/// The input of an encode during the recording: the WAV's raw samples, which
/// `wav::data_start` checked are 16 kHz mono PCM16, on stdin.
const PCM_INPUT_ARGUMENTS: [&str; 8] = ["-f", "s16le", "-ar", "16000", "-ac", "1", "-i", "pipe:0"];

/// How often a running encoder hands ffmpeg the audio recorded since.
const FEED_INTERVAL: Duration = Duration::from_millis(50);

const TOOL: PlatformTool = PlatformTool::Ffmpeg;

/// Encodes a saved WAV in one ffmpeg run, which is killed at `deadline`.
pub(crate) fn encode_file(
    ffmpeg: &PlatformExecutable,
    audio_path: &Path,
    deadline: Instant,
) -> Result<Vec<u8>, PlatformCommandError> {
    let mut arguments = vec![OsString::from("-loglevel"), "error".into(), "-i".into()];
    arguments.push(audio_path.into());
    arguments.extend(OUTPUT_ARGUMENTS.map(OsString::from));
    let encoded = SystemCommandRunner.run_output(
        PlatformCapability::AudioCompression,
        ffmpeg,
        &arguments,
        deadline,
    )?;
    non_empty(encoded)
}

fn non_empty(encoded: Vec<u8>) -> Result<Vec<u8>, PlatformCommandError> {
    if encoded.is_empty() {
        return Err(PlatformCommandError::UnexpectedOutput {
            tool: TOOL,
            detail: "no audio",
        });
    }
    Ok(encoded)
}

/// One ffmpeg process that encodes a recording while `pw-record` writes it.
/// A feeder thread hands ffmpeg what was appended to the WAV every
/// `FEED_INTERVAL`, and a reader thread collects the WebM. Dropping the
/// encoder, or the `FinishingEncode` it becomes, kills ffmpeg.
#[derive(Debug)]
pub(crate) struct OpusEncoder {
    ffmpeg: Child,
    /// Tells the feeder the WAV is finalized. Dropping it stops the feeder.
    finish: Sender<()>,
    /// How many PCM bytes the feeder handed ffmpeg before closing its input.
    fed: Receiver<io::Result<u64>>,
    /// ffmpeg's stdout and stderr once it closed them, and when it did.
    output: Receiver<(io::Result<Output>, Instant)>,
}

#[derive(Debug)]
struct Output {
    encoded: Vec<u8>,
    errors: Vec<u8>,
}

impl OpusEncoder {
    /// Starts encoding the WAV at `audio_path`, whose header `pw-record` has
    /// already written.
    pub(crate) fn start(
        ffmpeg: &PlatformExecutable,
        audio_path: &Path,
    ) -> Result<Self, PlatformCommandError> {
        let Some(program) = ffmpeg.path() else {
            return Err(PlatformCommandError::Unavailable(AvailabilityDiagnostic {
                capability: PlatformCapability::AudioCompression,
                missing_tools: vec![TOOL],
            }));
        };
        let mut child = Command::new(program)
            .args(["-loglevel", "error"])
            .args(PCM_INPUT_ARGUMENTS)
            .args(OUTPUT_ARGUMENTS)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|source| PlatformCommandError::Start { tool: TOOL, source })?;
        let input = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let (finish, finished) = channel();
        let (fed_sender, fed) = channel();
        let (output_sender, output) = channel();
        let encoder = Self {
            ffmpeg: child,
            finish,
            fed,
            output,
        };
        let audio_path = audio_path.to_path_buf();
        std::thread::Builder::new()
            .name("agentdictate-encoder-feed".into())
            .spawn(move || {
                let _ = fed_sender.send(feed(&audio_path, input, &finished));
            })
            .and_then(|_| {
                std::thread::Builder::new()
                    .name("agentdictate-encoder-output".into())
                    .spawn(move || {
                        let output = read_output(stdout, stderr);
                        let _ = output_sender.send((output, Instant::now()));
                    })
            })
            .map_err(|source| PlatformCommandError::Start { tool: TOOL, source })?;
        Ok(encoder)
    }

    /// Call once `pw-record` has exited: the feeder hands ffmpeg the rest of
    /// the finalized audio and closes its input, so ffmpeg finishes the WebM.
    pub(crate) fn finish(self) -> FinishingEncode {
        let _ = self.finish.send(());
        FinishingEncode {
            encoder: self,
            stopped_at: Instant::now(),
        }
    }
}

impl Drop for OpusEncoder {
    fn drop(&mut self) {
        // Both calls do nothing once `FinishingEncode::wait` reaped ffmpeg.
        let _ = self.ffmpeg.kill();
        let _ = self.ffmpeg.wait();
    }
}

/// A finalized recording's encode, which ffmpeg is finishing. Transcription
/// waits for it; dropping it kills ffmpeg.
#[derive(Debug)]
pub struct FinishingEncode {
    encoder: OpusEncoder,
    stopped_at: Instant,
}

impl FinishingEncode {
    /// Waits until `deadline` for the WebM, and returns it with how long
    /// after the stop it was ready.
    pub(crate) fn wait(
        mut self,
        deadline: Instant,
    ) -> Result<(Vec<u8>, Duration), PlatformCommandError> {
        let remaining = || deadline.saturating_duration_since(Instant::now());
        let (output, closed_at) = self
            .encoder
            .output
            .recv_timeout(remaining())
            .map_err(|_| PlatformCommandError::Deadline { tool: TOOL })?;
        let Output { encoded, errors } = output.map_err(communicate)?;
        let status = self.exit_status(deadline)?;
        if !status.success() {
            return Err(PlatformCommandError::Failed {
                tool: TOOL,
                code: status.code(),
                stderr: String::from_utf8_lossy(&errors).into_owned(),
            });
        }
        self.encoder
            .fed
            .recv_timeout(remaining())
            .map_err(|_| PlatformCommandError::Deadline { tool: TOOL })?
            .map_err(communicate)?;
        Ok((
            non_empty(encoded)?,
            closed_at.saturating_duration_since(self.stopped_at),
        ))
    }

    /// ffmpeg has closed its output, so it is exiting: poll briefly for it.
    fn exit_status(&mut self, deadline: Instant) -> Result<ExitStatus, PlatformCommandError> {
        loop {
            if let Some(status) = self.encoder.ffmpeg.try_wait().map_err(communicate)? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(PlatformCommandError::Deadline { tool: TOOL });
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

fn communicate(source: io::Error) -> PlatformCommandError {
    PlatformCommandError::Communicate { tool: TOOL, source }
}

/// Hands ffmpeg the WAV's samples as `pw-record` appends them. Once told the
/// WAV is finalized, it hands over the rest of the data chunk, closes the
/// input, and returns how many bytes it fed, each exactly once.
fn feed(audio_path: &Path, mut input: ChildStdin, finished: &Receiver<()>) -> io::Result<u64> {
    let mut file = File::open(audio_path)?;
    let data_start = wav::data_start(&mut file)?;
    let mut fed = 0;
    loop {
        fed += io::copy(&mut file, &mut input)?;
        match finished.recv_timeout(FEED_INTERVAL) {
            Ok(()) => break,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "the recording was not transcribed",
                ));
            }
        }
    }
    let remaining = (data_end(&mut file, data_start)? - data_start)
        .checked_sub(fed)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "more audio was fed than the finalized recording holds",
            )
        })?;
    file.seek(SeekFrom::Start(data_start + fed))?;
    let copied = io::copy(&mut file.take(remaining), &mut input)?;
    if copied < remaining {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(fed + copied)
}

/// Where a finalized WAV's samples end: the end of its data chunk, or of the
/// file when the chunk's size was never written.
fn data_end(file: &mut File, data_start: u64) -> io::Result<u64> {
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(data_start - 4))?;
    let mut size = [0; 4];
    file.read_exact(&mut size)?;
    let end = data_start + u64::from(u32::from_le_bytes(size));
    Ok(if end > data_start && end <= length {
        end
    } else {
        length
    })
}

fn read_output(mut stdout: ChildStdout, mut stderr: ChildStderr) -> io::Result<Output> {
    let mut encoded = Vec::new();
    stdout.read_to_end(&mut encoded)?;
    let mut errors = Vec::new();
    stderr.read_to_end(&mut errors)?;
    Ok(Output { encoded, errors })
}

/// Writes a shell script as a fake ffmpeg in `directory`. A child process
/// writes it: a test spawning a process meanwhile could otherwise inherit
/// this process's write handle and make running the fake fail with "Text
/// file busy".
#[cfg(test)]
pub(crate) fn fake_ffmpeg(directory: &Path, script: &str) -> PlatformExecutable {
    let program = directory.join("ffmpeg");
    let written = Command::new("sh")
        .args(["-c", "printf '%s\\n' \"$1\" > \"$0\" && chmod 755 \"$0\""])
        .arg(&program)
        .arg(format!("#!/bin/sh\n{script}"))
        .status()
        .unwrap();
    assert!(written.success());
    PlatformExecutable::at(TOOL, program)
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::path::PathBuf;

    use super::*;

    /// A canonical 16 kHz mono PCM16 header whose data size is `size`.
    fn wav_header(size: u32) -> Vec<u8> {
        let mut header =
            b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\x3e\0\0\0\x7d\0\0\x02\0\x10\0data"
                .to_vec();
        header.extend_from_slice(&size.to_le_bytes());
        header
    }

    fn append(path: &Path, bytes: &[u8]) {
        OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }

    /// Writes the data chunk's final size, as `pw-record` does at stop.
    fn finalize(path: &Path, size: u32) {
        let mut file = OpenOptions::new().write(true).open(path).unwrap();
        file.seek(SeekFrom::Start(40)).unwrap();
        file.write_all(&size.to_le_bytes()).unwrap();
    }

    fn wait_for_size(path: &Path, size: u64) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while std::fs::metadata(path).map_or(0, |metadata| metadata.len()) < size {
            assert!(Instant::now() < deadline, "ffmpeg never received the audio");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn every_sample_is_fed_once_including_audio_written_after_the_stop() {
        let directory = tempfile::tempdir().unwrap();
        let audio_path = directory.path().join("recording.wav");
        let fed_path = directory.path().join("fed");
        let ffmpeg = fake_ffmpeg(
            directory.path(),
            &format!("exec tee '{}'", fed_path.display()),
        );
        let chunks = [vec![1_u8; 3200], vec![2; 1601], vec![3; 3199]];
        std::fs::write(&audio_path, wav_header(0)).unwrap();
        append(&audio_path, &chunks[0]);

        let encoder = OpusEncoder::start(&ffmpeg, &audio_path).unwrap();
        wait_for_size(&fed_path, 3200);
        append(&audio_path, &chunks[1]);
        wait_for_size(&fed_path, 4801);
        append(&audio_path, &chunks[2]);
        finalize(&audio_path, 8000);
        let (encoded, _) = encoder
            .finish()
            .wait(Instant::now() + Duration::from_secs(3))
            .unwrap();

        assert_eq!(encoded, chunks.concat());
    }

    #[test]
    fn a_cancelled_recording_kills_its_encoder() {
        let directory = tempfile::tempdir().unwrap();
        let audio_path = directory.path().join("recording.wav");
        std::fs::write(&audio_path, wav_header(0)).unwrap();
        // Unlike ffmpeg, this fake would outlive the end of its input.
        let ffmpeg = fake_ffmpeg(directory.path(), "exec sleep 30");
        for finish_first in [false, true] {
            let encoder = OpusEncoder::start(&ffmpeg, &audio_path).unwrap();
            let process = PathBuf::from(format!("/proc/{}", encoder.ffmpeg.id()));
            assert!(process.exists());

            if finish_first {
                drop(encoder.finish());
            } else {
                drop(encoder);
            }

            assert!(!process.exists(), "ffmpeg outlived its cancelled recording");
        }
    }

    #[test]
    fn an_encode_during_the_recording_lasts_as_long_as_an_encode_of_the_saved_wav() {
        let ffmpeg = PlatformExecutable::discover(TOOL);
        if ffmpeg.path().is_none() {
            eprintln!("SKIPPED: ffmpeg is not installed");
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let audio_path = directory.path().join("recording.wav");
        // 2.5 s of a 440 Hz tone.
        let samples: Vec<u8> = (0..40_000)
            .flat_map(|index: i32| {
                let phase = f64::from(index) * 440.0 * std::f64::consts::TAU / 16_000.0;
                ((phase.sin() * 8000.0) as i16).to_le_bytes()
            })
            .collect();
        std::fs::write(&audio_path, wav_header(0)).unwrap();
        append(&audio_path, &samples[..32_000]);

        let encoder = OpusEncoder::start(&ffmpeg, &audio_path).unwrap();
        std::thread::sleep(FEED_INTERVAL * 2);
        append(&audio_path, &samples[32_000..]);
        finalize(&audio_path, samples.len() as u32);
        let deadline = Instant::now() + Duration::from_secs(10);
        let (streamed, _) = encoder.finish().wait(deadline).unwrap();
        let at_stop = encode_file(&ffmpeg, &audio_path, deadline).unwrap();

        // Every WebM file starts with the EBML magic number.
        for encoded in [&streamed, &at_stop] {
            assert!(encoded.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]));
        }
        let decoded_bytes = |encoded: &[u8], name: &str| {
            let path = directory.path().join(name);
            std::fs::write(&path, encoded).unwrap();
            let decoded = Command::new(ffmpeg.path().unwrap())
                .args(["-loglevel", "error", "-i"])
                .arg(&path)
                .args(["-f", "s16le", "-ac", "1", "-ar", "16000", "pipe:1"])
                .output()
                .unwrap();
            assert!(decoded.status.success());
            decoded.stdout.len()
        };
        assert_eq!(
            decoded_bytes(&streamed, "streamed.webm"),
            decoded_bytes(&at_stop, "at-stop.webm")
        );
    }
}
