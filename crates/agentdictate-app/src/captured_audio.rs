use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use agentdictate_linux::wav::data_start;

/// Only a valid, finalized, near-silent PCM capture may turn an empty ASR result
/// into a harmless stop. Unknown formats and audible clips remain recoverable.
pub(crate) fn is_near_silent(path: &Path) -> bool {
    inspect_quiet_pcm(path).unwrap_or(false)
}

fn inspect_quiet_pcm(path: &Path) -> anyhow::Result<bool> {
    let mut file = File::open(path)?;
    let start = data_start(&mut file)?;
    file.seek(SeekFrom::Start(start - 4))?;
    let mut length = [0; 4];
    file.read_exact(&mut length)?;
    let mut remaining = u64::from(u32::from_le_bytes(length));
    anyhow::ensure!(
        remaining > 0 && remaining % 2 == 0 && start + remaining <= file.metadata()?.len(),
        "invalid PCM length"
    );
    let samples = remaining / 2;
    let mut energy = 0f64;
    let mut buffer = [0; 8192];
    while remaining > 0 {
        let count = remaining.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..count])?;
        for pair in buffer[..count].chunks_exact(2) {
            let sample = i32::from(i16::from_le_bytes([pair[0], pair[1]]));
            // Peak below roughly -48 dBFS and RMS below roughly -60 dBFS.
            if sample.abs() > 128 {
                return Ok(false);
            }
            energy += f64::from(sample * sample);
        }
        remaining -= count as u64;
    }
    Ok(energy / samples as f64 <= 32.0 * 32.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quiet_pcm_ignores_metadata_but_never_accepts_audible_or_invalid_audio() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.wav");
        for (sample, quiet) in [(0i16, true), (12, true), (2000, false)] {
            let mut wav = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\x3e\0\0\0\x7d\0\0\x02\0\x10\0data\x80\x0c\0\0".to_vec();
            for _ in 0..1600 {
                wav.extend_from_slice(&sample.to_le_bytes());
            }
            wav.extend_from_slice(b"JUNK\x04\0\0\0loud");
            std::fs::write(&path, wav).unwrap();
            assert_eq!(is_near_silent(&path), quiet);
        }
        std::fs::write(&path, b"broken recording").unwrap();
        assert!(!is_near_silent(&path));
        assert!(!is_near_silent(&dir.path().join("missing.wav")));
    }
}
