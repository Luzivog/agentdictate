use std::{collections::BTreeMap, io, process::Command};

use agentdictate_core::Settings;

#[derive(Clone, Debug, Eq, PartialEq)]
struct SinkInput {
    id: String,
    volumes: Vec<u32>,
    corked: bool,
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
pub struct PlaybackDucker<P: Pactl = SystemPactl> {
    pactl: P,
    originals: BTreeMap<String, Vec<u32>>,
}

impl Default for PlaybackDucker<SystemPactl> {
    fn default() -> Self {
        Self {
            pactl: SystemPactl,
            originals: BTreeMap::new(),
        }
    }
}

impl<P: Pactl> PlaybackDucker<P> {
    pub fn duck(&mut self, settings: &Settings) {
        if !settings.audio_ducking_enabled {
            return;
        }
        // Clear an incomplete previous attempt before taking a new snapshot.
        self.restore();
        let Ok(output) = self.pactl.list_sink_inputs() else {
            return;
        };
        let ratio = f64::from(settings.audio_ducking_volume_percent.min(100)) / 100.0;
        for stream in parse_sink_inputs(&output)
            .into_iter()
            .filter(|stream| !stream.corked)
        {
            let target = stream
                .volumes
                .iter()
                .map(|volume| (f64::from(*volume) * ratio).round() as u32)
                .collect::<Vec<_>>();
            if self
                .pactl
                .set_sink_input_volume(&stream.id, &target)
                .is_ok()
            {
                self.originals.insert(stream.id, stream.volumes);
            }
        }
    }

    pub fn restore(&mut self) {
        for (id, volumes) in std::mem::take(&mut self.originals) {
            let _ = self.pactl.set_sink_input_volume(&id, &volumes);
        }
    }
}

impl<P: Pactl> Drop for PlaybackDucker<P> {
    fn drop(&mut self) {
        self.restore();
    }
}

fn parse_sink_inputs(output: &str) -> Vec<SinkInput> {
    let mut result = Vec::new();
    let mut current_id: Option<String> = None;
    let mut current_volumes = Vec::new();
    let mut corked = false;

    let flush = |result: &mut Vec<SinkInput>,
                 id: &mut Option<String>,
                 volumes: &mut Vec<u32>,
                 corked: &mut bool| {
        if let Some(id) = id.take()
            && !volumes.is_empty()
        {
            result.push(SinkInput {
                id,
                volumes: std::mem::take(volumes),
                corked: *corked,
            });
        }
        volumes.clear();
        *corked = false;
    };

    for line in output.lines() {
        if let Some(id) = line.strip_prefix("Sink Input #") {
            flush(
                &mut result,
                &mut current_id,
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
        }
    }
    flush(
        &mut result,
        &mut current_id,
        &mut current_volumes,
        &mut corked,
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakePactl {
        listing: String,
        changes: Vec<(String, Vec<u32>)>,
    }

    impl Pactl for FakePactl {
        fn list_sink_inputs(&mut self) -> io::Result<String> {
            Ok(self.listing.clone())
        }

        fn set_sink_input_volume(&mut self, id: &str, volumes: &[u32]) -> io::Result<()> {
            self.changes.push((id.to_owned(), volumes.to_vec()));
            Ok(())
        }
    }

    #[test]
    fn ducks_active_streams_and_restores_exact_channel_volumes() {
        let pactl = FakePactl {
            listing: "Sink Input #7\n\tCorked: no\n\tVolume: left: 40000 / 61%, right: 20000 / 31%\nSink Input #8\n\tCorked: yes\n\tVolume: mono: 65536 / 100%\n".into(),
            changes: Vec::new(),
        };
        let mut ducker = PlaybackDucker {
            pactl,
            originals: BTreeMap::new(),
        };
        let settings = Settings {
            audio_ducking_volume_percent: 25,
            ..Settings::default()
        };

        ducker.duck(&settings);
        assert_eq!(
            ducker.pactl.changes,
            vec![("7".into(), vec![10_000, 5_000])]
        );

        ducker.restore();
        assert_eq!(
            ducker.pactl.changes.last(),
            Some(&("7".into(), vec![40_000, 20_000]))
        );
    }

    #[test]
    fn disabled_ducking_never_touches_pactl() {
        let mut ducker = PlaybackDucker {
            pactl: FakePactl::default(),
            originals: BTreeMap::new(),
        };
        let settings = Settings {
            audio_ducking_enabled: false,
            ..Settings::default()
        };

        ducker.duck(&settings);

        assert!(ducker.pactl.changes.is_empty());
    }
}
