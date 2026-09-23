//! The settings window's data. History, usage and Recovery are read straight
//! from the daemon's database, so they never wait for a dictation; changes go
//! through daemon commands, and IPC otherwise only carries the daemon's
//! status snapshot.

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
        Arc, Mutex, MutexGuard,
        mpsc::{Receiver, channel},
    },
    time::{Duration, Instant},
};

use agentdictate_core::{
    AppSnapshot, ClientCommandKind, HISTORY_CONTINUATION_PAGE_SIZE, HistoryPageRequest,
    HistoryPageSnapshot, JobId, ServerMessageKind, WorkspaceSnapshot,
};
use agentdictate_runtime::{DatabaseObserver, IpcClient, IpcError, RuntimeError};
use agentdictate_ui::{RecoveryStage, UsagePeriod, WorkspaceAction, WorkspaceViewModel};
use chrono::Local;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("workspace state is unavailable")]
    StateUnavailable,
    #[error("invalid recovery id {id}")]
    InvalidRecoveryId { id: String },
    /// The daemon speaks another protocol, or its database has a newer
    /// schema: this window is from an older AgentDictate.
    #[error("AgentDictate was updated — reopen this window")]
    Outdated,
    #[error(transparent)]
    Ipc(IpcError),
    #[error("could not read your history: {0}")]
    Database(RuntimeError),
    #[error("{message}")]
    CommandRejected { message: String },
    #[error("daemon did not answer with its status")]
    UnexpectedResponse,
    #[error("daemon did not answer a settings request with its settings")]
    UnexpectedSettingsResponse,
}

impl From<IpcError> for WorkspaceError {
    fn from(error: IpcError) -> Self {
        match error {
            IpcError::ProtocolVersion { .. } => Self::Outdated,
            error => Self::Ipc(error),
        }
    }
}

impl From<RuntimeError> for WorkspaceError {
    fn from(error: RuntimeError) -> Self {
        match error {
            RuntimeError::NewerDatabase { .. } => Self::Outdated,
            error => Self::Database(error),
        }
    }
}

/// How long the watcher keeps collecting file events after the first one
/// before it reads: long enough for a commit to finish, and for the burst of
/// commits a dictation makes to cost fewer reads.
const CHANGE_SETTLE_TIME: Duration = Duration::from_millis(30);

pub struct WorkspaceClient {
    runtime_directory: PathBuf,
    database_file: PathBuf,
    state: Mutex<WorkspaceClientState>,
}

struct WorkspaceClientState {
    /// Opened at the first read, since the daemon creates the database.
    database: Option<DatabaseObserver>,
    snapshot: WorkspaceSnapshot,
    status: AppSnapshot,
    period: UsagePeriod,
    /// Set once the daemon or its database turned out newer than this
    /// window. The database is not read after that; the window asks to be
    /// reopened instead.
    outdated: bool,
    /// Set while the daemon does not answer; the window keeps showing what
    /// it last read and says it is reconnecting.
    unreachable: bool,
}

impl WorkspaceClientState {
    fn view_model(&self) -> WorkspaceViewModel {
        WorkspaceViewModel::from_snapshot(&self.snapshot, self.period, &Local::now())
            .with_status(&self.status)
            .with_window_outdated(self.outdated)
            .with_daemon_unreachable(self.unreachable)
    }

    fn database(&mut self, path: &Path) -> Result<&mut DatabaseObserver, WorkspaceError> {
        let database = match self.database.take() {
            Some(database) => database,
            None => DatabaseObserver::open(path)?,
        };
        Ok(self.database.insert(database))
    }

    /// Re-reads everything unless the window is outdated, which a newer
    /// database makes it. The History page keeps its search and returns to
    /// its first page.
    fn reload(&mut self, path: &Path) -> Result<(), WorkspaceError> {
        if self.outdated {
            return Ok(());
        }
        let history = HistoryPageRequest {
            search: self.snapshot.history.search.clone(),
            ..HistoryPageRequest::default()
        };
        let read = self
            .database(path)
            .and_then(|database| database.workspace(&history).map_err(Into::into));
        match self.note_outdated(read) {
            Ok(snapshot) => self.snapshot = snapshot,
            Err(WorkspaceError::Outdated) => {}
            Err(error) => return Err(error),
        }
        Ok(())
    }

    /// Marks the window outdated when `result` says so.
    fn note_outdated<T>(&mut self, result: Result<T, WorkspaceError>) -> Result<T, WorkspaceError> {
        if matches!(result, Err(WorkspaceError::Outdated)) {
            self.outdated = true;
        }
        result
    }
}

impl WorkspaceClient {
    /// A client for the daemon listening in `runtime_directory` and its
    /// database at `database_file`, starting from the daemon's `status`.
    #[must_use]
    pub fn new(runtime_directory: PathBuf, database_file: PathBuf, status: AppSnapshot) -> Self {
        Self {
            runtime_directory,
            database_file,
            state: Mutex::new(WorkspaceClientState {
                database: None,
                snapshot: WorkspaceSnapshot::default(),
                status,
                period: UsagePeriod::Last30Days,
                outdated: false,
                unreachable: false,
            }),
        }
    }

    pub fn view_model(&self) -> Result<WorkspaceViewModel, WorkspaceError> {
        Ok(self.lock_state()?.view_model())
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, WorkspaceClientState>, WorkspaceError> {
        self.state
            .lock()
            .map_err(|_| WorkspaceError::StateUnavailable)
    }

    /// Re-reads the database. A database from a newer AgentDictate is not
    /// an error here: the view model asks to reopen the window instead.
    pub fn refresh(&self) -> Result<WorkspaceViewModel, WorkspaceError> {
        let mut state = self.lock_state()?;
        state.reload(&self.database_file)?;
        Ok(state.view_model())
    }

    /// Re-reads the database when another connection committed since the
    /// last read, so file events that changed nothing, such as a checkpoint,
    /// cost one pragma. An outdated window has nothing to read.
    fn refresh_if_changed(&self) -> Result<Option<WorkspaceViewModel>, WorkspaceError> {
        let mut state = self.lock_state()?;
        if !state.outdated && !state.database(&self.database_file)?.changed()? {
            return Ok(None);
        }
        state.reload(&self.database_file)?;
        Ok(Some(state.view_model()))
    }

    /// The daemon's runtime directory, where its socket is.
    pub(crate) fn runtime_directory(&self) -> &Path {
        &self.runtime_directory
    }

    /// Asks the daemon for its status: its readiness and the notices the
    /// window shows. A daemon on another protocol means this window is
    /// outdated; one that does not answer, that it is reconnecting.
    pub(crate) fn refresh_status(&self) -> Result<WorkspaceViewModel, WorkspaceError> {
        let status = IpcClient::connect(&self.runtime_directory)
            .map_err(WorkspaceError::from)
            .and_then(|(_, message)| match message.kind {
                ServerMessageKind::Snapshot { snapshot, .. } => Ok(snapshot),
                _ => Err(WorkspaceError::UnexpectedResponse),
            });
        let mut state = self.lock_state()?;
        match state.note_outdated(status) {
            Ok(status) => {
                state.status = status;
                state.unreachable = false;
            }
            Err(WorkspaceError::Outdated) => {}
            Err(WorkspaceError::Ipc(error)) => {
                tracing::info!(%error, "the daemon does not answer");
                state.unreachable = true;
            }
            Err(error) => return Err(error),
        }
        Ok(state.view_model())
    }

    /// Runs one window action. Searches and "Show more" only read the
    /// database; everything that changes data is a daemon command, after
    /// which the database is read again.
    pub fn perform(&self, action: WorkspaceAction) -> Result<WorkspaceViewModel, WorkspaceError> {
        let command = match action {
            WorkspaceAction::SearchHistory { query } => {
                return self.read_history(|database, _| {
                    database.history_page(&HistoryPageRequest {
                        search: query,
                        ..HistoryPageRequest::default()
                    })
                });
            }
            WorkspaceAction::LoadMoreHistory => {
                return self.read_history(|database, shown| {
                    let Some(after) = shown.next_cursor.clone() else {
                        return Ok(shown.clone());
                    };
                    let mut page = database.history_page(&HistoryPageRequest {
                        search: shown.search.clone(),
                        page_size: HISTORY_CONTINUATION_PAGE_SIZE,
                        after: Some(after),
                    })?;
                    // An expired cursor restarts at the first page, which
                    // replaces the rows shown.
                    if !page.cursor_restarted {
                        let mut rows = shown.rows.clone();
                        for row in page.rows {
                            if !rows.iter().any(|existing| existing.id == row.id) {
                                rows.push(row);
                            }
                        }
                        page.rows = rows;
                    }
                    Ok(page)
                });
            }
            WorkspaceAction::SelectUsagePeriod(period) => {
                let mut state = self.lock_state()?;
                state.period = period;
                return Ok(state.view_model());
            }
            WorkspaceAction::RetryRecovery { id, stage } => {
                let job_id = parse_job_id(id)?;
                match stage {
                    RecoveryStage::Transcription | RecoveryStage::Cancelled => {
                        ClientCommandKind::RetryTranscription { job_id }
                    }
                    RecoveryStage::Delivery => ClientCommandKind::RetryDelivery { job_id },
                }
            }
            WorkspaceAction::DeleteRecovery { id } => ClientCommandKind::DeleteRecovery {
                job_id: parse_job_id(id)?,
            },
            WorkspaceAction::CopyTranscript { id } => ClientCommandKind::CopyTranscript { id },
            WorkspaceAction::DeleteTranscript { id } => ClientCommandKind::DeleteHistory { id },
            WorkspaceAction::ClearHistory => ClientCommandKind::ClearHistory,
        };
        let status = self.send(command);
        let mut state = self.lock_state()?;
        state.status = state.note_outdated(status)?;
        // The daemon committed before it replied, so this read sees it.
        state.reload(&self.database_file)?;
        Ok(state.view_model())
    }

    /// Replaces the History page's rows with what `read` returns for the
    /// rows shown.
    fn read_history(
        &self,
        read: impl FnOnce(
            &DatabaseObserver,
            &HistoryPageSnapshot,
        ) -> Result<HistoryPageSnapshot, RuntimeError>,
    ) -> Result<WorkspaceViewModel, WorkspaceError> {
        let mut state = self.lock_state()?;
        let shown = state.snapshot.history.clone();
        let page = state
            .database(&self.database_file)
            .and_then(|database| read(database, &shown).map_err(Into::into));
        state.snapshot.history = state.note_outdated(page)?;
        Ok(state.view_model())
    }

    /// Sends one command on its own short session and returns the status
    /// the daemon replied with once the command was done.
    fn send(&self, command: ClientCommandKind) -> Result<AppSnapshot, WorkspaceError> {
        let (mut client, _) = IpcClient::connect(&self.runtime_directory)?;
        match client.send(command.into())?.kind {
            ServerMessageKind::Snapshot { snapshot, .. } => Ok(snapshot),
            ServerMessageKind::CommandRejected { error } => {
                Err(WorkspaceError::CommandRejected { message: error })
            }
            ServerMessageKind::HotkeyCaptured { .. }
            | ServerMessageKind::ApiKeyChecked { .. }
            | ServerMessageKind::MicrophoneLevel { .. }
            | ServerMessageKind::MicrophoneTested { .. } => Err(WorkspaceError::UnexpectedResponse),
        }
    }

    /// Watches the database, with its WAL and rollback journal, the overlay
    /// health and daemon `status` files, and the daemon's socket, and sends
    /// a fresh view model after each change: a commit re-reads the database;
    /// anything else asks the daemon for its status, which also notices a
    /// daemon that stopped or restarted. Events are collected for
    /// `CHANGE_SETTLE_TIME` first, and those that committed nothing are
    /// skipped. Once the daemon or its database turns out newer than this
    /// window, the watcher sends the view model that says so and stops.
    pub fn watch(self: &Arc<Self>) -> io::Result<Receiver<WorkspaceViewModel>> {
        let mut watcher = FileWatcher::empty()?;
        let database = watcher.add_database(&self.database_file)?;
        let status = [
            crate::OVERLAY_HEALTH_FILE,
            crate::STATUS_FILE,
            agentdictate_runtime::SOCKET_FILE_NAME,
        ]
        .map(|name| watcher.add_file(&self.runtime_directory.join(name)));
        let status = status.into_iter().collect::<io::Result<Vec<_>>>()?;
        let client = Arc::clone(self);
        let (sender, receiver) = channel();
        std::thread::Builder::new()
            .name("agentdictate-workspace-watch".into())
            .spawn(move || {
                loop {
                    let changed = watcher.wait_for_change().and_then(|mut changed| {
                        watcher
                            .settle(CHANGE_SETTLE_TIME, &mut changed)
                            .map(|()| changed)
                    });
                    let changed = match changed {
                        Ok(changed) => changed,
                        Err(error) => {
                            tracing::warn!(%error, "workspace file watcher stopped");
                            return;
                        }
                    };
                    let mut update = None;
                    if status.iter().any(|file| changed.contains(file)) {
                        match client.refresh_status() {
                            Ok(model) => update = Some(model),
                            Err(error) => {
                                tracing::warn!(%error, "could not read the daemon's status");
                            }
                        }
                    }
                    if changed.contains(&database) {
                        match client.refresh_if_changed() {
                            Ok(Some(model)) => update = Some(model),
                            Ok(None) => {}
                            Err(error) => {
                                tracing::warn!(%error, "could not read the database after a change");
                            }
                        }
                    }
                    if let Some(model) = update {
                        let outdated = model.window_outdated;
                        if sender.send(model).is_err() || outdated {
                            return;
                        }
                    }
                }
            })?;
        Ok(receiver)
    }
}

fn parse_job_id(id: String) -> Result<JobId, WorkspaceError> {
    id.parse()
        .map_err(|_| WorkspaceError::InvalidRecoveryId { id })
}

/// Waits, with inotify, for writes to a set of files. A file is watched by
/// name in its directory, so it may be replaced or not exist yet. Each
/// `add_*` call returns a number that `wait_for_change` reports when one of
/// its files changes.
pub(crate) struct FileWatcher {
    descriptor: File,
    /// Each watched file name, with the number of the call that added it.
    watched_names: Vec<(Vec<u8>, usize)>,
    additions: usize,
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
            additions: 0,
        })
    }

    /// Watches a SQLite database with its rollback journal and WAL.
    fn add_database(&mut self, database_file: &Path) -> io::Result<usize> {
        let parent = database_file
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        self.watch_directory(
            parent,
            libc::IN_MASK_ADD
                | libc::IN_MODIFY
                | libc::IN_CLOSE_WRITE
                | libc::IN_MOVED_TO
                | libc::IN_CREATE
                | libc::IN_DELETE
                | libc::IN_ATTRIB,
        )?;
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
        let addition = self.next_addition();
        self.watched_names.extend(
            [
                database_name.as_bytes().to_vec(),
                sidecar_name("-wal"),
                sidecar_name("-journal"),
            ]
            .map(|name| (name, addition)),
        );
        Ok(addition)
    }

    pub(crate) fn add_file(&mut self, path: &Path) -> io::Result<usize> {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("watch file has no parent"))?;
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("watch file has no name"))?;
        self.watch_directory(
            parent,
            libc::IN_MASK_ADD
                | libc::IN_CLOSE_WRITE
                | libc::IN_CREATE
                | libc::IN_MOVED_TO
                | libc::IN_DELETE,
        )?;
        let addition = self.next_addition();
        self.watched_names
            .push((name.as_bytes().to_vec(), addition));
        Ok(addition)
    }

    fn next_addition(&mut self) -> usize {
        self.additions += 1;
        self.additions - 1
    }

    /// Adds `mask` to the events watched in `directory`.
    fn watch_directory(&self, directory: &Path, mask: u32) -> io::Result<()> {
        let directory = CString::new(directory.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "watched directory contains a NUL byte",
            )
        })?;
        // SAFETY: the descriptor is owned by `self`, and `directory` owns a
        // NUL-terminated path for the duration of the call.
        let result = unsafe {
            libc::inotify_add_watch(self.descriptor.as_raw_fd(), directory.as_ptr(), mask)
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Blocks until watched files are written, created, moved into place or
    /// deleted, and returns the numbers of the `add_*` calls whose files
    /// changed: all of them when the kernel dropped events.
    pub(crate) fn wait_for_change(&mut self) -> io::Result<Vec<usize>> {
        let mut changed = Vec::new();
        while changed.is_empty() {
            self.read_events(&mut changed)?;
        }
        Ok(changed)
    }

    /// Adds what changes within `window` to `changed`. A write still in
    /// progress when the first event arrived, such as a SQLite commit, which
    /// updates its index after its log, has finished by then.
    fn settle(&mut self, window: Duration, changed: &mut Vec<usize>) -> io::Result<()> {
        let deadline = Instant::now() + window;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let mut descriptor = libc::pollfd {
                fd: self.descriptor.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let timeout = i32::try_from(remaining.as_millis()).unwrap_or(i32::MAX);
            // SAFETY: `descriptor` is one valid `pollfd` that outlives the call.
            match unsafe { libc::poll(&raw mut descriptor, 1, timeout) } {
                0 => return Ok(()),
                ready if ready > 0 => self.read_events(changed)?,
                _ => {
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::Interrupted {
                        return Err(error);
                    }
                }
            }
        }
    }

    /// Reads one batch of events, adding the numbers of the `add_*` calls
    /// whose files they concern to `changed`.
    fn read_events(&mut self, changed: &mut Vec<usize>) -> io::Result<()> {
        let mut buffer = [0_u8; 4096];
        let bytes_read = loop {
            match self.descriptor.read(&mut buffer) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "file change watcher closed",
                    ));
                }
                Ok(bytes_read) => break bytes_read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        };
        let mut offset = 0;
        while offset + std::mem::size_of::<libc::inotify_event>() <= bytes_read {
            // SAFETY: the bounds check above guarantees the fixed event
            // header is present. Inotify records need not be Rust-aligned,
            // so this uses an unaligned read.
            let event = unsafe {
                std::ptr::read_unaligned(buffer.as_ptr().add(offset).cast::<libc::inotify_event>())
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
                    "a watched directory is no longer watched",
                ));
            }
            let overflowed = event.mask & libc::IN_Q_OVERFLOW != 0;
            for (watched_name, addition) in &self.watched_names {
                if (overflowed || watched_name.as_slice() == name) && !changed.contains(addition) {
                    changed.push(*addition);
                }
            }
            offset = name_end;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use agentdictate_core::{ClientCommand, ServerMessage, Settings, Workflow};
    use agentdictate_runtime::{IpcHandler, IpcServer, Runtime};
    use tempfile::{TempDir, tempdir};

    use super::*;

    fn status() -> AppSnapshot {
        AppSnapshot {
            workflow: Workflow::new().snapshot(),
            readiness: agentdictate_core::Readiness::default(),
            recoverable_count: 0,
            overlay_unavailable: false,
            history_set_aside: None,
        }
    }

    /// A daemon's runtime directory and database, as the window finds them.
    struct Instance {
        _directory: TempDir,
        runtime: PathBuf,
        database: PathBuf,
    }

    impl Instance {
        /// History holds `transcripts`, a minute apart, the last one newest.
        fn with_transcripts(transcripts: &[String]) -> Self {
            let directory = tempdir().unwrap();
            let runtime = directory.path().join("runtime");
            std::fs::create_dir_all(&runtime).unwrap();
            let database = directory.path().join("agentdictate.sqlite");
            drop(Runtime::open(&database).unwrap());
            for (index, text) in transcripts.iter().enumerate() {
                let at = format!("2026-08-18T{:02}:{:02}:00Z", 8 + index / 60, index % 60);
                insert_dictation(&database, &at, text);
            }
            Self {
                _directory: directory,
                runtime,
                database,
            }
        }

        fn client(&self) -> WorkspaceClient {
            WorkspaceClient::new(self.runtime.clone(), self.database.clone(), status())
        }
    }

    fn insert_dictation(database: &Path, at: &str, text: &str) {
        rusqlite::Connection::open(database)
            .unwrap()
            .execute(
                r#"
                INSERT INTO dictations (
                    started_at, ended_at, duration_seconds, transcription_provider,
                    transcription_model, word_count, character_count, estimated_cost,
                    final_text
                ) VALUES (?1, ?1, 6, 'openai_api', 'gpt-transcribe', 2, 10, 0.01, ?2)
                "#,
                rusqlite::params![at, text],
            )
            .unwrap();
    }

    fn texts(transcripts: &[agentdictate_ui::TranscriptViewModel]) -> Vec<&str> {
        transcripts.iter().map(|row| row.text.as_str()).collect()
    }

    #[test]
    fn home_and_history_come_from_the_database_without_the_daemon() {
        let instance = Instance::with_transcripts(
            &(0..81)
                .map(|index| format!("entry {index}"))
                .collect::<Vec<_>>(),
        );
        let client = instance.client();

        let workspace = client.refresh().unwrap();

        assert_eq!(workspace.recent_transcripts.len(), 30);
        assert_eq!(workspace.recent_transcripts[0].text, "entry 80");
        assert_eq!(workspace.history.transcript_count, 81);
        let more = client.perform(WorkspaceAction::LoadMoreHistory).unwrap();
        assert_eq!(more.history.transcripts.len(), 80);
        assert!(more.history.has_more);
        assert_eq!(more.recent_transcripts.len(), 30);
        let searched = client
            .perform(WorkspaceAction::SearchHistory {
                query: "ENTRY 7".into(),
            })
            .unwrap();
        assert_eq!(searched.history.transcripts.len(), 11);
        assert_eq!(searched.history.transcripts[0].text, "entry 79");
        assert_eq!(searched.history.transcripts[10].text, "entry 7");
        assert_eq!(searched.recent_transcripts[0].text, "entry 80");
        let all_time = client
            .perform(WorkspaceAction::SelectUsagePeriod(UsagePeriod::AllTime))
            .unwrap();
        assert_eq!(all_time.usage.totals.dictations, 81);
        // A refresh keeps the search.
        assert_eq!(client.refresh().unwrap().history.search, "ENTRY 7");
    }

    /// Deletes the History row it is asked to, as the daemon would, and
    /// replies with a status that reports the overlay unavailable.
    struct DeletingDaemon {
        database: PathBuf,
    }

    impl IpcHandler for DeletingDaemon {
        fn snapshot(&self) -> ServerMessage {
            ServerMessage::snapshot(
                AppSnapshot {
                    overlay_unavailable: true,
                    ..status()
                },
                &Settings::default(),
            )
        }

        fn handle(&self, command: ClientCommand) -> ServerMessage {
            let ClientCommandKind::DeleteHistory { id } = command.kind else {
                panic!("the window sent an unexpected command")
            };
            let deleted = Runtime::open_background_writer(&self.database)
                .unwrap()
                .delete_history(id)
                .unwrap();
            assert!(deleted.is_some());
            self.snapshot()
        }
    }

    #[test]
    fn a_change_is_a_daemon_command_and_the_window_rereads_its_result() {
        let instance = Instance::with_transcripts(&[
            "needle one".to_owned(),
            "haystack".to_owned(),
            "needle two".to_owned(),
        ]);
        let server = IpcServer::bind(&instance.runtime).unwrap();
        let daemon = DeletingDaemon {
            database: instance.database.clone(),
        };
        let server_thread = std::thread::spawn(move || server.serve_next(&daemon).unwrap());
        let client = instance.client();
        client
            .perform(WorkspaceAction::SearchHistory {
                query: "needle".into(),
            })
            .unwrap();

        let workspace = client
            .perform(WorkspaceAction::DeleteTranscript { id: 3 })
            .unwrap();

        assert_eq!(workspace.history.search, "needle");
        assert_eq!(texts(&workspace.history.transcripts), ["needle one"]);
        assert_eq!(
            texts(&workspace.recent_transcripts),
            ["haystack", "needle one"]
        );
        assert!(workspace.overlay_unavailable);
        server_thread.join().unwrap();
    }

    struct RejectingDaemon;

    impl IpcHandler for RejectingDaemon {
        fn snapshot(&self) -> ServerMessage {
            ServerMessage::snapshot(status(), &Settings::default())
        }

        fn handle(&self, _command: ClientCommand) -> ServerMessage {
            ServerMessage::command_rejected("daemon said no")
        }
    }

    #[test]
    fn a_rejected_command_keeps_the_daemons_message() {
        let instance = Instance::with_transcripts(&[]);
        let server = IpcServer::bind(&instance.runtime).unwrap();
        let server_thread =
            std::thread::spawn(move || server.serve_next(&RejectingDaemon).unwrap());

        let error = instance
            .client()
            .perform(WorkspaceAction::ClearHistory)
            .unwrap_err();

        assert!(matches!(
            error,
            WorkspaceError::CommandRejected { message } if message == "daemon said no"
        ));
        server_thread.join().unwrap();
    }

    #[test]
    fn invalid_recovery_id_is_typed() {
        let instance = Instance::with_transcripts(&[]);

        let error = instance
            .client()
            .perform(WorkspaceAction::RetryRecovery {
                id: "not-a-job-id".to_owned(),
                stage: RecoveryStage::Transcription,
            })
            .unwrap_err();

        assert!(matches!(
            error,
            WorkspaceError::InvalidRecoveryId { id } if id == "not-a-job-id"
        ));
    }

    #[test]
    fn a_commit_updates_the_window_without_polling_or_the_daemon() {
        let instance = Instance::with_transcripts(&["older".to_owned()]);
        let client = Arc::new(instance.client());
        client.refresh().unwrap();
        let updates = client.watch().unwrap();

        insert_dictation(
            &instance.database,
            "2026-09-01T10:00:00Z",
            "fresh transcript",
        );

        let update = updates.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(update.recent_transcripts[0].text, "fresh transcript");
        assert_eq!(client.view_model().unwrap(), update);
    }

    struct UnavailableOverlayDaemon;

    impl IpcHandler for UnavailableOverlayDaemon {
        fn snapshot(&self) -> ServerMessage {
            ServerMessage::snapshot(
                AppSnapshot {
                    overlay_unavailable: true,
                    ..status()
                },
                &Settings::default(),
            )
        }

        fn handle(&self, _command: ClientCommand) -> ServerMessage {
            panic!("a status refresh sends no command")
        }
    }

    #[test]
    fn an_overlay_health_change_rereads_the_daemons_status() {
        let instance = Instance::with_transcripts(&[]);
        let server = IpcServer::bind(&instance.runtime).unwrap();
        let server_thread =
            std::thread::spawn(move || server.serve_next(&UnavailableOverlayDaemon).unwrap());
        let client = Arc::new(instance.client());
        let updates = client.watch().unwrap();

        std::fs::write(instance.runtime.join(crate::OVERLAY_HEALTH_FILE), []).unwrap();

        assert!(
            updates
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .overlay_unavailable
        );
        server_thread.join().unwrap();
    }

    /// Answers every session with a status whose readiness lacks an API key.
    struct KeylessDaemon;

    impl IpcHandler for KeylessDaemon {
        fn snapshot(&self) -> ServerMessage {
            ServerMessage::snapshot(
                AppSnapshot {
                    readiness: agentdictate_core::Readiness {
                        transcription_key: false,
                        ..agentdictate_core::Readiness::default()
                    },
                    ..status()
                },
                &Settings::default(),
            )
        }

        fn handle(&self, _command: ClientCommand) -> ServerMessage {
            panic!("a status refresh sends no command")
        }
    }

    /// The window follows the daemon's readiness when its `status` file
    /// changes, says it is reconnecting once the daemon's socket goes, and
    /// keeps what it showed meanwhile.
    #[test]
    fn a_status_change_rereads_readiness_and_a_stopped_daemon_shows_reconnecting() {
        let instance = Instance::with_transcripts(&["kept".to_owned()]);
        let server = IpcServer::bind(&instance.runtime).unwrap();
        let client = Arc::new(instance.client());
        client.refresh().unwrap();
        let updates = client.watch().unwrap();
        let server_thread = std::thread::spawn(move || {
            server.serve_next(&KeylessDaemon).unwrap();
            server
        });

        std::fs::write(instance.runtime.join(crate::STATUS_FILE), []).unwrap();
        let update = updates.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!update.readiness.transcription_key);
        assert_eq!(update.daemon_banner(), None);

        // Stopping the daemon removes its socket.
        drop(server_thread.join().unwrap());
        let update = updates.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            update.daemon_banner(),
            Some("Reconnecting to AgentDictate…")
        );
        assert!(!update.readiness.transcription_key);
        assert_eq!(update.recent_transcripts[0].text, "kept");
    }

    /// A daemon from a later release restarts in place of the one the
    /// window opened with. The window says to reopen it, once, keeps what it
    /// showed, and stops watching.
    #[test]
    fn a_daemon_from_another_release_asks_to_reopen_the_window() {
        let instance = Instance::with_transcripts(&["kept".to_owned()]);
        let listener =
            std::os::unix::net::UnixListener::bind(instance.runtime.join("agentdictate.sock"))
                .unwrap();
        let server_thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            io::Write::write_all(
                &mut stream,
                b"{\"protocol_version\":9999,\"message\":\"reshaped\"}\n",
            )
            .unwrap();
        });
        let client = Arc::new(instance.client());
        client.refresh().unwrap();
        let updates = client.watch().unwrap();

        std::fs::write(instance.runtime.join(crate::OVERLAY_HEALTH_FILE), []).unwrap();

        let update = updates.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(update.window_outdated);
        assert_eq!(update.recent_transcripts[0].text, "kept");
        assert!(matches!(
            updates.recv_timeout(Duration::from_secs(2)),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
        ));
        server_thread.join().unwrap();
    }

    #[test]
    fn a_database_from_a_newer_release_asks_to_reopen_the_window() {
        let instance = Instance::with_transcripts(&["unread".to_owned()]);
        rusqlite::Connection::open(&instance.database)
            .unwrap()
            .pragma_update(None, "user_version", 99)
            .unwrap();

        let workspace = instance.client().refresh().unwrap();

        assert!(workspace.window_outdated);
        assert!(workspace.recent_transcripts.is_empty());
    }
}
