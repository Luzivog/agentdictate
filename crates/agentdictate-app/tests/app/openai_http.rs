use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc;
use std::thread;

use agentdictate_app::{
    OpenAiKeyCheck, ReqwestOpenAiTransport, SpeechTransport, TranscriptionRequest,
};
use agentdictate_core::{ApiKeyCheck, FailureKind};
use tempfile::tempdir;

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
            keywords: &["AgentDictate".into(), "GPUI".into()],
            audio_path: &audio_path,
            encoding: None,
            model: "gpt-transcribe",
            language: "en,fr",
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
    assert!(request.contains("name=\"languages[]\"\r\n\r\nfr"));
    assert!(request.contains("name=\"keywords[]\"\r\n\r\nAgentDictate"));
    assert!(request.contains("filename=\"recording.wav\""));
    assert!(request.contains("RIFFrecorded speech"));
}

#[test]
fn a_connection_dropped_before_any_status_is_retried_once_on_a_fresh_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut dropped, _) = listener.accept().unwrap();
        read_http_request(&mut dropped);
        drop(dropped);
        let (mut stream, _) = listener.accept().unwrap();
        read_http_request(&mut stream);
        let body = r#"{"text":"Recovered words."}"#;
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
        .transcribe_audio(transcription_request(&audio_path))
        .unwrap();

    server.join().unwrap();
    assert_eq!(text, "Recovered words.");
}

#[test]
fn an_http_error_status_is_never_retried() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_http_request(&mut stream);
        let body = r#"{"error":{"message":"overloaded"}}"#;
        write!(
            stream,
            "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
        listener.set_nonblocking(true).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        listener.accept().is_ok()
    });
    let directory = tempdir().unwrap();
    let audio_path = directory.path().join("recording.wav");
    std::fs::write(&audio_path, b"RIFFrecorded speech").unwrap();
    let mut transport =
        ReqwestOpenAiTransport::with_api_base("sk-test", format!("http://{address}/v1"));

    let error = transport
        .transcribe_audio(transcription_request(&audio_path))
        .unwrap_err();

    assert!(error.to_string().contains("overloaded"), "{error}");
    assert_eq!(error.kind(), FailureKind::ProviderError);
    assert!(
        !server.join().unwrap(),
        "a status error must not be sent again"
    );
}

#[test]
fn compressed_audio_rejected_as_a_bad_file_is_sent_again_as_the_original_wav() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (request_sender, request_receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        request_sender.send(read_http_request(&mut stream)).unwrap();
        let body = r#"{"error":{"message":"Invalid file format. Supported formats: ['mp3', 'wav']","param":"file"}}"#;
        write!(
            stream,
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
        let (mut stream, _) = listener.accept().unwrap();
        request_sender.send(read_http_request(&mut stream)).unwrap();
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
    let audio_path = directory.path().join("recording.wav");
    std::fs::write(&audio_path, b"RIFFrecorded speech").unwrap();
    let encoder = directory.path().join("ffmpeg");
    std::fs::write(&encoder, "#!/bin/sh\nprintf 'WEBM-OPUS'\n").unwrap();
    std::fs::set_permissions(&encoder, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut transport =
        ReqwestOpenAiTransport::with_api_base("sk-test", format!("http://{address}/v1"))
            .with_audio_encoder(&encoder);

    let text = transport
        .transcribe_audio(transcription_request(&audio_path))
        .unwrap();

    server.join().unwrap();
    let compressed = request_receiver.recv().unwrap();
    let original = request_receiver.recv().unwrap();
    assert_eq!(text, "Every spoken word.");
    assert!(compressed.contains("filename=\"recording.webm\""));
    assert!(compressed.contains("Content-Type: audio/webm"));
    assert!(compressed.contains("WEBM-OPUS"));
    assert!(original.contains("filename=\"recording.wav\""));
    assert!(original.contains("Content-Type: audio/wav"));
    assert!(original.contains("RIFFrecorded speech"));
}

/// Each failure reaches the user as the kind of problem it is, so the window
/// can say what to do instead of showing the service's raw error.
#[test]
fn transcription_failures_are_typed_for_the_user() {
    let directory = tempdir().unwrap();
    let audio_path = directory.path().join("recording.wav");
    std::fs::write(&audio_path, b"RIFFrecorded speech").unwrap();
    for (status, body, expected) in [
        (
            "401 Unauthorized",
            r#"{"error":{"message":"Incorrect API key provided"}}"#,
            FailureKind::CredentialRejected,
        ),
        (
            "403 Forbidden",
            r#"{"error":{"message":"Project does not have access"}}"#,
            FailureKind::CredentialRejected,
        ),
        (
            "429 Too Many Requests",
            r#"{"error":{"message":"You exceeded your current quota"}}"#,
            FailureKind::RateLimited,
        ),
        (
            "500 Internal Server Error",
            r#"{"error":{"message":"The server had an error"}}"#,
            FailureKind::ProviderError,
        ),
        (
            "400 Bad Request",
            r#"{"error":{"message":"Audio file is too short"}}"#,
            FailureKind::ProviderError,
        ),
        (
            "200 OK",
            r#"{"unexpected":true}"#,
            FailureKind::ProviderError,
        ),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_http_request(&mut stream);
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let mut transport =
            ReqwestOpenAiTransport::with_api_base("sk-test", format!("http://{address}/v1"));

        let error = transport
            .transcribe_audio(transcription_request(&audio_path))
            .unwrap_err();

        server.join().unwrap();
        assert_eq!(error.kind(), expected, "{status}: {error}");
    }

    let unreachable = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let error =
        ReqwestOpenAiTransport::with_api_base("sk-test", format!("http://{unreachable}/v1"))
            .transcribe_audio(transcription_request(&audio_path))
            .unwrap_err();
    assert_eq!(error.kind(), FailureKind::Offline, "{error}");

    let error = ReqwestOpenAiTransport::with_api_base("", format!("http://{unreachable}/v1"))
        .transcribe_audio(transcription_request(&audio_path))
        .unwrap_err();
    assert_eq!(error.kind(), FailureKind::CredentialMissing, "{error}");
}

/// Answers each request with `status` and hands the request to the test.
fn answer_once(status: &'static str) -> (std::net::SocketAddr, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
        )
        .unwrap();
        request
    });
    (address, server)
}

#[test]
fn an_api_key_check_asks_to_transcribe_nothing_and_reads_a_refusal() {
    for (status, expected) in [
        // Past the key and its permissions, only the audio is missing.
        ("400 Bad Request", ApiKeyCheck::Works),
        ("401 Unauthorized", ApiKeyCheck::Rejected),
        ("403 Forbidden", ApiKeyCheck::Rejected),
        ("503 Service Unavailable", ApiKeyCheck::Unreachable),
    ] {
        let (address, server) = answer_once(status);

        let outcome =
            OpenAiKeyCheck::with_api_base(format!("http://{address}/v1")).check("sk-test");

        let request = server.join().unwrap();
        assert_eq!(outcome, expected, "{status}");
        assert!(
            request.starts_with("POST /v1/audio/transcriptions HTTP/1.1"),
            "{request}"
        );
        assert!(request.contains("authorization: Bearer sk-test"));
        assert!(!request.contains("name=\"file\""));
    }

    let unreachable = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let check = OpenAiKeyCheck::with_api_base(format!("http://{unreachable}/v1"));
    assert_eq!(check.check("sk-test"), ApiKeyCheck::Unreachable);
    // A key that can't be sent in a header is refused without asking.
    assert_eq!(check.check("sk-te\nst"), ApiKeyCheck::Rejected);
}

fn transcription_request(audio_path: &std::path::Path) -> TranscriptionRequest<'_> {
    TranscriptionRequest {
        keywords: &[],
        audio_path,
        encoding: None,
        model: "gpt-transcribe",
        language: "en",
        prompt: "",
        duration_seconds: 1.0,
    }
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
