use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use agentdictate_runtime::ExternalError;
use base64::Engine as _;
use reqwest::StatusCode;
use serde_json::{Value, json};
use thiserror::Error;
use uuid::Uuid;

use crate::openai::{SpeechTransport, TranscriptionRequest};

const CHATGPT_TRANSCRIPTION_ENDPOINT: &str = "https://chatgpt.com/backend-api/transcribe";
const CODEX_DESKTOP_ORIGINATOR: &str = "Codex Desktop";
const CODEX_DESKTOP_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36";
const APP_SERVER_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_TIMEOUT: Duration = Duration::from_secs(180);

struct ChatGptAccessToken(String);

impl ChatGptAccessToken {
    fn expose(&self) -> &str {
        &self.0
    }
}

struct ChatGptAccountId(String);

impl ChatGptAccountId {
    fn expose(&self) -> &str {
        &self.0
    }
}

struct ChatGptAuth {
    access_token: ChatGptAccessToken,
    account_id: ChatGptAccountId,
}

trait ChatGptAuthProvider: Send {
    fn load(&mut self, refresh: bool) -> Result<ChatGptAuth, CodexSubscriptionError>;
}

struct CodexAppServerAuthProvider {
    codex_binary: OsString,
}

impl CodexAppServerAuthProvider {
    fn discover() -> Self {
        let binary = std::env::var_os("CODEX_BINARY")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join(".local/bin/codex"))
                    .filter(|path| path.is_file())
            })
            .or_else(|| {
                let bundled = PathBuf::from("/usr/lib/chatgpt/resources/codex");
                bundled.is_file().then_some(bundled)
            })
            .map_or_else(|| OsString::from("codex"), PathBuf::into_os_string);
        Self {
            codex_binary: binary,
        }
    }
}

impl ChatGptAuthProvider for CodexAppServerAuthProvider {
    fn load(&mut self, refresh: bool) -> Result<ChatGptAuth, CodexSubscriptionError> {
        load_auth_from_app_server(&self.codex_binary, refresh)
    }
}

#[derive(Debug, Error)]
enum CodexSubscriptionError {
    #[error("Codex could not be started. Install Codex and sign in with ChatGPT first.")]
    AppServerUnavailable,
    #[error("Codex did not respond while checking the ChatGPT sign-in.")]
    AppServerTimeout,
    #[error("Codex returned an invalid authentication response.")]
    InvalidAuthResponse,
    #[error("Codex is not signed in with ChatGPT.")]
    ChatGptSignInRequired,
    #[error("The ChatGPT sign-in does not include an account identifier.")]
    MissingAccountId,
    #[error("Could not read the captured recording: {0}")]
    AudioRead(std::io::Error),
    #[error("Could not reach ChatGPT transcription: {0}")]
    Transport(reqwest::Error),
    #[error("ChatGPT sign-in expired. Open Codex and sign in again.")]
    Unauthorized,
    #[error("ChatGPT subscription transcription is unavailable for this account.")]
    NotEntitled,
    #[error("ChatGPT transcription limit reached. Try again later.")]
    RateLimited,
    #[error("ChatGPT transcription is temporarily unavailable.")]
    ServiceUnavailable,
    #[error("ChatGPT transcription rejected the recording.")]
    RequestRejected,
    #[error("ChatGPT returned an invalid transcription response.")]
    InvalidResponse,
    #[error("No speech detected.")]
    EmptyTranscript,
}

fn reqwest_error_kind(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_body() {
        "body"
    } else if error.is_decode() {
        "decode"
    } else if error.is_redirect() {
        "redirect"
    } else if error.is_builder() {
        "request_builder"
    } else if error.is_request() {
        "request"
    } else {
        "transport"
    }
}

fn subscription_error_kind(error: &CodexSubscriptionError) -> &'static str {
    match error {
        CodexSubscriptionError::Transport(error) => reqwest_error_kind(error),
        _ => "non_transport",
    }
}

pub struct CodexSubscriptionTransport {
    client: reqwest::blocking::Client,
    endpoint: String,
    auth_provider: Box<dyn ChatGptAuthProvider>,
}

impl CodexSubscriptionTransport {
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: reqwest::blocking::Client::builder()
                .connect_timeout(Duration::from_secs(20))
                .timeout(HTTP_TIMEOUT)
                .build()
                .expect("the rustls HTTP client must be constructible"),
            endpoint: CHATGPT_TRANSCRIPTION_ENDPOINT.to_owned(),
            auth_provider: Box::new(CodexAppServerAuthProvider::discover()),
        }
    }

    #[cfg(test)]
    fn with_endpoint_and_auth(
        endpoint: impl Into<String>,
        auth_provider: impl ChatGptAuthProvider + 'static,
    ) -> Self {
        Self {
            client: reqwest::blocking::Client::builder()
                .connect_timeout(Duration::from_secs(2))
                .timeout(Duration::from_secs(5))
                .build()
                .expect("the test HTTP client must be constructible"),
            endpoint: endpoint.into(),
            auth_provider: Box::new(auth_provider),
        }
    }

    fn send(
        &self,
        auth: &ChatGptAuth,
        body: Vec<u8>,
        boundary: &str,
    ) -> Result<reqwest::blocking::Response, CodexSubscriptionError> {
        self.client
            .post(&self.endpoint)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", auth.access_token.expose()),
            )
            .header("ChatGPT-Account-Id", auth.account_id.expose())
            .header("originator", CODEX_DESKTOP_ORIGINATOR)
            .header(reqwest::header::USER_AGENT, CODEX_DESKTOP_USER_AGENT)
            .header(
                reqwest::header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .map_err(CodexSubscriptionError::Transport)
    }

    fn transcribe(
        &mut self,
        request: &TranscriptionRequest<'_>,
    ) -> Result<String, CodexSubscriptionError> {
        let audio = std::fs::read(request.audio_path).map_err(CodexSubscriptionError::AudioRead)?;
        let request_started_at = Instant::now();
        let boundary = format!("----codex-transcribe-{}", Uuid::new_v4());
        let body = multipart_body(&boundary, &audio, request.language);

        let mut auth = self.auth_provider.load(false)?;
        let mut response = self.send(&auth, body, &boundary).inspect_err(|error| {
            tracing::warn!(
                attempt = "initial",
                error_kind = subscription_error_kind(error),
                audio_bytes = audio.len(),
                recording_duration_seconds = request.duration_seconds,
                elapsed_millis = request_started_at.elapsed().as_millis(),
                "ChatGPT transcription request failed before receiving a response"
            );
        })?;
        let initial_status = response.status();
        let mut auth_refreshed = false;
        if matches!(
            initial_status,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ) {
            auth_refreshed = true;
            auth = self.auth_provider.load(true)?;
            response = self
                .send(
                    &auth,
                    multipart_body(&boundary, &audio, request.language),
                    &boundary,
                )
                .inspect_err(|error| {
                    tracing::warn!(
                        attempt = "after_auth_refresh",
                        error_kind = subscription_error_kind(error),
                        initial_http_status = initial_status.as_u16(),
                        audio_bytes = audio.len(),
                        recording_duration_seconds = request.duration_seconds,
                        elapsed_millis = request_started_at.elapsed().as_millis(),
                        "ChatGPT transcription request failed before receiving a response"
                    );
                })?;
        }
        let status = response.status();
        let payload = response
            .text()
            .inspect_err(|error| {
                tracing::warn!(
                    attempt = "response_body",
                    error_kind = reqwest_error_kind(error),
                    http_status = status.as_u16(),
                    audio_bytes = audio.len(),
                    recording_duration_seconds = request.duration_seconds,
                    elapsed_millis = request_started_at.elapsed().as_millis(),
                    "ChatGPT transcription response body could not be read"
                );
            })
            .map_err(CodexSubscriptionError::Transport)?;
        tracing::info!(
            initial_http_status = initial_status.as_u16(),
            final_http_status = status.as_u16(),
            http_success = status.is_success(),
            auth_refreshed,
            audio_bytes = audio.len(),
            recording_duration_seconds = request.duration_seconds,
            elapsed_millis = request_started_at.elapsed().as_millis(),
            "ChatGPT transcription response received"
        );
        if !status.is_success() {
            return Err(match status {
                StatusCode::UNAUTHORIZED => CodexSubscriptionError::Unauthorized,
                StatusCode::FORBIDDEN => CodexSubscriptionError::NotEntitled,
                StatusCode::TOO_MANY_REQUESTS => CodexSubscriptionError::RateLimited,
                StatusCode::REQUEST_TIMEOUT
                | StatusCode::INTERNAL_SERVER_ERROR
                | StatusCode::BAD_GATEWAY
                | StatusCode::SERVICE_UNAVAILABLE
                | StatusCode::GATEWAY_TIMEOUT => CodexSubscriptionError::ServiceUnavailable,
                _ => CodexSubscriptionError::RequestRejected,
            });
        }
        let text = serde_json::from_str::<Value>(&payload)
            .ok()
            .and_then(|payload| {
                payload
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .ok_or(CodexSubscriptionError::InvalidResponse)?;
        if text.trim().is_empty() {
            return Err(CodexSubscriptionError::EmptyTranscript);
        }
        Ok(text.trim().to_owned())
    }
}

impl Default for CodexSubscriptionTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl SpeechTransport for CodexSubscriptionTransport {
    fn transcribe_audio(
        &mut self,
        request: TranscriptionRequest<'_>,
    ) -> Result<String, ExternalError> {
        if request.language.contains(',') {
            return Err(ExternalError::new(
                "ChatGPT subscription accepts one language hint; choose one language or automatic detection",
            ));
        }
        self.transcribe(&request).map_err(|error| match error {
            CodexSubscriptionError::EmptyTranscript => ExternalError::NoSpeech,
            error => ExternalError::new(error.to_string()),
        })
    }
}

fn multipart_body(boundary: &str, audio: &[u8], language: &str) -> Vec<u8> {
    let mut body = Vec::with_capacity(audio.len() + 320);
    write!(
        body,
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"codex.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
    )
    .expect("writing to a Vec cannot fail");
    body.extend_from_slice(audio);
    let language = language.trim();
    if language.is_empty() {
        write!(body, "\r\n--{boundary}--\r\n").expect("writing to a Vec cannot fail");
    } else {
        write!(
            body,
            "\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"language\"\r\n\r\n{language}\r\n--{boundary}--\r\n"
        )
        .expect("writing to a Vec cannot fail");
    }
    body
}

fn load_auth_from_app_server(
    codex_binary: &OsString,
    refresh: bool,
) -> Result<ChatGptAuth, CodexSubscriptionError> {
    let mut command = Command::new(codex_binary);
    command
        .args(["app-server", "--stdio"])
        .env_remove("OPENAI_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|_| CodexSubscriptionError::AppServerUnavailable)?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or(CodexSubscriptionError::AppServerUnavailable)?;
    let stdout = child
        .stdout
        .take()
        .ok_or(CodexSubscriptionError::AppServerUnavailable)?;
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Ok(message) = serde_json::from_str::<Value>(&line)
                && sender.send(message).is_err()
            {
                break;
            }
        }
    });

    let result = (|| {
        send_app_server_request(
            &mut stdin,
            1,
            "initialize",
            json!({
                "clientInfo": {
                    "name": "agentdictate",
                    "title": "AgentDictate",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )?;
        receive_app_server_response(&receiver, 1)?;
        writeln!(stdin, "{}", json!({"method": "initialized", "params": {}}))
            .map_err(|_| CodexSubscriptionError::AppServerUnavailable)?;
        send_app_server_request(
            &mut stdin,
            2,
            "getAuthStatus",
            json!({"includeToken": true, "refreshToken": refresh}),
        )?;
        let result = receive_app_server_response(&receiver, 2)?;
        parse_chatgpt_auth(&result)
    })();

    drop(stdin);
    stop_child(&mut child);
    let _ = reader.join();
    result
}

fn send_app_server_request(
    stdin: &mut impl Write,
    id: u64,
    method: &str,
    params: Value,
) -> Result<(), CodexSubscriptionError> {
    writeln!(
        stdin,
        "{}",
        json!({"id": id, "method": method, "params": params})
    )
    .map_err(|_| CodexSubscriptionError::AppServerUnavailable)
}

fn receive_app_server_response(
    receiver: &Receiver<Value>,
    expected_id: u64,
) -> Result<Value, CodexSubscriptionError> {
    let deadline = Instant::now() + APP_SERVER_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(CodexSubscriptionError::AppServerTimeout);
        }
        let message = receiver
            .recv_timeout(remaining)
            .map_err(|_| CodexSubscriptionError::AppServerTimeout)?;
        if message.get("id").and_then(Value::as_u64) != Some(expected_id) {
            continue;
        }
        if message.get("error").is_some() {
            return Err(CodexSubscriptionError::InvalidAuthResponse);
        }
        return message
            .get("result")
            .cloned()
            .ok_or(CodexSubscriptionError::InvalidAuthResponse);
    }
}

fn parse_chatgpt_auth(result: &Value) -> Result<ChatGptAuth, CodexSubscriptionError> {
    if result.get("authMethod").and_then(Value::as_str) != Some("chatgpt") {
        return Err(CodexSubscriptionError::ChatGptSignInRequired);
    }
    let token = result
        .get("authToken")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or(CodexSubscriptionError::ChatGptSignInRequired)?;
    let claims = parse_jwt_claims(token)?;
    let account_id = claims
        .get("https://api.openai.com/auth")
        .and_then(Value::as_object)
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .filter(|account_id| !account_id.is_empty())
        .ok_or(CodexSubscriptionError::MissingAccountId)?;
    Ok(ChatGptAuth {
        access_token: ChatGptAccessToken(token.to_owned()),
        account_id: ChatGptAccountId(account_id.to_owned()),
    })
}

fn parse_jwt_claims(token: &str) -> Result<Value, CodexSubscriptionError> {
    let payload = token
        .split('.')
        .nth(1)
        .ok_or(CodexSubscriptionError::InvalidAuthResponse)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| CodexSubscriptionError::InvalidAuthResponse)?;
    serde_json::from_slice(&decoded).map_err(|_| CodexSubscriptionError::InvalidAuthResponse)
}

fn stop_child(child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use agentdictate_core::TranscriptionProvider;
    use base64::Engine as _;
    use tempfile::tempdir;

    use super::*;

    struct FixedAuthProvider {
        calls: Arc<Mutex<Vec<bool>>>,
        responses: VecDeque<ChatGptAuth>,
    }

    impl ChatGptAuthProvider for FixedAuthProvider {
        fn load(&mut self, refresh: bool) -> Result<ChatGptAuth, CodexSubscriptionError> {
            self.calls.lock().unwrap().push(refresh);
            self.responses
                .pop_front()
                .ok_or(CodexSubscriptionError::ChatGptSignInRequired)
        }
    }

    fn auth(token: &str) -> ChatGptAuth {
        ChatGptAuth {
            access_token: ChatGptAccessToken(token.to_owned()),
            account_id: ChatGptAccountId("account-test".to_owned()),
        }
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
            let headers_end = bytes
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|position| position + 4);
            if let Some(headers_end) = headers_end {
                let headers = String::from_utf8_lossy(&bytes[..headers_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|value| value.parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if bytes.len() >= headers_end + content_length {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    #[test]
    fn wav_request_matches_the_captured_chatgpt_contract() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            let body = r#"{"text":"Front, center."}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
            request
        });
        let calls = Arc::new(Mutex::new(Vec::new()));
        let provider = FixedAuthProvider {
            calls: calls.clone(),
            responses: VecDeque::from([auth("plan-token")]),
        };
        let directory = tempdir().unwrap();
        let audio_path = directory.path().join("recording.wav");
        std::fs::write(&audio_path, b"RIFFrecorded speech").unwrap();
        let mut transport = CodexSubscriptionTransport::with_endpoint_and_auth(
            format!("http://{address}/backend-api/transcribe"),
            provider,
        );

        let text = transport
            .transcribe_audio(TranscriptionRequest {
                keywords: &[],
                audio_path: &audio_path,
                provider: TranscriptionProvider::ChatGptSubscription,
                model: "gpt-transcribe",
                language: "en",
                prompt: "ignored by subscription transcription",
                duration_seconds: 1.4,
            })
            .unwrap();

        let request = server.join().unwrap();
        assert_eq!(text, "Front, center.");
        assert!(request.starts_with("POST /backend-api/transcribe HTTP/1.1"));
        assert!(request.contains("authorization: Bearer plan-token"));
        assert!(request.contains("chatgpt-account-id: account-test"));
        assert!(request.contains("originator: Codex Desktop"));
        assert!(request.contains("name=\"file\"; filename=\"codex.wav\""));
        assert!(request.contains("Content-Type: audio/wav"));
        assert!(request.contains("RIFFrecorded speech"));
        assert!(request.contains("name=\"language\"\r\n\r\nen"));
        assert_eq!(*calls.lock().unwrap(), vec![false]);
    }

    #[test]
    fn unauthorized_request_refreshes_once_without_api_fallback() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, body) in [
                ("401 Unauthorized", r#"{"detail":"expired"}"#),
                ("200 OK", r#"{"text":"Recovered."}"#),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                requests.push(read_http_request(&mut stream));
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
            requests
        });
        let calls = Arc::new(Mutex::new(Vec::new()));
        let provider = FixedAuthProvider {
            calls: calls.clone(),
            responses: VecDeque::from([auth("expired-token"), auth("fresh-token")]),
        };
        let directory = tempdir().unwrap();
        let audio_path = directory.path().join("recording.wav");
        std::fs::write(&audio_path, b"RIFFspeech").unwrap();
        let mut transport = CodexSubscriptionTransport::with_endpoint_and_auth(
            format!("http://{address}/backend-api/transcribe"),
            provider,
        );

        let text = transport
            .transcribe_audio(TranscriptionRequest {
                keywords: &[],
                audio_path: &audio_path,
                provider: TranscriptionProvider::ChatGptSubscription,
                model: "gpt-transcribe",
                language: "",
                prompt: "",
                duration_seconds: 1.0,
            })
            .unwrap();

        let requests = server.join().unwrap();
        assert_eq!(text, "Recovered.");
        assert!(requests[0].contains("authorization: Bearer expired-token"));
        assert!(requests[1].contains("authorization: Bearer fresh-token"));
        assert_eq!(*calls.lock().unwrap(), vec![false, true]);
    }

    #[test]
    fn app_server_auth_requires_chatgpt_and_reads_the_nested_account_claim() {
        let claims = json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": "account-123"}
        });
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&claims).unwrap());
        let token = format!("header.{payload}.signature");

        let auth = parse_chatgpt_auth(&json!({
            "authMethod": "chatgpt",
            "authToken": token
        }))
        .unwrap();

        assert_eq!(auth.account_id.expose(), "account-123");
        assert!(matches!(
            parse_chatgpt_auth(&json!({"authMethod": "apikey", "authToken": "secret"})),
            Err(CodexSubscriptionError::ChatGptSignInRequired)
        ));
    }
}
