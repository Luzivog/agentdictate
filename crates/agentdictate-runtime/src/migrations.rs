//! Numbered schema migrations. `PRAGMA user_version` counts the steps of
//! `MIGRATIONS` a database has had, and `Runtime::open` runs the missing ones.
//! Each step runs in one IMMEDIATE transaction that also records its number,
//! so a crash mid-step leaves the previous version and the next start
//! repeats the step.
//!
//! Before migrating an existing database, a compact copy is kept next to it
//! as `<file>.pre-v<latest>`, replacing any older copy. It is the only way
//! back, since a database from a newer version is refused rather than
//! downgraded, so it lasts until the migrated database opens again at a
//! later start, which proves the migration. It holds every transcript from
//! before, so deleting text also deletes it; see
//! `Runtime::erase_removed_text`.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, Transaction};

use crate::RuntimeError;

type Migration = fn(&Transaction<'_>) -> rusqlite::Result<()>;

/// Every schema step, oldest first. A database at version N has had the
/// first N; append new steps and never edit a released one.
const MIGRATIONS: &[Migration] = &[merge_dictations, add_failure_kinds];

/// The schema version this build migrates databases to.
pub(crate) const LATEST_VERSION: usize = MIGRATIONS.len();

/// Brings the database behind `connection`, stored at `path`, to the latest
/// version. A new, empty database is migrated without a backup.
pub(crate) fn migrate(connection: &mut Connection, path: &Path) -> Result<(), RuntimeError> {
    let latest = LATEST_VERSION;
    let version = user_version(connection)?;
    let pending = usize::try_from(version)
        .ok()
        .filter(|version| *version <= latest)
        .ok_or(RuntimeError::NewerDatabase { version, latest })?;
    if pending == latest {
        remove_backups(path);
        return Ok(());
    }
    if has_schema(connection)? {
        remove_backups(path);
        back_up(connection, &backup_path(path, latest))?;
    }
    for (step, migration) in MIGRATIONS.iter().enumerate().skip(pending) {
        let transaction = connection.transaction()?;
        // Another connection may have run this step since the check above.
        if user_version(&transaction)? > step as i64 {
            continue;
        }
        migration(&transaction)?;
        transaction.pragma_update(None, "user_version", step as i64 + 1)?;
        transaction.commit()?;
    }
    // Return the space of dropped tables and rewritten rows. Best-effort: a
    // failed VACUUM (for example, no room for its temporary copy) leaves a
    // correct database that is only larger than it needs to be.
    let _ = connection.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);");
    Ok(())
}

fn user_version(connection: &Connection) -> rusqlite::Result<i64> {
    connection.pragma_query_value(None, "user_version", |row| row.get(0))
}

fn has_schema(connection: &Connection) -> rusqlite::Result<bool> {
    connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master)", [], |row| {
        row.get(0)
    })
}

/// Names a backup `<file>.pre-v<version>`.
const BACKUP_SUFFIX: &str = ".pre-v";

fn backup_path(path: &Path, version: usize) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(format!("{BACKUP_SUFFIX}{version}"));
    path.with_file_name(name)
}

/// Deletes every pre-migration backup of the database at `path`, of any
/// version. Best-effort: a backup that cannot be deleted now is deleted at
/// a later start.
pub(crate) fn remove_backups(path: &Path) {
    let directory = path
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    let prefix = format!("{name}{BACKUP_SUFFIX}");
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let is_backup = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_prefix(&prefix))
            .is_some_and(|version| {
                !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit())
            });
        if is_backup {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Writes a consistent, compacted copy of the database, readable only by
/// its owner, replacing an earlier copy for the same version.
fn back_up(connection: &Connection, backup: &Path) -> Result<(), RuntimeError> {
    match fs::remove_file(backup) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    connection.execute("VACUUM INTO ?1", [backup.to_string_lossy()])?;
    fs::set_permissions(backup, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Version 1. One `dictations` row per completed dictation replaces the
/// strictly 1:1 pair of `dictation_sessions` (usage numbers) and
/// `transcript_history` (text); History keeps its row ids, so its order and
/// any id the window holds stay valid. Sessions of the retired ChatGPT
/// desktop import become `source = 'chatgpt_desktop'` rows with their
/// receipt id, and their import ledger goes. Timestamps are normalized to
/// the `YYYY-MM-DDTHH:MM:SSZ` form the app writes. The retired full-text
/// index and the tables nothing reads are dropped, legacy job spellings the
/// app never writes are renamed, and `dictation_jobs` stays for in-flight and
/// Recovery work only. `replacement_mappings` is left to
/// `Runtime::retire_replacement_rules`, which moves its rules into vocabulary
/// before dropping it.
fn merge_dictations(transaction: &Transaction<'_>) -> rusqlite::Result<()> {
    transaction.execute_batch(VERSION_0_TABLES)?;
    add_missing_columns(transaction)?;
    transaction.execute_batch(VERSION_1)
}

/// The tables before versioning. The version 1 step creates any that are
/// missing (a new database has none) so it has one shape to merge; only
/// the columns it reads matter, except for `dictation_jobs`, which stays.
const VERSION_0_TABLES: &str = r#"
CREATE TABLE IF NOT EXISTS dictation_sessions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at TEXT NOT NULL,
    ended_at TEXT NOT NULL,
    duration_seconds REAL NOT NULL DEFAULT 0,
    transcription_model TEXT NOT NULL,
    transcription_provider TEXT NOT NULL DEFAULT 'openai_api',
    final_word_count INTEGER NOT NULL DEFAULT 0,
    final_character_count INTEGER NOT NULL DEFAULT 0,
    estimated_total_cost REAL NOT NULL DEFAULT 0,
    runtime_job_id TEXT
);

CREATE TABLE IF NOT EXISTS transcript_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    raw_transcript TEXT NOT NULL DEFAULT '',
    final_text TEXT NOT NULL DEFAULT '',
    replacements_applied TEXT NOT NULL DEFAULT '[]'
);

CREATE TABLE IF NOT EXISTS external_dictation_imports (
    source TEXT NOT NULL,
    source_id TEXT NOT NULL,
    session_id INTEGER
);

CREATE TABLE IF NOT EXISTS dictation_jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    state TEXT NOT NULL,
    stage TEXT NOT NULL,
    audio_path TEXT NOT NULL UNIQUE,
    duration_seconds REAL NOT NULL DEFAULT 0,
    transcription_model TEXT NOT NULL DEFAULT '',
    transcription_provider TEXT NOT NULL DEFAULT 'openai_api',
    raw_transcript TEXT NOT NULL DEFAULT '',
    final_text TEXT NOT NULL DEFAULT '',
    copied_to_clipboard INTEGER NOT NULL DEFAULT 0,
    paste_triggered INTEGER NOT NULL DEFAULT 0,
    error_message TEXT,
    runtime_id TEXT,
    delivery_status TEXT NOT NULL DEFAULT 'not_attempted',
    cleaned_transcript TEXT,
    replacements_applied TEXT NOT NULL DEFAULT '[]',
    cleanup_error TEXT,
    processing_options TEXT
);
"#;

/// Columns that unversioned databases created before them lack.
const VERSION_0_ADDED_COLUMNS: &[(&str, &str, &str)] = &[
    ("dictation_jobs", "runtime_id", "TEXT"),
    (
        "dictation_jobs",
        "delivery_status",
        "TEXT NOT NULL DEFAULT 'not_attempted'",
    ),
    ("dictation_jobs", "processing_options", "TEXT"),
    (
        "dictation_jobs",
        "transcription_provider",
        "TEXT NOT NULL DEFAULT 'openai_api'",
    ),
    ("dictation_jobs", "cleaned_transcript", "TEXT"),
    (
        "dictation_jobs",
        "replacements_applied",
        "TEXT NOT NULL DEFAULT '[]'",
    ),
    ("dictation_jobs", "cleanup_error", "TEXT"),
    (
        "dictation_sessions",
        "transcription_provider",
        "TEXT NOT NULL DEFAULT 'openai_api'",
    ),
    ("dictation_sessions", "runtime_job_id", "TEXT"),
];

fn add_missing_columns(connection: &Connection) -> rusqlite::Result<()> {
    for (table, column, declaration) in VERSION_0_ADDED_COLUMNS {
        let exists: bool = connection.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1)"),
            [column],
            |row| row.get(0),
        )?;
        if !exists {
            connection.execute(
                &format!("ALTER TABLE {table} ADD COLUMN {column} {declaration}"),
                [],
            )?;
        }
    }
    Ok(())
}

const VERSION_1: &str = r#"
-- One row per completed dictation. Usage counts every row; History lists
-- the rows that still have text. `final_text` is NULL when the text was not
-- kept or has expired, `raw_text` is NULL when it equals `final_text`, and
-- `vocabulary_corrections` (JSON) is NULL when vocabulary changed nothing.
CREATE TABLE dictations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT UNIQUE,
    source TEXT NOT NULL DEFAULT 'agentdictate'
        CHECK (source IN ('agentdictate', 'chatgpt_desktop')),
    source_id TEXT,
    started_at TEXT NOT NULL,
    ended_at TEXT NOT NULL,
    duration_seconds REAL NOT NULL,
    transcription_provider TEXT NOT NULL,
    transcription_model TEXT NOT NULL,
    word_count INTEGER NOT NULL,
    character_count INTEGER NOT NULL,
    estimated_cost REAL NOT NULL,
    final_text TEXT,
    raw_text TEXT,
    vocabulary_corrections TEXT
);

-- Rows with History keep its id; sessions without History follow them.
INSERT INTO dictations (
    id, job_id, source, source_id, started_at, ended_at, duration_seconds,
    transcription_provider, transcription_model, word_count, character_count,
    estimated_cost, final_text, raw_text, vocabulary_corrections
)
SELECT h.id, s.runtime_job_id,
       CASE WHEN i.source_id IS NULL THEN 'agentdictate' ELSE 'chatgpt_desktop' END,
       i.source_id,
       COALESCE(strftime('%Y-%m-%dT%H:%M:%SZ', s.started_at), s.started_at),
       COALESCE(strftime('%Y-%m-%dT%H:%M:%SZ', s.ended_at), s.ended_at),
       s.duration_seconds, s.transcription_provider, s.transcription_model,
       s.final_word_count, s.final_character_count, s.estimated_total_cost,
       h.final_text, NULLIF(h.raw_transcript, h.final_text),
       NULLIF(h.replacements_applied, '[]')
FROM dictation_sessions s
LEFT JOIN transcript_history h ON h.session_id = s.id
LEFT JOIN (
    SELECT session_id, MIN(source_id) AS source_id
    FROM external_dictation_imports
    WHERE source = 'chatgpt_desktop' AND session_id IS NOT NULL
    GROUP BY session_id
) i ON i.session_id = s.id
ORDER BY h.id IS NULL, h.id, s.id;

-- History pages and transcript retention walk dictations by end time.
CREATE INDEX dictations_ended_at ON dictations(ended_at);

DROP TABLE IF EXISTS transcript_history_fts_vocab;
DROP TABLE IF EXISTS transcript_history_fts_trigram;
DROP TABLE IF EXISTS transcript_history_fts;
DROP TABLE transcript_history;
DROP TABLE external_dictation_imports;
DROP TABLE dictation_sessions;
DROP TABLE IF EXISTS history_search_state;
DROP TABLE IF EXISTS daily_stats;
DROP TABLE IF EXISTS pricing_settings;

-- Spellings of older releases that the app never writes.
UPDATE dictation_jobs SET delivery_status = 'submitted' WHERE delivery_status = 'committed';
UPDATE dictation_jobs SET stage = 'transcribing' WHERE stage = 'cleaning';
UPDATE dictation_jobs SET state = 'interrupted', stage = 'interrupted' WHERE stage = 'canceled';
UPDATE dictation_jobs
SET state = 'failed', stage = 'failed',
    delivery_status = CASE delivery_status
        WHEN 'not_attempted' THEN 'ambiguous'
        ELSE delivery_status
    END,
    error_message = COALESCE(
        error_message, 'delivery was interrupted after the attempt began'
    )
WHERE stage = 'delivering';

-- The job table is small, so only lookups by id and Recovery's expiry by
-- age keep an index.
DROP INDEX IF EXISTS idx_dictation_jobs_state;
DROP INDEX IF EXISTS idx_dictation_jobs_updated_at;
CREATE UNIQUE INDEX IF NOT EXISTS idx_dictation_jobs_runtime_id ON dictation_jobs(runtime_id);
CREATE INDEX dictation_jobs_recoverable ON dictation_jobs(updated_at)
    WHERE stage IN ('captured', 'ready_to_deliver', 'interrupted', 'failed');
"#;

/// Version 2. A Recovery item keeps why it failed as a `FailureKind` name,
/// so the window words the reason instead of showing the raw error. Items
/// from before have none and keep showing their stored message.
fn add_failure_kinds(transaction: &Transaction<'_>) -> rusqlite::Result<()> {
    transaction.execute_batch("ALTER TABLE dictation_jobs ADD COLUMN failure_kind TEXT;")
}
