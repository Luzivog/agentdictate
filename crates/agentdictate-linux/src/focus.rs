use std::{ffi::OsString, time::Instant};

use crate::{
    command::{
        PlatformCapability, PlatformCommandError, PlatformExecutable, SystemCommandRunner,
        require_tools,
    },
    paste::{X11FocusObservation, parse_x11_focus},
};

#[derive(Clone, Debug)]
pub struct X11FocusObserver {
    runner: SystemCommandRunner,
    xdotool: PlatformExecutable,
    xprop: PlatformExecutable,
}

impl X11FocusObserver {
    pub fn new(
        runner: SystemCommandRunner,
        xdotool: PlatformExecutable,
        xprop: PlatformExecutable,
    ) -> Self {
        Self {
            runner,
            xdotool,
            xprop,
        }
    }

    pub fn for_system(runner: SystemCommandRunner) -> Self {
        Self::new(
            runner,
            PlatformExecutable::discover(crate::command::PlatformTool::Xdotool),
            PlatformExecutable::discover(crate::command::PlatformTool::Xprop),
        )
    }

    pub fn observe(&self, deadline: Instant) -> Result<X11FocusObservation, PlatformCommandError> {
        require_tools(
            PlatformCapability::FocusObservation,
            &[&self.xdotool, &self.xprop],
        )?;
        let window_id = self.runner.run_output(
            PlatformCapability::FocusObservation,
            &self.xdotool,
            &[OsString::from("getactivewindow")],
            deadline,
        )?;
        let window_id = std::str::from_utf8(&window_id)
            .map_err(|_| PlatformCommandError::UnexpectedOutput {
                tool: self.xdotool.tool(),
                detail: "active window id is not UTF-8",
            })?
            .trim();
        if window_id.is_empty() {
            return Err(PlatformCommandError::UnexpectedOutput {
                tool: self.xdotool.tool(),
                detail: "active window id is empty",
            });
        }
        let properties = self.runner.run_output(
            PlatformCapability::FocusObservation,
            &self.xprop,
            &[
                OsString::from("-id"),
                OsString::from(window_id),
                OsString::from("WM_CLASS"),
                OsString::from("_NET_WM_STATE"),
            ],
            deadline,
        )?;
        let properties = std::str::from_utf8(&properties).map_err(|_| {
            PlatformCommandError::UnexpectedOutput {
                tool: self.xprop.tool(),
                detail: "window properties are not UTF-8",
            }
        })?;
        Ok(parse_x11_focus(window_id, properties))
    }
}
