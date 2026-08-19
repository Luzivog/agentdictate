use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use rusqlite::ErrorCode;

use crate::{Runtime, RuntimeError};

type InterruptMaintenance = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct MaintenanceState {
    recording_priorities: usize,
    maintenance_interrupt: Option<InterruptMaintenance>,
}

struct MaintenanceShared {
    state: Mutex<MaintenanceState>,
    changed: Condvar,
}

/// Coordinates deferred transcript-index work with the recording lifecycle.
/// Recording takes priority: a start request interrupts any active rebuild and
/// does not return until the rebuild transaction has released its write lock.
#[derive(Clone)]
pub struct HistoryIndexMaintenance {
    shared: Arc<MaintenanceShared>,
}

impl Default for HistoryIndexMaintenance {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoryIndexMaintenance {
    #[must_use]
    pub fn new() -> Self {
        Self {
            shared: Arc::new(MaintenanceShared {
                state: Mutex::new(MaintenanceState::default()),
                changed: Condvar::new(),
            }),
        }
    }

    /// Raises recording priority and waits for any active index transaction to
    /// yield. Holding the returned guard prevents a rebuild from starting.
    #[must_use]
    pub fn prioritize_recording(&self) -> RecordingPriorityGuard {
        let interrupt = {
            let mut state = self.lock_state();
            state.recording_priorities += 1;
            state.maintenance_interrupt.clone()
        };
        if let Some(interrupt) = interrupt {
            interrupt();
        }
        let mut state = self.lock_state();
        while state.maintenance_interrupt.is_some() {
            state = self.wait_for_change(state);
        }
        RecordingPriorityGuard {
            maintenance: self.clone(),
        }
    }

    /// Completes deferred search-index preparation without ever outranking an
    /// active recording. An interrupted transaction is retried only after the
    /// recording-priority guard is released.
    pub fn prepare_history_search(&self, runtime: &mut Runtime) -> Result<(), RuntimeError> {
        loop {
            let cancelled = Arc::new(AtomicBool::new(false));
            let interrupt_handle = runtime.connection.get_interrupt_handle();
            let interrupt_cancelled = Arc::clone(&cancelled);
            let permit = self.begin_maintenance(Arc::new(move || {
                interrupt_cancelled.store(true, Ordering::Release);
                interrupt_handle.interrupt();
            }));

            let result = run_interruptible_index_attempt(runtime, Arc::clone(&cancelled));
            let was_cancelled = cancelled.load(Ordering::Acquire);
            drop(permit);
            match result {
                Err(error) if was_cancelled && is_interrupted(&error) => continue,
                result => return result,
            }
        }
    }

    fn begin_maintenance(&self, interrupt: InterruptMaintenance) -> MaintenancePermit {
        let mut state = self.lock_state();
        while state.recording_priorities > 0 || state.maintenance_interrupt.is_some() {
            state = self.wait_for_change(state);
        }
        state.maintenance_interrupt = Some(interrupt);
        MaintenancePermit {
            maintenance: self.clone(),
        }
    }

    #[cfg(test)]
    fn begin_for_test(&self, interrupt: InterruptMaintenance) -> MaintenancePermit {
        self.begin_maintenance(interrupt)
    }

    fn lock_state(&self) -> MutexGuard<'_, MaintenanceState> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn wait_for_change<'a>(
        &self,
        state: MutexGuard<'a, MaintenanceState>,
    ) -> MutexGuard<'a, MaintenanceState> {
        self.shared
            .changed
            .wait(state)
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub struct RecordingPriorityGuard {
    maintenance: HistoryIndexMaintenance,
}

impl Drop for RecordingPriorityGuard {
    fn drop(&mut self) {
        let mut state = self.maintenance.lock_state();
        debug_assert!(state.recording_priorities > 0);
        state.recording_priorities = state.recording_priorities.saturating_sub(1);
        self.maintenance.shared.changed.notify_all();
    }
}

struct MaintenancePermit {
    maintenance: HistoryIndexMaintenance,
}

impl Drop for MaintenancePermit {
    fn drop(&mut self) {
        let mut state = self.maintenance.lock_state();
        state.maintenance_interrupt = None;
        self.maintenance.shared.changed.notify_all();
    }
}

fn run_interruptible_index_attempt(
    runtime: &mut Runtime,
    cancelled: Arc<AtomicBool>,
) -> Result<(), RuntimeError> {
    runtime
        .connection
        .progress_handler(1_000, Some(move || cancelled.load(Ordering::Acquire)));
    let result = runtime.ensure_history_search_index();
    runtime.connection.progress_handler(0, None::<fn() -> bool>);
    result
}

fn is_interrupted(error: &RuntimeError) -> bool {
    matches!(
        error,
        RuntimeError::Database(error)
            if error.sqlite_error_code() == Some(ErrorCode::OperationInterrupted)
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::sync_channel;

    use chrono::Utc;
    use rusqlite::params;
    use tempfile::tempdir;

    use super::{is_interrupted, run_interruptible_index_attempt};
    use crate::{
        ExternalError, HistoryIndexMaintenance, HistoryQuery, JobStage, Recorder, RecordingJob,
        RecordingRequest, Runtime,
    };

    struct ReadyRecorder;

    impl Recorder for ReadyRecorder {
        fn start(&mut self, _job: &RecordingJob) -> Result<(), ExternalError> {
            Ok(())
        }
    }

    #[test]
    fn recording_priority_releases_a_maintenance_write_lock_before_returning() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("history.sqlite");
        let mut foreground = Runtime::open(&database).unwrap();
        let maintenance = HistoryIndexMaintenance::new();
        let background_maintenance = maintenance.clone();
        let (locked_sender, locked_receiver) = sync_channel(0);
        let (interrupted_sender, interrupted_receiver) = sync_channel(0);

        let worker = std::thread::spawn(move || {
            let background = Runtime::open(&database).unwrap();
            let permit = background_maintenance.begin_for_test(Arc::new(move || {
                interrupted_sender.send(()).unwrap();
            }));
            background
                .connection
                .execute_batch("BEGIN IMMEDIATE")
                .unwrap();
            locked_sender.send(()).unwrap();
            interrupted_receiver.recv().unwrap();
            background.connection.execute_batch("ROLLBACK").unwrap();
            drop(permit);
        });
        locked_receiver.recv().unwrap();

        let recording_priority = maintenance.prioritize_recording();
        let job = foreground
            .start_recording(
                RecordingRequest {
                    audio_path: directory.path().join("recording.wav"),
                    started_at: Utc::now(),
                    transcription_model: "gpt-transcribe".to_owned(),
                },
                &mut ReadyRecorder,
            )
            .unwrap();

        assert_eq!(
            foreground.job(job.id).unwrap().unwrap().stage,
            JobStage::Recording
        );
        drop(recording_priority);
        worker.join().unwrap();
    }

    #[test]
    fn interrupted_index_work_stays_unready_until_a_verified_retry_commits() {
        let directory = tempdir().unwrap();
        let mut runtime = Runtime::open(directory.path().join("history.sqlite")).unwrap();
        let transaction = runtime.connection.transaction().unwrap();
        for index in 0..100 {
            transaction
                .execute(
                    r#"
                    INSERT INTO dictation_sessions (
                        started_at, ended_at, duration_seconds, transcription_model
                    ) VALUES (?1, ?1, 1, 'gpt-transcribe')
                    "#,
                    [format!("2026-08-19T12:00:{:02}Z", index % 60)],
                )
                .unwrap();
            let session_id = transaction.last_insert_rowid();
            transaction
                .execute(
                    r#"
                    INSERT INTO transcript_history (
                        session_id, created_at, raw_transcript, final_text
                    ) VALUES (?1, ?2, ?3, ?3)
                    "#,
                    params![
                        session_id,
                        format!("2026-08-19T12:00:{:02}Z", index % 60),
                        format!("bulletproof transcript number {index}"),
                    ],
                )
                .unwrap();
        }
        transaction.commit().unwrap();

        let cancelled = Arc::new(AtomicBool::new(true));
        let error = run_interruptible_index_attempt(&mut runtime, cancelled).unwrap_err();

        assert!(is_interrupted(&error));
        assert_eq!(
            runtime
                .connection
                .query_row(
                    "SELECT ready FROM history_search_state WHERE id = 1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert!(
            runtime
                .history_page(HistoryQuery {
                    search: "buletproof".to_owned(),
                    limit: 10,
                    ..HistoryQuery::default()
                })
                .unwrap()
                .matches
                .is_empty()
        );

        HistoryIndexMaintenance::new()
            .prepare_history_search(&mut runtime)
            .unwrap();

        assert_eq!(
            runtime
                .connection
                .query_row(
                    "SELECT ready FROM history_search_state WHERE id = 1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            runtime
                .history_page(HistoryQuery {
                    search: "buletproof".to_owned(),
                    limit: 10,
                    ..HistoryQuery::default()
                })
                .unwrap()
                .total_matches,
            100
        );
    }
}
