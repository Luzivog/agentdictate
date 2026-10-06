//! Replays dictation cases through the production text normalization and,
//! in speech mode, the production transcription transport, and builds case
//! files from kept recordings. It never captures a microphone or delivers
//! text to another application.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Instant;

use agentdictate_app::{
    AppPaths, ReqwestOpenAiTransport, SpeechTransport, TranscriptionRequest, UploadFormat,
};
use agentdictate_core::{
    DictationOptions, JobId, Settings, TRANSCRIPTION_MODEL, normalize_transcript,
};
use agentdictate_runtime::{DatabaseObserver, ExternalError};
use anyhow::{Context, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;

const USAGE: &str = "\
Usage:
  agentdictate-evaluate --cases <cases.jsonl> --output <results.jsonl>
      [--mode offline|speech] [--config <config.json>]
    Speech mode only (calls OpenAI and costs money):
      [--model <id>] [--upload-format webm|wav|flac]
      [--prompt <text> | --no-prompt] [--no-keywords] [--language <codes>]
      [--repeat <n>]
  agentdictate-evaluate export-cases --output <cases.jsonl>
    Writes one case per kept recording whose dictation still has its text.";

/// One line of a case file.
#[derive(Deserialize, Serialize)]
struct Case {
    id: String,
    /// What the model heard, for offline mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    /// The exact text the dictation should deliver.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    preserve: Vec<String>,
    /// The recording, for speech mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    audio: Option<PathBuf>,
    /// Whether a person checked `expected` against the audio.
    #[serde(default)]
    reference_verified: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Offline,
    Speech,
}

impl FromStr for Mode {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "offline" => Ok(Self::Offline),
            "speech" => Ok(Self::Speech),
            other => bail!("unknown --mode {other:?}: use offline or speech"),
        }
    }
}

fn main() -> anyhow::Result<()> {
    let mut arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        println!("{USAGE}");
        return Ok(());
    }
    let export = arguments
        .first()
        .is_some_and(|first| first == "export-cases");
    if export {
        arguments.remove(0);
    }
    let arguments = Arguments::parse(arguments)?;
    if export {
        export_cases(arguments)
    } else {
        evaluate(arguments)
    }
}

/// Command-line flags: `--flag value` pairs and bare switches. Each is
/// taken by the command or mode that uses it, so a leftover one is
/// reported instead of silently ignored.
struct Arguments {
    values: BTreeMap<String, String>,
    switches: BTreeSet<String>,
}

const SWITCHES: [&str; 2] = ["--no-prompt", "--no-keywords"];

impl Arguments {
    fn parse(arguments: Vec<String>) -> anyhow::Result<Self> {
        let mut parsed = Self {
            values: BTreeMap::new(),
            switches: BTreeSet::new(),
        };
        let mut arguments = arguments.into_iter();
        while let Some(flag) = arguments.next() {
            ensure!(
                flag.starts_with("--"),
                "unexpected argument {flag:?}; see --help"
            );
            let repeated = if SWITCHES.contains(&flag.as_str()) {
                !parsed.switches.insert(flag.clone())
            } else {
                let value = arguments
                    .next()
                    .with_context(|| format!("{flag} needs a value; see --help"))?;
                parsed.values.insert(flag.clone(), value).is_some()
            };
            ensure!(!repeated, "{flag} is given twice");
        }
        Ok(parsed)
    }

    fn value(&mut self, flag: &str) -> Option<String> {
        self.values.remove(flag)
    }

    fn required(&mut self, flag: &str) -> anyhow::Result<String> {
        self.value(flag)
            .with_context(|| format!("{flag} is required; see --help"))
    }

    fn switch(&mut self, flag: &str) -> bool {
        self.switches.remove(flag)
    }

    /// Fails on a flag nothing took: unknown, or not for `command`.
    fn finish(self, command: &str) -> anyhow::Result<()> {
        if let Some(flag) = self.values.keys().chain(&self.switches).next() {
            bail!("{flag} is unknown or does not apply to {command}; see --help");
        }
        Ok(())
    }
}

/// What speech mode sends, after the A/B flags changed the configuration.
struct Speech {
    transport: ReqwestOpenAiTransport,
    model: String,
    /// `None` uploads as production does.
    upload_format: Option<UploadFormat>,
    keywords: Vec<String>,
    repeat: usize,
}

impl Speech {
    /// Takes the speech flags, applying `--prompt`, `--no-prompt` and
    /// `--language` to `options`.
    fn from_arguments(
        arguments: &mut Arguments,
        settings: &Settings,
        options: &mut DictationOptions,
    ) -> anyhow::Result<Self> {
        let upload_format = arguments
            .value("--upload-format")
            .map(|format| match format.as_str() {
                "webm" => Ok(UploadFormat::WebmOpus),
                "wav" => Ok(UploadFormat::Wav),
                "flac" => Ok(UploadFormat::Flac),
                other => bail!("unknown --upload-format {other:?}: use webm, wav or flac"),
            })
            .transpose()?;
        let prompt = arguments.value("--prompt");
        if arguments.switch("--no-prompt") {
            ensure!(prompt.is_none(), "use --prompt or --no-prompt, not both");
            options.context.clear();
        }
        if let Some(prompt) = prompt {
            prompt.trim().clone_into(&mut options.context);
        }
        if let Some(language) = arguments.value("--language") {
            options.language = language;
        }
        let keywords = if arguments.switch("--no-keywords") {
            Vec::new()
        } else {
            options.keywords()
        };
        let repeat = arguments
            .value("--repeat")
            .map(|repeat| {
                repeat
                    .parse::<usize>()
                    .ok()
                    .filter(|repeat| *repeat > 0)
                    .with_context(|| format!("--repeat needs a positive count, not {repeat:?}"))
            })
            .transpose()?
            .unwrap_or(1);
        ensure!(
            !settings.openai_api_key.trim().is_empty(),
            "speech mode needs a configuration with an OpenAI API key"
        );
        Ok(Self {
            transport: ReqwestOpenAiTransport::new(&settings.openai_api_key),
            model: arguments
                .value("--model")
                .unwrap_or_else(|| TRANSCRIPTION_MODEL.to_owned()),
            upload_format,
            keywords,
            repeat,
        })
    }

    /// Transcribes `audio` as a dictation with `options` would. No speech
    /// reads as an empty transcript, which the reference then scores.
    fn transcribe(
        &mut self,
        audio: &Path,
        options: &DictationOptions,
    ) -> Result<String, ExternalError> {
        // 16 kHz mono PCM16 is 32 000 bytes a second; this only sizes the
        // encode deadline.
        let duration_seconds = fs::metadata(audio).map_or(0.0, |file| file.len() as f64 / 32_000.0);
        let heard = self.transport.transcribe_audio(TranscriptionRequest {
            keywords: &self.keywords,
            audio_path: audio,
            encoding: None,
            model: &self.model,
            language: &options.language,
            prompt: &options.context,
            duration_seconds,
            upload_format: self.upload_format,
        });
        match heard {
            Err(ExternalError::NoSpeech) => Ok(String::new()),
            heard => heard,
        }
    }

    fn upload_format_name(&self) -> &'static str {
        match self.upload_format {
            None => "production",
            Some(UploadFormat::WebmOpus) => "webm",
            Some(UploadFormat::Wav) => "wav",
            Some(UploadFormat::Flac) => "flac",
        }
    }
}

fn evaluate(mut arguments: Arguments) -> anyhow::Result<()> {
    let cases_path = arguments.required("--cases")?;
    let output = PathBuf::from(arguments.required("--output")?);
    let mode = arguments
        .value("--mode")
        .as_deref()
        .unwrap_or("offline")
        .parse::<Mode>()?;
    let config_path = match arguments.value("--config") {
        Some(path) => PathBuf::from(path),
        None => AppPaths::from_environment()?.config_file,
    };
    let settings = read_settings(&config_path)?;
    let mut options = DictationOptions::from_settings(&settings);
    let mut speech = match mode {
        Mode::Offline => None,
        Mode::Speech => Some(Speech::from_arguments(
            &mut arguments,
            &settings,
            &mut options,
        )?),
    };
    arguments.finish(match mode {
        Mode::Offline => "--mode offline",
        Mode::Speech => "--mode speech",
    })?;
    let cases = read_cases(Path::new(&cases_path), mode)?;
    let mut file = create_private(&output)?;
    let mut summary = Summary::default();
    for case in &cases {
        let runs = speech.as_ref().map_or(1, |speech| speech.repeat);
        let mut outcomes = Vec::with_capacity(runs);
        for run in 1..=runs {
            let started = Instant::now();
            let heard = match (&mut speech, &case.audio, &case.text) {
                (Some(speech), Some(audio), _) => speech
                    .transcribe(audio, &options)
                    .map_err(|error| error.to_string()),
                (None, _, Some(text)) => Ok(text.clone()),
                _ => unreachable!("read_cases checks each case has its mode's input"),
            };
            let elapsed_ms = started.elapsed().as_millis();
            let outcome = Outcome::judge(case, mode, heard, &options);
            writeln!(
                file,
                "{}",
                json!({
                    "id": case.id,
                    "run": run,
                    "mode": mode,
                    "model": speech.as_ref().map(|speech| &speech.model),
                    "upload_format": speech.as_ref().map(Speech::upload_format_name),
                    "elapsed_ms": elapsed_ms,
                    "raw": outcome.raw,
                    "delivered": outcome.delivered,
                    "word_error_rate": outcome.errors.map(|errors| errors.rate()),
                    "raw_word_error_rate": outcome.raw_errors.map(|errors| errors.rate()),
                    "exact_reference": outcome.exact,
                    "protected_ok": outcome.protected_ok,
                    "passed": outcome.passed,
                    "transport_error": outcome.error,
                    "reference_verified": case.reference_verified,
                    "options": options,
                    "keywords": speech.as_ref().map(|speech| &speech.keywords),
                })
            )?;
            outcomes.push(outcome);
        }
        println!("{}", case_line(case, &outcomes));
        summary.add(case, &outcomes);
    }
    summary.print(cases.len());
    ensure!(
        summary.passed == summary.runs,
        "evaluation checks failed; the results are in {}",
        output.display()
    );
    Ok(())
}

/// Reads the configuration without changing it: `load_settings` would
/// create a missing file and fix its mode.
fn read_settings(path: &Path) -> anyhow::Result<Settings> {
    if !path.exists() {
        return Ok(Settings::default());
    }
    serde_json::from_slice(&fs::read(path)?)
        .with_context(|| format!("could not read the configuration {}", path.display()))
}

/// Reads a case file and checks each case has what `mode` replays.
fn read_cases(path: &Path, mode: Mode) -> anyhow::Result<Vec<Case>> {
    let input = fs::read_to_string(path)
        .with_context(|| format!("could not read the cases {}", path.display()))?;
    let cases = input
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str::<Case>(line)
                .with_context(|| format!("line {} of {}", index + 1, path.display()))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    ensure!(!cases.is_empty(), "empty case set");
    for case in &cases {
        match mode {
            Mode::Offline => ensure!(case.text.is_some(), "case {} has no text", case.id),
            Mode::Speech => ensure!(case.audio.is_some(), "case {} has no audio", case.id),
        }
    }
    Ok(cases)
}

/// Creates `path` readable only by its owner, refusing to replace a file.
fn create_private(path: &Path) -> anyhow::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("could not create {} (use a new path)", path.display()))
}

/// One replay of one case, judged against its checks.
struct Outcome {
    /// What the model heard, or `None` when the request failed.
    raw: Option<String>,
    /// The text after normalization, as it would be pasted.
    delivered: Option<String>,
    error: Option<String>,
    /// Word errors of `delivered` and `raw` against `expected`.
    errors: Option<WordErrors>,
    raw_errors: Option<WordErrors>,
    exact: Option<bool>,
    protected_ok: bool,
    passed: bool,
}

impl Outcome {
    /// Offline mode passes on an exact match with `expected`; speech mode,
    /// where wording legitimately varies, only on `preserve`.
    fn judge(
        case: &Case,
        mode: Mode,
        heard: Result<String, String>,
        options: &DictationOptions,
    ) -> Self {
        let (raw, error) = match heard {
            Ok(raw) => (Some(raw), None),
            Err(error) => (None, Some(error)),
        };
        let delivered = raw
            .as_deref()
            .map(|raw| normalize_transcript(raw, options).text);
        let against =
            |text: &Option<String>| Some(word_errors(case.expected.as_deref()?, text.as_deref()?));
        let exact = case
            .expected
            .as_ref()
            .zip(delivered.as_ref())
            .map(|(expected, delivered)| expected == delivered);
        let protected_ok = delivered.as_ref().is_some_and(|delivered| {
            let delivered = delivered.to_lowercase();
            case.preserve
                .iter()
                .all(|part| delivered.contains(&part.to_lowercase()))
        });
        let passed =
            error.is_none() && protected_ok && (mode == Mode::Speech || exact != Some(false));
        Self {
            errors: against(&delivered),
            raw_errors: against(&raw),
            raw,
            delivered,
            error,
            exact,
            protected_ok,
            passed,
        }
    }
}

/// Word-level edits needed to turn a hypothesis into its reference, and the
/// reference's length. Summing both over cases gives the total WER.
#[derive(Clone, Copy, Debug, Default)]
struct WordErrors {
    edits: usize,
    words: usize,
}

impl WordErrors {
    fn rate(self) -> f64 {
        self.edits as f64 / self.words.max(1) as f64
    }
}

impl std::ops::AddAssign for WordErrors {
    fn add_assign(&mut self, other: Self) {
        self.edits += other.edits;
        self.words += other.words;
    }
}

/// Levenshtein distance over words, ignoring case and surrounding
/// punctuation. WER is descriptive unless the reference has been checked
/// against the audio.
fn word_errors(reference: &str, hypothesis: &str) -> WordErrors {
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
    WordErrors {
        edits: previous[hypothesis.len()],
        words: reference.len(),
    }
}

fn percent(errors: WordErrors) -> String {
    format!("{:.1}%", errors.rate() * 100.0)
}

/// One case's line: pass or fail (passes out of runs when repeated), its
/// WER after and before normalization, and with repeats, the spread of WER
/// across runs and how many different transcripts they returned.
fn case_line(case: &Case, outcomes: &[Outcome]) -> String {
    let passed = outcomes.iter().filter(|outcome| outcome.passed).count();
    let status = match outcomes.len() {
        1 if passed == 1 => "pass".to_owned(),
        1 => "FAIL".to_owned(),
        runs => format!("{passed}/{runs}"),
    };
    let mut line = format!("{status:>5}  {}", case.id);
    let mut total = WordErrors::default();
    let mut raw_total = WordErrors::default();
    for outcome in outcomes {
        total += outcome.errors.unwrap_or_default();
        raw_total += outcome.raw_errors.unwrap_or_default();
    }
    if total.words > 0 {
        line.push_str(&format!(
            "  WER {} (raw {})",
            percent(total),
            percent(raw_total)
        ));
    }
    if outcomes.len() > 1 {
        let rates = outcomes
            .iter()
            .filter_map(|outcome| outcome.errors.map(WordErrors::rate))
            .collect::<Vec<_>>();
        if let (Some(low), Some(high)) = (
            rates.iter().copied().reduce(f64::min),
            rates.iter().copied().reduce(f64::max),
        ) {
            line.push_str(&format!(", spread {:.1}-{:.1}%", low * 100.0, high * 100.0));
        }
        let distinct = outcomes
            .iter()
            .filter_map(|outcome| outcome.raw.as_deref())
            .collect::<BTreeSet<_>>()
            .len();
        line.push_str(&format!(", {distinct} distinct transcripts"));
    }
    if let Some(error) = outcomes.iter().find_map(|outcome| outcome.error.as_ref()) {
        line.push_str(&format!("  error: {error}"));
    }
    line
}

/// Totals over every run of every case.
#[derive(Default)]
struct Summary {
    runs: usize,
    passed: usize,
    /// Runs whose case has an `expected` and that returned text.
    referenced: usize,
    exact: usize,
    errors: WordErrors,
    raw_errors: WordErrors,
    verified_cases: usize,
}

impl Summary {
    fn add(&mut self, case: &Case, outcomes: &[Outcome]) {
        self.verified_cases += usize::from(case.reference_verified);
        for outcome in outcomes {
            self.runs += 1;
            self.passed += usize::from(outcome.passed);
            self.referenced += usize::from(outcome.exact.is_some());
            self.exact += usize::from(outcome.exact == Some(true));
            self.errors += outcome.errors.unwrap_or_default();
            self.raw_errors += outcome.raw_errors.unwrap_or_default();
        }
    }

    fn print(&self, cases: usize) {
        println!(
            "\n{}/{} runs passed their checks. Exact matches: {}/{} runs with a reference.",
            self.passed, self.runs, self.exact, self.referenced
        );
        if self.errors.words > 0 {
            println!(
                "Total WER {} (raw {}) over {} reference words; {}/{cases} cases have verified references.",
                percent(self.errors),
                percent(self.raw_errors),
                self.errors.words,
                self.verified_cases,
            );
        }
        println!("These checks do not establish semantic equivalence or personal speech accuracy.");
    }
}

/// Writes a case for each kept recording whose dictation still has its
/// text: its audio, what the model heard as `text`, and the delivered text
/// as an unverified `expected`. Reads the database without changing it.
fn export_cases(mut arguments: Arguments) -> anyhow::Result<()> {
    let output = PathBuf::from(arguments.required("--output")?);
    arguments.finish("export-cases")?;
    let paths = AppPaths::from_environment()?;
    let observer = DatabaseObserver::open(&paths.database_file)
        .with_context(|| format!("could not read {}", paths.database_file.display()))?;
    let mut recordings = fs::read_dir(&paths.recordings)
        .with_context(|| format!("could not list {}", paths.recordings.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    // Names start with the recording's start time.
    recordings.sort();
    let mut cases = Vec::new();
    for audio in recordings {
        let Some(job_id) = recording_job(&audio) else {
            continue;
        };
        if let Some(text) = observer.dictation_text(job_id)? {
            cases.push(Case {
                id: job_id.to_string(),
                text: Some(text.raw_text),
                expected: Some(text.final_text),
                preserve: Vec::new(),
                audio: Some(audio),
                reference_verified: false,
            });
        }
    }
    ensure!(
        !cases.is_empty(),
        "no recording in {} belongs to a dictation that still has its text; turn on \
         Keep audio recordings to collect some",
        paths.recordings.display()
    );
    let mut file = create_private(&output)?;
    for case in &cases {
        writeln!(file, "{}", serde_json::to_string(case)?)?;
    }
    println!(
        "Wrote {} cases to {}. Check each `expected` against its audio before setting \
         `reference_verified` to true.",
        cases.len(),
        output.display()
    );
    Ok(())
}

/// The job a kept recording belongs to. The daemon names each recording
/// `dictation-<start time>-<job id>.wav`.
fn recording_job(path: &Path) -> Option<JobId> {
    if path.extension()? != "wav" {
        return None;
    }
    let stem = path.file_stem()?.to_str()?.strip_prefix("dictation-")?;
    stem.get(stem.len().checked_sub(36)?..)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_errors_count_edits_against_the_reference_length() {
        let errors = word_errors("Open the work trees now.", "open the worktrees");

        assert_eq!((errors.edits, errors.words), (3, 5));
    }

    #[test]
    fn a_recording_names_its_job() {
        let job = JobId::new();
        let named = |name: String| recording_job(Path::new(&name));

        assert_eq!(
            named(format!("/r/dictation-20261006T192659.496073525Z-{job}.wav")),
            Some(job)
        );
        assert_eq!(named(format!("/r/dictation-{job}.webm")), None);
        assert_eq!(named("/r/microphone-test.wav".to_owned()), None);
    }
}
