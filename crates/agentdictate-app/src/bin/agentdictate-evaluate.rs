//! Explicit replay tool. It never captures a microphone or delivers text to another application.
use agentdictate_app::{AppPaths, ReqwestOpenAiTransport, SpeechTransport, TranscriptionRequest};
use agentdictate_core::{DictationOptions, Settings, normalize_vocabulary};
use serde::Deserialize;
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Deserialize)]
struct Case {
    id: String,
    text: String,
    #[serde(default)]
    expected: Option<String>,
    #[serde(default)]
    preserve: Vec<String>,
    #[serde(default)]
    audio: Option<PathBuf>,
    #[serde(default)]
    reference_verified: bool,
}

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let get = |key: &str| args.windows(2).find(|w| w[0] == key).map(|w| w[1].clone());
    let cases = get("--cases").ok_or_else(|| anyhow::anyhow!("Usage: agentdictate-evaluate --cases cases.jsonl --output results.jsonl [--mode offline|speech|live] [--config config.json] [--model ID]"))?;
    let output = get("--output").ok_or_else(|| anyhow::anyhow!("--output is required"))?;
    let mode = get("--mode").unwrap_or_else(|| "offline".into());
    anyhow::ensure!(
        ["offline", "speech", "live"].contains(&mode.as_str()),
        "unsupported mode"
    );
    let config_path = get("--config")
        .map(PathBuf::from)
        .unwrap_or(AppPaths::from_environment()?.config_file);
    // Read without pricing repair or other mutation of the user's configuration.
    let mut settings: Settings = if config_path.exists() {
        serde_json::from_slice(&fs::read(config_path)?)?
    } else {
        Settings::default()
    };
    if let Some(model) = get("--model") {
        settings.transcription_model = model;
    }
    anyhow::ensure!(
        args.len() % 2 == 0
            && args.chunks_exact(2).all(|pair| [
                "--cases", "--output", "--mode", "--config", "--model"
            ]
            .contains(&pair[0].as_str())),
        "unknown or incomplete argument"
    );
    let options = DictationOptions::from_settings(&settings);
    let keywords = options.keywords();
    let mut transport = ReqwestOpenAiTransport::new(&settings.openai_api_key);
    let input = fs::read_to_string(&cases)?;
    let parsed: Vec<Case> = input
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    anyhow::ensure!(!parsed.is_empty(), "empty case set");
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output)?;
    let mut passed = 0;
    let mut count = 0;
    for case in parsed {
        let start = Instant::now();
        let mut stop_ms = None;
        let result = match mode.as_str() {
            "live" => {
                let audio = case
                    .audio
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("case {} has no audio path", case.id))?;
                let (result, elapsed) = replay_live(&mut transport, audio, &settings, &options)?;
                stop_ms = Some(elapsed);
                result
            }
            "speech" => {
                let audio = case
                    .audio
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("case {} has no audio path", case.id))?;
                transport.transcribe_audio(TranscriptionRequest {
                    keywords: &keywords,
                    audio_path: audio,
                    model: &settings.transcription_model,
                    language: &options.language,
                    prompt: &options.context,
                    duration_seconds: 0.0,
                })
            }
            _ => Ok(case.text.clone()),
        };
        let elapsed_ms = start.elapsed().as_millis();
        let error = result.as_ref().err().map(ToString::to_string);
        let candidate = result.unwrap_or_else(|_| case.text.clone());
        let normalized = normalize_vocabulary(&candidate, &options.vocabulary);
        let protected_ok = case.preserve.iter().all(|part| {
            normalized
                .text
                .to_lowercase()
                .contains(&part.to_lowercase())
        });
        let exact = case
            .expected
            .as_ref()
            .map(|expected| expected == &normalized.text);
        let ok = protected_ok && error.is_none() && (mode != "offline" || exact != Some(false));
        passed += usize::from(ok);
        count += 1;
        writeln!(
            file,
            "{}",
            json!({"id":case.id,"mode":mode,"model":&settings.transcription_model,"elapsed_ms":elapsed_ms,"stop_to_final_ms":stop_ms,"word_error_rate":case.expected.as_ref().map(|r| word_error_rate(r, &candidate)),"actual_speech_model":transport.actual_model(),"candidate":candidate,"delivered":normalized.text,"transport_error":error,"protected_ok":protected_ok,"exact_reference":exact,"reference_verified":case.reference_verified,"options":options})
        )?;
    }
    println!(
        "{passed}/{count} cases passed explicit checks. These checks do not establish semantic equivalence or personal speech accuracy."
    );
    anyhow::ensure!(passed == count, "evaluation checks failed");
    Ok(())
}

/// WER is descriptive unless the reference has been checked against the audio.
fn word_error_rate(reference: &str, hypothesis: &str) -> f64 {
    let tokens = |s: &str| {
        s.split_whitespace()
            .map(|w| {
                w.trim_matches(|c: char| !c.is_alphanumeric())
                    .to_lowercase()
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
    };
    let reference = tokens(reference);
    let hypothesis = tokens(hypothesis);
    let mut previous: Vec<usize> = (0..=hypothesis.len()).collect();
    for (i, word) in reference.iter().enumerate() {
        let mut row = vec![i + 1];
        for (j, candidate) in hypothesis.iter().enumerate() {
            row.push(
                (previous[j] + usize::from(word != candidate))
                    .min(previous[j + 1] + 1)
                    .min(row[j] + 1),
            );
        }
        previous = row;
    }
    previous[hypothesis.len()] as f64 / reference.len().max(1) as f64
}

/// Pace a supplied WAV through the production streaming adapter without opening a microphone.
fn replay_live(
    transport: &mut ReqwestOpenAiTransport,
    audio: &std::path::Path,
    settings: &Settings,
    options: &DictationOptions,
) -> anyhow::Result<(Result<String, agentdictate_runtime::ExternalError>, u128)> {
    use std::io::{Seek, SeekFrom};
    let decoded = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(audio)
        .args(["-f", "s16le", "-ac", "1", "-ar", "16000", "pipe:1"])
        .output()?;
    anyhow::ensure!(decoded.status.success(), "could not decode replay audio");
    let pcm = decoded.stdout;
    anyhow::ensure!(
        !pcm.is_empty() && pcm.len() < u32::MAX as usize - 36,
        "invalid replay size"
    );
    struct TempAudio(PathBuf);
    impl Drop for TempAudio {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let path = TempAudio(
        std::env::temp_dir().join(format!("agentdictate-replay-{}.wav", uuid::Uuid::new_v4())),
    );
    use std::os::unix::fs::OpenOptionsExt;
    let mut writer = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path.0)?;
    let mut header = Vec::new();
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&0u32.to_le_bytes());
    header.extend_from_slice(b"WAVEfmt ");
    for value in [16u32, 65537, 16000, 32000, 1048578] {
        header.extend_from_slice(&value.to_le_bytes());
    }
    header.extend_from_slice(b"data");
    header.extend_from_slice(&0u32.to_le_bytes());
    writer.write_all(&header)?;
    writer.flush()?;
    let now = chrono::Utc::now();
    let mut live_options = options.clone();
    live_options.streaming = true;
    let job = agentdictate_runtime::RecordingJob {
        id: agentdictate_core::JobId::new(),
        options: Some(live_options.clone()),
        started_at: now,
        updated_at: now,
        stage: agentdictate_core::JobStage::Recording,
        audio_path: path.0.clone(),
        duration_seconds: pcm.len() as f64 / 32000.0,
        transcription_model: settings.transcription_model.clone(),
        raw_transcript: String::new(),
        final_text: String::new(),
        copied_to_clipboard: false,
        paste_triggered: false,
        delivery_status: agentdictate_runtime::DeliveryStatus::NotAttempted,
        error_message: None,
    };
    transport.begin_recording(&job, &live_options);
    let started = Instant::now();
    for (index, chunk) in pcm.chunks(3200).enumerate() {
        writer.write_all(chunk)?;
        writer.flush()?;
        std::thread::sleep(
            (started + Duration::from_millis((index as u64 + 1) * 100))
                .saturating_duration_since(Instant::now()),
        );
    }
    writer.seek(SeekFrom::Start(4))?;
    writer.write_all(&(pcm.len() as u32 + 36).to_le_bytes())?;
    writer.seek(SeekFrom::Start(40))?;
    writer.write_all(&(pcm.len() as u32).to_le_bytes())?;
    writer.flush()?;
    let stopped = Instant::now();
    let keywords = options.keywords();
    let result = transport.transcribe_audio(TranscriptionRequest {
        keywords: &keywords,
        audio_path: &path.0,
        model: &settings.transcription_model,
        language: &options.language,
        prompt: &options.context,
        duration_seconds: job.duration_seconds,
    });
    Ok((result, stopped.elapsed().as_millis()))
}
