use std::{
    ffi::{CString, OsString},
    fs::File,
    io::{self, Read},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, channel},
    },
};

use agentdictate_core::{
    ClientCommand, ClientCommandKind, DEFAULT_HISTORY_PAGE_SIZE, HISTORY_CONTINUATION_PAGE_SIZE,
    HistoryPageCursor, HistoryPageSnapshot, JobId, ServerMessageKind, UsageSnapshot,
    UsageTotalsSnapshot, WorkspaceSnapshot, format_duration_clock,
};
use agentdictate_runtime::IpcClient;
use thiserror::Error;

use agentdictate_ui::{
    HistoryViewModel, RecoveryItemViewModel, RecoveryStage, TranscriptViewModel, UsageDayViewModel,
    UsagePeriod, UsageTotals, UsageViewModel, WorkspaceAction, WorkspaceViewModel,
    format_history_time,
};
use chrono::{DateTime, Local};

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("workspace state is unavailable")]
    StateUnavailable,
    #[error("workspace request state is unavailable")]
    RequestStateUnavailable,
    #[error("invalid recovery id {id}")]
    InvalidRecoveryId { id: String },
    #[error(transparent)]
    Ipc(#[from] agentdictate_runtime::IpcError),
    #[error("{message}")]
    CommandRejected { message: String },
    #[error("daemon returned unrelated data for a history request")]
    UnexpectedHistoryResponse,
    #[error("daemon returned a lifecycle snapshot for a workspace request")]
    UnexpectedLifecycleSnapshot,
    #[error("daemon returned history data for a workspace request")]
    UnexpectedHistoryPage,
    #[error("daemon returned a shortcut capture for a workspace request")]
    UnexpectedHotkeyCapture,
    #[error("daemon did not answer a settings request with its settings")]
    UnexpectedSettingsResponse,
}

pub struct WorkspaceClient {
    runtime_directory: PathBuf,
    request_gate: Mutex<()>,
    state: Mutex<WorkspaceClientState>,
}

struct WorkspaceClientState {
    snapshot: WorkspaceSnapshot,
    period: UsagePeriod,
    /// The History search as last typed.
    search: String,
    /// The History tab's rows after a search or "Show more". `None` shows the
    /// workspace's own first page, which also fills the overview.
    history: Option<HistoryPageSnapshot>,
}

impl WorkspaceClientState {
    fn shown_history(&self) -> &HistoryPageSnapshot {
        self.history.as_ref().unwrap_or(&self.snapshot.history)
    }

    fn view_model(&self) -> WorkspaceViewModel {
        workspace_view_model(&self.snapshot, self.shown_history(), self.period)
    }
}

impl WorkspaceClient {
    #[must_use]
    pub fn new(runtime_directory: PathBuf, snapshot: WorkspaceSnapshot) -> Self {
        Self {
            runtime_directory,
            request_gate: Mutex::new(()),
            state: Mutex::new(WorkspaceClientState {
                snapshot,
                period: UsagePeriod::Last30Days,
                search: String::new(),
                history: None,
            }),
        }
    }

    pub fn view_model(&self) -> Result<WorkspaceViewModel, WorkspaceError> {
        Ok(self.lock_state()?.view_model())
    }

    fn lock_state(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, WorkspaceClientState>, WorkspaceError> {
        self.state
            .lock()
            .map_err(|_| WorkspaceError::StateUnavailable)
    }

    pub fn perform(&self, action: WorkspaceAction) -> Result<WorkspaceViewModel, WorkspaceError> {
        if let WorkspaceAction::SearchHistory { query } = action {
            self.lock_state()?.search.clone_from(&query);
            return self.load_history(query, None);
        }
        if matches!(action, WorkspaceAction::LoadMoreHistory) {
            let (search, after) = {
                let state = self.lock_state()?;
                let shown = state.shown_history();
                (shown.search.clone(), shown.next_cursor.clone())
            };
            let Some(after) = after else {
                return self.view_model();
            };
            return self.load_history(search, Some(after));
        }
        if let WorkspaceAction::SelectUsagePeriod(period) = action {
            let mut state = self.lock_state()?;
            state.period = period;
            return Ok(state.view_model());
        }

        // A searched or extended History page is this client's own copy, so
        // what a delete removed must also leave it.
        if let WorkspaceAction::DeleteTranscript { id } = action {
            self.send_workspace_command(ClientCommandKind::DeleteHistory { id }.into())?;
            let mut state = self.lock_state()?;
            if let Some(page) = state.history.as_mut() {
                let shown = page.rows.len();
                page.rows.retain(|row| row.id != id);
                if page.rows.len() < shown {
                    page.total_matches = page.total_matches.saturating_sub(1);
                }
            }
            return Ok(state.view_model());
        }
        if matches!(action, WorkspaceAction::ClearHistory) {
            self.send_workspace_command(ClientCommandKind::ClearHistory.into())?;
            let mut state = self.lock_state()?;
            state.history = None;
            return Ok(state.view_model());
        }
        let command = match action {
            WorkspaceAction::RetryRecovery { id, stage } => {
                let job_id = id
                    .parse::<JobId>()
                    .map_err(|_| WorkspaceError::InvalidRecoveryId { id })?;
                match stage {
                    RecoveryStage::Transcription => {
                        ClientCommandKind::RetryTranscription { job_id }
                    }
                    RecoveryStage::Delivery => ClientCommandKind::RetryDelivery { job_id },
                }
            }
            WorkspaceAction::DeleteRecovery { id } => {
                let job_id = id
                    .parse::<JobId>()
                    .map_err(|_| WorkspaceError::InvalidRecoveryId { id })?;
                ClientCommandKind::DeleteRecovery { job_id }
            }
            WorkspaceAction::CopyTranscript { id } => ClientCommandKind::CopyTranscript { id },
            WorkspaceAction::SearchHistory { .. }
            | WorkspaceAction::LoadMoreHistory
            | WorkspaceAction::DeleteTranscript { .. }
            | WorkspaceAction::ClearHistory => {
                unreachable!("handled above")
            }
            WorkspaceAction::SelectUsagePeriod(_) => unreachable!("handled above"),
        };

        self.send_workspace_command(command.into())
    }

    /// Re-queries the daemon and atomically replaces the cached workspace.
    /// Requests from actions and filesystem refreshes are serialized so an
    /// older response cannot overwrite a newer local snapshot. A searched or
    /// extended History tab reloads its search's first page.
    pub fn refresh(&self) -> Result<WorkspaceViewModel, WorkspaceError> {
        let workspace = self.send_workspace_command(ClientCommandKind::GetWorkspace.into())?;
        let search = {
            let state = self.lock_state()?;
            state.history.is_some().then(|| state.search.clone())
        };
        match search {
            Some(search) => self.load_history(search, None),
            None => Ok(workspace),
        }
    }

    /// Shows the History page for `search` that starts `after` a cursor, or
    /// its first page. A continuation appends to the rows shown, unless the
    /// daemon had to restart an expired cursor at the first page. A response
    /// for a search the user has since changed is dropped.
    fn load_history(
        &self,
        search: String,
        after: Option<HistoryPageCursor>,
    ) -> Result<WorkspaceViewModel, WorkspaceError> {
        let continuation = after.is_some();
        let response = {
            let _request = self
                .request_gate
                .lock()
                .map_err(|_| WorkspaceError::RequestStateUnavailable)?;
            let (mut client, _) = IpcClient::connect(&self.runtime_directory)?;
            client.send(ClientCommand::get_history_page(
                search,
                if continuation {
                    HISTORY_CONTINUATION_PAGE_SIZE
                } else {
                    DEFAULT_HISTORY_PAGE_SIZE
                },
                after,
            ))?
        };
        let mut page = match response.kind {
            ServerMessageKind::HistoryPage { page, .. } => *page,
            ServerMessageKind::CommandRejected { error, .. } => {
                return Err(WorkspaceError::CommandRejected { message: error });
            }
            ServerMessageKind::Snapshot { .. }
            | ServerMessageKind::Workspace { .. }
            | ServerMessageKind::HotkeyCaptured { .. } => {
                return Err(WorkspaceError::UnexpectedHistoryResponse);
            }
        };
        let mut state = self.lock_state()?;
        if page.search != state.search {
            return Ok(state.view_model());
        }
        if continuation && !page.cursor_restarted {
            let mut rows = state.shown_history().rows.clone();
            for row in page.rows {
                if !rows.iter().any(|existing| existing.id == row.id) {
                    rows.push(row);
                }
            }
            page.rows = rows;
        }
        if continuation || !page.search.trim().is_empty() {
            state.history = Some(page);
        } else {
            state.history = None;
            state.snapshot.history = page;
        }
        Ok(state.view_model())
    }

    fn send_workspace_command(
        &self,
        command: ClientCommand,
    ) -> Result<WorkspaceViewModel, WorkspaceError> {
        let response = {
            let _request = self
                .request_gate
                .lock()
                .map_err(|_| WorkspaceError::RequestStateUnavailable)?;
            let (mut client, _) = IpcClient::connect(&self.runtime_directory)?;
            client.send(command)?
        };
        let workspace = match response.kind {
            ServerMessageKind::Workspace { workspace, .. } => *workspace,
            ServerMessageKind::CommandRejected { error, .. } => {
                return Err(WorkspaceError::CommandRejected { message: error });
            }
            ServerMessageKind::Snapshot { .. } => {
                return Err(WorkspaceError::UnexpectedLifecycleSnapshot);
            }
            ServerMessageKind::HistoryPage { .. } => {
                return Err(WorkspaceError::UnexpectedHistoryPage);
            }
            ServerMessageKind::HotkeyCaptured { .. } => {
                return Err(WorkspaceError::UnexpectedHotkeyCapture);
            }
        };
        let mut state = self.lock_state()?;
        state.snapshot = workspace;
        Ok(state.view_model())
    }

    /// Watches SQLite database, rollback-journal, and WAL writes, and the
    /// overlay health file, and emits a freshly queried workspace after each
    /// filesystem event batch. This is event-driven: callers do not need a
    /// refresh interval or debounce delay.
    pub fn watch(
        self: &Arc<Self>,
        database_file: impl AsRef<Path>,
    ) -> io::Result<Receiver<WorkspaceViewModel>> {
        let mut watcher = FileWatcher::database(database_file.as_ref())?;
        watcher.add_file(&self.runtime_directory.join(crate::OVERLAY_HEALTH_FILE))?;
        self.watch_changes(watcher)
    }

    fn watch_changes(
        self: &Arc<Self>,
        mut watcher: FileWatcher,
    ) -> io::Result<Receiver<WorkspaceViewModel>> {
        let client = Arc::clone(self);
        let (sender, receiver) = channel();
        std::thread::Builder::new()
            .name("agentdictate-workspace-watch".into())
            .spawn(move || {
                loop {
                    if let Err(error) = watcher.wait_for_change() {
                        tracing::warn!(%error, "workspace file watcher stopped");
                        return;
                    }
                    match client.refresh() {
                        Ok(workspace) => {
                            if sender.send(workspace).is_err() {
                                return;
                            }
                        }
                        Err(error) => {
                            tracing::warn!(%error, "could not refresh workspace after database change");
                        }
                    }
                }
            })?;
        Ok(receiver)
    }
}

/// Waits, with inotify, for writes to a set of files. A file is watched by
/// name in its directory, so it may be replaced or not exist yet.
pub(crate) struct FileWatcher {
    descriptor: File,
    watched_names: Vec<Vec<u8>>,
}

impl FileWatcher {
    /// A watcher with no files yet; see `add_file`.
    pub(crate) fn empty() -> io::Result<Self> {
        // SAFETY: `inotify_init1` has no pointer parameters. On success the
        // returned descriptor is uniquely owned by `File` below.
        let raw_descriptor = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
        if raw_descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw_descriptor` was just returned by `inotify_init1` and has
        // not been wrapped or closed elsewhere.
        let descriptor = unsafe { File::from_raw_fd(raw_descriptor) };
        Ok(Self {
            descriptor,
            watched_names: Vec::new(),
        })
    }

    /// Watches a SQLite database with its rollback journal and WAL.
    fn database(database_file: &Path) -> io::Result<Self> {
        let mut watcher = Self::empty()?;
        let mask = libc::IN_MODIFY
            | libc::IN_CLOSE_WRITE
            | libc::IN_MOVED_TO
            | libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_ATTRIB;
        let parent = database_file
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = CString::new(parent.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "workspace directory contains a NUL byte",
            )
        })?;
        // SAFETY: the descriptor is live and `parent` owns a NUL-terminated
        // path for the duration of the call.
        if unsafe { libc::inotify_add_watch(watcher.descriptor.as_raw_fd(), parent.as_ptr(), mask) }
            < 0
        {
            return Err(io::Error::last_os_error());
        }
        let database_name = database_file.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "database path has no file name",
            )
        })?;
        let sidecar_name = |suffix: &str| {
            let mut name = OsString::from(database_name);
            name.push(suffix);
            name.as_os_str().as_bytes().to_vec()
        };
        watcher.watched_names.extend([
            database_name.as_bytes().to_vec(),
            sidecar_name("-wal"),
            sidecar_name("-journal"),
        ]);
        Ok(watcher)
    }

    pub(crate) fn add_file(&mut self, path: &Path) -> io::Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("watch file has no parent"))?;
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("watch file has no name"))?;
        let parent = CString::new(parent.as_os_str().as_bytes()).map_err(io::Error::other)?;
        // SAFETY: descriptor is owned, and parent is NUL-terminated for this call.
        let result = unsafe {
            libc::inotify_add_watch(
                self.descriptor.as_raw_fd(),
                parent.as_ptr(),
                libc::IN_MASK_ADD | libc::IN_CLOSE_WRITE | libc::IN_CREATE | libc::IN_MOVED_TO,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        self.watched_names.push(name.as_bytes().to_vec());
        Ok(())
    }

    /// Blocks until a watched file is written, created or moved into place.
    pub(crate) fn wait_for_change(&mut self) -> io::Result<()> {
        let mut buffer = [0_u8; 4096];
        loop {
            let bytes_read = match self.descriptor.read(&mut buffer) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "database change watcher closed",
                    ));
                }
                Ok(bytes_read) => bytes_read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            let mut offset = 0;
            let mut relevant = false;
            while offset + std::mem::size_of::<libc::inotify_event>() <= bytes_read {
                // SAFETY: the bounds check above guarantees the fixed event
                // header is present. Inotify records need not be Rust-aligned,
                // so this uses an unaligned read.
                let event = unsafe {
                    std::ptr::read_unaligned(
                        buffer.as_ptr().add(offset).cast::<libc::inotify_event>(),
                    )
                };
                let name_start = offset + std::mem::size_of::<libc::inotify_event>();
                let name_end = name_start
                    .saturating_add(event.len as usize)
                    .min(bytes_read);
                let name = buffer[name_start..name_end]
                    .split(|byte| *byte == 0)
                    .next()
                    .unwrap_or_default();
                if event.mask & libc::IN_IGNORED != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "database directory is no longer watched",
                    ));
                }
                relevant |= event.mask & libc::IN_Q_OVERFLOW != 0
                    || self
                        .watched_names
                        .iter()
                        .any(|watched_name| watched_name.as_slice() == name);
                offset = name_end;
            }
            if relevant {
                return Ok(());
            }
        }
    }
}

/// Presents a workspace. `history` is the History tab's page; the overview
/// always lists the workspace's own newest transcripts. Times are shown on
/// the local clock, relative to now.
fn workspace_view_model(
    snapshot: &WorkspaceSnapshot,
    history: &HistoryPageSnapshot,
    period: UsagePeriod,
) -> WorkspaceViewModel {
    let now = Local::now();
    let recoveries = snapshot
        .recoveries
        .iter()
        .map(|entry| {
            let delivery = !entry.final_text.trim().is_empty()
                && (entry.delivery_ambiguous
                    || matches!(
                        entry.stage,
                        agentdictate_core::JobStage::ReadyToDeliver
                            | agentdictate_core::JobStage::Failed
                    ));
            RecoveryItemViewModel::new(
                entry.job_id.to_string(),
                if delivery {
                    RecoveryStage::Delivery
                } else {
                    RecoveryStage::Transcription
                },
                format_history_time(entry.updated_at, &now),
                format_duration_clock(entry.duration_seconds),
                entry
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "Recording saved safely".to_owned()),
                (!entry.final_text.trim().is_empty()).then(|| entry.final_text.clone()),
            )
        })
        .collect();
    let transcripts = history
        .rows
        .iter()
        .map(|entry| transcript_view_model(entry, &now))
        .collect();
    let recent_transcripts = snapshot
        .history
        .rows
        .iter()
        .map(|entry| transcript_view_model(entry, &now))
        .collect();
    WorkspaceViewModel::new(
        HistoryViewModel::from_page(
            recoveries,
            transcripts,
            history.total_matches,
            history.search.clone(),
            history.next_cursor.is_some(),
        ),
        recent_transcripts,
        usage_view_model(&snapshot.usage, period),
    )
    .with_overlay_unavailable(snapshot.overlay_unavailable)
    .with_history_set_aside(
        snapshot
            .history_set_aside
            .as_ref()
            .map(|path| path.display().to_string()),
    )
}

fn transcript_view_model(
    entry: &agentdictate_core::HistorySnapshot,
    now: &DateTime<Local>,
) -> TranscriptViewModel {
    TranscriptViewModel::new(
        entry.id,
        format_history_time(entry.created_at, now),
        entry.text.clone(),
        entry.word_count,
        format_duration_clock(entry.duration_seconds),
    )
    .with_preview(entry.preview_text.clone())
}

fn usage_view_model(snapshot: &UsageSnapshot, period: UsagePeriod) -> UsageViewModel {
    let totals = match period {
        UsagePeriod::Last7Days => snapshot.last_7_days,
        UsagePeriod::Last30Days => snapshot.last_30_days,
        UsagePeriod::AllTime => snapshot.all_time,
    };
    let (activity, limit, weekly) = match period {
        UsagePeriod::Last7Days => (&snapshot.activity, Some(7), false),
        UsagePeriod::Last30Days => (&snapshot.activity, Some(30), false),
        UsagePeriod::AllTime => (&snapshot.weekly_activity, None, true),
    };
    let activity = activity
        .iter()
        .rev()
        .take(limit.unwrap_or(usize::MAX))
        .rev()
        .map(|day| {
            UsageDayViewModel::new(
                if weekly {
                    format!("Week of {}", day.date.format("%b %-d"))
                } else {
                    day.date.format("%b %-d").to_string()
                },
                day.totals.dictations,
                day.totals.words,
                day.totals.audio_seconds.round().max(0.0) as u64,
                day.totals.estimated_cost,
            )
        })
        .collect();
    UsageViewModel::new(period, ui_usage_totals(totals), activity)
}

fn ui_usage_totals(totals: UsageTotalsSnapshot) -> UsageTotals {
    UsageTotals {
        dictations: totals.dictations,
        words: totals.words,
        audio_seconds: totals.audio_seconds.round().max(0.0) as u64,
        estimated_cost_usd: totals.estimated_cost,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    use agentdictate_core::{
        AppSnapshot, ClientCommandKind, HistoryPageSnapshot, HistorySnapshot, ServerMessage,
        UsageDaySnapshot,
    };
    use agentdictate_runtime::{IpcHandler, IpcServer};
    use chrono::{TimeZone, Utc};
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn maps_workspace_data_and_switches_period_without_losing_activity() {
        let snapshot = WorkspaceSnapshot {
            history: HistoryPageSnapshot {
                rows: vec![HistorySnapshot {
                    id: 4,
                    created_at: Utc.with_ymd_and_hms(2026, 8, 18, 9, 30, 0).unwrap(),
                    preview_text: "one two three".into(),
                    text: "one two three".into(),
                    word_count: 3,
                    duration_seconds: 7.0,
                }],
                ..HistoryPageSnapshot::default()
            },
            usage: UsageSnapshot {
                last_7_days: UsageTotalsSnapshot {
                    dictations: 2,
                    ..UsageTotalsSnapshot::default()
                },
                last_30_days: UsageTotalsSnapshot {
                    dictations: 5,
                    ..UsageTotalsSnapshot::default()
                },
                all_time: UsageTotalsSnapshot {
                    dictations: 9,
                    ..UsageTotalsSnapshot::default()
                },
                activity: vec![UsageDaySnapshot {
                    date: chrono::NaiveDate::from_ymd_opt(2026, 8, 18).unwrap(),
                    totals: UsageTotalsSnapshot {
                        dictations: 2,
                        ..UsageTotalsSnapshot::default()
                    },
                }],
                weekly_activity: vec![UsageDaySnapshot {
                    date: chrono::NaiveDate::from_ymd_opt(2026, 8, 17).unwrap(),
                    totals: UsageTotalsSnapshot {
                        dictations: 9,
                        ..UsageTotalsSnapshot::default()
                    },
                }],
            },
            ..WorkspaceSnapshot::default()
        };

        let week = workspace_view_model(&snapshot, &snapshot.history, UsagePeriod::Last7Days);
        let all = workspace_view_model(&snapshot, &snapshot.history, UsagePeriod::AllTime);

        assert_eq!(week.history.transcripts[0].text, "one two three");
        assert_eq!(week.usage.totals.dictations, 2);
        assert_eq!(week.usage.activity[0].dictations, 2);
        assert_eq!(all.usage.totals.dictations, 9);
        assert_eq!(all.usage.activity.len(), 1);
        assert_eq!(all.usage.activity[0].dictations, 9);
        assert_eq!(all.usage.activity[0].label, "Week of Aug 17");
    }

    #[test]
    fn invalid_recovery_id_is_typed() {
        let directory = tempdir().unwrap();
        let client = WorkspaceClient::new(
            directory.path().join("runtime"),
            WorkspaceSnapshot::default(),
        );

        let error = client
            .perform(WorkspaceAction::RetryRecovery {
                id: "not-a-job-id".to_owned(),
                stage: RecoveryStage::Transcription,
            })
            .unwrap_err();

        match error {
            WorkspaceError::InvalidRecoveryId { id } => assert_eq!(id, "not-a-job-id"),
            error => panic!("expected InvalidRecoveryId, got {error:?}"),
        }
    }

    #[test]
    fn missing_socket_surfaces_ipc_error() {
        let directory = tempdir().unwrap();
        let client = WorkspaceClient::new(
            directory.path().join("missing-runtime"),
            WorkspaceSnapshot::default(),
        );

        let error = client.refresh().unwrap_err();

        match error {
            WorkspaceError::Ipc(agentdictate_runtime::IpcError::Io(error)) => {
                assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
            }
            error => panic!("expected Ipc, got {error:?}"),
        }
    }

    enum WorkspaceResponse {
        Rejected(String),
        LifecycleSnapshot,
    }

    struct WorkspaceResponseHandler {
        response: WorkspaceResponse,
    }

    impl IpcHandler for WorkspaceResponseHandler {
        fn snapshot(&self) -> ServerMessage {
            lifecycle_snapshot_message()
        }

        fn handle(&self, command: ClientCommand) -> ServerMessage {
            let ClientCommandKind::GetWorkspace = command.kind else {
                panic!("workspace client sent an unexpected command")
            };
            match &self.response {
                WorkspaceResponse::Rejected(message) => {
                    ServerMessage::command_rejected(message.clone())
                }
                WorkspaceResponse::LifecycleSnapshot => lifecycle_snapshot_message(),
            }
        }
    }

    fn lifecycle_snapshot_message() -> ServerMessage {
        ServerMessage::snapshot(
            AppSnapshot {
                workflow: agentdictate_core::Workflow::new().snapshot(),
                hotkey: agentdictate_core::HotkeyReadiness::Ready,
                recoverable_count: 0,
                last_transcript: None,
            },
            &agentdictate_core::Settings::default(),
        )
    }

    #[test]
    fn command_rejection_message_is_preserved() {
        let directory = tempdir().unwrap();
        let runtime_directory = directory.path().join("runtime");
        let server = IpcServer::bind(&runtime_directory).unwrap();
        let server_thread = std::thread::spawn(move || {
            server
                .serve_next(&WorkspaceResponseHandler {
                    response: WorkspaceResponse::Rejected("daemon said no".to_owned()),
                })
                .unwrap();
        });
        let client = WorkspaceClient::new(runtime_directory, WorkspaceSnapshot::default());

        let error = client.refresh().unwrap_err();

        match error {
            WorkspaceError::CommandRejected { message } => assert_eq!(message, "daemon said no"),
            error => panic!("expected CommandRejected, got {error:?}"),
        }
        server_thread.join().unwrap();
    }

    #[test]
    fn unexpected_workspace_response_is_typed() {
        let directory = tempdir().unwrap();
        let runtime_directory = directory.path().join("runtime");
        let server = IpcServer::bind(&runtime_directory).unwrap();
        let server_thread = std::thread::spawn(move || {
            server
                .serve_next(&WorkspaceResponseHandler {
                    response: WorkspaceResponse::LifecycleSnapshot,
                })
                .unwrap();
        });
        let client = WorkspaceClient::new(runtime_directory, WorkspaceSnapshot::default());

        let error = client.refresh().unwrap_err();

        assert!(matches!(error, WorkspaceError::UnexpectedLifecycleSnapshot));
        server_thread.join().unwrap();
    }

    struct WorkspaceHandler {
        snapshot: Arc<Mutex<WorkspaceSnapshot>>,
    }

    struct HistoryHandler;

    impl IpcHandler for HistoryHandler {
        fn snapshot(&self) -> ServerMessage {
            ServerMessage::snapshot(
                AppSnapshot {
                    workflow: agentdictate_core::Workflow::new().snapshot(),
                    hotkey: agentdictate_core::HotkeyReadiness::Ready,
                    recoverable_count: 0,
                    last_transcript: None,
                },
                &agentdictate_core::Settings::default(),
            )
        }

        fn handle(&self, command: ClientCommand) -> ServerMessage {
            let ClientCommandKind::GetHistoryPage { request } = command.kind else {
                panic!("history client sent an unexpected command")
            };
            assert_eq!(request.search, "needle");
            assert_eq!(request.page_size, DEFAULT_HISTORY_PAGE_SIZE);
            assert!(request.after.is_none());
            ServerMessage::history_page(HistoryPageSnapshot {
                search: request.search,
                total_matches: 3,
                cursor_restarted: false,
                next_cursor: None,
                rows: vec![HistorySnapshot {
                    id: 99,
                    created_at: Utc.with_ymd_and_hms(2026, 8, 18, 13, 0, 0).unwrap(),
                    preview_text: "needle transcript".into(),
                    text: "needle transcript".into(),
                    word_count: 2,
                    duration_seconds: 3.0,
                }],
            })
        }
    }

    #[test]
    fn history_search_updates_only_the_bounded_history_projection() {
        let directory = tempdir().unwrap();
        let runtime_directory = directory.path().join("runtime");
        let server = IpcServer::bind(&runtime_directory).unwrap();
        let server_thread = std::thread::spawn(move || server.serve_next(&HistoryHandler).unwrap());
        let client = WorkspaceClient::new(
            runtime_directory,
            WorkspaceSnapshot {
                history: HistoryPageSnapshot {
                    next_cursor: Some(HistoryPageCursor::new("stale-query-cursor")),
                    rows: vec![HistorySnapshot {
                        id: 7,
                        created_at: Utc.with_ymd_and_hms(2026, 8, 18, 12, 0, 0).unwrap(),
                        preview_text: "newest transcript".into(),
                        text: "newest transcript".into(),
                        word_count: 2,
                        duration_seconds: 2.0,
                    }],
                    ..HistoryPageSnapshot::default()
                },
                ..WorkspaceSnapshot::default()
            },
        );

        let workspace = client
            .perform(WorkspaceAction::SearchHistory {
                query: "needle".into(),
            })
            .unwrap();

        assert_eq!(workspace.history.search, "needle");
        assert_eq!(workspace.history.transcript_count, 3);
        assert_eq!(workspace.history.transcripts.len(), 1);
        assert_eq!(workspace.history.transcripts[0].text, "needle transcript");
        assert_eq!(workspace.recent_transcripts.len(), 1);
        assert_eq!(workspace.recent_transcripts[0].id, 7);
        assert_eq!(workspace.recent_transcripts[0].text, "newest transcript");
        server_thread.join().unwrap();
    }

    struct LoadMoreHistoryHandler;

    impl IpcHandler for LoadMoreHistoryHandler {
        fn snapshot(&self) -> ServerMessage {
            ServerMessage::snapshot(
                AppSnapshot {
                    workflow: agentdictate_core::Workflow::new().snapshot(),
                    hotkey: agentdictate_core::HotkeyReadiness::Ready,
                    recoverable_count: 0,
                    last_transcript: None,
                },
                &agentdictate_core::Settings::default(),
            )
        }

        fn handle(&self, command: ClientCommand) -> ServerMessage {
            let ClientCommandKind::GetHistoryPage { request } = command.kind else {
                panic!("history client sent an unexpected command")
            };
            assert!(request.search.is_empty());
            assert_eq!(request.page_size, 50);
            assert_eq!(
                request.after.as_ref().map(HistoryPageCursor::as_str),
                Some("page-one")
            );
            ServerMessage::history_page(HistoryPageSnapshot {
                search: request.search,
                total_matches: 88,
                cursor_restarted: false,
                next_cursor: Some(HistoryPageCursor::new("page-two")),
                rows: vec![HistorySnapshot {
                    id: 88,
                    created_at: Utc.with_ymd_and_hms(2026, 8, 17, 13, 0, 0).unwrap(),
                    preview_text: "older transcript page".into(),
                    text: "older transcript page".into(),
                    word_count: 3,
                    duration_seconds: 4.0,
                }],
            })
        }
    }

    #[test]
    fn loading_more_history_keeps_the_overview_on_the_newest_page() {
        let directory = tempdir().unwrap();
        let runtime_directory = directory.path().join("runtime");
        let server = IpcServer::bind(&runtime_directory).unwrap();
        let server_thread =
            std::thread::spawn(move || server.serve_next(&LoadMoreHistoryHandler).unwrap());
        let client = WorkspaceClient::new(
            runtime_directory,
            WorkspaceSnapshot {
                history: HistoryPageSnapshot {
                    next_cursor: Some(HistoryPageCursor::new("page-one")),
                    rows: vec![HistorySnapshot {
                        id: 89,
                        created_at: Utc.with_ymd_and_hms(2026, 8, 18, 13, 0, 0).unwrap(),
                        preview_text: "newer transcript page".into(),
                        text: "newer transcript page".into(),
                        word_count: 3,
                        duration_seconds: 3.0,
                    }],
                    ..HistoryPageSnapshot::default()
                },
                ..WorkspaceSnapshot::default()
            },
        );

        let workspace = client.perform(WorkspaceAction::LoadMoreHistory).unwrap();

        assert_eq!(workspace.history.transcript_count, 88);
        assert_eq!(
            workspace
                .history
                .transcripts
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            vec![89, 88]
        );
        assert_eq!(workspace.recent_transcripts.len(), 1);
        assert_eq!(workspace.recent_transcripts[0].id, 89);
        server_thread.join().unwrap();
    }

    struct RestartedHistoryCursorHandler;

    impl IpcHandler for RestartedHistoryCursorHandler {
        fn snapshot(&self) -> ServerMessage {
            HistoryHandler.snapshot()
        }

        fn handle(&self, command: ClientCommand) -> ServerMessage {
            let ClientCommandKind::GetHistoryPage { request } = command.kind else {
                panic!("history client sent an unexpected command")
            };
            assert_eq!(
                request.after.as_ref().map(HistoryPageCursor::as_str),
                Some("expired-cursor")
            );
            assert_eq!(request.page_size, 50);
            ServerMessage::history_page(HistoryPageSnapshot {
                search: request.search,
                total_matches: 1,
                cursor_restarted: true,
                next_cursor: None,
                rows: vec![HistorySnapshot {
                    id: 100,
                    created_at: Utc.with_ymd_and_hms(2026, 8, 19, 13, 0, 0).unwrap(),
                    preview_text: "fresh first page".into(),
                    text: "fresh first page".into(),
                    word_count: 3,
                    duration_seconds: 4.0,
                }],
            })
        }
    }

    #[test]
    fn an_expired_history_cursor_atomically_replaces_stale_rows_with_page_one() {
        let directory = tempdir().unwrap();
        let runtime_directory = directory.path().join("runtime");
        let server = IpcServer::bind(&runtime_directory).unwrap();
        let server_thread = std::thread::spawn(move || {
            server.serve_next(&RestartedHistoryCursorHandler).unwrap();
        });
        let client = WorkspaceClient::new(
            runtime_directory,
            WorkspaceSnapshot {
                history: HistoryPageSnapshot {
                    next_cursor: Some(HistoryPageCursor::new("expired-cursor")),
                    rows: vec![HistorySnapshot {
                        id: 99,
                        created_at: Utc.with_ymd_and_hms(2026, 8, 18, 13, 0, 0).unwrap(),
                        preview_text: "stale prior page".into(),
                        text: "stale prior page".into(),
                        word_count: 3,
                        duration_seconds: 3.0,
                    }],
                    ..HistoryPageSnapshot::default()
                },
                ..WorkspaceSnapshot::default()
            },
        );

        let workspace = client.perform(WorkspaceAction::LoadMoreHistory).unwrap();

        assert_eq!(workspace.history.transcript_count, 1);
        assert_eq!(workspace.history.transcripts.len(), 1);
        assert_eq!(workspace.history.transcripts[0].id, 100);
        assert_eq!(workspace.history.transcripts[0].text, "fresh first page");
        assert!(!workspace.history.has_more);
        server_thread.join().unwrap();
    }

    /// Answers one search for "needle" with two rows, then deletes row 12.
    struct DeleteFromSearchHandler;

    impl IpcHandler for DeleteFromSearchHandler {
        fn snapshot(&self) -> ServerMessage {
            HistoryHandler.snapshot()
        }

        fn handle(&self, command: ClientCommand) -> ServerMessage {
            match command.kind {
                ClientCommandKind::GetHistoryPage { request } => {
                    ServerMessage::history_page(HistoryPageSnapshot {
                        search: request.search,
                        total_matches: 2,
                        cursor_restarted: false,
                        next_cursor: None,
                        rows: [12, 11]
                            .into_iter()
                            .map(|id| HistorySnapshot {
                                id,
                                created_at: Utc.with_ymd_and_hms(2026, 8, 18, 13, 0, 0).unwrap(),
                                preview_text: "needle".into(),
                                text: "needle".into(),
                                word_count: 1,
                                duration_seconds: 1.0,
                            })
                            .collect(),
                    })
                }
                ClientCommandKind::DeleteHistory { id } => {
                    assert_eq!(id, 12);
                    ServerMessage::workspace(WorkspaceSnapshot::default())
                }
                _ => panic!("history client sent an unexpected command"),
            }
        }
    }

    #[test]
    fn a_deleted_transcript_leaves_the_searched_page_at_once() {
        let directory = tempdir().unwrap();
        let runtime_directory = directory.path().join("runtime");
        let server = IpcServer::bind(&runtime_directory).unwrap();
        let server_thread = std::thread::spawn(move || {
            for _ in 0..2 {
                server.serve_next(&DeleteFromSearchHandler).unwrap();
            }
        });
        let client = WorkspaceClient::new(runtime_directory, WorkspaceSnapshot::default());
        client
            .perform(WorkspaceAction::SearchHistory {
                query: "needle".into(),
            })
            .unwrap();

        let workspace = client
            .perform(WorkspaceAction::DeleteTranscript { id: 12 })
            .unwrap();

        assert_eq!(workspace.history.search, "needle");
        assert_eq!(workspace.history.transcript_count, 1);
        assert_eq!(
            workspace
                .history
                .transcripts
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            vec![11]
        );
        server_thread.join().unwrap();
    }

    impl IpcHandler for WorkspaceHandler {
        fn snapshot(&self) -> ServerMessage {
            ServerMessage::snapshot(
                AppSnapshot {
                    workflow: agentdictate_core::Workflow::new().snapshot(),
                    hotkey: agentdictate_core::HotkeyReadiness::Ready,
                    recoverable_count: 0,
                    last_transcript: None,
                },
                &agentdictate_core::Settings::default(),
            )
        }

        fn handle(&self, command: ClientCommand) -> ServerMessage {
            let ClientCommandKind::GetWorkspace = command.kind else {
                panic!("workspace watcher sent an unexpected command")
            };
            ServerMessage::workspace(self.snapshot.lock().unwrap().clone())
        }
    }

    #[test]
    fn database_wal_change_emits_a_fresh_workspace_view_model_without_polling() {
        let directory = tempdir().unwrap();
        let runtime_directory = directory.path().join("runtime");
        let database_file = directory.path().join("agentdictate.sqlite");
        std::fs::write(&database_file, []).unwrap();
        let server = IpcServer::bind(&runtime_directory).unwrap();
        let remote_snapshot = Arc::new(Mutex::new(WorkspaceSnapshot::default()));
        let server_snapshot = Arc::clone(&remote_snapshot);
        let server_thread = std::thread::spawn(move || {
            server
                .serve_next(&WorkspaceHandler {
                    snapshot: server_snapshot,
                })
                .unwrap();
        });
        let client = Arc::new(WorkspaceClient::new(
            runtime_directory,
            WorkspaceSnapshot::default(),
        ));
        let updates = client.watch(&database_file).unwrap();
        remote_snapshot.lock().unwrap().history.rows = vec![HistorySnapshot {
            id: 99,
            created_at: Utc.with_ymd_and_hms(2026, 8, 18, 13, 0, 0).unwrap(),
            preview_text: "fresh transcript".into(),
            text: "fresh transcript".into(),
            word_count: 2,
            duration_seconds: 3.0,
        }];

        std::fs::write(
            database_file.with_extension("sqlite-wal"),
            b"committed change",
        )
        .unwrap();

        let update = updates.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(update.history.transcripts.len(), 1);
        assert_eq!(update.history.transcripts[0].text, "fresh transcript");
        assert_eq!(client.view_model().unwrap(), update);
        server_thread.join().unwrap();
    }

    #[test]
    fn overlay_health_change_refreshes_the_workspace_without_a_database_write() {
        let directory = tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        let database = directory.path().join("history.sqlite");
        let server = IpcServer::bind(&runtime).unwrap();
        let server_thread = std::thread::spawn(move || {
            server
                .serve_next(&WorkspaceHandler {
                    snapshot: Arc::new(Mutex::new(WorkspaceSnapshot {
                        overlay_unavailable: true,
                        ..WorkspaceSnapshot::default()
                    })),
                })
                .unwrap();
        });
        let client = Arc::new(WorkspaceClient::new(
            runtime.clone(),
            WorkspaceSnapshot::default(),
        ));
        let updates = client.watch(&database).unwrap();
        std::fs::write(runtime.join(crate::OVERLAY_HEALTH_FILE), []).unwrap();
        assert!(
            updates
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .overlay_unavailable
        );
        assert!(client.view_model().unwrap().overlay_unavailable);
        server_thread.join().unwrap();
    }
}
