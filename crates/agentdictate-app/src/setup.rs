//! What the settings window's Setup screen asks of the daemon and the
//! system: an API key check and a microphone test through the daemon, and
//! the pkexec grant of keyboard and paste access.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use agentdictate_core::{
    ApiKeyCheck, ClientCommand, ClientCommandKind, MicrophoneCheck, Readiness, ServerMessageKind,
};
use agentdictate_runtime::IpcClient;
use agentdictate_ui::{SetupActions, UiActionError, has_input_access};

use crate::{NativeAccessError, WorkspaceClient, WorkspaceError, grant_native_access};

/// How long a grant waits for the shortcut listener to open the keyboards
/// it may now read.
const GRANT_SETTLE_TIME: Duration = Duration::from_secs(3);
const GRANT_RECHECK_INTERVAL: Duration = Duration::from_millis(200);

pub struct SetupClient {
    /// The window's workspace, whose cached daemon status a grant refreshes.
    workspace: Arc<WorkspaceClient>,
    /// Where the grant writes its helper; see `grant_native_access`.
    native_access: PathBuf,
    grant: fn(&Path) -> Result<(), NativeAccessError>,
}

impl SetupClient {
    /// A client for the daemon that `workspace` reads.
    #[must_use]
    pub fn new(workspace: Arc<WorkspaceClient>, native_access: PathBuf) -> Self {
        Self {
            workspace,
            native_access,
            grant: grant_native_access,
        }
    }

    /// Sends `command` on its own session and returns the daemon's reply,
    /// handing each interim message before it to `interim`.
    fn send(
        &self,
        command: ClientCommand,
        mut interim: impl FnMut(ServerMessageKind),
    ) -> Result<ServerMessageKind, WorkspaceError> {
        let (mut client, _) = IpcClient::connect(self.workspace.runtime_directory())?;
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
    /// The status is read through the window's workspace, so its later
    /// updates, such as after a dictation, keep the new readiness.
    fn grant_access(&self) -> Result<Readiness, UiActionError> {
        (self.grant)(&self.native_access)?;
        let deadline = Instant::now() + GRANT_SETTLE_TIME;
        loop {
            let readiness = self.workspace.refresh_status()?.readiness;
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

#[cfg(test)]
mod tests {
    use agentdictate_core::{
        AppSnapshot, DesktopReadiness, HotkeyReadiness, ServerMessage, Settings, Workflow,
    };
    use agentdictate_runtime::{IpcHandler, IpcServer};

    use super::*;

    fn status(paste_access: bool) -> AppSnapshot {
        AppSnapshot {
            workflow: Workflow::new().snapshot(),
            readiness: Readiness {
                shortcut: HotkeyReadiness::Ready,
                transcription_key: true,
                desktop: DesktopReadiness {
                    paste_access,
                    ..DesktopReadiness::default()
                },
            },
            recoverable_count: 0,
            overlay_unavailable: false,
            history_set_aside: None,
        }
    }

    /// A daemon that can paste, as after a grant.
    struct GrantedDaemon;

    impl IpcHandler for GrantedDaemon {
        fn snapshot(&self) -> ServerMessage {
            ServerMessage::snapshot(status(true), &Settings::default())
        }

        fn handle(&self, _command: ClientCommand) -> ServerMessage {
            unreachable!("a status read needs no command")
        }
    }

    #[test]
    fn a_grant_updates_the_status_the_window_keeps() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        let server = IpcServer::bind(&runtime).unwrap();
        let serving = thread::spawn(move || server.serve_next(&GrantedDaemon).unwrap());
        let workspace = Arc::new(WorkspaceClient::new(
            runtime,
            directory.path().join("agentdictate.sqlite"),
            status(false),
        ));
        let setup = SetupClient {
            workspace: Arc::clone(&workspace),
            native_access: directory.path().join("native-access"),
            grant: |_| Ok(()),
        };

        let readiness = setup.grant_access().unwrap();

        serving.join().unwrap();
        assert!(readiness.desktop.paste_access);
        // The window's next update, such as after a dictation, keeps it.
        assert!(
            workspace
                .view_model()
                .unwrap()
                .readiness
                .desktop
                .paste_access
        );
    }
}
