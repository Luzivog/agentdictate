use std::{
    collections::{HashMap, HashSet},
    ffi::CString,
    fmt, io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStrExt, net::UnixDatagram},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use evdev::{Device, EventType, InputEvent};

use crate::hotkey::{
    DeviceId, HotkeyListenerStatus, HotkeyParseError, HotkeySession, HotkeySignal, HotkeySpec,
    KeyInput, KeyState, keyboard_event_paths,
};

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

enum ListenerCommand {
    Reconfigure {
        requested: HotkeySpec,
        response: mpsc::SyncSender<Result<(), ReconfigurationFailure>>,
    },
    Stop,
}

#[derive(Debug)]
struct ReconfigurationFailure {
    hotkey: String,
    reason: String,
}

#[derive(Clone)]
pub struct NativeHotkeyControl {
    commands: Sender<ListenerCommand>,
    wake: Arc<UnixDatagram>,
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

    fn stop(&self) -> Result<(), NativeHotkeyControlError> {
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

    fn start_with_discovery(
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

type DiscoverDevices = dyn Fn(&HotkeySpec) -> io::Result<Vec<PathBuf>> + Send + Sync;

impl Drop for NativeHotkeyListener {
    fn drop(&mut self) {
        let _ = self.control.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct OpenKeyboard {
    id: DeviceId,
    device: Device,
}

struct ListenerWorker {
    spec: HotkeySpec,
    session: HotkeySession,
    devices: HashMap<PathBuf, OpenKeyboard>,
    next_device_id: DeviceId,
    watcher: InputDirectoryWatcher,
    control: UnixDatagram,
    commands: Receiver<ListenerCommand>,
    events: mpsc::Sender<NativeHotkeyEvent>,
    discover: Arc<DiscoverDevices>,
}

fn open_initial_devices<'a>(
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

fn open_keyboard(path: &Path, id: DeviceId) -> io::Result<OpenKeyboard> {
    let device = Device::open(path)?;
    device.set_nonblocking(true)?;
    Ok(OpenKeyboard { id, device })
}

impl ListenerWorker {
    fn run(self) {
        let Self {
            mut spec,
            mut session,
            mut devices,
            mut next_device_id,
            watcher,
            control,
            commands,
            events,
            discover,
        } = self;
        loop {
            let device_paths = devices.keys().cloned().collect::<Vec<_>>();
            let mut poll_descriptors = Vec::with_capacity(device_paths.len() + 2);
            poll_descriptors.push(poll_descriptor(control.as_raw_fd()));
            poll_descriptors.push(poll_descriptor(watcher.as_raw_fd()));
            poll_descriptors.extend(device_paths.iter().filter_map(|path| {
                devices
                    .get(path)
                    .map(|keyboard| poll_descriptor(keyboard.device.as_raw_fd()))
            }));

            // SAFETY: the pointer and length describe the live mutable vector, and
            // `poll` only mutates each `pollfd.revents` field before returning.
            let result = unsafe {
                libc::poll(
                    poll_descriptors.as_mut_ptr(),
                    poll_descriptors.len() as libc::nfds_t,
                    -1,
                )
            };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                let _ = events.send(NativeHotkeyEvent::DiscoveryError(error.to_string()));
                return;
            }
            if poll_descriptors[0].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0 {
                if let Err(error) = drain_control_wake(&control) {
                    let _ = events.send(NativeHotkeyEvent::ControlError(error.to_string()));
                }
                for command in commands.try_iter() {
                    match command {
                        ListenerCommand::Stop => return,
                        ListenerCommand::Reconfigure {
                            requested,
                            response,
                        } => {
                            let result = reconfigure_listener(
                                requested,
                                discover.as_ref(),
                                &mut spec,
                                &mut session,
                                &mut devices,
                                &mut next_device_id,
                                &events,
                            );
                            let _ = response.send(result);
                        }
                    }
                }
            }

            let previous_status = session.status();
            let mut disconnected = Vec::new();
            for (index, path) in device_paths.iter().enumerate() {
                let revents = poll_descriptors[index + 2].revents;
                if revents == 0 {
                    continue;
                }
                if revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                    disconnected.push(path.clone());
                    continue;
                }
                let Some(keyboard) = devices.get_mut(path) else {
                    continue;
                };
                let inputs = match keyboard.device.fetch_events() {
                    Ok(device_events) => device_events
                        .filter_map(evdev_key_input)
                        .collect::<Vec<_>>(),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => Vec::new(),
                    Err(error) => {
                        let _ = events.send(NativeHotkeyEvent::DeviceError(DeviceOpenFailure {
                            path: path.clone(),
                            message: error.to_string(),
                        }));
                        disconnected.push(path.clone());
                        Vec::new()
                    }
                };
                for input in inputs {
                    if let Some(signal) = session.input(keyboard.id, input) {
                        let _ = events.send(NativeHotkeyEvent::Signal(signal));
                    }
                }
            }
            for path in disconnected {
                disconnect_path(&path, &mut devices, &mut session, &events);
            }

            if poll_descriptors[1].revents & libc::POLLIN != 0 {
                if let Err(error) = watcher.drain() {
                    let _ = events.send(NativeHotkeyEvent::DiscoveryError(error.to_string()));
                }
                match discover(&spec) {
                    Ok(paths) => reconcile_devices(
                        paths,
                        &mut devices,
                        &mut session,
                        &mut next_device_id,
                        &events,
                    ),
                    Err(error) => {
                        let _ = events.send(NativeHotkeyEvent::DiscoveryError(error.to_string()));
                    }
                }
            }
            let status = session.status();
            if status != previous_status {
                let _ = events.send(NativeHotkeyEvent::Status(status));
            }
        }
    }
}

fn drain_control_wake(control: &UnixDatagram) -> io::Result<()> {
    let mut buffer = [0_u8; 64];
    loop {
        match control.recv(&mut buffer) {
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(error),
        }
    }
}

fn reconfigure_listener(
    requested: HotkeySpec,
    discover: &DiscoverDevices,
    current_spec: &mut HotkeySpec,
    session: &mut HotkeySession,
    devices: &mut HashMap<PathBuf, OpenKeyboard>,
    next_device_id: &mut DeviceId,
    events: &mpsc::Sender<NativeHotkeyEvent>,
) -> Result<(), ReconfigurationFailure> {
    let hotkey = requested.display().to_owned();
    let paths = match discover(&requested) {
        Ok(paths) => paths,
        Err(error) => {
            let reason = error.to_string();
            let _ = events.send(NativeHotkeyEvent::ReconfigurationRejected {
                hotkey: hotkey.clone(),
                reason: reason.clone(),
            });
            let _ = events.send(NativeHotkeyEvent::Status(session.status()));
            return Err(ReconfigurationFailure { hotkey, reason });
        }
    };

    let mut candidate_session = HotkeySession::new(requested.clone());
    let mut candidate_next_device_id = *next_device_id;
    let (candidate_devices, _) = open_initial_devices(
        paths.iter(),
        &mut candidate_session,
        &mut candidate_next_device_id,
    );
    let candidate_status = candidate_session.finish_initial_scan();
    if !matches!(candidate_status, HotkeyListenerStatus::Ready { .. }) {
        let reason = if paths.is_empty() {
            "no keyboard supports the requested hotkey".to_owned()
        } else {
            "no supporting keyboard could be opened".to_owned()
        };
        let _ = events.send(NativeHotkeyEvent::ReconfigurationRejected {
            hotkey: hotkey.clone(),
            reason: reason.clone(),
        });
        let _ = events.send(NativeHotkeyEvent::Status(session.status()));
        return Err(ReconfigurationFailure { hotkey, reason });
    }

    // Candidate discovery and every device open completed before this swap.
    // Replacing the session also clears every pressed key from the old spec.
    *current_spec = requested;
    *session = candidate_session;
    *devices = candidate_devices;
    *next_device_id = candidate_next_device_id;
    let _ = events.send(NativeHotkeyEvent::Reconfigured { hotkey });
    let _ = events.send(NativeHotkeyEvent::Status(candidate_status));
    Ok(())
}

fn reconcile_devices(
    paths: Vec<PathBuf>,
    devices: &mut HashMap<PathBuf, OpenKeyboard>,
    session: &mut HotkeySession,
    next_device_id: &mut DeviceId,
    events: &mpsc::Sender<NativeHotkeyEvent>,
) {
    let desired = paths.iter().cloned().collect::<HashSet<_>>();
    let removed = devices
        .keys()
        .filter(|path| !desired.contains(*path))
        .cloned()
        .collect::<Vec<_>>();
    for path in removed {
        disconnect_path(&path, devices, session, events);
    }
    for path in paths {
        if devices.contains_key(&path) {
            continue;
        }
        match open_keyboard(&path, *next_device_id) {
            Ok(keyboard) => {
                session.connect_device(keyboard.id);
                devices.insert(path, keyboard);
                *next_device_id += 1;
            }
            Err(error) => {
                let _ = events.send(NativeHotkeyEvent::DeviceError(DeviceOpenFailure {
                    path,
                    message: error.to_string(),
                }));
            }
        }
    }
}

fn disconnect_path(
    path: &Path,
    devices: &mut HashMap<PathBuf, OpenKeyboard>,
    session: &mut HotkeySession,
    events: &mpsc::Sender<NativeHotkeyEvent>,
) {
    if let Some(keyboard) = devices.remove(path)
        && let Some(signal) = session.disconnect_device(keyboard.id)
    {
        let _ = events.send(NativeHotkeyEvent::Signal(signal));
    }
}

pub fn evdev_key_input(event: InputEvent) -> Option<KeyInput> {
    if event.event_type() != EventType::KEY {
        return None;
    }
    let state = match event.value() {
        0 => KeyState::Released,
        1 => KeyState::Pressed,
        2 => KeyState::Repeated,
        _ => return None,
    };
    Some(KeyInput::new(event.code(), state))
}

const fn poll_descriptor(file_descriptor: i32) -> libc::pollfd {
    libc::pollfd {
        fd: file_descriptor,
        events: libc::POLLIN | libc::POLLERR | libc::POLLHUP,
        revents: 0,
    }
}

struct InputDirectoryWatcher {
    file: OwnedFd,
}

impl InputDirectoryWatcher {
    fn new(path: &Path) -> io::Result<Self> {
        // SAFETY: `inotify_init1` has no pointer parameters.
        let descriptor = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: ownership of the newly-created descriptor transfers here.
        let file = unsafe { OwnedFd::from_raw_fd(descriptor) };
        let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "input path contains a NUL byte",
            )
        })?;
        let mask = libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_MOVED_FROM
            | libc::IN_MOVED_TO
            | libc::IN_ATTRIB;
        // SAFETY: the C string is NUL-terminated and valid for this call.
        let watch = unsafe { libc::inotify_add_watch(file.as_raw_fd(), path.as_ptr(), mask) };
        if watch < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { file })
    }

    fn drain(&self) -> io::Result<()> {
        let mut buffer = [0_u8; 4096];
        loop {
            // SAFETY: the buffer points to writable memory of the stated size.
            let read = unsafe {
                libc::read(
                    self.file.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if read > 0 {
                continue;
            }
            if read == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                return Ok(());
            }
            return Err(error);
        }
    }
}

impl AsRawFd for InputDirectoryWatcher {
    fn as_raw_fd(&self) -> i32 {
        self.file.as_raw_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use evdev::{AttributeSet, KeyCode, uinput::VirtualDevice};
    use std::{sync::Mutex, time::Instant};

    #[test]
    fn native_listener_opens_polls_reads_and_reconnects_evdev_keyboards() {
        if !Path::new("/dev/uinput").exists() {
            return;
        }
        let Ok((mut keyboard, path)) = virtual_keyboard() else {
            return;
        };
        let discovered = Arc::new(Mutex::new(vec![path]));
        let discovery_state = Arc::clone(&discovered);
        let discover: Arc<DiscoverDevices> = Arc::new(move |_| {
            Ok(discovery_state
                .lock()
                .expect("discovery paths lock")
                .clone())
        });
        let listener = NativeHotkeyListener::start_with_discovery(
            "Ctrl+Space".parse().expect("valid hotkey"),
            discover,
        )
        .expect("native listener starts");

        if !listener.readiness().is_ready() {
            receive_until(&listener, |event| {
                matches!(
                    event,
                    NativeHotkeyEvent::Status(HotkeyListenerStatus::Ready { active_devices: 1 })
                )
            });
        }
        emit_chord(&mut keyboard);
        assert_eq!(
            receive_until(&listener, |event| {
                *event == NativeHotkeyEvent::Signal(HotkeySignal::Pressed)
            }),
            NativeHotkeyEvent::Signal(HotkeySignal::Pressed)
        );

        discovered.lock().expect("discovery paths lock").clear();
        drop(keyboard);
        let mut released = false;
        let mut unavailable = false;
        while !released || !unavailable {
            match receive_until(&listener, |event| {
                matches!(
                    event,
                    NativeHotkeyEvent::Signal(HotkeySignal::Released)
                        | NativeHotkeyEvent::Status(HotkeyListenerStatus::Unavailable {
                            active_devices: 0,
                        })
                )
            }) {
                NativeHotkeyEvent::Signal(HotkeySignal::Released) => released = true,
                NativeHotkeyEvent::Status(HotkeyListenerStatus::Unavailable {
                    active_devices: 0,
                }) => unavailable = true,
                _ => unreachable!("predicate only accepts disconnect events"),
            }
        }

        let (mut replacement, replacement_path) =
            virtual_keyboard().expect("replacement virtual keyboard");
        *discovered.lock().expect("discovery paths lock") = vec![replacement_path];
        receive_until(&listener, |event| {
            matches!(
                event,
                NativeHotkeyEvent::Status(HotkeyListenerStatus::Ready { active_devices: 1 })
            )
        });
        emit_chord(&mut replacement);
        assert_eq!(
            receive_until(&listener, |event| {
                *event == NativeHotkeyEvent::Signal(HotkeySignal::Pressed)
            }),
            NativeHotkeyEvent::Signal(HotkeySignal::Pressed)
        );
    }

    #[test]
    fn cloneable_control_reconfigures_the_live_listener_without_a_polling_delay() {
        if !Path::new("/dev/uinput").exists() {
            return;
        }
        let Ok((mut keyboard, path)) = virtual_keyboard() else {
            return;
        };
        let discovered = vec![path];
        let discover: Arc<DiscoverDevices> = Arc::new(move |_| Ok(discovered.clone()));
        let listener = NativeHotkeyListener::start_with_discovery(
            "Ctrl+Space".parse().expect("valid initial hotkey"),
            discover,
        )
        .expect("native listener starts");
        wait_until_ready(&listener);

        keyboard
            .emit(&[InputEvent::new(
                EventType::KEY.0,
                KeyCode::KEY_LEFTCTRL.code(),
                1,
            )])
            .expect("partial old chord is emitted");
        let control = listener.control_handle().clone();
        assert!(matches!(
            control.reconfigure_text("Ctrl+Hyper"),
            Err(NativeHotkeyControlError::Parse(_))
        ));
        control
            .reconfigure("F9".parse().expect("valid replacement hotkey"))
            .expect("reconfiguration is queued and wakes poll");
        receive_until(&listener, |event| {
            matches!(
                event,
                NativeHotkeyEvent::Status(HotkeyListenerStatus::Ready { active_devices: 1 })
            )
        });

        keyboard
            .emit(&[InputEvent::new(EventType::KEY.0, KeyCode::KEY_F9.code(), 1)])
            .expect("new hotkey is emitted");
        assert_eq!(
            receive_until(&listener, |event| {
                *event == NativeHotkeyEvent::Signal(HotkeySignal::Pressed)
            }),
            NativeHotkeyEvent::Signal(HotkeySignal::Pressed)
        );
    }

    #[test]
    fn failed_reconfiguration_keeps_the_ready_hotkey_active() {
        if !Path::new("/dev/uinput").exists() {
            return;
        }
        let Ok((mut keyboard, path)) = virtual_keyboard() else {
            return;
        };
        let discover: Arc<DiscoverDevices> = Arc::new(move |spec| {
            Ok((spec.display() == "Ctrl+Space")
                .then(|| path.clone())
                .into_iter()
                .collect())
        });
        let listener = NativeHotkeyListener::start_with_discovery(
            "Ctrl+Space".parse().expect("valid initial hotkey"),
            discover,
        )
        .expect("native listener starts");
        wait_until_ready(&listener);

        let error = listener
            .control_handle()
            .reconfigure_text("F9")
            .expect_err("the caller learns that the live listener rejected F9");
        assert!(error.to_string().contains("no keyboard supports"));
        receive_until(&listener, |event| {
            matches!(
                event,
                NativeHotkeyEvent::ReconfigurationRejected { hotkey, .. } if hotkey == "F9"
            )
        });
        assert_eq!(
            receive_until(&listener, |event| {
                matches!(
                    event,
                    NativeHotkeyEvent::Status(HotkeyListenerStatus::Ready { active_devices: 1 })
                )
            }),
            NativeHotkeyEvent::Status(HotkeyListenerStatus::Ready { active_devices: 1 })
        );

        emit_chord(&mut keyboard);
        assert_eq!(
            receive_until(&listener, |event| {
                *event == NativeHotkeyEvent::Signal(HotkeySignal::Pressed)
            }),
            NativeHotkeyEvent::Signal(HotkeySignal::Pressed)
        );
    }

    #[test]
    fn control_only_returns_success_after_the_worker_accepts_reconfiguration() {
        let (wake, control_reader) = UnixDatagram::pair().unwrap();
        wake.set_nonblocking(true).unwrap();
        control_reader.set_nonblocking(true).unwrap();
        let (command_sender, commands) = mpsc::channel();
        let control = NativeHotkeyControl {
            commands: command_sender,
            wake: Arc::new(wake),
        };
        let worker = thread::spawn(move || {
            let command = commands.recv().unwrap();
            let ListenerCommand::Reconfigure { response, .. } = command else {
                panic!("expected reconfiguration")
            };
            response
                .send(Err(ReconfigurationFailure {
                    hotkey: "F9".into(),
                    reason: "no keyboard supports the requested hotkey".into(),
                }))
                .unwrap();
            drop(control_reader);
        });

        let error = control
            .reconfigure_text("F9")
            .expect_err("rejection must be synchronous");

        assert!(matches!(
            error,
            NativeHotkeyControlError::ReconfigurationRejected { .. }
        ));
        worker.join().unwrap();
    }

    fn virtual_keyboard() -> io::Result<(VirtualDevice, PathBuf)> {
        let mut keys = AttributeSet::<KeyCode>::new();
        keys.insert(KeyCode::KEY_LEFTCTRL);
        keys.insert(KeyCode::KEY_SPACE);
        keys.insert(KeyCode::KEY_F9);
        let mut keyboard = VirtualDevice::builder()?
            .name("AgentDictate listener test")
            .with_keys(&keys)?
            .build()?;
        let path = keyboard
            .enumerate_dev_nodes_blocking()?
            .next()
            .transpose()?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "virtual event node"))?;
        Ok((keyboard, path))
    }

    fn emit_chord(keyboard: &mut VirtualDevice) {
        keyboard
            .emit(&[
                InputEvent::new(EventType::KEY.0, KeyCode::KEY_LEFTCTRL.code(), 1),
                InputEvent::new(EventType::KEY.0, KeyCode::KEY_SPACE.code(), 1),
            ])
            .expect("virtual chord is emitted");
    }

    fn wait_until_ready(listener: &NativeHotkeyListener) {
        if !listener.readiness().is_ready() {
            receive_until(listener, |event| {
                matches!(
                    event,
                    NativeHotkeyEvent::Status(HotkeyListenerStatus::Ready { active_devices: 1 })
                )
            });
        }
    }

    fn receive_until(
        listener: &NativeHotkeyListener,
        predicate: impl Fn(&NativeHotkeyEvent) -> bool,
    ) -> NativeHotkeyEvent {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = listener
                .recv_timeout(remaining)
                .expect("native listener event before deadline");
            if predicate(&event) {
                return event;
            }
        }
    }
}
