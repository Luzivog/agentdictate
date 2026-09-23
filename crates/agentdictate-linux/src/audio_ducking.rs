use std::{
    ffi::OsString,
    fs,
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use agentdictate_core::Settings;
use serde::{Deserialize, Serialize};

use crate::command::{PlatformCapability, PlatformExecutable, PlatformTool, SystemCommandRunner};

const RAMP_STEP_MS: u32 = 50;
const MAX_RAMP_STEPS: u32 = 100;
/// Ducking is optional: a sound server that hangs must never stall a
/// dictation, so every pactl call gets this long.
const PACTL_TIMEOUT: Duration = Duration::from_secs(1);

pub trait Pactl {
    fn default_sink(&self) -> io::Result<String>;
    fn sink_volume(&self, name: &str) -> io::Result<Vec<u32>>;
    fn set_sink_volume(&self, name: &str, volumes: &[u32]) -> io::Result<()>;
}

/// `pactl` on the user's session, each call bounded by `PACTL_TIMEOUT`.
pub struct SystemPactl {
    executable: PlatformExecutable,
}

impl SystemPactl {
    #[must_use]
    pub fn discover() -> Self {
        Self::at(PlatformExecutable::discover(PlatformTool::Pactl))
    }

    #[must_use]
    pub const fn at(executable: PlatformExecutable) -> Self {
        Self { executable }
    }

    fn output(&self, arguments: &[&str]) -> io::Result<String> {
        let arguments = arguments.iter().map(OsString::from).collect::<Vec<_>>();
        let stdout = SystemCommandRunner
            .run_output(
                PlatformCapability::AudioDucking,
                &self.executable,
                &arguments,
                Instant::now() + PACTL_TIMEOUT,
            )
            .map_err(io::Error::other)?;
        Ok(String::from_utf8_lossy(&stdout).trim().to_owned())
    }
}

impl Pactl for SystemPactl {
    fn default_sink(&self) -> io::Result<String> {
        let name = self.output(&["get-default-sink"])?;
        if name.is_empty() {
            return Err(io::Error::other("no default audio output"));
        }
        Ok(name)
    }

    fn sink_volume(&self, name: &str) -> io::Result<Vec<u32>> {
        parse_volume(&self.output(&["get-sink-volume", name])?)
    }

    fn set_sink_volume(&self, name: &str, volumes: &[u32]) -> io::Result<()> {
        let volumes = volumes.iter().map(u32::to_string).collect::<Vec<_>>();
        let mut arguments = vec!["set-sink-volume", name];
        arguments.extend(volumes.iter().map(String::as_str));
        self.output(&arguments).map(|_| ())
    }
}

fn parse_volume(output: &str) -> io::Result<Vec<u32>> {
    let volumes = output
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("Volume:"))
        .and_then(|line| {
            line.split(',')
                .map(|channel| {
                    channel
                        .split_once(':')?
                        .1
                        .split_whitespace()
                        .next()?
                        .parse()
                        .ok()
                })
                .collect::<Option<Vec<u32>>>()
        })
        .filter(|volumes| !volumes.is_empty());
    volumes.ok_or_else(|| io::Error::other("pactl returned an invalid output volume"))
}

/// The output being ducked: its volume before ducking and the volume
/// AgentDictate last wrote. It is mirrored to disk while the volume is
/// reduced, so a daemon that dies mid-recording can put it back at its next
/// start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SavedOutput {
    #[serde(rename = "sink")]
    name: String,
    #[serde(rename = "original_volume")]
    original: Vec<u32>,
    #[serde(rename = "applied_volume")]
    applied: Vec<u32>,
}

/// Volume control plus the durable record of the ducked output. Fade
/// workers share it with their ducker.
struct VolumeControl<P> {
    pactl: P,
    state_file: PathBuf,
}

impl<P> VolumeControl<P> {
    /// Reads a record left by an earlier run. A corrupt record is removed:
    /// it can restore nothing and would otherwise block ducking forever.
    fn load(&self) -> Option<SavedOutput> {
        let bytes = match fs::read(&self.state_file) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
            Err(error) => {
                tracing::warn!(%error, "could not read the saved audio ducking state");
                return None;
            }
        };
        match serde_json::from_slice(&bytes) {
            Ok(saved) => Some(saved),
            Err(error) => {
                tracing::warn!(%error, "discarding corrupt audio ducking state");
                self.forget();
                None
            }
        }
    }

    /// Replaces the record atomically (temporary file, then rename). The
    /// file is private because sink names identify the user's devices.
    fn persist(&self, saved: &SavedOutput) -> io::Result<()> {
        let parent = self
            .state_file
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let temporary = self.state_file.with_extension("json.tmp");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(saved).map_err(io::Error::other)?)?;
        drop(file);
        fs::rename(&temporary, &self.state_file)
    }

    fn forget(&self) {
        match fs::remove_file(&self.state_file) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(%error, "could not remove the audio ducking state"),
        }
    }
}

/// A running duck or fade. It owns the saved output until it is stopped, so
/// exactly one party writes the volume at a time and no lock is held across
/// a pactl call.
struct RampWorker {
    cancel: mpsc::Sender<()>,
    handle: thread::JoinHandle<Option<SavedOutput>>,
}

/// Duck the default output selected at recording start. App stream volumes are
/// never changed: replacing a tab cannot inherit or compound a ducked baseline.
/// Restore that same output even if the default changes in the meantime.
pub struct PlaybackDucker<P: Pactl + Send + Sync + 'static = SystemPactl> {
    control: Arc<VolumeControl<P>>,
    saved: Option<SavedOutput>,
    fade_in_ms: u32,
    worker: Option<RampWorker>,
}

impl<P: Pactl + Send + Sync + 'static> PlaybackDucker<P> {
    /// Creates the daemon's ducker, recording ducked volumes at `state_file`.
    /// A record left by a daemon that died while ducking is settled now: the
    /// original volume comes back if the output still has the volume
    /// AgentDictate set, and a later user change is kept. If the output
    /// cannot be read yet, the record stays and the next recording retries
    /// it before ducking again.
    pub fn open(pactl: P, state_file: impl Into<PathBuf>) -> Self {
        let control = VolumeControl {
            pactl,
            state_file: state_file.into(),
        };
        let saved = control.load();
        let mut ducker = Self {
            control: Arc::new(control),
            saved,
            fade_in_ms: 0,
            worker: None,
        };
        if let Some(saved) = &ducker.saved {
            tracing::info!(sink = %saved.name, "audio ducking state found from an earlier run");
            ducker.restore_with_fade(0);
        }
        ducker
    }

    /// Starts ducking the default output and returns at once, so the recorder
    /// never waits for the sound server. The worker first retries an output
    /// an earlier restore could not put back, then snapshots, records, and
    /// fades the default output. `restore` cancels it at any point.
    pub fn duck(&mut self, settings: &Settings) {
        self.stop_worker();
        if !settings.audio_ducking_enabled && self.saved.is_none() {
            return;
        }
        let plan = settings.audio_ducking_enabled.then_some(DuckPlan {
            percent: settings.audio_ducking_volume_percent,
            fade_ms: settings.audio_ducking_fade_out_ms,
        });
        if plan.is_some() {
            self.fade_in_ms = settings.audio_ducking_fade_in_ms;
        }
        let previous = self.saved.take();
        let (cancel, cancelled) = mpsc::channel();
        let control = Arc::clone(&self.control);
        let owned = previous.clone();
        match thread::Builder::new()
            .name("agentdictate-audio-ducking".into())
            .spawn(move || duck_output(&control, owned, plan, &cancelled))
        {
            Ok(handle) => self.worker = Some(RampWorker { cancel, handle }),
            Err(error) => {
                tracing::warn!(%error, "audio ducking worker unavailable; ducking inline");
                let (_keep, never_cancelled) = mpsc::channel();
                self.saved = duck_output(&self.control, previous, plan, &never_cancelled);
            }
        }
    }

    pub fn restore(&mut self) {
        self.restore_with_fade(self.fade_in_ms);
    }

    fn restore_with_fade(&mut self, fade_ms: u32) {
        self.stop_worker();
        let Some(output) = &self.saved else { return };
        match still_ducked(&self.control, output) {
            Ok(true) => {}
            Ok(false) => {
                self.saved = None;
                return;
            }
            Err(error) => {
                tracing::warn!(sink = %output.name, %error, "audio ducking restore read failed");
                return;
            }
        }
        let target = output.original.clone();
        self.ramp(target, fade_ms, true);
    }

    /// Cancels a running duck or fade and takes back the output it owned. Waits at
    /// most for one in-flight pactl call, which its deadline bounds.
    fn stop_worker(&mut self) {
        let Some(RampWorker { cancel, handle }) = self.worker.take() else {
            return;
        };
        drop(cancel);
        self.saved = handle.join().unwrap_or_else(|_| {
            tracing::error!("audio ducking fade worker panicked");
            self.control.load()
        });
    }

    /// Restores `saved` to `target`, fading on a worker when `fade_ms` > 0.
    fn ramp(&mut self, target: Vec<u32>, fade_ms: u32, restoring: bool) {
        let Some(saved) = self.saved.take() else {
            return;
        };
        if fade_ms > 0 {
            let (cancel, cancelled) = mpsc::channel();
            let control = Arc::clone(&self.control);
            let owned = saved.clone();
            let owned_target = target.clone();
            match thread::Builder::new()
                .name("agentdictate-audio-ducking".into())
                .spawn(move || {
                    fade(
                        &control,
                        owned,
                        &owned_target,
                        fade_ms,
                        restoring,
                        &cancelled,
                    )
                }) {
                Ok(handle) => {
                    self.worker = Some(RampWorker { cancel, handle });
                    return;
                }
                Err(error) => {
                    tracing::warn!(%error, "audio ducking fade worker unavailable; changing volume at once");
                }
            }
        }
        self.saved = apply_ramp(&self.control, saved, &[target], restoring, |_| true);
    }
}

/// How far and how fast a recording ducks the output.
#[derive(Clone, Copy)]
struct DuckPlan {
    percent: u8,
    fade_ms: u32,
}

/// The ducking worker. Puts back an output an earlier restore left reduced
/// (never snapshotting a reduced volume as a new baseline), then, with a
/// plan, snapshots the default output, records it durably, and fades it
/// down. Returns the output that now needs restoring. Once the owner
/// cancels, it stops before its next volume write.
fn duck_output<P: Pactl>(
    control: &VolumeControl<P>,
    previous: Option<SavedOutput>,
    plan: Option<DuckPlan>,
    cancelled: &mpsc::Receiver<()>,
) -> Option<SavedOutput> {
    if let Some(output) = previous {
        match still_ducked(control, &output) {
            Ok(true) => {
                let original = output.original.clone();
                if let Some(unrestored) = apply_ramp(control, output, &[original], true, |_| true) {
                    return Some(unrestored);
                }
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(sink = %output.name, %error, "audio ducking restore read failed");
                return Some(output);
            }
        }
    }
    let plan = plan?;
    if matches!(cancelled.try_recv(), Err(mpsc::TryRecvError::Disconnected)) {
        return None;
    }
    let pactl = &control.pactl;
    let snapshot = (|| {
        let name = pactl.default_sink()?;
        let original = pactl.sink_volume(&name)?;
        Ok::<_, io::Error>(SavedOutput {
            name,
            applied: original.clone(),
            original,
        })
    })();
    let saved = match snapshot {
        Ok(saved) => saved,
        Err(error) => {
            tracing::warn!(%error, "audio ducking output snapshot failed");
            return None;
        }
    };
    // Without a durable record, a crash could leave the output ducked.
    if let Err(error) = control.persist(&saved) {
        tracing::warn!(%error, "audio ducking skipped: could not save the volume to restore");
        return None;
    }
    let target = ducking_target_volumes(&saved.original, plan.percent);
    tracing::info!(sink = %saved.name, original = ?saved.original, ?target, "audio output ducking started");
    fade(control, saved, &target, plan.fade_ms, false, cancelled)
}

/// Moves `saved` to `target` in `fade_ms` steps until the owner cancels.
fn fade<P: Pactl>(
    control: &VolumeControl<P>,
    saved: SavedOutput,
    target: &[u32],
    fade_ms: u32,
    restoring: bool,
    cancelled: &mpsc::Receiver<()>,
) -> Option<SavedOutput> {
    let steps = ramp_plan(&saved.applied, target, fade_ms);
    let step_delay = Duration::from_millis(u64::from(fade_ms.div_ceil(steps.len() as u32)));
    let started_at = Instant::now();
    apply_ramp(control, saved, &steps, restoring, |step| {
        wait_unless_cancelled(cancelled, started_at + step_delay * step)
    })
}

/// Reads the output's volume. `Ok(false)` means the user changed it since
/// AgentDictate's last write: that is their new preference, so the record is
/// dropped and nothing is restored.
fn still_ducked<P: Pactl>(control: &VolumeControl<P>, output: &SavedOutput) -> io::Result<bool> {
    let current = control.pactl.sink_volume(&output.name)?;
    if current != output.applied {
        tracing::info!(sink = %output.name, ?current, "audio ducking preserved external volume change");
        control.forget();
        return Ok(false);
    }
    Ok(true)
}

impl<P: Pactl + Send + Sync + 'static> Drop for PlaybackDucker<P> {
    fn drop(&mut self) {
        self.restore_with_fade(0);
    }
}

/// Writes `steps` in order, calling `wait` with the 1-based step number
/// before each write, and keeps the durable record in step. Returns the
/// output that still needs restoring, or `None` once a restore has written
/// the original volume back.
fn apply_ramp<P: Pactl>(
    control: &VolumeControl<P>,
    mut saved: SavedOutput,
    steps: &[Vec<u32>],
    restoring: bool,
    mut wait: impl FnMut(u32) -> bool,
) -> Option<SavedOutput> {
    for (step, volumes) in (1..).zip(steps) {
        if !wait(step) {
            return Some(saved);
        }
        if let Err(error) = control.pactl.set_sink_volume(&saved.name, volumes) {
            tracing::warn!(sink = %saved.name, ?volumes, %error, "audio ducking volume write failed");
            return Some(saved);
        }
        saved.applied.clone_from(volumes);
        let restored = restoring && step as usize == steps.len();
        if !restored && let Err(error) = control.persist(&saved) {
            tracing::warn!(%error, "could not save the audio ducking state");
        }
    }
    if !restoring {
        return Some(saved);
    }
    control.forget();
    tracing::info!(sink = %saved.name, volumes = ?saved.applied, "audio output volume restored");
    None
}

/// Sleeps until `deadline`. Returns false as soon as the owner cancels.
fn wait_unless_cancelled(cancelled: &mpsc::Receiver<()>, deadline: Instant) -> bool {
    matches!(
        cancelled.recv_timeout(deadline.saturating_duration_since(Instant::now())),
        Err(mpsc::RecvTimeoutError::Timeout)
    )
}

fn ducking_target_volumes(volumes: &[u32], percent: u8) -> Vec<u32> {
    // PulseAudio maps raw software volume to linear amplitude cubically, so a
    // perceived loudness fraction needs its cube root in raw-volume space.
    let raw_scale = (f64::from(percent.min(100)) / 100.0).cbrt();
    volumes
        .iter()
        .map(|volume| (f64::from(*volume) * raw_scale).round() as u32)
        .collect()
}

fn ramp_plan(start: &[u32], target: &[u32], fade_ms: u32) -> Vec<Vec<u32>> {
    let step_count = if fade_ms == 0 {
        1
    } else {
        fade_ms.div_ceil(RAMP_STEP_MS).min(MAX_RAMP_STEPS) as usize
    };

    (1..=step_count)
        .map(|step| {
            if step == step_count {
                return target.to_vec();
            }
            let progress = step as f64 / step_count as f64;
            start
                .iter()
                .zip(target)
                .map(|(start, target)| {
                    (f64::from(*start) + (f64::from(*target) - f64::from(*start)) * progress)
                        .round() as u32
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Mutex, MutexGuard};

    struct FakeState {
        default: String,
        volumes: HashMap<String, Vec<u32>>,
        writes: Vec<(String, Vec<u32>)>,
        fail_write: Option<Vec<u32>>,
    }

    #[derive(Clone)]
    struct FakePactl(Arc<Mutex<FakeState>>);

    impl FakePactl {
        fn state(&self) -> MutexGuard<'_, FakeState> {
            self.0.lock().unwrap()
        }
    }

    impl Pactl for FakePactl {
        fn default_sink(&self) -> io::Result<String> {
            Ok(self.state().default.clone())
        }

        fn sink_volume(&self, name: &str) -> io::Result<Vec<u32>> {
            self.state()
                .volumes
                .get(name)
                .cloned()
                .ok_or_else(|| io::Error::other("output unavailable"))
        }

        fn set_sink_volume(&self, name: &str, volumes: &[u32]) -> io::Result<()> {
            let mut state = self.state();
            if state.fail_write.as_deref() == Some(volumes) {
                return Err(io::Error::other("volume write failed"));
            }
            let current = state
                .volumes
                .get_mut(name)
                .ok_or_else(|| io::Error::other("output unavailable"))?;
            *current = volumes.to_vec();
            state.writes.push((name.to_owned(), volumes.to_vec()));
            Ok(())
        }
    }

    /// A ducker over a fake output plus the directory holding its durable
    /// record. Bind the directory first so it outlives the ducker.
    fn fixture() -> (tempfile::TempDir, PlaybackDucker<FakePactl>, FakePactl) {
        let directory = tempfile::tempdir().unwrap();
        let pactl = FakePactl(Arc::new(Mutex::new(FakeState {
            default: "headphones".into(),
            volumes: HashMap::from([
                ("headphones".into(), vec![65_536, 32_768]),
                ("speakers".into(), vec![40_000, 40_000]),
            ]),
            writes: Vec::new(),
            fail_write: None,
        })));
        let ducker = PlaybackDucker::open(pactl.clone(), state_file(&directory));
        (directory, ducker, pactl)
    }

    fn state_file(directory: &tempfile::TempDir) -> PathBuf {
        directory.path().join("ducking.json")
    }

    fn settings(fade_out_ms: u32, fade_in_ms: u32) -> Settings {
        Settings {
            audio_ducking_enabled: true,
            audio_ducking_volume_percent: 15,
            audio_ducking_fade_out_ms: fade_out_ms,
            audio_ducking_fade_in_ms: fade_in_ms,
            ..Settings::default()
        }
    }

    /// Lets a running duck or fade finish instead of cancelling it.
    fn finish_fade(ducker: &mut PlaybackDucker<FakePactl>) {
        if let Some(RampWorker { cancel, handle }) = ducker.worker.take() {
            ducker.saved = handle.join().unwrap();
            drop(cancel);
        }
    }

    #[test]
    fn output_volume_parser_preserves_channels_and_rejects_partial_data() {
        assert_eq!(parse_volume("Volume: front-left: 65536 / 100% / 0 dB, front-right: 32768 / 50% / -18 dB\n        balance -0.5").unwrap(), vec![65_536, 32_768]);
        assert_eq!(
            parse_volume("Volume: mono: 0 / 0% / -inf dB").unwrap(),
            vec![0]
        );
        assert!(parse_volume("Volume: front-left: 123 / 1%, front-right: invalid").is_err());
        assert!(parse_volume("").is_err());
    }

    #[test]
    fn repeated_recordings_restore_exact_channel_volumes_with_or_without_fades() {
        for fade_ms in [0, 100] {
            let (_directory, mut ducker, pactl) = fixture();
            for _ in 0..3 {
                ducker.duck(&settings(fade_ms, fade_ms));
                finish_fade(&mut ducker);
                assert_eq!(pactl.state().volumes["headphones"], vec![34_821, 17_411]);
                ducker.restore();
                finish_fade(&mut ducker);
                assert_eq!(pactl.state().volumes["headphones"], vec![65_536, 32_768]);
                assert!(ducker.saved.is_none());
            }
        }
    }

    #[test]
    fn default_output_change_does_not_redirect_restoration() {
        let (_directory, mut ducker, pactl) = fixture();
        ducker.duck(&settings(0, 0));
        finish_fade(&mut ducker);
        pactl.state().default = "speakers".into();
        ducker.restore();
        {
            let state = pactl.state();
            assert_eq!(state.volumes["headphones"], vec![65_536, 32_768]);
            assert_eq!(state.volumes["speakers"], vec![40_000, 40_000]);
            assert!(state.writes.iter().all(|(name, _)| name == "headphones"));
        }
        ducker.duck(&settings(0, 0));
        finish_fade(&mut ducker);
        assert_eq!(pactl.state().volumes["speakers"], vec![21_253, 21_253]);
        drop(ducker);
        assert_eq!(pactl.state().volumes["speakers"], vec![40_000, 40_000]);
    }

    #[test]
    fn failed_restore_never_becomes_a_new_ducking_baseline() {
        let (_directory, mut ducker, pactl) = fixture();
        ducker.duck(&settings(0, 0));
        finish_fade(&mut ducker);
        pactl.state().fail_write = Some(vec![65_536, 32_768]);
        ducker.restore();
        ducker.duck(&settings(0, 0));
        finish_fade(&mut ducker);
        assert_eq!(pactl.state().writes.len(), 1);
        assert_eq!(
            ducker.saved.as_ref().unwrap().original,
            vec![65_536, 32_768]
        );
        pactl.state().fail_write = None;
        ducker.duck(&settings(0, 0));
        finish_fade(&mut ducker);
        ducker.restore();
        assert_eq!(pactl.state().volumes["headphones"], vec![65_536, 32_768]);
    }

    #[test]
    fn disappeared_output_keeps_its_original_without_touching_another_device() {
        let (_directory, mut ducker, pactl) = fixture();
        ducker.duck(&settings(0, 0));
        finish_fade(&mut ducker);
        let disconnected = {
            let mut state = pactl.state();
            state.default = "speakers".into();
            state.volumes.remove("headphones").unwrap()
        };
        ducker.restore();
        ducker.duck(&settings(0, 0));
        finish_fade(&mut ducker);
        assert_eq!(pactl.state().writes.len(), 1);
        pactl
            .state()
            .volumes
            .insert("headphones".into(), disconnected);
        ducker.restore();
        assert_eq!(pactl.state().volumes["headphones"], vec![65_536, 32_768]);
    }

    #[test]
    fn user_volume_change_is_preserved_on_stop() {
        let (_directory, mut ducker, pactl) = fixture();
        ducker.duck(&settings(0, 0));
        finish_fade(&mut ducker);
        pactl
            .state()
            .volumes
            .insert("headphones".into(), vec![20_000, 10_000]);
        ducker.restore();
        assert_eq!(pactl.state().volumes["headphones"], vec![20_000, 10_000]);
        assert!(ducker.saved.is_none());
    }

    #[test]
    fn new_recording_cancels_a_restore_fade_before_it_can_overwrite_ducking() {
        let (_directory, mut ducker, pactl) = fixture();
        ducker.duck(&settings(0, 100));
        finish_fade(&mut ducker);
        ducker.restore();
        ducker.duck(&settings(0, 0));
        finish_fade(&mut ducker);
        thread::sleep(Duration::from_millis(150));
        assert_eq!(pactl.state().volumes["headphones"], vec![34_821, 17_411]);
        drop(ducker);
        assert_eq!(pactl.state().volumes["headphones"], vec![65_536, 32_768]);
    }

    #[test]
    fn stop_during_fade_out_cancels_all_later_ducking_writes() {
        let (_directory, mut ducker, pactl) = fixture();
        ducker.duck(&settings(100, 0));
        // Stop after the first of the fade's two steps.
        while pactl.state().writes.is_empty() {
            thread::sleep(Duration::from_millis(1));
        }
        ducker.restore();
        thread::sleep(Duration::from_millis(150));
        assert_eq!(pactl.state().volumes["headphones"], vec![65_536, 32_768]);
    }

    #[test]
    fn failed_fade_restoration_retains_original_for_retry() {
        let (_directory, mut ducker, pactl) = fixture();
        ducker.duck(&settings(0, 100));
        finish_fade(&mut ducker);
        pactl.state().fail_write = Some(vec![65_536, 32_768]);
        ducker.restore();
        finish_fade(&mut ducker);
        assert!(ducker.saved.is_some());
        pactl.state().fail_write = None;
        drop(ducker);
        assert_eq!(pactl.state().volumes["headphones"], vec![65_536, 32_768]);
    }

    #[test]
    fn disabled_ducking_leaves_output_untouched() {
        let (_directory, mut ducker, pactl) = fixture();
        ducker.duck(&Settings {
            audio_ducking_enabled: false,
            ..settings(0, 0)
        });
        assert!(pactl.state().writes.is_empty());
    }

    #[test]
    fn a_daemon_that_died_while_ducking_restores_the_volume_at_its_next_start() {
        let (directory, mut ducker, pactl) = fixture();
        ducker.duck(&settings(0, 0));
        finish_fade(&mut ducker);
        let record = fs::metadata(state_file(&directory)).unwrap();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&record.permissions()) & 0o777,
            0o600
        );
        // Skipping Drop stands in for SIGKILL: nothing restores in-process.
        std::mem::forget(ducker);
        assert_eq!(pactl.state().volumes["headphones"], vec![34_821, 17_411]);

        let mut ducker = PlaybackDucker::open(pactl.clone(), state_file(&directory));

        assert_eq!(pactl.state().volumes["headphones"], vec![65_536, 32_768]);
        assert!(!state_file(&directory).exists());
        // The next recording ducks from the real volume, not the ducked one.
        ducker.duck(&settings(0, 0));
        finish_fade(&mut ducker);
        assert_eq!(pactl.state().volumes["headphones"], vec![34_821, 17_411]);
        ducker.restore();
        assert_eq!(pactl.state().volumes["headphones"], vec![65_536, 32_768]);
    }

    #[test]
    fn a_volume_changed_after_a_crash_is_kept_and_its_record_dropped() {
        let (directory, mut ducker, pactl) = fixture();
        ducker.duck(&settings(0, 0));
        finish_fade(&mut ducker);
        std::mem::forget(ducker);
        pactl
            .state()
            .volumes
            .insert("headphones".into(), vec![20_000, 10_000]);
        let writes = pactl.state().writes.len();

        let _ducker = PlaybackDucker::open(pactl.clone(), state_file(&directory));

        assert_eq!(pactl.state().volumes["headphones"], vec![20_000, 10_000]);
        assert_eq!(pactl.state().writes.len(), writes);
        assert!(!state_file(&directory).exists());
    }

    #[test]
    fn ramp_is_bounded_monotonic_and_finishes_at_exact_target() {
        for (start, target) in [(vec![100, 50], vec![0, 25]), (vec![0, 25], vec![100, 50])] {
            let ramp = ramp_plan(&start, &target, 200);
            assert_eq!(ramp.len(), 4);
            assert_eq!(ramp.last(), Some(&target));
            for steps in ramp.windows(2) {
                for channel in 0..2 {
                    assert_eq!(
                        steps[1][channel].cmp(&steps[0][channel]),
                        target[channel].cmp(&start[channel])
                    );
                }
            }
        }
        assert_eq!(ramp_plan(&[100], &[0], 0), vec![vec![0]]);
        assert_eq!(
            ramp_plan(&[100], &[0], RAMP_STEP_MS * (MAX_RAMP_STEPS + 1)).len(),
            MAX_RAMP_STEPS as usize
        );
    }
}
