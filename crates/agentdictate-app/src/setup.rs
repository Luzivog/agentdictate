//! What the settings window's Setup screen asks of the daemon and the
//! system: an API key check and a microphone test through the daemon, and
//! the pkexec grant of keyboard and paste access.

use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use agentdictate_core::{
    ApiKeyCheck, ClientCommand, ClientCommandKind, MicrophoneCheck, Readiness, ServerMessageKind,
};
use agentdictate_runtime::IpcClient;
use agentdictate_ui::{SetupActions, UiActionError, has_input_access};

use crate::{WorkspaceError, grant_native_access};

/// How long a grant waits for the shortcut listener to open the keyboards
/// it may now read.
const GRANT_SETTLE_TIME: Duration = Duration::from_secs(3);
const GRANT_RECHECK_INTERVAL: Duration = Duration::from_millis(200);

pub struct SetupClient {
    runtime_directory: PathBuf,
    /// Where the grant writes its helper; see `grant_native_access`.
    native_access: PathBuf,
}

impl SetupClient {
    /// A client for the daemon listening in `runtime_directory`.
    #[must_use]
    pub const fn new(runtime_directory: PathBuf, native_access: PathBuf) -> Self {
        Self {
            runtime_directory,
            native_access,
        }
    }

    /// Sends `command` on its own session and returns the daemon's reply,
    /// handing each interim message before it to `interim`.
    fn send(
        &self,
        command: ClientCommand,
        mut interim: impl FnMut(ServerMessageKind),
    ) -> Result<ServerMessageKind, WorkspaceError> {
        let (mut client, _) = IpcClient::connect(&self.runtime_directory)?;
        match client
            .send_reporting(command, |message| interim(message.kind))?
            .kind
        {
            ServerMessageKind::CommandRejected { error } => {
                Err(WorkspaceError::CommandRejected { message: error })
            }
            reply => Ok(reply),
        }
    }

    fn readiness(&self) -> Result<Readiness, WorkspaceError> {
        match self.send(ClientCommandKind::GetSnapshot.into(), |_| {})? {
            ServerMessageKind::Snapshot { snapshot, .. } => Ok(snapshot.readiness),
            _ => Err(WorkspaceError::UnexpectedResponse),
        }
    }
}

impl SetupActions for SetupClient {
    fn check_api_key(&self, api_key: Option<String>) -> Result<ApiKeyCheck, UiActionError> {
        match self.send(ClientCommand::check_api_key(api_key), |_| {})? {
            ServerMessageKind::ApiKeyChecked { outcome } => Ok(outcome),
            _ => Err(WorkspaceError::UnexpectedResponse.into()),
        }
    }

    /// Runs the grant helper through pkexec, then waits up to
    /// `GRANT_SETTLE_TIME` for the daemon to report the access. udev has
    /// applied the rule once the helper returns; the shortcut listener
    /// follows moments later.
    fn grant_access(&self) -> Result<Readiness, UiActionError> {
        grant_native_access(&self.native_access)?;
        let deadline = Instant::now() + GRANT_SETTLE_TIME;
        loop {
            let readiness = self.readiness()?;
            if has_input_access(&readiness) || Instant::now() >= deadline {
                return Ok(readiness);
            }
            thread::sleep(GRANT_RECHECK_INTERVAL);
        }
    }

    fn test_microphone(&self, level: &mut dyn FnMut(u8)) -> Result<MicrophoneCheck, UiActionError> {
        let reply = self.send(ClientCommandKind::TestMicrophone.into(), |message| {
            if let ServerMessageKind::MicrophoneLevel { level: heard } = message {
                level(heard);
            }
        })?;
        match reply {
            ServerMessageKind::MicrophoneTested { outcome } => Ok(outcome),
            _ => Err(WorkspaceError::UnexpectedResponse.into()),
        }
    }
}
