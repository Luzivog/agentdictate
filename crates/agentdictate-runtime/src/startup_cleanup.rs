//! Daemon-start cleanup of finished jobs and the recordings directory.
//!
//! `dictation_jobs` only holds in-flight and recoverable dictations: delivered,
//! empty, and deleted jobs leave it as they finish. These passes finish what a
//! crash interrupted, and the first run migrates databases from before
//! finished jobs were deleted.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use agentdictate_core::{JobId, Settings};
use rusqlite::{OptionalExtension, params};

use crate::{Runtime, RuntimeError};

/// Recordings younger than this are never orphans: they may belong to a
/// dictation that is still being written or cleaned up.
const ORPHAN_RECORDING_AGE: Duration = Duration::from_secs(60 * 60);

const QUARANTINE_PREFIX: &str = ".agentdictate-delete-";
const QUARANTINE_SUFFIX: &str = ".pending";

/// What `Runtime::clean_up_finished_jobs` did, for the daemon's log.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FinishedJobCleanup {
    /// Deliveries a crash left before their dictation was recorded.
    pub recorded_deliveries: usize,
    pub removed_jobs: usize,
    pub removed_recordings: usize,
    /// Recordings that could not be deleted. A finished job whose audio
    /// could not be deleted keeps its row, so the next start retries it.
    pub failed_removals: usize,
}

impl Runtime {
    /// Finishes or undoes recovery deletes that a crash interrupted. A delete
    /// renames the audio to a quarantine file next to it, deletes the job row,
    /// then unlinks the quarantine file. A leftover quarantine file whose job
    /// is gone was committed and is removed; if the job still exists the
    /// delete never committed, so its audio is moved back. Run this before the
    /// daemon accepts commands, so it never races a live delete.
    pub fn reconcile_recovery_deletions(&self, recordings: &Path) -> Result<(), RuntimeError> {
        let entries = match fs::read_dir(recordings) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            let Some(id) = quarantined_job_id(&entry.file_name()) else {
                continue;
            };
            let job: Option<(String, PathBuf)> = self
                .connection
                .query_row(
                    "SELECT stage, audio_path FROM dictation_jobs WHERE runtime_id = ?1",
                    [id.to_string()],
                    |row| Ok((row.get(0)?, PathBuf::from(row.get::<_, String>(1)?))),
                )
                .optional()?;
            match job {
                // Rows in `deleted` exist only in databases from before
                // deleted jobs were removed; that delete did commit.
                Some((stage, audio_path)) if stage != "deleted" && !audio_path.exists() => {
                    fs::rename(entry.path(), audio_path)?;
                }
                _ => {
                    remove_if_present(&entry.path())?;
                }
            }
        }
        Ok(())
    }

    /// Removes finished jobs left in the job table and recordings no job
    /// needs. Delivered jobs get their dictation recorded exactly once, as
    /// `Runtime::complete_delivered` records it. Audio of finished
    /// jobs is deleted first, unless `preserve_temp_audio` keeps it; the user
    /// deleted `deleted` jobs, so their audio always goes. With
    /// `preserve_temp_audio` off, `.wav` files in `recordings` that no job
    /// owns and that are older than an hour are deleted too; a live
    /// recording always owns a job row, so it is never one of them.
    pub fn clean_up_finished_jobs(
        &mut self,
        settings: &Settings,
        recordings: &Path,
    ) -> Result<FinishedJobCleanup, RuntimeError> {
        let mut cleanup = FinishedJobCleanup::default();
        for (id, stage, audio_path) in self.finished_jobs()? {
            if stage == FinishedStage::Deleted || !settings.preserve_temp_audio {
                match remove_if_present(&audio_path) {
                    Ok(true) => cleanup.removed_recordings += 1,
                    Ok(false) => {}
                    Err(_) => {
                        cleanup.failed_removals += 1;
                        continue;
                    }
                }
            }
            match stage {
                FinishedStage::Delivered => {
                    if self.complete_delivered_job(id, settings)? {
                        cleanup.recorded_deliveries += 1;
                    }
                }
                FinishedStage::NoSpeech | FinishedStage::Deleted => {
                    self.connection.execute(
                        "DELETE FROM dictation_jobs WHERE runtime_id = ?1",
                        params![id.to_string()],
                    )?;
                }
            }
            cleanup.removed_jobs += 1;
        }
        if !settings.preserve_temp_audio {
            self.remove_orphan_recordings(recordings, &mut cleanup)?;
        }
        Ok(cleanup)
    }

    fn finished_jobs(&self) -> Result<Vec<(JobId, FinishedStage, PathBuf)>, RuntimeError> {
        let mut statement = self.connection.prepare(
            r#"
            SELECT runtime_id, stage, audio_path
            FROM dictation_jobs
            WHERE stage IN ('delivered', 'no_speech', 'deleted')
            ORDER BY id ASC
            "#,
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    PathBuf::from(row.get::<_, String>(2)?),
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(runtime_id, stage, audio_path)| {
                let id = runtime_id
                    .parse::<JobId>()
                    .map_err(|_| RuntimeError::InvalidJobId(runtime_id))?;
                let stage = match stage.as_str() {
                    "delivered" => FinishedStage::Delivered,
                    "no_speech" => FinishedStage::NoSpeech,
                    // The query selects only finished stages.
                    _ => FinishedStage::Deleted,
                };
                Ok((id, stage, audio_path))
            })
            .collect()
    }

    fn remove_orphan_recordings(
        &self,
        recordings: &Path,
        cleanup: &mut FinishedJobCleanup,
    ) -> Result<(), RuntimeError> {
        let entries = match fs::read_dir(recordings) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        // File names, not full paths: a differently spelled recordings path
        // must never make an owned recording look like an orphan.
        let owned = {
            let mut statement = self
                .connection
                .prepare("SELECT audio_path FROM dictation_jobs")?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .filter_map(|path| Path::new(&path).file_name().map(OsStr::to_owned))
                .collect::<HashSet<OsString>>()
        };
        let Some(cutoff) = SystemTime::now().checked_sub(ORPHAN_RECORDING_AGE) else {
            return Ok(());
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension() != Some(OsStr::new("wav")) || owned.contains(&entry.file_name()) {
                continue;
            }
            let old_file = entry.metadata().is_ok_and(|metadata| {
                metadata.is_file() && metadata.modified().is_ok_and(|modified| modified < cutoff)
            });
            if !old_file {
                continue;
            }
            match remove_if_present(&path) {
                Ok(true) => cleanup.removed_recordings += 1,
                Ok(false) => {}
                Err(_) => cleanup.failed_removals += 1,
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FinishedStage {
    Delivered,
    NoSpeech,
    Deleted,
}

/// Where a recovery delete parks a job's audio until the delete commits.
pub(crate) fn recovery_delete_path(audio_path: &Path, id: JobId) -> PathBuf {
    audio_path.with_file_name(format!("{QUARANTINE_PREFIX}{id}{QUARANTINE_SUFFIX}"))
}

fn quarantined_job_id(file_name: &OsStr) -> Option<JobId> {
    file_name
        .to_str()?
        .strip_prefix(QUARANTINE_PREFIX)?
        .strip_suffix(QUARANTINE_SUFFIX)?
        .parse()
        .ok()
}

/// Returns whether a file was removed; a missing file is not an error.
fn remove_if_present(path: &Path) -> io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
