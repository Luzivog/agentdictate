use std::{
    fmt, io,
    os::unix::net::UnixDatagram,
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Sender},
    },
};

use crate::hotkey::{HotkeyListenerStatus, HotkeyParseError, HotkeySignal, HotkeySpec};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceOpenFailure {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeHotkeyReadiness {
    pub status: HotkeyListenerStatus,
    pub discovered_devices: usize,
    pub failed_devices: Vec<DeviceOpenFailure>,
}

impl NativeHotkeyReadiness {
    pub const fn is_ready(&self) -> bool {
        matches!(self.status, HotkeyListenerStatus::Ready { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeHotkeyEvent {
    Signal(HotkeySignal),
    Status(HotkeyListenerStatus),
    DeviceError(DeviceOpenFailure),
    DiscoveryError(String),
    Reconfigured { hotkey: String },
    ReconfigurationRejected { hotkey: String, reason: String },
    ControlError(String),
}

#[derive(Debug)]
pub enum NativeHotkeyControlError {
    Parse(HotkeyParseError),
    ReconfigurationRejected { hotkey: String, reason: String },
    ListenerStopped,
    Wake(io::Error),
}

impl fmt::Display for NativeHotkeyControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => write!(formatter, "invalid hotkey: {error}"),
            Self::ReconfigurationRejected { hotkey, reason } => {
                write!(formatter, "hotkey {hotkey} was rejected: {reason}")
            }
            Self::ListenerStopped => formatter.write_str("hotkey listener has stopped"),
            Self::Wake(_) => formatter.write_str("could not wake hotkey listener"),
        }
    }
}

impl std::error::Error for NativeHotkeyControlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Parse(error) => Some(error),
            Self::Wake(error) => Some(error),
            Self::ListenerStopped | Self::ReconfigurationRejected { .. } => None,
        }
    }
}

pub(super) enum ListenerCommand {
    Reconfigure {
        requested: HotkeySpec,
        response: mpsc::SyncSender<Result<(), ReconfigurationFailure>>,
    },
    Stop,
}

#[derive(Debug)]
pub(super) struct ReconfigurationFailure {
    pub(super) hotkey: String,
    pub(super) reason: String,
}

#[derive(Clone)]
pub struct NativeHotkeyControl {
    pub(super) commands: Sender<ListenerCommand>,
    pub(super) wake: Arc<UnixDatagram>,
}

impl NativeHotkeyControl {
    pub fn reconfigure(&self, spec: HotkeySpec) -> Result<(), NativeHotkeyControlError> {
        let (response_sender, response) = mpsc::sync_channel(1);
        self.send(ListenerCommand::Reconfigure {
            requested: spec,
            response: response_sender,
        })?;
        match response
            .recv()
            .map_err(|_| NativeHotkeyControlError::ListenerStopped)?
        {
            Ok(()) => Ok(()),
            Err(error) => Err(NativeHotkeyControlError::ReconfigurationRejected {
                hotkey: error.hotkey,
                reason: error.reason,
            }),
        }
    }

    pub fn reconfigure_text(&self, spec: &str) -> Result<(), NativeHotkeyControlError> {
        self.reconfigure(spec.parse().map_err(NativeHotkeyControlError::Parse)?)
    }

    pub(super) fn stop(&self) -> Result<(), NativeHotkeyControlError> {
        self.send(ListenerCommand::Stop)
    }

    fn send(&self, command: ListenerCommand) -> Result<(), NativeHotkeyControlError> {
        self.commands
            .send(command)
            .map_err(|_| NativeHotkeyControlError::ListenerStopped)?;
        match self.wake.send(&[1]) {
            Ok(_) => Ok(()),
            // A full datagram buffer already guarantees the worker's control
            // descriptor is readable; it will drain and process this command.
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(()),
            Err(error) => Err(NativeHotkeyControlError::Wake(error)),
        }
    }
}

#[derive(Debug)]
pub enum NativeHotkeyError {
    Watch(io::Error),
    Discover(io::Error),
    Control(io::Error),
    ThreadSpawn(io::Error),
}

impl fmt::Display for NativeHotkeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Watch(_) => formatter.write_str("could not watch Linux input devices"),
            Self::Discover(_) => formatter.write_str("could not discover Linux keyboards"),
            Self::Control(_) => {
                formatter.write_str("could not create hotkey listener control pipe")
            }
            Self::ThreadSpawn(_) => formatter.write_str("could not start hotkey listener thread"),
        }
    }
}

impl std::error::Error for NativeHotkeyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Watch(source)
            | Self::Discover(source)
            | Self::Control(source)
            | Self::ThreadSpawn(source) => Some(source),
        }
    }
}
