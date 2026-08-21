use std::{
    ffi::{CString, OsString},
    fs::File,
    io::{self, Read},
    os::{fd::FromRawFd, unix::ffi::OsStrExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, channel},
    },
};

use agentdictate_core::{
    ClientCommand, DEFAULT_HISTORY_PAGE_SIZE, HISTORY_CONTINUATION_PAGE_SIZE, HistoryPageCursor,
    JobId, ReplacementRule, ServerMessageKind, UsageSnapshot, UsageTotalsSnapshot,
    WorkspaceSnapshot,
};
use agentdictate_runtime::IpcClient;

use crate::daemon::OVERVIEW_RECENT_HISTORY_LIMIT;
use agentdictate_ui::{
    HistoryViewModel, ModelCatalogViewModel, RecoveryItemViewModel, RecoveryStage,
    ReplacementRuleViewModel, ReplacementsViewModel, TranscriptViewModel, UsageDayViewModel,
    UsagePeriod, UsageTotals, UsageViewModel, WorkspaceAction, WorkspaceViewModel,
};

pub struct WorkspaceClient {
    runtime_directory: PathBuf,
    next_request_id: AtomicU64,
    request_gate: Mutex<()>,
    state: Mutex<WorkspaceClientState>,
}

struct WorkspaceClientState {
    snapshot: WorkspaceSnapshot,
    period: UsagePeriod,
    history_next_cursor: Option<HistoryPageCursor>,
    history_customized: bool,
}

impl WorkspaceClient {
    #[must_use]
    pub fn new(runtime_directory: PathBuf, snapshot: WorkspaceSnapshot) -> Self {
        Self {
            runtime_directory,
            next_request_id: AtomicU64::new(10_000),
            request_gate: Mutex::new(()),
            state: Mutex::new(WorkspaceClientState {
                history_next_cursor: snapshot.history_next_cursor.clone(),
                snapshot,
                period: UsagePeriod::Last30Days,
                history_customized: false,
            }),
        }
    }

    pub fn view_model(&self) -> Result<WorkspaceViewModel, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "workspace state is unavailable")?;
        Ok(workspace_view_model(&state.snapshot, state.period))
    }

    pub fn perform(&self, action: WorkspaceAction) -> Result<WorkspaceViewModel, String> {
        if let WorkspaceAction::SearchHistory { query } = action {
            {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| "workspace state is unavailable")?;
                state.snapshot.history_search = query;
                state.history_next_cursor = None;
                state.history_customized = !state.snapshot.history_search.trim().is_empty();
            }
            return self.refresh_history_page(false);
        }
        if matches!(action, WorkspaceAction::LoadMoreHistory) {
            {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| "workspace state is unavailable")?;
                state.history_customized = true;
            }
            return self.refresh_history_page(true);
        }
        if let WorkspaceAction::SelectUsagePeriod(period) = action {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "workspace state is unavailable")?;
            state.period = period;
            return Ok(workspace_view_model(&state.snapshot, state.period));
        }

        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let command = match action {
            WorkspaceAction::RetryRecovery { id, stage } => {
                let job_id = id
                    .parse::<JobId>()
                    .map_err(|_| format!("invalid recovery id {id}"))?;
                match stage {
                    RecoveryStage::Transcription => {
                        ClientCommand::retry_transcription(request_id, job_id)
                    }
                    RecoveryStage::Delivery => ClientCommand::retry_delivery(request_id, job_id),
                }
            }
            WorkspaceAction::DeleteRecovery { id } => {
                let job_id = id
                    .parse::<JobId>()
                    .map_err(|_| format!("invalid recovery id {id}"))?;
                ClientCommand::delete_recovery(request_id, job_id)
            }
            WorkspaceAction::CopyTranscript { id } => {
                ClientCommand::copy_transcript(request_id, id)
            }
            WorkspaceAction::SearchHistory { .. } | WorkspaceAction::LoadMoreHistory => {
                unreachable!("handled above")
            }
            WorkspaceAction::CreateReplacement { draft } => ClientCommand::create_replacement(
                request_id,
                ReplacementRule {
                    id: None,
                    source_phrase: draft.source,
                    replacement_phrase: draft.replacement,
                    enabled: draft.enabled,
                    case_sensitive: draft.case_sensitive,
                    whole_word_only: draft.whole_word_only,
                },
            ),
            WorkspaceAction::UpdateReplacement { id, draft } => ClientCommand::update_replacement(
                request_id,
                ReplacementRule {
                    id: Some(id),
                    source_phrase: draft.source,
                    replacement_phrase: draft.replacement,
                    enabled: draft.enabled,
                    case_sensitive: draft.case_sensitive,
                    whole_word_only: draft.whole_word_only,
                },
            ),
            WorkspaceAction::SetReplacementEnabled { id, enabled } => {
                let state = self
                    .state
                    .lock()
                    .map_err(|_| "workspace state is unavailable")?;
                let mut rule = state
                    .snapshot
                    .replacements
                    .iter()
                    .find(|rule| rule.id == Some(id))
                    .cloned()
                    .ok_or_else(|| format!("replacement {id} was not found"))?;
                rule.enabled = enabled;
                ClientCommand::update_replacement(request_id, rule)
            }
            WorkspaceAction::DeleteReplacement { id } => {
                ClientCommand::delete_replacement(request_id, id)
            }
            WorkspaceAction::SelectUsagePeriod(_) => unreachable!("handled above"),
        };

        self.send_workspace_command(command)
    }

    /// Re-queries the daemon and atomically replaces the cached workspace.
    /// Requests from actions and filesystem refreshes are serialized so an
    /// older response cannot overwrite a newer local snapshot.
    pub fn refresh(&self) -> Result<WorkspaceViewModel, String> {
        let needs_history_refresh = {
            let state = self
                .state
                .lock()
                .map_err(|_| "workspace state is unavailable")?;
            state.history_customized
        };
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let workspace = self.send_workspace_command(ClientCommand::get_workspace(request_id))?;
        if needs_history_refresh {
            self.refresh_history_page(false)
        } else {
            Ok(workspace)
        }
    }

    fn refresh_history_page(&self, append: bool) -> Result<WorkspaceViewModel, String> {
        let (search, after) = {
            let state = self
                .state
                .lock()
                .map_err(|_| "workspace state is unavailable")?;
            (
                state.snapshot.history_search.clone(),
                if append {
                    state.history_next_cursor.clone()
                } else {
                    None
                },
            )
        };
        if append && after.is_none() {
            return self.view_model();
        }
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let _request = self
            .request_gate
            .lock()
            .map_err(|_| "workspace request state is unavailable")?;
        let (mut client, _) =
            IpcClient::connect(&self.runtime_directory).map_err(|error| error.to_string())?;
        let response = client
            .send(ClientCommand::get_history_page(
                request_id,
                search.clone(),
                if append {
                    HISTORY_CONTINUATION_PAGE_SIZE
                } else {
                    DEFAULT_HISTORY_PAGE_SIZE
                },
                after,
            ))
            .map_err(|error| error.to_string())?;
        let page = match response.kind {
            ServerMessageKind::HistoryPage { page, .. } => *page,
            ServerMessageKind::CommandRejected { error, .. } => return Err(error),
            ServerMessageKind::Snapshot { .. } | ServerMessageKind::Workspace { .. } => {
                return Err("daemon returned unrelated data for a history request".into());
            }
        };
        let mut state = self
            .state
            .lock()
            .map_err(|_| "workspace state is unavailable")?;
        if state.snapshot.history_search != page.search {
            return Ok(workspace_view_model(&state.snapshot, state.period));
        }
        if append && !page.cursor_restarted {
            for row in page.rows {
                if !state
                    .snapshot
                    .history
                    .iter()
                    .any(|existing| existing.id == row.id)
                {
                    state.snapshot.history.push(row);
                }
            }
        } else {
            state.snapshot.history = page.rows;
        }
        state.snapshot.history_total = page.total_matches;
        state.history_next_cursor = page.next_cursor.clone();
        state.snapshot.history_next_cursor = page.next_cursor;
        state.snapshot.history_has_more = state.history_next_cursor.is_some();
        state.snapshot.history_search = page.search;
        Ok(workspace_view_model(&state.snapshot, state.period))
    }

    fn send_workspace_command(&self, command: ClientCommand) -> Result<WorkspaceViewModel, String> {
        let _request = self
            .request_gate
            .lock()
            .map_err(|_| "workspace request state is unavailable")?;
        let (mut client, _) =
            IpcClient::connect(&self.runtime_directory).map_err(|error| error.to_string())?;
        let response = client.send(command).map_err(|error| error.to_string())?;
        let mut workspace = match response.kind {
            ServerMessageKind::Workspace { workspace, .. } => *workspace,
            ServerMessageKind::CommandRejected { error, .. } => return Err(error),
            ServerMessageKind::Snapshot { .. } => {
                return Err("daemon returned a lifecycle snapshot for a workspace request".into());
            }
            ServerMessageKind::HistoryPage { .. } => {
                return Err("daemon returned history data for a workspace request".into());
            }
        };
        let mut state = self
            .state
            .lock()
            .map_err(|_| "workspace state is unavailable")?;
        if state.history_customized {
            workspace.history = state.snapshot.history.clone();
            workspace.history_total = state.snapshot.history_total;
            workspace.history_has_more = state.snapshot.history_has_more;
            workspace.history_search = state.snapshot.history_search.clone();
            workspace.history_next_cursor = state.history_next_cursor.clone();
        } else {
            state.history_next_cursor = workspace.history_next_cursor.clone();
        }
        state.snapshot = workspace;
        Ok(workspace_view_model(&state.snapshot, state.period))
    }

    /// Watches SQLite database, rollback-journal, and WAL writes and emits a
    /// freshly queried workspace after each filesystem event batch. This is
    /// event-driven: callers do not need a refresh interval or debounce delay.
    pub fn watch(
        self: &Arc<Self>,
        database_file: impl AsRef<Path>,
    ) -> io::Result<Receiver<WorkspaceViewModel>> {
        let watcher = DatabaseChangeWatcher::new(database_file.as_ref())?;
        self.watch_changes(watcher)
    }

    /// Also observes the daemon's atomically replaced model catalog so a
    /// completed background refresh can update Settings without polling.
    pub fn watch_with_catalog(
        self: &Arc<Self>,
        database_file: impl AsRef<Path>,
        catalog_file: impl AsRef<Path>,
    ) -> io::Result<Receiver<WorkspaceViewModel>> {
        let watcher =
            DatabaseChangeWatcher::with_catalog(database_file.as_ref(), catalog_file.as_ref())?;
        self.watch_changes(watcher)
    }

    fn watch_changes(
        self: &Arc<Self>,
        mut watcher: DatabaseChangeWatcher,
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

struct DatabaseChangeWatcher {
    descriptor: File,
    watched_names: Vec<Vec<u8>>,
}

impl DatabaseChangeWatcher {
    fn new(database_file: &Path) -> io::Result<Self> {
        Self::for_files(database_file, None)
    }

    fn with_catalog(database_file: &Path, catalog_file: &Path) -> io::Result<Self> {
        Self::for_files(database_file, Some(catalog_file))
    }

    fn for_files(database_file: &Path, catalog_file: Option<&Path>) -> io::Result<Self> {
        // SAFETY: `inotify_init1` has no pointer parameters. On success the
        // returned descriptor is uniquely owned by `File` below.
        let raw_descriptor = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
        if raw_descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw_descriptor` was just returned by `inotify_init1` and has
        // not been wrapped or closed elsewhere.
        let descriptor = unsafe { File::from_raw_fd(raw_descriptor) };
        let mask = libc::IN_MODIFY
            | libc::IN_CLOSE_WRITE
            | libc::IN_MOVED_TO
            | libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_ATTRIB;
        for file in [Some(database_file), catalog_file].into_iter().flatten() {
            let parent = file
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let parent = CString::new(parent.as_os_str().as_bytes()).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "workspace directory contains a NUL byte",
                )
            })?;
            // SAFETY: the descriptor is live and `parent` owns a
            // NUL-terminated path for the duration of the call.
            if unsafe { libc::inotify_add_watch(raw_descriptor, parent.as_ptr(), mask) } < 0 {
                return Err(io::Error::last_os_error());
            }
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
        let mut watched_names = vec![
            database_name.as_bytes().to_vec(),
            sidecar_name("-wal"),
            sidecar_name("-journal"),
        ];
        if let Some(catalog_name) = catalog_file.and_then(Path::file_name) {
            watched_names.push(catalog_name.as_bytes().to_vec());
        }
        Ok(Self {
            descriptor,
            watched_names,
        })
    }

    fn wait_for_change(&mut self) -> io::Result<()> {
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

#[must_use]
pub fn workspace_view_model(
    snapshot: &WorkspaceSnapshot,
    period: UsagePeriod,
) -> WorkspaceViewModel {
    let recoveries = snapshot
        .recoveries
        .iter()
        .map(|entry| {
            let delivery = !entry.final_text.trim().is_empty()
                && (entry.delivery_ambiguous
                    || matches!(
                        entry.stage,
                        agentdictate_core::JobStage::ReadyToDeliver
                            | agentdictate_core::JobStage::Delivering
                            | agentdictate_core::JobStage::Failed
                    ));
            RecoveryItemViewModel::new(
                entry.job_id.to_string(),
                if delivery {
                    RecoveryStage::Delivery
                } else {
                    RecoveryStage::Transcription
                },
                entry.updated_at.format("%b %e, %H:%M UTC").to_string(),
                format_duration(entry.duration_seconds),
                entry
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "Recording saved safely".to_owned()),
                (!entry.final_text.trim().is_empty()).then(|| entry.final_text.clone()),
            )
        })
        .collect();
    let transcripts = snapshot.history.iter().map(transcript_view_model).collect();
    let recent_transcripts = snapshot
        .recent_history
        .iter()
        .take(OVERVIEW_RECENT_HISTORY_LIMIT)
        .map(transcript_view_model)
        .collect();
    let replacements = snapshot
        .replacements
        .iter()
        .filter_map(|rule| {
            Some(ReplacementRuleViewModel::new(
                rule.id?,
                rule.source_phrase.clone(),
                rule.replacement_phrase.clone(),
                rule.enabled,
                rule.case_sensitive,
                rule.whole_word_only,
            ))
        })
        .collect();
    WorkspaceViewModel::new(
        HistoryViewModel::from_page(
            recoveries,
            transcripts,
            snapshot.history_total,
            snapshot.history_search.clone(),
            snapshot.history_has_more,
        ),
        recent_transcripts,
        ReplacementsViewModel::new(replacements),
        usage_view_model(&snapshot.usage, period),
    )
    .with_model_catalog(ModelCatalogViewModel::from(snapshot.model_catalog.clone()))
}

fn transcript_view_model(entry: &agentdictate_core::HistorySnapshot) -> TranscriptViewModel {
    TranscriptViewModel::new(
        entry.id,
        entry.created_at.format("%b %e, %H:%M UTC").to_string(),
        entry.preview_text.clone(),
        entry.word_count,
        format_duration(entry.duration_seconds),
    )
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

fn format_duration(seconds: f64) -> String {
    let seconds = seconds.round().max(0.0) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    use agentdictate_core::{
        AppSnapshot, ClientCommandKind, HistoryPageSnapshot, HistorySnapshot, ModelCatalogEntry,
        ModelCatalogOrigin, ModelCatalogSnapshot, ModelCatalogStatus, ModelCatalogSupport,
        ServerMessage, UsageDaySnapshot,
    };
    use agentdictate_runtime::{IpcHandler, IpcServer};
    use chrono::{TimeZone, Utc};
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn maps_workspace_data_and_switches_period_without_losing_activity() {
        let snapshot = WorkspaceSnapshot {
            history: vec![HistorySnapshot {
                id: 4,
                created_at: Utc.with_ymd_and_hms(2026, 8, 18, 9, 30, 0).unwrap(),
                preview_text: "one two three".into(),
                word_count: 3,
                duration_seconds: 7.0,
            }],
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
            model_catalog: ModelCatalogSnapshot {
                transcription_models: vec![ModelCatalogEntry {
                    id: "account-transcriber".to_owned(),
                    origin: ModelCatalogOrigin::Account,
                    support: ModelCatalogSupport::Unverified,
                    reasoning_efforts: Vec::new(),
                }],
                status: ModelCatalogStatus::Builtin,
                ..ModelCatalogSnapshot::default()
            },
            ..WorkspaceSnapshot::default()
        };

        let week = workspace_view_model(&snapshot, UsagePeriod::Last7Days);
        let all = workspace_view_model(&snapshot, UsagePeriod::AllTime);

        assert_eq!(week.history.transcripts[0].text, "one two three");
        assert_eq!(week.usage.totals.dictations, 2);
        assert_eq!(week.usage.activity[0].dictations, 2);
        assert_eq!(
            week.model_catalog.transcription_models[0].label,
            "account-transcriber — compatibility unverified"
        );
        assert_eq!(all.usage.totals.dictations, 9);
        assert_eq!(all.usage.activity.len(), 1);
        assert_eq!(all.usage.activity[0].dictations, 9);
        assert_eq!(all.usage.activity[0].label, "Week of Aug 17");
    }

    struct WorkspaceHandler {
        snapshot: Arc<Mutex<WorkspaceSnapshot>>,
    }

    struct HistoryHandler;

    impl IpcHandler for HistoryHandler {
        fn snapshot(&self, request_id: u64) -> ServerMessage {
            ServerMessage::snapshot(
                request_id,
                AppSnapshot {
                    sequence: 0,
                    workflow: agentdictate_core::Workflow::new().snapshot(),
                    hotkey: agentdictate_core::HotkeyReadiness::Ready,
                    recoverable_count: 0,
                    last_transcript: None,
                },
                &agentdictate_core::Settings::default(),
            )
        }

        fn handle(&mut self, command: ClientCommand) -> ServerMessage {
            let ClientCommandKind::GetHistoryPage {
                request_id,
                request,
            } = command.kind
            else {
                panic!("history client sent an unexpected command")
            };
            assert_eq!(request.search, "needle");
            assert_eq!(request.page_size, 20);
            assert!(request.after.is_none());
            ServerMessage::history_page(
                request_id,
                HistoryPageSnapshot {
                    search: request.search,
                    total_matches: 3,
                    cursor_restarted: false,
                    next_cursor: None,
                    rows: vec![HistorySnapshot {
                        id: 99,
                        created_at: Utc.with_ymd_and_hms(2026, 8, 18, 13, 0, 0).unwrap(),
                        preview_text: "needle transcript".into(),
                        word_count: 2,
                        duration_seconds: 3.0,
                    }],
                },
            )
        }
    }

    #[test]
    fn history_search_updates_only_the_bounded_history_projection() {
        let directory = tempdir().unwrap();
        let runtime_directory = directory.path().join("runtime");
        let server = IpcServer::bind(&runtime_directory).unwrap();
        let server_thread =
            std::thread::spawn(move || server.serve_next(&mut HistoryHandler).unwrap());
        let client = WorkspaceClient::new(
            runtime_directory,
            WorkspaceSnapshot {
                history_next_cursor: Some(HistoryPageCursor::new("stale-query-cursor")),
                history_has_more: true,
                recent_history: vec![HistorySnapshot {
                    id: 7,
                    created_at: Utc.with_ymd_and_hms(2026, 8, 18, 12, 0, 0).unwrap(),
                    preview_text: "newest transcript".into(),
                    word_count: 2,
                    duration_seconds: 2.0,
                }],
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
        fn snapshot(&self, request_id: u64) -> ServerMessage {
            ServerMessage::snapshot(
                request_id,
                AppSnapshot {
                    sequence: 0,
                    workflow: agentdictate_core::Workflow::new().snapshot(),
                    hotkey: agentdictate_core::HotkeyReadiness::Ready,
                    recoverable_count: 0,
                    last_transcript: None,
                },
                &agentdictate_core::Settings::default(),
            )
        }

        fn handle(&mut self, command: ClientCommand) -> ServerMessage {
            let ClientCommandKind::GetHistoryPage {
                request_id,
                request,
            } = command.kind
            else {
                panic!("history client sent an unexpected command")
            };
            assert!(request.search.is_empty());
            assert_eq!(request.page_size, 50);
            assert_eq!(
                request.after.as_ref().map(HistoryPageCursor::as_str),
                Some("page-one")
            );
            ServerMessage::history_page(
                request_id,
                HistoryPageSnapshot {
                    search: request.search,
                    total_matches: 88,
                    cursor_restarted: false,
                    next_cursor: Some(HistoryPageCursor::new("page-two")),
                    rows: vec![HistorySnapshot {
                        id: 88,
                        created_at: Utc.with_ymd_and_hms(2026, 8, 17, 13, 0, 0).unwrap(),
                        preview_text: "older transcript page".into(),
                        word_count: 3,
                        duration_seconds: 4.0,
                    }],
                },
            )
        }
    }

    #[test]
    fn loading_more_history_does_not_replace_overview_recents() {
        let directory = tempdir().unwrap();
        let runtime_directory = directory.path().join("runtime");
        let server = IpcServer::bind(&runtime_directory).unwrap();
        let server_thread =
            std::thread::spawn(move || server.serve_next(&mut LoadMoreHistoryHandler).unwrap());
        let client = WorkspaceClient::new(
            runtime_directory,
            WorkspaceSnapshot {
                history: vec![HistorySnapshot {
                    id: 89,
                    created_at: Utc.with_ymd_and_hms(2026, 8, 18, 13, 0, 0).unwrap(),
                    preview_text: "newer transcript page".into(),
                    word_count: 3,
                    duration_seconds: 3.0,
                }],
                history_next_cursor: Some(HistoryPageCursor::new("page-one")),
                history_has_more: true,
                recent_history: vec![HistorySnapshot {
                    id: 7,
                    created_at: Utc.with_ymd_and_hms(2026, 8, 18, 12, 0, 0).unwrap(),
                    preview_text: "newest transcript".into(),
                    word_count: 2,
                    duration_seconds: 2.0,
                }],
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
        assert_eq!(workspace.recent_transcripts[0].id, 7);
        server_thread.join().unwrap();
    }

    struct RestartedHistoryCursorHandler;

    impl IpcHandler for RestartedHistoryCursorHandler {
        fn snapshot(&self, request_id: u64) -> ServerMessage {
            HistoryHandler.snapshot(request_id)
        }

        fn handle(&mut self, command: ClientCommand) -> ServerMessage {
            let ClientCommandKind::GetHistoryPage {
                request_id,
                request,
            } = command.kind
            else {
                panic!("history client sent an unexpected command")
            };
            assert_eq!(
                request.after.as_ref().map(HistoryPageCursor::as_str),
                Some("expired-cursor")
            );
            assert_eq!(request.page_size, 50);
            ServerMessage::history_page(
                request_id,
                HistoryPageSnapshot {
                    search: request.search,
                    total_matches: 1,
                    cursor_restarted: true,
                    next_cursor: None,
                    rows: vec![HistorySnapshot {
                        id: 100,
                        created_at: Utc.with_ymd_and_hms(2026, 8, 19, 13, 0, 0).unwrap(),
                        preview_text: "fresh first page".into(),
                        word_count: 3,
                        duration_seconds: 4.0,
                    }],
                },
            )
        }
    }

    #[test]
    fn an_expired_history_cursor_atomically_replaces_stale_rows_with_page_one() {
        let directory = tempdir().unwrap();
        let runtime_directory = directory.path().join("runtime");
        let server = IpcServer::bind(&runtime_directory).unwrap();
        let server_thread = std::thread::spawn(move || {
            server
                .serve_next(&mut RestartedHistoryCursorHandler)
                .unwrap();
        });
        let client = WorkspaceClient::new(
            runtime_directory,
            WorkspaceSnapshot {
                history: vec![HistorySnapshot {
                    id: 99,
                    created_at: Utc.with_ymd_and_hms(2026, 8, 18, 13, 0, 0).unwrap(),
                    preview_text: "stale prior page".into(),
                    word_count: 3,
                    duration_seconds: 3.0,
                }],
                history_next_cursor: Some(HistoryPageCursor::new("expired-cursor")),
                history_has_more: true,
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

    impl IpcHandler for WorkspaceHandler {
        fn snapshot(&self, request_id: u64) -> ServerMessage {
            ServerMessage::snapshot(
                request_id,
                AppSnapshot {
                    sequence: 0,
                    workflow: agentdictate_core::Workflow::new().snapshot(),
                    hotkey: agentdictate_core::HotkeyReadiness::Ready,
                    recoverable_count: 0,
                    last_transcript: None,
                },
                &agentdictate_core::Settings::default(),
            )
        }

        fn handle(&mut self, command: ClientCommand) -> ServerMessage {
            let ClientCommandKind::GetWorkspace { request_id } = command.kind else {
                panic!("workspace watcher sent an unexpected command")
            };
            ServerMessage::workspace(request_id, self.snapshot.lock().unwrap().clone())
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
                .serve_next(&mut WorkspaceHandler {
                    snapshot: server_snapshot,
                })
                .unwrap();
        });
        let client = Arc::new(WorkspaceClient::new(
            runtime_directory,
            WorkspaceSnapshot::default(),
        ));
        let updates = client.watch(&database_file).unwrap();
        remote_snapshot.lock().unwrap().history = vec![HistorySnapshot {
            id: 99,
            created_at: Utc.with_ymd_and_hms(2026, 8, 18, 13, 0, 0).unwrap(),
            preview_text: "fresh transcript".into(),
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
    fn atomic_model_catalog_change_emits_a_fresh_workspace_without_polling() {
        let directory = tempdir().unwrap();
        let runtime_directory = directory.path().join("runtime");
        let database_file = directory.path().join("data/agentdictate.sqlite");
        let catalog_file = directory.path().join("cache/model-catalog.json");
        std::fs::create_dir_all(database_file.parent().unwrap()).unwrap();
        std::fs::create_dir_all(catalog_file.parent().unwrap()).unwrap();
        std::fs::write(&database_file, []).unwrap();
        let server = IpcServer::bind(&runtime_directory).unwrap();
        let remote_snapshot = Arc::new(Mutex::new(WorkspaceSnapshot::default()));
        let server_snapshot = Arc::clone(&remote_snapshot);
        let server_thread = std::thread::spawn(move || {
            server
                .serve_next(&mut WorkspaceHandler {
                    snapshot: server_snapshot,
                })
                .unwrap();
        });
        let client = Arc::new(WorkspaceClient::new(
            runtime_directory,
            WorkspaceSnapshot::default(),
        ));
        let updates = client
            .watch_with_catalog(&database_file, &catalog_file)
            .unwrap();
        remote_snapshot.lock().unwrap().history_total = 41;

        let temporary = catalog_file.with_extension("json.tmp");
        std::fs::write(&temporary, b"fresh catalog").unwrap();
        std::fs::rename(temporary, catalog_file).unwrap();

        let update = updates.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(update.history.transcript_count, 41);
        server_thread.join().unwrap();
    }
}
