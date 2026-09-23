//! How long finished work stays on this computer. Transcript text follows
//! the Keep transcripts setting, and a Recovery item, text and audio, lasts
//! `recovery_lifetime` after its last change. Both rules run at daemon start
//! and after each completed dictation; nothing runs on a timer.

use agentdictate_core::{JobId, JobStage, KeepTranscripts};
use chrono::{DateTime, TimeDelta, Utc};

use crate::{Runtime, RuntimeError, timestamp};

/// How long a Recovery item stays after its last change.
const RECOVERY_LIFETIME: TimeDelta = TimeDelta::days(7);

/// How long a recording discarded with Esc stays in Recovery.
const CANCELLED_LIFETIME: TimeDelta = TimeDelta::days(1);

/// A discarded recording longer than this many seconds is kept as a
/// `Cancelled` Recovery item; a shorter one is deleted at once.
pub(crate) const KEPT_CANCEL_SECONDS: f64 = 5.0;

/// How long a Recovery item at `stage` stays after its last change.
pub(crate) const fn recovery_lifetime(stage: JobStage) -> TimeDelta {
    match stage {
        JobStage::Cancelled => CANCELLED_LIFETIME,
        JobStage::Starting
        | JobStage::Recording
        | JobStage::Captured
        | JobStage::Transcribing
        | JobStage::ReadyToDeliver
        | JobStage::Delivered
        | JobStage::NoSpeech
        | JobStage::Interrupted
        | JobStage::Failed
        | JobStage::Deleted => RECOVERY_LIFETIME,
    }
}

/// What `Runtime::apply_retention` removed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Retention {
    pub(crate) purged_transcripts: usize,
    pub(crate) expired_recoveries: usize,
    /// Expired Recovery items that could not be deleted; the next run
    /// tries again.
    pub(crate) failed_expiries: usize,
}

impl Runtime {
    /// Deletes the text of dictations older than `keep` allows, keeping
    /// their usage numbers, and deletes expired Recovery items as the user's
    /// Delete would. When anything was removed, the write-ahead log is
    /// truncated so the removed text leaves the disk too.
    pub(crate) fn apply_retention(
        &mut self,
        keep: KeepTranscripts,
        now: DateTime<Utc>,
    ) -> Result<Retention, RuntimeError> {
        let purged_transcripts = match keep.limit() {
            Some(limit) => self.connection.execute(
                r#"
                UPDATE dictations
                SET final_text = NULL, raw_text = NULL, vocabulary_corrections = NULL
                WHERE ended_at < ?1 AND final_text IS NOT NULL
                "#,
                [timestamp(now - limit)],
            )?,
            None => 0,
        };
        let mut retention = Retention {
            purged_transcripts,
            ..Retention::default()
        };
        for id in self.expired_recoveries(now)? {
            match self.delete_recovery(id) {
                Ok(_) => retention.expired_recoveries += 1,
                Err(error @ RuntimeError::Database(_)) => return Err(error),
                Err(_) => retention.failed_expiries += 1,
            }
        }
        if retention.purged_transcripts > 0 || retention.expired_recoveries > 0 {
            self.truncate_write_ahead_log();
        }
        Ok(retention)
    }

    fn expired_recoveries(&self, now: DateTime<Utc>) -> Result<Vec<JobId>, RuntimeError> {
        let mut statement = self.connection.prepare(
            r#"
            SELECT runtime_id FROM dictation_jobs
            WHERE (stage IN ('captured', 'ready_to_deliver', 'interrupted', 'failed')
                   AND updated_at < ?1)
               OR (stage = 'cancelled' AND updated_at < ?2)
            "#,
        )?;
        let ids = statement
            .query_map(
                [
                    timestamp(now - RECOVERY_LIFETIME),
                    timestamp(now - CANCELLED_LIFETIME),
                ],
                |row| row.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| id.parse().map_err(|_| RuntimeError::InvalidJobId(id)))
            .collect()
    }

    /// Copies committed pages into the database file and empties the
    /// write-ahead log, which otherwise keeps old page images, deleted text
    /// included, until later writes overwrite them. Best-effort: a reader
    /// that holds an old snapshot only postpones it to a later removal.
    pub(crate) fn truncate_write_ahead_log(&self) {
        let _ = self
            .connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    }
}
