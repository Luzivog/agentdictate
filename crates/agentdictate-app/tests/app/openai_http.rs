use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

use agentdictate_app::{
    CleanupRequest, CleanupTransport, ReqwestOpenAiTransport, SpeechTransport, TranscriptionRequest,
};
use agentdictate_core::TranscriptionProvider;
use tempfile::tempdir;

#[test]
fn cleanup_uses_the_responses_endpoint_and_extracts_output_text() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (request_sender, request_receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        request_sender.send(request).unwrap();
        let body = r#"{"output":[{"content":[{"type":"output_text","text":"Clean result."}]}]}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    let mut transport =
        ReqwestOpenAiTransport::with_api_base("sk-test", format!("http://{address}/v1"));

    let text = transport
        .cleanup_text(CleanupRequest {
            transcript: "raw words",
            model: "gpt-5.4-nano",
            instruction: "Clean lightly",
            reasoning_effort: Some("high"),
        })
        .unwrap();

    let request = request_receiver.recv().unwrap();
    server.join().unwrap();
    assert_eq!(text, "Clean result.");
    assert!(request.starts_with("POST /v1/responses HTTP/1.1"));
    assert!(request.contains("authorization: Bearer sk-test"));
    assert!(request.contains(r#""model":"gpt-5.4-nano""#));
    assert!(request.contains(r#""input":"raw words""#));
    assert!(request.contains(r#""effort":"high""#));
}

#[test]
fn every_explicit_reasoning_effort_is_serialized_to_the_responses_api() {
    for effort in ["none", "minimal", "low", "medium", "high", "xhigh", "max"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (request_sender, request_receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            request_sender.send(request).unwrap();
            let body =
                r#"{"output":[{"content":[{"type":"output_text","text":"Clean result."}]}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let mut transport =
            ReqwestOpenAiTransport::with_api_base("sk-test", format!("http://{address}/v1"));

        transport
            .cleanup_text(CleanupRequest {
                transcript: "raw words",
                model: "gpt-reasoner",
                instruction: "Clean lightly",
                reasoning_effort: Some(effort),
            })
            .unwrap();

        let request = request_receiver.recv().unwrap();
        server.join().unwrap();
        assert!(
            request.contains(&format!(r#""effort":"{effort}""#)),
            "missing reasoning effort {effort} in request"
        );
    }
}

#[test]
fn gpt_transcription_uploads_audio_with_languages_and_context() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (request_sender, request_receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        request_sender.send(request).unwrap();
        let body = r#"{"text":"Every spoken word."}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    let directory = tempdir().unwrap();
    let audio_path = directory.path().join("five minutes.wav");
    std::fs::write(&audio_path, b"RIFFrecorded speech").unwrap();
    let mut transport =
        ReqwestOpenAiTransport::with_api_base("sk-test", format!("http://{address}/v1"));

    let text = transport
        .transcribe_audio(TranscriptionRequest {
            audio_path: &audio_path,
            provider: TranscriptionProvider::OpenAiApi,
            model: "gpt-transcribe",
            language: "en",
            prompt: "AgentDictate and GPUI",
            duration_seconds: 300.0,
        })
        .unwrap();

    let request = request_receiver.recv().unwrap();
    server.join().unwrap();
    assert_eq!(text, "Every spoken word.");
    assert!(request.starts_with("POST /v1/audio/transcriptions HTTP/1.1"));
    assert!(request.contains("authorization: Bearer sk-test"));
    assert!(request.contains("name=\"model\"\r\n\r\ngpt-transcribe"));
    assert!(request.contains("name=\"response_format\"\r\n\r\njson"));
    assert!(request.contains("name=\"languages[]\"\r\n\r\nen"));
    assert!(request.contains("name=\"prompt\"\r\n\r\nAgentDictate and GPUI"));
    assert!(request.contains("filename=\"five minutes.wav\""));
    assert!(request.contains("RIFFrecorded speech"));
}

#[test]
fn openai_gpt_transcription_uses_its_json_response_and_language_profile() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (request_sender, request_receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        request_sender.send(request).unwrap();
        let body = r#"{"text":"Complete result."}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    let directory = tempdir().unwrap();
    let audio_path = directory.path().join("recording.wav");
    std::fs::write(&audio_path, b"RIFFrecorded speech").unwrap();
    let mut transport =
        ReqwestOpenAiTransport::with_api_base("sk-test", format!("http://{address}/v1"));

    let text = transport
        .transcribe_audio(TranscriptionRequest {
            audio_path: &audio_path,
            provider: TranscriptionProvider::OpenAiApi,
            model: "gpt-4o-mini-transcribe",
            language: "fr",
            prompt: "AgentDictate",
            duration_seconds: 2.0,
        })
        .unwrap();

    let request = request_receiver.recv().unwrap();
    server.join().unwrap();
    assert_eq!(text, "Complete result.");
    assert!(request.contains("name=\"response_format\"\r\n\r\njson"));
    assert!(request.contains("name=\"language\"\r\n\r\nfr"));
    assert!(!request.contains("name=\"languages[]\""));
}

#[test]
fn a_future_unverified_transcription_model_is_sent_using_the_safe_standard_profile() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (request_sender, request_receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        request_sender.send(request).unwrap();
        let body = "Complete future-model result.";
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    let directory = tempdir().unwrap();
    let audio_path = directory.path().join("recording.wav");
    std::fs::write(&audio_path, b"RIFFrecorded speech").unwrap();
    let mut transport =
        ReqwestOpenAiTransport::with_api_base("sk-test", format!("http://{address}/v1"));

    let text = transport
        .transcribe_audio(TranscriptionRequest {
            audio_path: &audio_path,
            provider: TranscriptionProvider::OpenAiApi,
            model: "gpt-6-transcribe",
            language: "en",
            prompt: "AgentDictate",
            duration_seconds: 2.0,
        })
        .unwrap();

    let request = request_receiver.recv().unwrap();
    server.join().unwrap();
    assert_eq!(text, "Complete future-model result.");
    assert!(request.contains("name=\"model\"\r\n\r\ngpt-6-transcribe"));
    assert!(request.contains("name=\"response_format\"\r\n\r\ntext"));
    // The user's prompt is forwarded verbatim; no synthetic completeness
    // instructions are injected for unknown models.
    assert!(request.contains("name=\"prompt\"\r\n\r\nAgentDictate"));
    assert!(!request.contains("Transcribe the entire recording"));
}

fn read_http_request(stream: &mut impl Read) -> String {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let read = stream.read(&mut chunk).unwrap();
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(headers_end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers_end = headers_end + 4;
            let headers = String::from_utf8_lossy(&bytes[..headers_end]);
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(str::trim)
                        .and_then(|value| value.parse::<usize>().ok())
                })
                .unwrap_or(0);
            if bytes.len() >= headers_end + length {
                break;
            }
        }
    }
    String::from_utf8(bytes).unwrap()
}
