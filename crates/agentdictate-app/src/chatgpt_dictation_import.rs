use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use agentdictate_runtime::{
    ExternalDictationImportOutcome, ExternalDictationReceipt, ExternalDictationSource, Runtime,
};
use chrono::{TimeZone, Utc};
use serde::Deserialize;

const CHATGPT_MODEL_LABEL: &str = "Managed by ChatGPT";
const MAX_METADATA_BYTES: u64 = 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_secs(2);

pub(crate) struct ChatGptDictationImporter {
    history_directory: PathBuf,
    history_fingerprint: Option<MetadataFingerprint>,
    known_directories: HashSet<PathBuf>,
    pending_directories: HashSet<PathBuf>,
    observed: HashMap<PathBuf, MetadataFingerprint>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MetadataFingerprint {
    length: u64,
    modified: Option<SystemTime>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ScanReport {
    imported: usize,
    already_imported: usize,
    invalid: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatGptDictationStatus {
    status: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompletedChatGptDictationMetadata {
    created_at_ms: i64,
    duration_ms: Option<f64>,
    id: String,
    text: Option<String>,
}

impl ChatGptDictationImporter {
    fn new(codex_home: impl Into<PathBuf>) -> Self {
        Self {
            history_directory: codex_home.into().join("dictation-history"),
            history_fingerprint: None,
            known_directories: HashSet::new(),
            pending_directories: HashSet::new(),
            observed: HashMap::new(),
        }
    }

    fn scan(&mut self, runtime: &mut Runtime) -> io::Result<ScanReport> {
        self.discover_directories()?;
        let mut report = ScanReport::default();
        let pending = self.pending_directories.iter().cloned().collect::<Vec<_>>();
        for directory in pending {
            let metadata_path = directory.join("metadata.json");
            let file_metadata = match fs::symlink_metadata(&metadata_path) {
                Ok(metadata) if metadata.file_type().is_file() => metadata,
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    tracing::warn!(%error, path = %metadata_path.display(), "could not inspect ChatGPT dictation metadata");
                    continue;
                }
            };
            let fingerprint = MetadataFingerprint {
                length: file_metadata.len(),
                modified: file_metadata.modified().ok(),
            };
            if self.observed.get(&metadata_path) == Some(&fingerprint) {
                continue;
            }

            let receipt = match read_completed_receipt(&metadata_path, file_metadata.len()) {
                Ok(receipt) => receipt,
                Err(error) => {
                    report.invalid += 1;
                    self.observed.insert(metadata_path.clone(), fingerprint);
                    tracing::warn!(%error, path = %metadata_path.display(), "ignored invalid ChatGPT dictation metadata");
                    continue;
                }
            };
            let Some(receipt) = receipt else {
                self.observed.insert(metadata_path, fingerprint);
                continue;
            };
            match runtime.import_external_dictation(&receipt) {
                Ok(ExternalDictationImportOutcome::Imported { .. }) => {
                    report.imported += 1;
                    self.pending_directories.remove(&directory);
                    self.observed.remove(&metadata_path);
                }
                Ok(ExternalDictationImportOutcome::AlreadyImported) => {
                    report.already_imported += 1;
                    self.pending_directories.remove(&directory);
                    self.observed.remove(&metadata_path);
                }
                Err(error) => {
                    tracing::warn!(%error, "could not import ChatGPT dictation usage");
                }
            }
        }
        Ok(report)
    }

    fn discover_directories(&mut self) -> io::Result<()> {
        let history_metadata = match fs::symlink_metadata(&self.history_directory) {
            Ok(metadata) if metadata.file_type().is_dir() => metadata,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "ChatGPT dictation history is not a directory",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        let fingerprint = MetadataFingerprint {
            length: history_metadata.len(),
            modified: history_metadata.modified().ok(),
        };
        if self.history_fingerprint == Some(fingerprint) {
            return Ok(());
        }

        let mut current_directories = HashSet::new();
        for entry in fs::read_dir(&self.history_directory)? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::warn!(%error, "could not inspect ChatGPT dictation receipt");
                    continue;
                }
            };
            match entry.file_type() {
                Ok(file_type) if file_type.is_dir() => {}
                Ok(_) => continue,
                Err(error) => {
                    tracing::warn!(%error, path = %entry.path().display(), "could not inspect ChatGPT dictation receipt type");
                    continue;
                }
            }
            let directory = entry.path();
            if !self.known_directories.contains(&directory) {
                self.pending_directories.insert(directory.clone());
            }
            current_directories.insert(directory);
        }
        self.pending_directories
            .retain(|directory| current_directories.contains(directory));
        self.observed.retain(|metadata_path, _| {
            metadata_path
                .parent()
                .is_some_and(|directory| current_directories.contains(directory))
        });
        self.known_directories = current_directories;
        self.history_fingerprint = Some(fingerprint);
        Ok(())
    }
}

pub(crate) fn start_chatgpt_dictation_importer(
    database_file: PathBuf,
) -> io::Result<std::thread::JoinHandle<()>> {
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Codex home is unavailable"))?;
    std::thread::Builder::new()
        .name("agentdictate-chatgpt-usage".into())
        .spawn(move || run_import_loop(database_file, codex_home))
}

fn run_import_loop(database_file: PathBuf, codex_home: PathBuf) {
    let mut runtime = match Runtime::open_background_writer(&database_file) {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::warn!(%error, "could not open the ChatGPT dictation usage database");
            return;
        }
    };
    let mut importer = ChatGptDictationImporter::new(&codex_home);
    tracing::info!(
        path = %importer.history_directory.display(),
        poll_interval_seconds = POLL_INTERVAL.as_secs(),
        "ChatGPT dictation usage importer started"
    );
    loop {
        match importer.scan(&mut runtime) {
            Ok(report) if report.imported > 0 => {
                tracing::info!(
                    imported = report.imported,
                    already_imported = report.already_imported,
                    invalid = report.invalid,
                    "imported ChatGPT dictation usage"
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, path = %importer.history_directory.display(), "could not scan ChatGPT dictation usage");
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn read_completed_receipt(
    path: &Path,
    reported_length: u64,
) -> io::Result<Option<ExternalDictationReceipt>> {
    if reported_length > MAX_METADATA_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "metadata is larger than the accepted limit",
        ));
    }
    let mut contents = String::new();
    File::open(path)?
        .take(MAX_METADATA_BYTES + 1)
        .read_to_string(&mut contents)?;
    if contents.len() as u64 > MAX_METADATA_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "metadata grew beyond the accepted limit",
        ));
    }
    let status: ChatGptDictationStatus = serde_json::from_str(&contents)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if status.status != "completed" {
        return Ok(None);
    }
    let metadata: CompletedChatGptDictationMetadata = serde_json::from_str(&contents)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let Some(duration_ms) = metadata.duration_ms else {
        return Ok(None);
    };
    let Some(text) = metadata.text.filter(|text| !text.trim().is_empty()) else {
        return Ok(None);
    };
    let started_at = Utc
        .timestamp_millis_opt(metadata.created_at_ms)
        .single()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid creation time"))?;
    if !duration_ms.is_finite() || duration_ms < 0.0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid audio duration",
        ));
    }
    Ok(Some(ExternalDictationReceipt {
        source: ExternalDictationSource::ChatGptDesktop,
        source_id: metadata.id,
        started_at,
        duration_seconds: duration_ms / 1_000.0,
        transcription_model: CHATGPT_MODEL_LABEL.to_owned(),
        raw_transcript: text.clone(),
        final_text: text,
        replacements_applied: Vec::new(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentdictate_runtime::UsageMetric;
    use tempfile::TempDir;

    const COMPLETED: &str = r#"{
        "createdAtMs": 1787447897045,
        "durationMs": 9621.4,
        "id": "2bb564e1-1878-4df7-a8d0-0fe2e49f6767",
        "mimeType": "audio/webm;codecs=opus",
        "sizeBytes": 153139,
        "status": "completed",
        "surface": "composer",
        "text": "Keep the receipt private."
    }"#;

    #[test]
    fn completed_metadata_becomes_a_statistics_receipt() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("metadata.json");
        fs::write(&path, COMPLETED).unwrap();

        let receipt = read_completed_receipt(&path, COMPLETED.len() as u64)
            .unwrap()
            .unwrap();

        assert_eq!(receipt.source, ExternalDictationSource::ChatGptDesktop);
        assert_eq!(receipt.duration_seconds, 9.6214);
        assert_eq!(receipt.transcription_model, "Managed by ChatGPT");
        assert_eq!(receipt.raw_transcript, "Keep the receipt private.");
        assert_eq!(receipt.final_text, "Keep the receipt private.");
        assert!(receipt.replacements_applied.is_empty());
    }

    #[test]
    fn unfinished_metadata_is_not_imported() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("metadata.json");
        let recording = r#"{
            "createdAtMs": 1787447897045,
            "id": "2bb564e1-1878-4df7-a8d0-0fe2e49f6767",
            "mimeType": "audio/webm;codecs=opus",
            "sizeBytes": 0,
            "status": "recording",
            "surface": "composer"
        }"#;
        fs::write(&path, &recording).unwrap();

        assert!(
            read_completed_receipt(&path, recording.len() as u64)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn completed_metadata_waits_until_transcript_text_is_filled() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("metadata.json");
        let awaiting_text = COMPLETED.replace("Keep the receipt private.", "");
        fs::write(&path, &awaiting_text).unwrap();

        assert!(
            read_completed_receipt(&path, awaiting_text.len() as u64)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn scanner_imports_each_completed_transcript_once_into_history() {
        let directory = TempDir::new().unwrap();
        let codex_home = directory.path().join("codex");
        let receipt_directory = codex_home.join("dictation-history/receipt-one");
        fs::create_dir_all(&receipt_directory).unwrap();
        fs::write(receipt_directory.join("metadata.json"), COMPLETED).unwrap();
        let database_path = directory.path().join("agentdictate.sqlite");
        let mut runtime = Runtime::open(&database_path).unwrap();
        let mut importer = ChatGptDictationImporter::new(codex_home);

        let first = importer.scan(&mut runtime).unwrap();
        let unchanged = importer.scan(&mut runtime).unwrap();

        assert_eq!(first.imported, 1);
        assert_eq!(unchanged, ScanReport::default());
        assert_eq!(
            runtime.usage_series(1, UsageMetric::Words).unwrap()[0].value,
            4.0
        );
        let history = runtime.list_history(Default::default()).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].final_text, "Keep the receipt private.");
    }

    #[test]
    fn scanner_tracks_a_new_recording_until_its_completed_metadata_appears() {
        let directory = TempDir::new().unwrap();
        let codex_home = directory.path().join("codex");
        let receipt_directory = codex_home.join("dictation-history/receipt-in-progress");
        fs::create_dir_all(&receipt_directory).unwrap();
        let mut runtime = Runtime::open(directory.path().join("agentdictate.sqlite")).unwrap();
        let mut importer = ChatGptDictationImporter::new(codex_home);

        assert_eq!(importer.scan(&mut runtime).unwrap(), ScanReport::default());
        fs::write(receipt_directory.join("metadata.json"), COMPLETED).unwrap();
        let completed = importer.scan(&mut runtime).unwrap();

        assert_eq!(completed.imported, 1);
        assert!(importer.pending_directories.is_empty());
    }
}
