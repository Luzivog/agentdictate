//! Explicit replay tool. It never captures a microphone or delivers text to another application.
use agentdictate_app::{AppPaths, ReqwestOpenAiTransport, SpeechTransport, TranscriptionRequest};
use agentdictate_core::{DictationOptions, Settings, normalize_vocabulary};
use serde::Deserialize;
use serde_json::json;
use std::{fs, io::Write, path::PathBuf, time::Instant};

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
    let cases = get("--cases").ok_or_else(|| anyhow::anyhow!("Usage: agentdictate-evaluate --cases cases.jsonl --output results.jsonl [--mode offline|speech] [--config config.json] [--model ID]"))?;
    let output = get("--output").ok_or_else(|| anyhow::anyhow!("--output is required"))?;
    let mode = get("--mode").unwrap_or_else(|| "offline".into());
    anyhow::ensure!(
        ["offline", "speech"].contains(&mode.as_str()),
        "unsupported mode"
    );
    let config_path = get("--config")
        .map(PathBuf::from)
        .unwrap_or(AppPaths::from_environment()?.config_file);
    // Read only: `load_settings` would create a missing file and fix its mode.
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
        let result = match mode.as_str() {
            "speech" => {
                let audio = case
                    .audio
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("case {} has no audio path", case.id))?;
                transport.transcribe_audio(TranscriptionRequest {
                    keywords: &keywords,
                    audio_path: audio,
                    encoding: None,
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
            json!({"id":case.id,"mode":mode,"model":&settings.transcription_model,"elapsed_ms":elapsed_ms,"word_error_rate":case.expected.as_ref().map(|r| word_error_rate(r, &candidate)),"candidate":candidate,"delivered":normalized.text,"transport_error":error,"protected_ok":protected_ok,"exact_reference":exact,"reference_verified":case.reference_verified,"options":options})
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
