use std::{ffi::OsString, thread, time::Instant};

use crate::{
    command::{
        PlatformCapability, PlatformCommandError, PlatformExecutable, PlatformProcess,
        PlatformTool, SystemCommandRunner, require_tools,
    },
    paste::{ClipboardProtocol, ClipboardReadinessEvidence},
};

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
    wl_copy: PlatformExecutable,
    wl_paste: PlatformExecutable,
    xsel: PlatformExecutable,
}

impl CommandClipboard {
    pub fn new(
        runner: SystemCommandRunner,
        wl_copy: PlatformExecutable,
        wl_paste: PlatformExecutable,
        xsel: PlatformExecutable,
    ) -> Self {
        Self {
            runner,
            wl_copy,
            wl_paste,
            xsel,
        }
    }

    pub fn for_system(runner: SystemCommandRunner) -> Self {
        Self::new(
            runner,
            PlatformExecutable::discover(PlatformTool::WlCopy),
            PlatformExecutable::discover(PlatformTool::WlPaste),
            PlatformExecutable::discover(PlatformTool::Xsel),
        )
    }

    pub fn publish(
        &self,
        protocol: ClipboardProtocol,
        contents: &[u8],
        deadline: Instant,
    ) -> Result<ClipboardPublication, PlatformCommandError> {
        self.publish_selection(protocol, ClipboardSelection::Clipboard, contents, deadline)
    }

    pub fn publish_selection(
        &self,
        protocol: ClipboardProtocol,
        selection: ClipboardSelection,
        contents: &[u8],
        deadline: Instant,
    ) -> Result<ClipboardPublication, PlatformCommandError> {
        let capability = clipboard_capability(protocol);
        let (owner_tool, reader_tool) = match protocol {
            ClipboardProtocol::Wayland => (&self.wl_copy, &self.wl_paste),
            ClipboardProtocol::X11 => (&self.xsel, &self.xsel),
        };
        require_tools(capability, &[owner_tool, reader_tool])?;
        let owner_arguments = match (protocol, selection) {
            (ClipboardProtocol::Wayland, ClipboardSelection::Clipboard) => {
                vec![OsString::from("--foreground")]
            }
            (ClipboardProtocol::Wayland, ClipboardSelection::Primary) => {
                vec![OsString::from("--foreground"), OsString::from("--primary")]
            }
            (ClipboardProtocol::X11, _) => vec![
                OsString::from(match selection {
                    ClipboardSelection::Clipboard => "--clipboard",
                    ClipboardSelection::Primary => "--primary",
                }),
                OsString::from("--input"),
                OsString::from("--nodetach"),
            ],
        };
        let mut owner =
            self.runner
                .spawn_owner(capability, owner_tool, &owner_arguments, contents)?;

        loop {
            if !owner.is_alive()? {
                return Err(PlatformCommandError::Failed {
                    tool: owner.tool(),
                    code: None,
                    stderr: "clipboard owner exited before readiness was observed".into(),
                });
            }
            let read_arguments = match (protocol, selection) {
                (ClipboardProtocol::Wayland, ClipboardSelection::Clipboard) => {
                    vec![OsString::from("--no-newline")]
                }
                (ClipboardProtocol::Wayland, ClipboardSelection::Primary) => {
                    vec![OsString::from("--no-newline"), OsString::from("--primary")]
                }
                (ClipboardProtocol::X11, _) => {
                    vec![
                        OsString::from(match selection {
                            ClipboardSelection::Clipboard => "--clipboard",
                            ClipboardSelection::Primary => "--primary",
                        }),
                        OsString::from("--output"),
                    ]
                }
            };
            match self
                .runner
                .run_output(capability, reader_tool, &read_arguments, deadline)
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
                Err(error @ PlatformCommandError::Deadline { .. }) => return Err(error),
                Ok(_) | Err(_) if Instant::now() < deadline => thread::yield_now(),
                Ok(_) | Err(_) => {
                    return Err(PlatformCommandError::Deadline {
                        tool: reader_tool.tool(),
                    });
                }
            }
        }
    }
}

const fn clipboard_capability(protocol: ClipboardProtocol) -> PlatformCapability {
    match protocol {
        ClipboardProtocol::Wayland => PlatformCapability::WaylandClipboard,
        ClipboardProtocol::X11 => PlatformCapability::X11Clipboard,
    }
}
