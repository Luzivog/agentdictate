use std::{
    io,
    process::Command,
    sync::{Arc, Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};

use agentdictate_core::Settings;

const RAMP_STEP_MS: u32 = 50;
const MAX_RAMP_STEPS: u32 = 100;

#[derive(Clone, Debug, Eq, PartialEq)]
struct SinkInput {
    id: String,
    restore_key: Option<String>,
    volumes: Vec<u32>,
    corked: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct OriginalVolume {
    id: String,
    restore_key: Option<String>,
    volumes: Vec<u32>,
}

struct VolumeRamp {
    id: String,
    steps: Vec<Vec<u32>>,
}

struct RestoreRamp {
    id: String,
    original: OriginalVolume,
    steps: Vec<Vec<u32>>,
}

struct RestoreWork {
    generation: u64,
    ramps: Vec<RestoreRamp>,
    step_delay: Duration,
}

pub trait Pactl {
    fn list_sink_inputs(&mut self) -> io::Result<String>;
    fn set_sink_input_volume(&mut self, id: &str, volumes: &[u32]) -> io::Result<()>;
}

#[derive(Default)]
pub struct SystemPactl;

impl Pactl for SystemPactl {
    fn list_sink_inputs(&mut self) -> io::Result<String> {
        let output = Command::new("pactl")
            .args(["list", "sink-inputs"])
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other("pactl could not list playback streams"));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn set_sink_input_volume(&mut self, id: &str, volumes: &[u32]) -> io::Result<()> {
        let status = Command::new("pactl")
            .arg("set-sink-input-volume")
            .arg(id)
            .args(volumes.iter().map(u32::to_string))
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other("pactl could not change playback volume"))
        }
    }
}

/// Best-effort playback ducking. Recording never depends on this optional
/// affordance, while every changed stream keeps an exact restoration value.
pub struct PlaybackDucker<P: Pactl + Send + 'static = SystemPactl> {
    inner: Arc<Mutex<Inner<P>>>,
    worker: Option<thread::JoinHandle<()>>,
}

struct Inner<P: Pactl> {
    pactl: P,
    originals: Vec<OriginalVolume>,
    generation: u64,
    fade_in_ms: u32,
}

impl Default for PlaybackDucker<SystemPactl> {
    fn default() -> Self {
        Self::with_pactl(SystemPactl)
    }
}

impl<P: Pactl + Send + 'static> PlaybackDucker<P> {
    fn with_pactl(pactl: P) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                pactl,
                originals: Vec::new(),
                generation: 0,
                fade_in_ms: 0,
            })),
            worker: None,
        }
    }

    pub fn duck(&mut self, settings: &Settings) {
        if !settings.audio_ducking_enabled {
            return;
        }
        // Clear an incomplete previous attempt before taking a new snapshot.
        self.restore_immediately();

        let (generation, ramps, step_delay) = {
            let mut inner = lock_unpoisoned(&self.inner);
            let Ok(output) = inner.pactl.list_sink_inputs() else {
                return;
            };
            let streams = parse_sink_inputs(&output)
                .into_iter()
                .filter(|stream| !stream.corked)
                .collect::<Vec<_>>();
            if streams.is_empty() {
                return;
            }

            inner.generation = inner.generation.wrapping_add(1);
            let generation = inner.generation;
            inner.fade_in_ms = settings.audio_ducking_fade_in_ms;
            let pending_count = inner.originals.len();
            let mut adopted_pending = vec![false; pending_count];
            inner.originals.reserve(streams.len());
            let mut ramps = Vec::with_capacity(streams.len());
            for stream in streams {
                let pending_index = (0..pending_count)
                    .find(|index| {
                        !adopted_pending[*index] && inner.originals[*index].id == stream.id
                    })
                    .or_else(|| {
                        stream.restore_key.as_ref().and_then(|restore_key| {
                            (0..pending_count).find(|index| {
                                !adopted_pending[*index]
                                    && inner.originals[*index].restore_key.as_ref()
                                        == Some(restore_key)
                            })
                        })
                    });
                let target = if let Some(pending_index) = pending_index {
                    adopted_pending[pending_index] = true;
                    let original = &mut inner.originals[pending_index];
                    tracing::info!(
                        previous_sink_input_id = %original.id,
                        sink_input_id = %stream.id,
                        "audio ducking adopted pending restore"
                    );
                    original.id.clone_from(&stream.id);
                    ducking_target_volumes(&original.volumes, settings.audio_ducking_volume_percent)
                } else {
                    let target = ducking_target_volumes(
                        &stream.volumes,
                        settings.audio_ducking_volume_percent,
                    );
                    inner.originals.push(OriginalVolume {
                        id: stream.id.clone(),
                        restore_key: stream.restore_key.clone(),
                        volumes: stream.volumes.clone(),
                    });
                    target
                };
                ramps.push(VolumeRamp {
                    id: stream.id.clone(),
                    steps: ramp_plan(&stream.volumes, &target, settings.audio_ducking_fade_out_ms),
                });
            }

            if settings.audio_ducking_fade_out_ms == 0 {
                for ramp in ramps {
                    let _ = inner.pactl.set_sink_input_volume(&ramp.id, &ramp.steps[0]);
                }
                return;
            }

            let step_count = ramps[0].steps.len() as u32;
            let step_delay = Duration::from_millis(u64::from(
                settings.audio_ducking_fade_out_ms.div_ceil(step_count),
            ));
            (generation, ramps, step_delay)
        };

        let inner = Arc::clone(&self.inner);
        if let Ok(worker) = thread::Builder::new()
            .name("agentdictate-audio-ducking".into())
            .spawn(move || run_ramp(inner, generation, ramps, step_delay))
        {
            self.worker = Some(worker);
        }
    }

    pub fn restore(&mut self) {
        let work = {
            let mut inner = lock_unpoisoned(&self.inner);
            let fade_in_ms = inner.fade_in_ms;
            prepare_restore(&mut inner, fade_in_ms)
        };
        // Dropping the handle detaches a sleeping worker. The generation was
        // already bumped under the lock, so it exits before its next write.
        self.worker = None;
        let Some(work) = work else {
            return;
        };

        let inner = Arc::clone(&self.inner);
        if let Ok(worker) = thread::Builder::new()
            .name("agentdictate-audio-ducking".into())
            .spawn(move || run_restore_ramp(inner, work))
        {
            self.worker = Some(worker);
        }
    }

    fn restore_immediately(&mut self) {
        prepare_restore(&mut lock_unpoisoned(&self.inner), 0);
        self.worker = None;
    }
}

impl<P: Pactl + Send + 'static> Drop for PlaybackDucker<P> {
    fn drop(&mut self) {
        self.restore_immediately();
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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

fn remaining_until_ramp_step(
    started_at: Instant,
    now: Instant,
    step_delay: Duration,
    step_number: u32,
) -> Duration {
    (started_at + step_delay * step_number).saturating_duration_since(now)
}

fn wait_for_ramp_step(started_at: Instant, step_delay: Duration, step_number: u32) {
    let remaining = remaining_until_ramp_step(started_at, Instant::now(), step_delay, step_number);
    if !remaining.is_zero() {
        thread::sleep(remaining);
    }
}

fn run_ramp<P: Pactl + Send + 'static>(
    inner: Arc<Mutex<Inner<P>>>,
    generation: u64,
    ramps: Vec<VolumeRamp>,
    step_delay: Duration,
) {
    let started_at = Instant::now();
    for step in 0..ramps[0].steps.len() {
        wait_for_ramp_step(started_at, step_delay, step as u32 + 1);
        let mut inner = lock_unpoisoned(&inner);
        if inner.generation != generation {
            return;
        }
        for ramp in &ramps {
            let _ = inner
                .pactl
                .set_sink_input_volume(&ramp.id, &ramp.steps[step]);
        }
    }
}

fn run_restore_ramp<P: Pactl + Send + 'static>(inner: Arc<Mutex<Inner<P>>>, work: RestoreWork) {
    let started_at = Instant::now();
    for step in 0..work.ramps[0].steps.len() {
        wait_for_ramp_step(started_at, work.step_delay, step as u32 + 1);
        let mut inner = lock_unpoisoned(&inner);
        if inner.generation != work.generation {
            return;
        }

        let final_step = step + 1 == work.ramps[0].steps.len();
        for ramp in &work.ramps {
            let result = inner
                .pactl
                .set_sink_input_volume(&ramp.id, &ramp.steps[step]);
            if !final_step {
                continue;
            }

            match result {
                Ok(()) => {
                    if let Some(index) = inner
                        .originals
                        .iter()
                        .position(|original| original == &ramp.original)
                    {
                        inner.originals.remove(index);
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        sink_input_id = %ramp.id,
                        %error,
                        "audio ducking restore volume write failed"
                    );
                }
            }
        }
    }
}

fn prepare_restore<P: Pactl>(inner: &mut Inner<P>, fade_in_ms: u32) -> Option<RestoreWork> {
    inner.generation = inner.generation.wrapping_add(1);
    let generation = inner.generation;
    if inner.originals.is_empty() {
        return None;
    }

    let output = match inner.pactl.list_sink_inputs() {
        Ok(output) => output,
        Err(error) => {
            tracing::warn!(%error, "audio ducking restore listing failed");
            return None;
        }
    };
    let live_streams = parse_sink_inputs(&output);
    let mut retained = Vec::new();
    let mut ramps = Vec::new();
    for original in std::mem::take(&mut inner.originals) {
        let target = live_streams
            .iter()
            .find(|stream| stream.id == original.id)
            .map(|stream| (stream, false))
            .or_else(|| {
                original.restore_key.as_ref().and_then(|restore_key| {
                    live_streams
                        .iter()
                        .find(|stream| stream.restore_key.as_ref() == Some(restore_key))
                        .map(|stream| (stream, true))
                })
            });

        let Some((target, rematched)) = target else {
            retained.push(original);
            continue;
        };
        if rematched {
            tracing::info!(
                previous_sink_input_id = %original.id,
                sink_input_id = %target.id,
                "audio ducking restore rematched sink input"
            );
        }
        if fade_in_ms == 0 {
            if let Err(error) = inner
                .pactl
                .set_sink_input_volume(&target.id, &original.volumes)
            {
                tracing::warn!(
                    sink_input_id = %target.id,
                    %error,
                    "audio ducking restore volume write failed"
                );
                retained.push(original);
            }
        } else {
            ramps.push(RestoreRamp {
                id: target.id.clone(),
                original: original.clone(),
                steps: ramp_plan(&target.volumes, &original.volumes, fade_in_ms),
            });
            retained.push(original);
        }
    }
    inner.originals = retained;

    if ramps.is_empty() {
        return None;
    }
    let step_count = ramps[0].steps.len() as u32;
    Some(RestoreWork {
        generation,
        ramps,
        step_delay: Duration::from_millis(u64::from(fade_in_ms.div_ceil(step_count))),
    })
}

fn parse_sink_inputs(output: &str) -> Vec<SinkInput> {
    let mut result = Vec::new();
    let mut current_id: Option<String> = None;
    let mut current_restore_key: Option<String> = None;
    let mut current_volumes = Vec::new();
    let mut corked = false;

    let flush = |result: &mut Vec<SinkInput>,
                 id: &mut Option<String>,
                 restore_key: &mut Option<String>,
                 volumes: &mut Vec<u32>,
                 corked: &mut bool| {
        if let Some(id) = id.take()
            && !volumes.is_empty()
        {
            result.push(SinkInput {
                id,
                restore_key: restore_key.take(),
                volumes: std::mem::take(volumes),
                corked: *corked,
            });
        }
        *restore_key = None;
        volumes.clear();
        *corked = false;
    };

    for line in output.lines() {
        if let Some(id) = line.strip_prefix("Sink Input #") {
            flush(
                &mut result,
                &mut current_id,
                &mut current_restore_key,
                &mut current_volumes,
                &mut corked,
            );
            current_id = Some(id.trim().to_owned());
            continue;
        }
        let line = line.trim();
        if let Some(value) = line.strip_prefix("Corked:") {
            corked = value.trim().eq_ignore_ascii_case("yes");
        } else if let Some(value) = line.strip_prefix("Volume:")
            && current_volumes.is_empty()
        {
            current_volumes = value
                .split(',')
                .filter_map(|channel| {
                    let (_, value) = channel.split_once(':')?;
                    value
                        .trim()
                        .split_once(' ')
                        .and_then(|(raw, _)| raw.parse().ok())
                })
                .collect();
        } else if let Some(value) = line.strip_prefix("module-stream-restore.id =") {
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .unwrap_or(value);
            if !value.is_empty() {
                current_restore_key = Some(value.to_owned());
            }
        }
    }
    flush(
        &mut result,
        &mut current_id,
        &mut current_restore_key,
        &mut current_volumes,
        &mut corked,
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakePactlState {
        listing: String,
        list_calls: usize,
        attempts: Vec<(String, Vec<u32>)>,
        changes: Vec<(String, Vec<u32>)>,
        fail_listing: bool,
        fail_sets: bool,
        fail_volumes: Option<Vec<u32>>,
    }

    struct FakePactl {
        state: Arc<Mutex<FakePactlState>>,
    }

    impl Pactl for FakePactl {
        fn list_sink_inputs(&mut self) -> io::Result<String> {
            let mut state = lock_unpoisoned(&self.state);
            state.list_calls += 1;
            if state.fail_listing {
                Err(io::Error::other("fake listing failure"))
            } else {
                Ok(state.listing.clone())
            }
        }

        fn set_sink_input_volume(&mut self, id: &str, volumes: &[u32]) -> io::Result<()> {
            let mut state = lock_unpoisoned(&self.state);
            let change = (id.to_owned(), volumes.to_vec());
            state.attempts.push(change.clone());
            if state.fail_sets || state.fail_volumes.as_deref() == Some(volumes) {
                Err(io::Error::other("fake volume failure"))
            } else {
                state.changes.push(change);
                Ok(())
            }
        }
    }

    fn fake_ducker(listing: &str) -> (PlaybackDucker<FakePactl>, Arc<Mutex<FakePactlState>>) {
        let state = Arc::new(Mutex::new(FakePactlState {
            listing: listing.to_owned(),
            ..FakePactlState::default()
        }));
        let ducker = PlaybackDucker::with_pactl(FakePactl {
            state: Arc::clone(&state),
        });
        (ducker, state)
    }

    fn immediate_settings(percent: u8) -> Settings {
        Settings {
            audio_ducking_volume_percent: percent,
            audio_ducking_fade_out_ms: 0,
            audio_ducking_fade_in_ms: 0,
            ..Settings::default()
        }
    }

    fn settings_with_fades(percent: u8, fade_out_ms: u32, fade_in_ms: u32) -> Settings {
        Settings {
            audio_ducking_volume_percent: percent,
            audio_ducking_fade_out_ms: fade_out_ms,
            audio_ducking_fade_in_ms: fade_in_ms,
            ..Settings::default()
        }
    }

    const STREAM_7_WITH_KEY: &str = concat!(
        "Sink Input #7\n",
        "\tCorked: no\n",
        "\tVolume: mono: 65536 / 100%\n",
        "\tProperties:\n",
        "\t\tmodule-stream-restore.id = \"K\"\n",
    );

    const STREAM_9_WITH_KEY: &str = concat!(
        "Sink Input #9\n",
        "\tCorked: no\n",
        "\tVolume: mono: 34821 / 53%\n",
        "\tProperties:\n",
        "\t\tmodule-stream-restore.id = \"K\"\n",
    );

    const DUCKED_STREAM_7_WITH_KEY: &str = concat!(
        "Sink Input #7\n",
        "\tCorked: no\n",
        "\tVolume: mono: 34821 / 53%\n",
        "\tProperties:\n",
        "\t\tmodule-stream-restore.id = \"K\"\n",
    );

    const STREAM_11_WITH_OTHER_KEY: &str = concat!(
        "Sink Input #11\n",
        "\tCorked: no\n",
        "\tVolume: mono: 65536 / 100%\n",
        "\tProperties:\n",
        "\t\tmodule-stream-restore.id = \"OTHER\"\n",
    );

    #[test]
    fn ducking_percent_scales_perceived_loudness() {
        let (mut ducker, state) = fake_ducker(STREAM_7_WITH_KEY);

        ducker.duck(&immediate_settings(15));

        assert_eq!(
            lock_unpoisoned(&state).changes,
            vec![("7".into(), vec![34_821])]
        );
    }

    #[test]
    fn zero_fade_writes_each_active_stream_once_at_the_target() {
        let listing = concat!(
            "Sink Input #7\n",
            "\tCorked: no\n",
            "\tVolume: left: 40000 / 61%, right: 20000 / 31%\n",
            "Sink Input #8\n",
            "\tCorked: no\n",
            "\tVolume: mono: 65536 / 100%\n",
            "Sink Input #9\n",
            "\tCorked: yes\n",
            "\tVolume: mono: 65536 / 100%\n",
        );
        let (mut ducker, state) = fake_ducker(listing);

        ducker.duck(&immediate_settings(0));

        let state = lock_unpoisoned(&state);
        let expected = vec![("7".into(), vec![0, 0]), ("8".into(), vec![0])];
        assert_eq!(state.attempts, expected);
        assert_eq!(state.changes, expected);
    }

    #[test]
    fn ramp_plan_is_stepped_monotonic_and_ends_exactly_at_the_target() {
        let plan = ramp_plan(&[1_000, 400], &[0, 200], 200);

        assert_eq!(plan.len(), 4);
        assert!(plan.windows(2).all(|steps| {
            steps[1]
                .iter()
                .zip(&steps[0])
                .all(|(next, previous)| next <= previous)
        }));
        assert_eq!(plan.last(), Some(&vec![0, 200]));
        assert_eq!(
            ramp_plan(&[100], &[0], RAMP_STEP_MS * (MAX_RAMP_STEPS + 1)).len(),
            MAX_RAMP_STEPS as usize
        );

        let restore_plan = ramp_plan(&[0, 200], &[1_000, 400], 200);
        assert!(restore_plan.windows(2).all(|steps| {
            steps[1]
                .iter()
                .zip(&steps[0])
                .all(|(next, previous)| next >= previous)
        }));
        assert_eq!(restore_plan.last(), Some(&vec![1_000, 400]));
        assert_eq!(
            ramp_plan(&[200], &[1_000, 400], 200).last(),
            Some(&vec![1_000, 400])
        );
    }

    #[test]
    fn ramp_step_wait_uses_absolute_deadlines_and_skips_sleep_when_behind() {
        let started_at = Instant::now();
        let step_delay = Duration::from_millis(50);

        assert_eq!(
            remaining_until_ramp_step(started_at, started_at, step_delay, 1),
            Duration::from_millis(50)
        );
        assert_eq!(
            remaining_until_ramp_step(
                started_at,
                started_at + Duration::from_millis(80),
                step_delay,
                2,
            ),
            Duration::from_millis(20)
        );
        assert_eq!(
            remaining_until_ramp_step(
                started_at,
                started_at + Duration::from_millis(120),
                step_delay,
                2,
            ),
            Duration::ZERO
        );
    }

    #[test]
    fn zero_fade_in_restores_synchronously() {
        let (mut ducker, state) = fake_ducker(STREAM_7_WITH_KEY);
        ducker.duck(&immediate_settings(15));
        lock_unpoisoned(&state).listing = DUCKED_STREAM_7_WITH_KEY.to_owned();

        ducker.restore();

        assert!(ducker.worker.is_none());
        assert!(lock_unpoisoned(&ducker.inner).originals.is_empty());
        assert_eq!(
            lock_unpoisoned(&state).changes.last(),
            Some(&("7".into(), vec![65_536]))
        );
    }

    #[test]
    fn fade_in_starts_at_the_listed_volume_and_clears_on_exact_final_write() {
        let (mut ducker, state) = fake_ducker(STREAM_7_WITH_KEY);
        ducker.duck(&settings_with_fades(15, 0, 100));
        {
            let mut state = lock_unpoisoned(&state);
            state.listing = DUCKED_STREAM_7_WITH_KEY.to_owned();
            state.fail_volumes = Some(vec![50_179]);
        }

        ducker.restore();
        ducker.worker.take().unwrap().join().unwrap();

        let state = lock_unpoisoned(&state);
        assert_eq!(
            state.attempts,
            vec![
                ("7".into(), vec![34_821]),
                ("7".into(), vec![50_179]),
                ("7".into(), vec![65_536]),
            ]
        );
        assert_eq!(
            state.changes,
            vec![("7".into(), vec![34_821]), ("7".into(), vec![65_536]),]
        );
        assert!(lock_unpoisoned(&ducker.inner).originals.is_empty());
    }

    #[test]
    fn failed_final_fade_in_write_retains_the_original() {
        let (mut ducker, state) = fake_ducker(STREAM_7_WITH_KEY);
        ducker.duck(&settings_with_fades(15, 0, 100));
        {
            let mut state = lock_unpoisoned(&state);
            state.listing = DUCKED_STREAM_7_WITH_KEY.to_owned();
            state.fail_volumes = Some(vec![65_536]);
        }

        ducker.restore();
        ducker.worker.take().unwrap().join().unwrap();

        assert_eq!(
            lock_unpoisoned(&state).attempts,
            vec![
                ("7".into(), vec![34_821]),
                ("7".into(), vec![50_179]),
                ("7".into(), vec![65_536]),
            ]
        );
        assert_eq!(lock_unpoisoned(&ducker.inner).originals.len(), 1);
    }

    #[test]
    fn duck_preamble_restores_immediately_even_with_a_nonzero_fade_in() {
        let settings = settings_with_fades(15, 0, 600);
        let (mut ducker, state) = fake_ducker(STREAM_7_WITH_KEY);
        ducker.duck(&settings);

        ducker.duck(&settings);

        let state = lock_unpoisoned(&state);
        assert_eq!(state.list_calls, 3);
        assert_eq!(
            state.attempts,
            vec![
                ("7".into(), vec![34_821]),
                ("7".into(), vec![65_536]),
                ("7".into(), vec![34_821]),
            ]
        );
        assert!(ducker.worker.is_none());
    }

    #[test]
    fn restore_rematches_a_recreated_stream_by_restore_key() {
        let (mut ducker, state) = fake_ducker(STREAM_7_WITH_KEY);
        ducker.duck(&immediate_settings(15));
        lock_unpoisoned(&state).listing = STREAM_9_WITH_KEY.to_owned();

        ducker.restore();

        assert_eq!(
            lock_unpoisoned(&state).changes.last(),
            Some(&("9".into(), vec![65_536]))
        );
        assert!(lock_unpoisoned(&ducker.inner).originals.is_empty());
    }

    #[test]
    fn failed_live_restore_is_adopted_without_compounding_the_duck_target() {
        let (mut ducker, state) = fake_ducker(STREAM_7_WITH_KEY);
        ducker.duck(&immediate_settings(15));
        {
            let mut state = lock_unpoisoned(&state);
            state.listing = STREAM_9_WITH_KEY.to_owned();
            state.fail_volumes = Some(vec![65_536]);
        }

        ducker.restore();
        ducker.duck(&immediate_settings(15));

        assert_eq!(
            lock_unpoisoned(&state).attempts,
            vec![
                ("7".into(), vec![34_821]),
                ("9".into(), vec![65_536]),
                ("9".into(), vec![65_536]),
                ("9".into(), vec![34_821]),
            ]
        );
        assert_eq!(
            lock_unpoisoned(&ducker.inner).originals,
            vec![OriginalVolume {
                id: "9".into(),
                restore_key: Some("K".into()),
                volumes: vec![65_536],
            }]
        );
    }

    #[test]
    fn failed_restore_listing_keeps_all_originals_for_retry() {
        let (mut ducker, state) = fake_ducker(STREAM_7_WITH_KEY);
        ducker.duck(&immediate_settings(15));
        lock_unpoisoned(&state).fail_listing = true;

        ducker.restore();

        assert_eq!(lock_unpoisoned(&ducker.inner).originals.len(), 1);
        lock_unpoisoned(&state).fail_listing = false;

        ducker.restore();

        assert!(lock_unpoisoned(&ducker.inner).originals.is_empty());
    }

    #[test]
    fn unmatched_pending_restore_does_not_block_ducking_another_stream() {
        let (mut ducker, state) = fake_ducker(STREAM_7_WITH_KEY);
        ducker.duck(&immediate_settings(15));
        lock_unpoisoned(&state).listing.clear();

        ducker.restore();

        assert_eq!(lock_unpoisoned(&ducker.inner).originals.len(), 1);
        assert_eq!(lock_unpoisoned(&state).attempts.len(), 1);
        lock_unpoisoned(&state).listing = STREAM_11_WITH_OTHER_KEY.to_owned();

        ducker.duck(&immediate_settings(15));

        assert_eq!(
            lock_unpoisoned(&state).changes.last(),
            Some(&("11".into(), vec![34_821]))
        );
        assert_eq!(
            lock_unpoisoned(&ducker.inner).originals,
            vec![
                OriginalVolume {
                    id: "7".into(),
                    restore_key: Some("K".into()),
                    volumes: vec![65_536],
                },
                OriginalVolume {
                    id: "11".into(),
                    restore_key: Some("OTHER".into()),
                    volumes: vec![65_536],
                },
            ]
        );
    }

    #[test]
    fn disabled_ducking_never_touches_pactl() {
        let (mut ducker, state) = fake_ducker(STREAM_7_WITH_KEY);
        let settings = Settings {
            audio_ducking_enabled: false,
            ..Settings::default()
        };

        ducker.duck(&settings);

        let state = lock_unpoisoned(&state);
        assert_eq!(state.list_calls, 0);
        assert!(state.attempts.is_empty());
    }
}
