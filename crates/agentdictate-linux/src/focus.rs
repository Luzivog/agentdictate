//! Reads the active X11 window (on X11 or XWayland) straight from the X
//! server, without spawning tools.

use std::{fmt, sync::mpsc, thread, time::Instant};

use x11rb::{
    connection::Connection,
    protocol::xproto::{AtomEnum, ConnectionExt as _},
};

use crate::paste::X11FocusObservation;

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        _NET_ACTIVE_WINDOW,
        _NET_WM_STATE,
        _NET_WM_STATE_FOCUSED,
    }
}

#[derive(Debug)]
pub enum FocusError {
    /// No X server was reachable, or one of its requests failed.
    Display(String),
    /// The X server names no active window.
    NoActiveWindow,
    /// The X server did not answer before the delivery deadline.
    Deadline,
}

impl fmt::Display for FocusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Display(error) => write!(formatter, "X server unavailable: {error}"),
            Self::NoActiveWindow => formatter.write_str("the X server reports no active window"),
            Self::Deadline => formatter.write_str("the X server did not answer in time"),
        }
    }
}

impl std::error::Error for FocusError {}

/// Reads the active window, its `WM_CLASS`, and whether it holds focus.
/// Each call opens its own connection, so an X server that restarted since
/// the last dictation is harmless. The read runs on a worker thread so a
/// hung X server costs at most the time until `deadline`.
pub fn observe_x11_focus(deadline: Instant) -> Result<X11FocusObservation, FocusError> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("agentdictate-focus".into())
        .spawn(move || {
            let _ = sender.send(read_active_window());
        })
        .map_err(|error| FocusError::Display(error.to_string()))?;
    receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| FocusError::Deadline)?
}

fn read_active_window() -> Result<X11FocusObservation, FocusError> {
    let failed = |error: &dyn fmt::Display| FocusError::Display(error.to_string());
    let (connection, screen) = x11rb::connect(None).map_err(|error| failed(&error))?;
    let root = connection.setup().roots[screen].root;
    let atoms = Atoms::new(&connection)
        .map_err(|error| failed(&error))?
        .reply()
        .map_err(|error| failed(&error))?;
    let window = connection
        .get_property(
            false,
            root,
            atoms._NET_ACTIVE_WINDOW,
            AtomEnum::WINDOW,
            0,
            1,
        )
        .map_err(|error| failed(&error))?
        .reply()
        .map_err(|error| failed(&error))?
        .value32()
        .and_then(|mut windows| windows.next())
        .filter(|window| *window != 0)
        .ok_or(FocusError::NoActiveWindow)?;
    // Both requests go out before either reply is awaited.
    let class = connection
        .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 1024)
        .map_err(|error| failed(&error))?;
    let state = connection
        .get_property(false, window, atoms._NET_WM_STATE, AtomEnum::ATOM, 0, 1024)
        .map_err(|error| failed(&error))?;
    let class = class.reply().map_err(|error| failed(&error))?;
    let state = state.reply().map_err(|error| failed(&error))?;
    Ok(focus_observation(
        window,
        &class.value,
        state.value32().into_iter().flatten(),
        atoms._NET_WM_STATE_FOCUSED,
    ))
}

/// Decodes the active window's raw properties: `WM_CLASS` holds the
/// NUL-terminated instance and class names, joined here with a space;
/// `_NET_WM_STATE` holds atoms, and the window has focus when they include
/// `focused_atom` (`_NET_WM_STATE_FOCUSED`).
pub fn focus_observation(
    window: u32,
    wm_class: &[u8],
    states: impl IntoIterator<Item = u32>,
    focused_atom: u32,
) -> X11FocusObservation {
    let window_class = wm_class
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(String::from_utf8_lossy)
        .collect::<Vec<_>>()
        .join(" ");
    X11FocusObservation {
        window_id: window,
        window_class,
        focused: states.into_iter().any(|state| state == focused_atom),
    }
}
