//! Selection ownership for both Wayland and X11 focus targets via `xsel`
//! over XWayland.
//!
//! wl-clipboard is deliberately not used: GNOME's compositor offers no
//! data-control protocol, so every `wl-copy`/`wl-paste` invocation creates a
//! transient Wayland toplevel to reach the clipboard. Those windows fire
//! window-added/removed through the shell at paste time and visibly
//! re-layout the taskbar. X11 selection ownership needs no mapped window,
//! and mutter's XWayland selection bridge carries both CLIPBOARD and PRIMARY
//! to Wayland-native applications in both directions. The readback below
//! verifies the X side only; the bridge is the compositor's responsibility.

use std::{
    ffi::OsString,
    thread,
    time::{Duration, Instant},
};

use crate::{
    command::{
        PlatformCapability, PlatformCommandError, PlatformExecutable, PlatformProcess,
        PlatformTool, SystemCommandRunner, require_tools,
    },
    paste::ClipboardReadinessEvidence,
};

/// Upper bound for one readback attempt; see the retry loop in
/// `publish_selection`.
const READBACK_ATTEMPT_BUDGET: Duration = Duration::from_millis(800);
/// Pause between readback attempts so retries don't hammer the compositor.
const READBACK_RETRY_GAP: Duration = Duration::from_millis(25);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardSelection {
    Clipboard,
    Primary,
}

#[derive(Debug)]
pub struct ClipboardPublication {
    pub evidence: ClipboardReadinessEvidence,
    pub owner: PlatformProcess,
}

#[derive(Clone, Debug)]
pub struct CommandClipboard {
    runner: SystemCommandRunner,
    xsel: PlatformExecutable,
}

impl CommandClipboard {
    pub fn new(runner: SystemCommandRunner, xsel: PlatformExecutable) -> Self {
        Self { runner, xsel }
    }

    pub fn for_system(runner: SystemCommandRunner) -> Self {
        Self::new(runner, PlatformExecutable::discover(PlatformTool::Xsel))
    }

    pub fn publish(
        &self,
        contents: &[u8],
        deadline: Instant,
    ) -> Result<ClipboardPublication, PlatformCommandError> {
        self.publish_selection(ClipboardSelection::Clipboard, contents, deadline)
    }

    pub fn publish_selection(
        &self,
        selection: ClipboardSelection,
        contents: &[u8],
        deadline: Instant,
    ) -> Result<ClipboardPublication, PlatformCommandError> {
        let capability = PlatformCapability::Clipboard;
        require_tools(capability, &[&self.xsel])?;
        let selection_flag = OsString::from(match selection {
            ClipboardSelection::Clipboard => "--clipboard",
            ClipboardSelection::Primary => "--primary",
        });
        let owner_arguments = vec![
            selection_flag.clone(),
            OsString::from("--input"),
            OsString::from("--nodetach"),
        ];
        let mut owner =
            self.runner
                .spawn_owner(capability, &self.xsel, &owner_arguments, contents)?;

        loop {
            if !owner.is_alive()? {
                return Err(PlatformCommandError::Failed {
                    tool: owner.tool(),
                    code: None,
                    stderr: "clipboard owner exited before readiness was observed".into(),
                });
            }
            let read_arguments = vec![selection_flag.clone(), OsString::from("--output")];
            // A readback can hang indefinitely on a stale selection offer
            // while ownership is still settling (seen under heavy system
            // load). Budget each attempt so one hung reader is killed and
            // retried instead of consuming the whole delivery deadline.
            let attempt_deadline = deadline.min(Instant::now() + READBACK_ATTEMPT_BUDGET);
            match self
                .runner
                .run_output(capability, &self.xsel, &read_arguments, attempt_deadline)
            {
                Ok(readback) if readback == contents => {
                    if !owner.is_alive()? {
                        return Err(PlatformCommandError::Failed {
                            tool: owner.tool(),
                            code: None,
                            stderr: "clipboard owner exited after readback".into(),
                        });
                    }
                    return Ok(ClipboardPublication {
                        evidence: ClipboardReadinessEvidence::ReadbackMatches,
                        owner,
                    });
                }
                Ok(_) | Err(_) if Instant::now() < deadline => {
                    thread::sleep(READBACK_RETRY_GAP);
                }
                Ok(_) | Err(_) => {
                    return Err(PlatformCommandError::Deadline {
                        tool: self.xsel.tool(),
                    });
                }
            }
        }
    }
}

