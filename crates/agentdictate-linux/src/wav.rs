//! The header of the 16 kHz mono PCM16 WAV files that `pw-record` writes.

use std::io::{self, Read, Seek, SeekFrom};

/// Returns the offset of the first PCM sample. `pw-record` can write chunks
/// besides `fmt ` before `data`, so the offset is found rather than assumed
/// to be 44. A header that is still being written fails with
/// `UnexpectedEof`; anything other than 16 kHz mono PCM16 fails with
/// `InvalidData`.
pub fn data_start(file: &mut (impl Read + Seek)) -> io::Result<u64> {
    file.seek(SeekFrom::Start(0))?;
    let mut header = [0; 12];
    file.read_exact(&mut header)?;
    if &header[..4] != b"RIFF" || &header[8..] != b"WAVE" {
        return Err(invalid("not WAV"));
    }
    let mut format_seen = false;
    loop {
        if file.stream_position()? >= 65_536 {
            return Err(invalid("oversized WAV header"));
        }
        let mut chunk = [0; 8];
        file.read_exact(&mut chunk)?;
        let size = u64::from(u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]));
        if &chunk[..4] == b"data" {
            return if format_seen {
                file.stream_position()
            } else {
                Err(invalid("PCM format missing"))
            };
        }
        let start = file.stream_position()?;
        if &chunk[..4] == b"fmt " {
            read_pcm_format(file, size)?;
            format_seen = true;
        }
        file.seek(SeekFrom::Start(start + size + size % 2))?;
    }
}

fn read_pcm_format(file: &mut impl Read, size: u64) -> io::Result<()> {
    if size < 16 {
        return Err(invalid("short format"));
    }
    let mut format = [0; 16];
    file.read_exact(&mut format)?;
    let field = |at: usize| u16::from_le_bytes([format[at], format[at + 1]]);
    let encoding = field(0);
    let rate = u32::from_le_bytes([format[4], format[5], format[6], format[7]]);
    if encoding != 1 && encoding != 0xFFFE {
        return Err(invalid("not PCM"));
    }
    if field(2) != 1 || rate != 16_000 || field(14) != 16 {
        return Err(invalid("expected mono 16 kHz PCM16"));
    }
    if encoding == 0xFFFE {
        if size < 40 {
            return Err(invalid("short extensible format"));
        }
        let mut extension = [0; 24];
        file.read_exact(&mut extension)?;
        const PCM_SUBFORMAT: [u8; 16] = [1, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113];
        if extension[8..] != PCM_SUBFORMAT {
            return Err(invalid("not extensible PCM"));
        }
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    const FORMAT: &[u8] = b"fmt \x10\0\0\0\x01\0\x01\0\x80\x3e\0\0\0\x7d\0\0\x02\0\x10\0";

    fn wav(chunks: &[&[u8]]) -> Cursor<Vec<u8>> {
        let mut bytes = b"RIFF\0\0\0\0WAVE".to_vec();
        for chunk in chunks {
            bytes.extend_from_slice(chunk);
        }
        Cursor::new(bytes)
    }

    #[test]
    fn samples_start_after_every_chunk_that_precedes_data() {
        assert_eq!(
            data_start(&mut wav(&[FORMAT, b"data\0\0\0\0"])).unwrap(),
            44
        );
        assert_eq!(
            data_start(&mut wav(&[FORMAT, b"LIST\x03\0\0\0abc\0", b"data\0\0\0\0"])).unwrap(),
            56
        );
    }

    #[test]
    fn a_header_still_being_written_is_incomplete_and_other_formats_are_invalid() {
        let partial = &b"data\0\0\0\0"[..5];
        assert_eq!(
            data_start(&mut wav(&[FORMAT, partial])).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        let stereo = b"fmt \x10\0\0\0\x01\0\x02\0\x80\x3e\0\0\0\xfa\0\0\x04\0\x10\0";
        assert_eq!(
            data_start(&mut wav(&[stereo, b"data\0\0\0\0"]))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}
