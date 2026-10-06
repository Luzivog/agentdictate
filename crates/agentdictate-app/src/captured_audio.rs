use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use agentdictate_linux::wav::{BYTES_PER_SECOND, data_start};

/// Only a valid, finalized, near-silent PCM capture may turn an empty ASR result
/// into a harmless stop. Unknown formats and audible clips remain recoverable.
pub(crate) fn is_near_silent(path: &Path) -> bool {
    File::open(path)
        .map_err(anyhow::Error::from)
        .and_then(|mut file| AudioLevels::scan(&mut file))
        .is_ok_and(|levels| levels.is_near_silent())
}

/// The levels of a finalized recording's samples, for the near-silence check
/// and the per-recording log.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AudioLevels {
    samples: u64,
    /// The largest absolute sample, up to 32768.
    peak: u32,
    sum_of_squares: f64,
    /// Samples at full scale, where the microphone or its gain clipped.
    clipped: u64,
}

impl AudioLevels {
    /// Scans the data chunk of a finalized 16 kHz mono PCM16 WAV. A data
    /// chunk whose size was never written fails.
    pub(crate) fn scan(file: &mut File) -> anyhow::Result<Self> {
        let start = data_start(file)?;
        file.seek(SeekFrom::Start(start - 4))?;
        let mut length = [0; 4];
        file.read_exact(&mut length)?;
        let mut remaining = u64::from(u32::from_le_bytes(length));
        anyhow::ensure!(
            remaining > 0 && remaining % 2 == 0 && start + remaining <= file.metadata()?.len(),
            "invalid PCM length"
        );
        let mut levels = Self {
            samples: remaining / 2,
            peak: 0,
            sum_of_squares: 0.0,
            clipped: 0,
        };
        let mut buffer = [0; 8192];
        while remaining > 0 {
            let count = remaining.min(buffer.len() as u64) as usize;
            file.read_exact(&mut buffer[..count])?;
            for pair in buffer[..count].chunks_exact(2) {
                let sample = i16::from_le_bytes([pair[0], pair[1]]);
                levels.peak = levels.peak.max(u32::from(sample.unsigned_abs()));
                levels.sum_of_squares += f64::from(sample).powi(2);
                levels.clipped += u64::from(sample == i16::MAX || sample == i16::MIN);
            }
            remaining -= count as u64;
        }
        Ok(levels)
    }

    /// Peak below roughly -48 dBFS and RMS below roughly -60 dBFS.
    fn is_near_silent(&self) -> bool {
        self.peak <= 128 && self.mean_square() <= 32.0 * 32.0
    }

    fn mean_square(&self) -> f64 {
        self.sum_of_squares / self.samples as f64
    }

    pub(crate) fn seconds(&self) -> f64 {
        (self.samples * 2) as f64 / BYTES_PER_SECOND as f64
    }

    pub(crate) fn peak_dbfs(&self) -> f64 {
        dbfs(f64::from(self.peak))
    }

    pub(crate) fn rms_dbfs(&self) -> f64 {
        dbfs(self.mean_square().sqrt())
    }

    pub(crate) const fn clipped_samples(&self) -> u64 {
        self.clipped
    }

    pub(crate) fn clipped_fraction(&self) -> f64 {
        self.clipped as f64 / self.samples as f64
    }
}

/// An amplitude relative to full scale, to 0.1 dB; digital silence is -inf.
fn dbfs(amplitude: f64) -> f64 {
    (200.0 * (amplitude / 32768.0).log10()).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav(path: &Path, samples: impl IntoIterator<Item = i16>) {
        let samples: Vec<u8> = samples.into_iter().flat_map(i16::to_le_bytes).collect();
        let mut wav =
            b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\x3e\0\0\0\x7d\0\0\x02\0\x10\0data"
                .to_vec();
        wav.extend_from_slice(&(samples.len() as u32).to_le_bytes());
        wav.extend_from_slice(&samples);
        wav.extend_from_slice(b"JUNK\x04\0\0\0loud");
        std::fs::write(path, wav).unwrap();
    }

    #[test]
    fn quiet_pcm_ignores_metadata_but_never_accepts_audible_or_invalid_audio() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.wav");
        for (sample, quiet) in [(0i16, true), (12, true), (2000, false)] {
            write_wav(&path, std::iter::repeat_n(sample, 1600));
            assert_eq!(is_near_silent(&path), quiet);
        }
        std::fs::write(&path, b"broken recording").unwrap();
        assert!(!is_near_silent(&path));
        assert!(!is_near_silent(&dir.path().join("missing.wav")));
    }

    #[test]
    fn levels_report_length_peak_rms_and_clipping_of_the_samples_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.wav");
        // Half a second: a square wave at half scale, then two clipped samples.
        let samples = (0..7998)
            .map(|index| if index % 2 == 0 { 16384 } else { -16384 })
            .chain([i16::MAX, i16::MIN]);
        write_wav(&path, samples);

        let levels = AudioLevels::scan(&mut File::open(&path).unwrap()).unwrap();

        assert_eq!(levels.seconds(), 0.5);
        assert_eq!(levels.peak_dbfs(), 0.0);
        assert_eq!(levels.rms_dbfs(), -6.0);
        assert_eq!(levels.clipped_samples(), 2);
        assert_eq!(levels.clipped_fraction(), 2.0 / 8000.0);
    }
}
