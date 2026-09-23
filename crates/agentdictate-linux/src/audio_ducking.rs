use std::{
    ffi::OsString,
    io,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use agentdictate_core::Settings;

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

#[derive(Clone)]
struct SavedOutput {
    name: String,
    original: Vec<u32>,
    applied: Vec<u32>,
}

/// A running fade. It owns the saved output until it is stopped, so exactly
/// one party writes the volume at a time and no lock is held across a
/// pactl call.
struct RampWorker {
    cancel: mpsc::Sender<()>,
    handle: thread::JoinHandle<Option<SavedOutput>>,
}

/// Duck the default output selected at recording start. App stream volumes are
/// never changed: replacing a tab cannot inherit or compound a ducked baseline.
/// Restore that same output even if the default changes in the meantime.
pub struct PlaybackDucker<P: Pactl + Send + Sync + 'static = SystemPactl> {
    pactl: Arc<P>,
    saved: Option<SavedOutput>,
    fade_in_ms: u32,
    worker: Option<RampWorker>,
}

impl<P: Pactl + Send + Sync + 'static> PlaybackDucker<P> {
    pub fn new(pactl: P) -> Self {
        Self {
            pactl: Arc::new(pactl),
            saved: None,
            fade_in_ms: 0,
            worker: None,
        }
    }

    pub fn duck(&mut self, settings: &Settings) {
        self.restore_with_fade(0);
        // A failed restore keeps its original. Never snapshot a reduced volume
        // as the baseline for another recording, including on a different output.
        if !settings.audio_ducking_enabled || self.saved.is_some() {
            return;
        }
        let snapshot = (|| {
            let name = self.pactl.default_sink()?;
            let original = self.pactl.sink_volume(&name)?;
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
                return;
            }
        };
        let target = ducking_target_volumes(&saved.original, settings.audio_ducking_volume_percent);
        tracing::info!(sink = %saved.name, original = ?saved.original, ?target, "audio output ducking started");
        self.fade_in_ms = settings.audio_ducking_fade_in_ms;
        self.saved = Some(saved);
        self.ramp(target, settings.audio_ducking_fade_out_ms, false);
    }

    pub fn restore(&mut self) {
        self.restore_with_fade(self.fade_in_ms);
    }

    fn restore_with_fade(&mut self, fade_ms: u32) {
        self.stop_worker();
        let Some(output) = &self.saved else { return };
        match self.pactl.sink_volume(&output.name) {
            Ok(current) if current != output.applied => {
                // A volume key or mixer change is the user's new preference.
                tracing::info!(sink = %output.name, ?current, "audio ducking preserved external volume change");
                self.saved = None;
                return;
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(sink = %output.name, %error, "audio ducking restore read failed");
                return;
            }
        }
        let target = output.original.clone();
        self.ramp(target, fade_ms, true);
    }

    /// Cancels a running fade and takes back the output it owned. Waits at
    /// most for one in-flight pactl call, which its deadline bounds.
    fn stop_worker(&mut self) {
        let Some(RampWorker { cancel, handle }) = self.worker.take() else {
            return;
        };
        drop(cancel);
        match handle.join() {
            Ok(saved) => self.saved = saved,
            Err(_) => tracing::error!("audio ducking fade worker panicked"),
        }
    }

    // One ramp implementation owns writes in both directions.
    fn ramp(&mut self, target: Vec<u32>, fade_ms: u32, restoring: bool) {
        let Some(saved) = self.saved.take() else {
            return;
        };
        if fade_ms > 0 {
            let steps = ramp_plan(&saved.applied, &target, fade_ms);
            let step_delay = Duration::from_millis(u64::from(fade_ms.div_ceil(steps.len() as u32)));
            let (cancel, cancelled) = mpsc::channel();
            let pactl = Arc::clone(&self.pactl);
            let owned = saved.clone();
            match thread::Builder::new()
                .name("agentdictate-audio-ducking".into())
                .spawn(move || {
                    let started_at = Instant::now();
                    apply_ramp(&*pactl, owned, &steps, restoring, |step| {
                        wait_unless_cancelled(&cancelled, started_at + step_delay * step)
                    })
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
        self.saved = apply_ramp(&*self.pactl, saved, &[target], restoring, |_| true);
    }
}

impl Default for PlaybackDucker<SystemPactl> {
    fn default() -> Self {
        Self::new(SystemPactl::discover())
    }
}

impl<P: Pactl + Send + Sync + 'static> Drop for PlaybackDucker<P> {
    fn drop(&mut self) {
        self.restore_with_fade(0);
    }
}

/// Writes `steps` in order, calling `wait` with the 1-based step number
/// before each write. Returns the output that still needs restoring, or
/// `None` once a restore has written the original volume back.
fn apply_ramp<P: Pactl>(
    pactl: &P,
    mut saved: SavedOutput,
    steps: &[Vec<u32>],
    restoring: bool,
    mut wait: impl FnMut(u32) -> bool,
) -> Option<SavedOutput> {
    for (step, volumes) in (1..).zip(steps) {
        if !wait(step) {
            return Some(saved);
        }
        if let Err(error) = pactl.set_sink_volume(&saved.name, volumes) {
            tracing::warn!(sink = %saved.name, ?volumes, %error, "audio ducking volume write failed");
            return Some(saved);
        }
        saved.applied.clone_from(volumes);
    }
    if !restoring {
        return Some(saved);
    }
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

    fn fixture() -> (PlaybackDucker<FakePactl>, FakePactl) {
        let pactl = FakePactl(Arc::new(Mutex::new(FakeState {
            default: "headphones".into(),
            volumes: HashMap::from([
                ("headphones".into(), vec![65_536, 32_768]),
                ("speakers".into(), vec![40_000, 40_000]),
            ]),
            writes: Vec::new(),
            fail_write: None,
        })));
        (PlaybackDucker::new(pactl.clone()), pactl)
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

    /// Lets a running fade finish instead of cancelling it.
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
            let (mut ducker, pactl) = fixture();
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
        let (mut ducker, pactl) = fixture();
        ducker.duck(&settings(0, 0));
        pactl.state().default = "speakers".into();
        ducker.restore();
        {
            let state = pactl.state();
            assert_eq!(state.volumes["headphones"], vec![65_536, 32_768]);
            assert_eq!(state.volumes["speakers"], vec![40_000, 40_000]);
            assert!(state.writes.iter().all(|(name, _)| name == "headphones"));
        }
        ducker.duck(&settings(0, 0));
        assert_eq!(pactl.state().volumes["speakers"], vec![21_253, 21_253]);
        drop(ducker);
        assert_eq!(pactl.state().volumes["speakers"], vec![40_000, 40_000]);
    }

    #[test]
    fn failed_restore_never_becomes_a_new_ducking_baseline() {
        let (mut ducker, pactl) = fixture();
        ducker.duck(&settings(0, 0));
        pactl.state().fail_write = Some(vec![65_536, 32_768]);
        ducker.restore();
        ducker.duck(&settings(0, 0));
        assert_eq!(pactl.state().writes.len(), 1);
        assert_eq!(
            ducker.saved.as_ref().unwrap().original,
            vec![65_536, 32_768]
        );
        pactl.state().fail_write = None;
        ducker.duck(&settings(0, 0));
        ducker.restore();
        assert_eq!(pactl.state().volumes["headphones"], vec![65_536, 32_768]);
    }

    #[test]
    fn disappeared_output_keeps_its_original_without_touching_another_device() {
        let (mut ducker, pactl) = fixture();
        ducker.duck(&settings(0, 0));
        let disconnected = {
            let mut state = pactl.state();
            state.default = "speakers".into();
            state.volumes.remove("headphones").unwrap()
        };
        ducker.restore();
        ducker.duck(&settings(0, 0));
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
        let (mut ducker, pactl) = fixture();
        ducker.duck(&settings(0, 0));
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
        let (mut ducker, pactl) = fixture();
        ducker.duck(&settings(0, 100));
        ducker.restore();
        ducker.duck(&settings(0, 0));
        thread::sleep(Duration::from_millis(150));
        assert_eq!(pactl.state().volumes["headphones"], vec![34_821, 17_411]);
        drop(ducker);
        assert_eq!(pactl.state().volumes["headphones"], vec![65_536, 32_768]);
    }

    #[test]
    fn stop_during_fade_out_cancels_all_later_ducking_writes() {
        let (mut ducker, pactl) = fixture();
        ducker.duck(&settings(100, 0));
        ducker.restore();
        thread::sleep(Duration::from_millis(150));
        assert_eq!(pactl.state().volumes["headphones"], vec![65_536, 32_768]);
    }

    #[test]
    fn failed_fade_restoration_retains_original_for_retry() {
        let (mut ducker, pactl) = fixture();
        ducker.duck(&settings(0, 100));
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
        let (mut ducker, pactl) = fixture();
        ducker.duck(&Settings {
            audio_ducking_enabled: false,
            ..settings(0, 0)
        });
        assert!(pactl.state().writes.is_empty());
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
