use std::{ffi::OsString, time::Instant};

use crate::{
    command::{
        PlatformCapability, PlatformCommandError, PlatformExecutable, PlatformTool,
        SystemCommandRunner, require_tools,
    },
    paste::{ClipboardProtocol, PasteShortcut},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InjectionReceipt {
    pub tool: PlatformTool,
}

#[derive(Clone, Debug)]
pub struct PasteInjector {
    runner: SystemCommandRunner,
    ydotool: PlatformExecutable,
    xdotool: PlatformExecutable,
}

impl PasteInjector {
    pub fn new(
        runner: SystemCommandRunner,
        ydotool: PlatformExecutable,
        xdotool: PlatformExecutable,
    ) -> Self {
        Self {
            runner,
            ydotool,
            xdotool,
        }
    }

    pub fn for_system(runner: SystemCommandRunner) -> Self {
        Self::new(
            runner,
            PlatformExecutable::discover(PlatformTool::Ydotool),
            PlatformExecutable::discover(PlatformTool::Xdotool),
        )
    }

    /// Sends exactly one paste command. Failures are intentionally returned to
    /// the delivery state machine because retrying an ambiguous injection can
    /// duplicate text in the focused application.
    pub fn inject(
        &self,
        protocol: ClipboardProtocol,
        shortcut: PasteShortcut,
        deadline: Instant,
    ) -> Result<InjectionReceipt, PlatformCommandError> {
        let shortcut = match shortcut {
            PasteShortcut::Standard => "ctrl+v",
            PasteShortcut::Terminal => "ctrl+shift+v",
        };
        let (capability, executable, arguments) = match protocol {
            ClipboardProtocol::Wayland => (
                PlatformCapability::WaylandPasteInjection,
                &self.ydotool,
                // Paced like a physical chord: busy application event loops
                // (Electron in particular) intermittently drop zero-gap
                // synthetic press/release bursts.
                vec![
                    OsString::from("key"),
                    OsString::from("--delay"),
                    OsString::from("50"),
                    OsString::from("--key-delay"),
                    OsString::from("25"),
                    OsString::from(shortcut),
                ],
            ),
            ClipboardProtocol::X11 => (
                PlatformCapability::X11PasteInjection,
                &self.xdotool,
                vec![
                    OsString::from("key"),
                    OsString::from("--clearmodifiers"),
                    OsString::from("--delay"),
                    OsString::from("50"),
                    OsString::from(shortcut),
                ],
            ),
        };
        require_tools(capability, &[executable])?;
        self.runner
            .run_output(capability, executable, &arguments, deadline)?;
        Ok(InjectionReceipt {
            tool: executable.tool(),
        })
    }
}
