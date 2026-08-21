use std::{
    collections::{HashMap, HashSet},
    io,
    os::{fd::AsRawFd, unix::net::UnixDatagram},
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
};

use super::events::{
    DeviceOpenFailure, ListenerCommand, NativeHotkeyEvent, ReconfigurationFailure,
};
use super::input::{InputDirectoryWatcher, evdev_key_input, poll_descriptor};
use crate::hotkey::{DeviceId, HotkeyListenerStatus, HotkeySession, HotkeySpec};

use super::listener::{DiscoverDevices, OpenKeyboard, open_initial_devices, open_keyboard};

pub(super) struct ListenerWorker {
    pub(super) spec: HotkeySpec,
    pub(super) session: HotkeySession,
    pub(super) devices: HashMap<PathBuf, OpenKeyboard>,
    pub(super) next_device_id: DeviceId,
    pub(super) watcher: InputDirectoryWatcher,
    pub(super) control: UnixDatagram,
    pub(super) commands: Receiver<ListenerCommand>,
    pub(super) events: mpsc::Sender<NativeHotkeyEvent>,
    pub(super) discover: Arc<DiscoverDevices>,
}

impl ListenerWorker {
    pub(super) fn run(self) {
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
