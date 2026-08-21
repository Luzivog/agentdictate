use std::{
    collections::HashMap,
    io,
    os::{fd::AsRawFd, unix::net::UnixDatagram},
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver, RecvTimeoutError, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use evdev::Device;

use super::events::{
    DeviceOpenFailure, NativeHotkeyControl, NativeHotkeyError, NativeHotkeyEvent,
    NativeHotkeyReadiness,
};
use super::input::{InputDirectoryWatcher, poll_descriptor};
use super::worker::ListenerWorker;
use crate::hotkey::{DeviceId, HotkeySession, HotkeySpec, keyboard_event_paths};

/// A running evdev listener. Construction completes only after the initial
/// device scan and open attempts, making `readiness` safe for startup gating.
pub struct NativeHotkeyListener {
    readiness: NativeHotkeyReadiness,
    events: Receiver<NativeHotkeyEvent>,
    control: NativeHotkeyControl,
    worker: Option<JoinHandle<()>>,
}

/// Event-driven retry source for the rare case where the native listener
/// cannot be constructed or its worker terminates. It watches `/dev/input`,
/// falling back to `/dev` only when needed; no timer-based retry loop is used.
pub struct NativeHotkeyRetryWatcher {
    watcher: InputDirectoryWatcher,
}

impl NativeHotkeyRetryWatcher {
    pub fn new() -> io::Result<Self> {
        let watcher = InputDirectoryWatcher::new(Path::new("/dev/input"))
            .or_else(|_| InputDirectoryWatcher::new(Path::new("/dev")))?;
        Ok(Self { watcher })
    }

    /// Blocks until udev changes the input environment. The inotify watch is
    /// installed by `new`, so a change cannot be lost between failure and wait.
    pub fn wait(&self) -> io::Result<()> {
        let mut descriptor = poll_descriptor(self.watcher.as_raw_fd());
        loop {
            // SAFETY: `descriptor` remains live and mutable for the duration
            // of `poll`, which only writes its `revents` field.
            let result = unsafe { libc::poll(std::ptr::from_mut(&mut descriptor), 1, -1) };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if descriptor.revents != 0 {
                self.watcher.drain()?;
                return Ok(());
            }
        }
    }
}

impl NativeHotkeyListener {
    pub fn start(spec: HotkeySpec) -> Result<Self, NativeHotkeyError> {
        Self::start_with_discovery(spec, Arc::new(keyboard_event_paths))
    }

    pub(super) fn start_with_discovery(
        spec: HotkeySpec,
        discover: Arc<DiscoverDevices>,
    ) -> Result<Self, NativeHotkeyError> {
        // Install the inotify watch before scanning so a hotplug between the
        // initial scan and the poll loop cannot be lost.
        let watcher = InputDirectoryWatcher::new(Path::new("/dev/input"))
            .map_err(NativeHotkeyError::Watch)?;
        let paths = discover(&spec).map_err(NativeHotkeyError::Discover)?;
        let mut session = HotkeySession::new(spec.clone());
        let mut next_device_id = 1;
        let (devices, failed_devices) =
            open_initial_devices(paths.iter(), &mut session, &mut next_device_id);
        let readiness = NativeHotkeyReadiness {
            status: session.finish_initial_scan(),
            discovered_devices: paths.len(),
            failed_devices,
        };

        let (wake, control_reader) = UnixDatagram::pair().map_err(NativeHotkeyError::Control)?;
        wake.set_nonblocking(true)
            .map_err(NativeHotkeyError::Control)?;
        control_reader
            .set_nonblocking(true)
            .map_err(NativeHotkeyError::Control)?;
        let (command_sender, commands) = mpsc::channel();
        let control = NativeHotkeyControl {
            commands: command_sender,
            wake: Arc::new(wake),
        };
        let (event_sender, events) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("agentdictate-hotkey".into())
            .spawn(move || {
                ListenerWorker {
                    spec,
                    session,
                    devices,
                    next_device_id,
                    watcher,
                    control: control_reader,
                    commands,
                    events: event_sender,
                    discover,
                }
                .run();
            })
            .map_err(NativeHotkeyError::ThreadSpawn)?;

        Ok(Self {
            readiness,
            events,
            control,
            worker: Some(worker),
        })
    }

    pub fn readiness(&self) -> &NativeHotkeyReadiness {
        &self.readiness
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<NativeHotkeyEvent, RecvTimeoutError> {
        self.events.recv_timeout(timeout)
    }

    pub fn recv(&self) -> Result<NativeHotkeyEvent, mpsc::RecvError> {
        self.events.recv()
    }

    pub fn try_recv(&self) -> Result<NativeHotkeyEvent, TryRecvError> {
        self.events.try_recv()
    }

    pub fn control_handle(&self) -> NativeHotkeyControl {
        self.control.clone()
    }
}

pub(super) type DiscoverDevices = dyn Fn(&HotkeySpec) -> io::Result<Vec<PathBuf>> + Send + Sync;

impl Drop for NativeHotkeyListener {
    fn drop(&mut self) {
        let _ = self.control.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(super) struct OpenKeyboard {
    pub(super) id: DeviceId,
    pub(super) device: Device,
}

pub(super) fn open_initial_devices<'a>(
    paths: impl IntoIterator<Item = &'a PathBuf>,
    session: &mut HotkeySession,
    next_device_id: &mut DeviceId,
) -> (HashMap<PathBuf, OpenKeyboard>, Vec<DeviceOpenFailure>) {
    let mut devices = HashMap::new();
    let mut failures = Vec::new();
    for path in paths {
        match open_keyboard(path, *next_device_id) {
            Ok(keyboard) => {
                session.connect_device(keyboard.id);
                devices.insert(path.clone(), keyboard);
                *next_device_id += 1;
            }
            Err(error) => failures.push(DeviceOpenFailure {
                path: path.clone(),
                message: error.to_string(),
            }),
        }
    }
    (devices, failures)
}

pub(super) fn open_keyboard(path: &Path, id: DeviceId) -> io::Result<OpenKeyboard> {
    let device = Device::open(path)?;
    device.set_nonblocking(true)?;
    Ok(OpenKeyboard { id, device })
}
