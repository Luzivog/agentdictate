//! Optional live recognition tails the durable WAV; the recording writer never waits on it.
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    net::{TcpStream, ToSocketAddrs},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
        mpsc::{Receiver, channel},
    },
    time::{Duration, Instant},
};

use crate::captured_audio::data_start;

use agentdictate_core::{DictationOptions, JobId};
use agentdictate_runtime::ExternalError;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tungstenite::{Message, client::IntoClientRequest};

const RUNNING: u8 = 0;
const FINISH: u8 = 1;
const CANCEL: u8 = 2;

pub(crate) struct LiveTranscription {
    pub job_id: JobId,
    pub audio_path: PathBuf,
    command: Arc<AtomicU8>,
    result: Receiver<Result<String, ExternalError>>,
}

impl LiveTranscription {
    pub fn start(
        job_id: JobId,
        audio: PathBuf,
        options: DictationOptions,
        key: String,
        url: String,
    ) -> Result<Self, ExternalError> {
        let audio_path = audio.clone();
        let command = Arc::new(AtomicU8::new(RUNNING));
        let control = Arc::clone(&command);
        let (sender, result) = channel();
        std::thread::Builder::new()
            .name("dictation-live-transcription".into())
            .spawn(move || {
                let result = stream_audio(audio, &options, &key, &url, &control).map_err(|_| {
                    ExternalError::new("Live transcription unavailable; using saved audio")
                });
                let _ = sender.send(result);
            })
            .map_err(|_| ExternalError::new("Could not start live transcription"))?;
        Ok(Self {
            job_id,
            audio_path,
            command,
            result,
        })
    }

    pub fn finish(self) -> Result<String, ExternalError> {
        self.command.store(FINISH, Ordering::Release);
        self.result
            .recv_timeout(Duration::from_secs(8))
            .map_err(|_| ExternalError::new("Live transcription timed out; using saved audio"))?
    }
}

impl Drop for LiveTranscription {
    fn drop(&mut self) {
        self.command.store(CANCEL, Ordering::Release);
    }
}

#[derive(Default)]
struct Resampler {
    previous: Option<i16>,
    input_index: u64,
    output_index: u64,
    pending_byte: Option<u8>,
}

impl Resampler {
    // Continuous linear interpolation at the exact 3:2 rate, including across chunk boundaries.
    fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        for &byte in bytes {
            let Some(first) = self.pending_byte.take() else {
                self.pending_byte = Some(byte);
                continue;
            };
            let sample = i16::from_le_bytes([first, byte]);
            while self.output_index * 2 <= self.input_index * 3 {
                let value = if let Some(previous) = self.previous {
                    let weight = (self.output_index * 2 - (self.input_index - 1) * 3) as i32;
                    ((i32::from(previous) * (3 - weight) + i32::from(sample) * weight) / 3) as i16
                } else {
                    sample
                };
                output.extend_from_slice(&value.to_le_bytes());
                self.output_index += 1;
            }
            self.previous = Some(sample);
            self.input_index += 1;
        }
        output
    }
}

fn stream_audio(
    audio: PathBuf,
    options: &DictationOptions,
    key: &str,
    url: &str,
    command: &AtomicU8,
) -> anyhow::Result<String> {
    let mut request = url.into_client_request()?;
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {key}").parse()?);
    let host = request
        .uri()
        .host()
        .ok_or_else(|| anyhow::anyhow!("missing host"))?;
    let port = request
        .uri()
        .port_u16()
        .unwrap_or(if request.uri().scheme_str() == Some("wss") {
            443
        } else {
            80
        });
    let address = (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow::anyhow!("no address"))?;
    let tcp = TcpStream::connect_timeout(&address, Duration::from_secs(3))?;
    tcp.set_read_timeout(Some(Duration::from_secs(3)))?;
    tcp.set_write_timeout(Some(Duration::from_secs(2)))?;
    let (mut socket, _) = tungstenite::client_tls_with_config(request, tcp, None, None)?;
    let tcp = match socket.get_mut() {
        tungstenite::stream::MaybeTlsStream::Plain(tcp) => tcp,
        tungstenite::stream::MaybeTlsStream::Rustls(tls) => &mut tls.sock,
        _ => anyhow::bail!("unsupported transport"),
    };
    tcp.set_read_timeout(Some(Duration::from_millis(40)))?;
    socket.send(Message::text(json!({"type":"session.update","session":{"type":"transcription","audio":{"input":{
        "format":{"type":"audio/pcm","rate":24000}, "turn_detection":null,
        "transcription":{"model":"gpt-live-transcribe","prompt":options.context,"keywords":options.keywords(),"languages":options.languages(),"delay":"medium"}
    }}}}).to_string()))?;
    let mut file = File::open(audio)?;
    let data_offset = data_start(&mut file)?;
    let mut final_end = None;
    let mut resampler = Resampler::default();
    let mut buffer = [0; 3200];
    let started = Instant::now();
    let mut ready = false;
    let mut committed = false;
    let mut commit_id: Option<String> = None;
    let mut completed = std::collections::BTreeMap::<String, String>::new();
    let mut finish_deadline = None;
    loop {
        let control = command.load(Ordering::Acquire);
        anyhow::ensure!(control != CANCEL, "canceled");
        anyhow::ensure!(
            started.elapsed() < Duration::from_secs(3600),
            "session duration exceeded"
        );
        if !ready {
            anyhow::ensure!(
                started.elapsed() < Duration::from_secs(5),
                "session setup timeout"
            );
        }
        if control == FINISH && finish_deadline.is_none() {
            finish_deadline = Some(Instant::now() + Duration::from_secs(6));
        }
        if let Some(deadline) = finish_deadline {
            anyhow::ensure!(Instant::now() < deadline, "final transcript timeout");
        }
        if ready && !committed {
            let mut read_end = final_end;
            if final_end.is_none() {
                let position = file.stream_position()?;
                file.seek(SeekFrom::Start(data_offset - 4))?;
                let mut size = [0; 4];
                file.read_exact(&mut size)?;
                let size = u64::from(u32::from_le_bytes(size));
                let end = data_offset + size;
                if size > 0 && end <= file.metadata()?.len() {
                    read_end = Some(end);
                }
                if control == FINISH {
                    final_end = read_end;
                    anyhow::ensure!(final_end.is_some(), "WAV not finalized");
                }
                file.seek(SeekFrom::Start(position))?;
            }
            let remaining = if let Some(end) = read_end {
                let position = file.stream_position()?;
                anyhow::ensure!(
                    position <= end,
                    "audio was read beyond finalized PCM; use saved audio"
                );
                (end - position).min(buffer.len() as u64) as usize
            } else {
                buffer.len()
            };
            let count = file.read(&mut buffer[..remaining])?;
            if count > 0 {
                let pcm = resampler.push(&buffer[..count]);
                if !pcm.is_empty() {
                    socket.send(Message::text(
                        json!({"type":"input_audio_buffer.append","audio":STANDARD.encode(pcm)})
                            .to_string(),
                    ))?;
                }
            } else if control == FINISH {
                anyhow::ensure!(resampler.pending_byte.is_none(), "incomplete PCM sample");
                socket.send(Message::text(
                    json!({"type":"input_audio_buffer.commit"}).to_string(),
                ))?;
                committed = true;
            }
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                let value: Value = serde_json::from_str(&text)?;
                match value.get("type").and_then(Value::as_str) {
                    Some("session.updated" | "transcription_session.updated") => ready = true,
                    Some("error" | "conversation.item.input_audio_transcription.failed") => {
                        anyhow::bail!("service rejected transcription")
                    }
                    Some("input_audio_buffer.committed") if committed => {
                        commit_id = value
                            .get("item_id")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                    }
                    Some("conversation.item.input_audio_transcription.completed") if committed => {
                        if let (Some(id), Some(text)) = (
                            value.get("item_id").and_then(Value::as_str),
                            value.get("transcript").and_then(Value::as_str),
                        ) {
                            anyhow::ensure!(completed.len() < 8, "unexpected transcript items");
                            completed.insert(id.into(), text.into());
                        }
                    }
                    _ => {}
                }
            }
            Ok(Message::Close(_)) => anyhow::bail!("connection closed"),
            Ok(_) => {
                socket.flush()?;
            }
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error.into()),
        }
        if let Some(text) = commit_id.as_ref().and_then(|id| completed.get(id)) {
            anyhow::ensure!(!text.trim().is_empty(), "empty transcript");
            let _ = socket.close(None);
            return Ok(text.trim().into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn live_stream_sends_before_stop_and_waits_for_matching_final_item() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recording.wav");
        let mut wav = File::create(&path).unwrap();
        let header = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\x3e\0\0\0\x7d\0\0\x02\0\x10\0data\0\0\0\0".to_vec();
        wav.write_all(&header).unwrap();
        wav.write_all(&[0; 3200]).unwrap();
        wav.flush().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sent, received) = channel();
        let server = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            tcp.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut ws = tungstenite::accept(tcp).unwrap();
            let session: Value =
                serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(
                session["session"]["audio"]["input"]["format"]["rate"],
                24000
            );
            assert!(session["session"]["audio"]["input"]["turn_detection"].is_null());
            ws.send(Message::text(json!({"type":"session.updated"}).to_string()))
                .unwrap();
            let mut bytes = 0;
            loop {
                let event: Value =
                    serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
                if event["type"] == "input_audio_buffer.commit" {
                    break;
                }
                assert_eq!(event["type"], "input_audio_buffer.append");
                bytes += STANDARD
                    .decode(event["audio"].as_str().unwrap())
                    .unwrap()
                    .len();
                if bytes < 5000 {
                    sent.send(()).unwrap();
                }
            }
            assert_eq!(bytes, 9598); // 3,200 input samples, continuous 3:2 interpolation.
            for event in [
                json!({"type":"conversation.item.input_audio_transcription.delta","delta":"unstable"}),
                json!({"type":"conversation.item.input_audio_transcription.completed","item_id":"right","transcript":"Final request."}),
                json!({"type":"conversation.item.input_audio_transcription.completed","item_id":"unrelated","transcript":"Wrong request."}),
                json!({"type":"input_audio_buffer.committed","item_id":"right"}),
            ] {
                ws.send(Message::text(event.to_string())).unwrap();
            }
        });
        let options =
            DictationOptions::from_settings(&agentdictate_core::Settings::default(), vec![]);
        let live = LiveTranscription::start(
            JobId::new(),
            path,
            options,
            "test".into(),
            format!("ws://{address}"),
        )
        .unwrap();
        received.recv_timeout(Duration::from_secs(3)).unwrap(); // Audio reached the server before finish.
        wav.write_all(&[0; 3200]).unwrap();
        wav.seek(SeekFrom::Start(40)).unwrap();
        wav.write_all(&6400u32.to_le_bytes()).unwrap();
        wav.seek(SeekFrom::End(0)).unwrap();
        wav.write_all(b"JUNK\x04\0\0\0tail").unwrap();
        wav.flush().unwrap();
        assert_eq!(live.finish().unwrap(), "Final request.");
        server.join().unwrap();
    }
    #[test]
    fn resampling_preserves_chunk_boundaries_and_partial_samples() {
        let samples: Vec<u8> = [0i16, 300, 600, 900, 1200]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect();
        let expected = Resampler::default().push(&samples);
        let mut split = Resampler::default();
        let actual: Vec<_> = samples.chunks(3).flat_map(|b| split.push(b)).collect();
        assert_eq!(actual, expected);
        let values: Vec<_> = actual
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(values, [0, 200, 400, 600, 800, 1000, 1200]);
    }
}
