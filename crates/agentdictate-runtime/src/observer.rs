use std::path::Path;

use agentdictate_core::{HistoryPageRequest, HistoryPageSnapshot, WorkspaceSnapshot};

use crate::migrations::LATEST_VERSION;
use crate::{Runtime, RuntimeError};

/// The settings window's read-only view of the daemon's database. The window
/// reads History, usage and Recovery here, so they never wait for the daemon
/// or a dictation in progress; changes still go through daemon commands.
/// Opening it never migrates or reconciles anything.
pub struct DatabaseObserver {
    runtime: Runtime,
    /// `PRAGMA data_version` of what `workspace` last read.
    read_version: Option<i64>,
}

impl DatabaseObserver {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RuntimeError> {
        Ok(Self {
            runtime: Runtime::open_observer(path)?,
            read_version: None,
        })
    }

    /// Whether another connection committed a change since `workspace` last
    /// read, or nothing was read yet. A watcher uses it to skip file events
    /// that changed nothing, such as SQLite's automatic checkpoint.
    pub fn changed(&self) -> Result<bool, RuntimeError> {
        Ok(Some(self.data_version()?) != self.read_version)
    }

    /// Everything the settings window shows, read in one transaction so the
    /// parts agree: Recovery, Home's newest transcripts, `history` for the
    /// History page, and usage.
    pub fn workspace(
        &mut self,
        history: &HistoryPageRequest,
    ) -> Result<WorkspaceSnapshot, RuntimeError> {
        let read = self.runtime.connection.unchecked_transaction()?;
        self.require_known_schema()?;
        let version = self.data_version()?;
        let workspace = WorkspaceSnapshot {
            recoveries: self.runtime.recoveries()?,
            recent: self.runtime.history_page(&HistoryPageRequest::default())?,
            history: self.runtime.history_page(history)?,
            usage: self.runtime.usage()?,
        };
        read.finish()?;
        self.read_version = Some(version);
        Ok(workspace)
    }

    /// Changes whenever another connection commits; inside a transaction,
    /// it identifies the snapshot that transaction reads.
    fn data_version(&self) -> Result<i64, RuntimeError> {
        Ok(self
            .runtime
            .connection
            .pragma_query_value(None, "data_version", |row| row.get(0))?)
    }

    /// One History page, as `Runtime::history_page` reads it.
    pub fn history_page(
        &self,
        request: &HistoryPageRequest,
    ) -> Result<HistoryPageSnapshot, RuntimeError> {
        self.require_known_schema()?;
        self.runtime.history_page(request)
    }

    /// Refuses a database that a newer AgentDictate has migrated: its tables
    /// may no longer read the way this build expects.
    fn require_known_schema(&self) -> Result<(), RuntimeError> {
        let version: i64 =
            self.runtime
                .connection
                .pragma_query_value(None, "user_version", |row| row.get(0))?;
        if usize::try_from(version).is_ok_and(|version| version <= LATEST_VERSION) {
            Ok(())
        } else {
            Err(RuntimeError::NewerDatabase {
                version,
                latest: LATEST_VERSION,
            })
        }
    }
}
