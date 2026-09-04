//! Primary-monitor placement for the unmanaged X11 recording surface.
//!
//! One connection observes RandR and desktop work-area changes. The recorder
//! never waits for these queries, and waveform updates never reposition a window.

use std::{
    io::{self, Write},
    os::{fd::AsRawFd, unix::net::UnixStream},
    thread::JoinHandle,
};

use x11rb::{
    connection::Connection,
    protocol::{
        Event,
        randr::{ConnectionExt as _, NotifyMask},
        xproto::{
            AtomEnum, ChangeWindowAttributesAux, ConfigureWindowAux, ConnectionExt as _, EventMask,
        },
    },
    rust_connection::RustConnection,
};

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        _NET_CURRENT_DESKTOP,
        _NET_WORKAREA,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScreenRect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

impl ScreenRect {
    fn intersect(self, other: Self) -> Option<Self> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (i64::from(self.x) + i64::from(self.width))
            .min(i64::from(other.x) + i64::from(other.width));
        let bottom = (i64::from(self.y) + i64::from(self.height))
            .min(i64::from(other.y) + i64::from(other.height));
        Some(Self {
            x,
            y,
            width: u32::try_from(right - i64::from(x))
                .ok()
                .filter(|n| *n > 0)?,
            height: u32::try_from(bottom - i64::from(y))
                .ok()
                .filter(|n| *n > 0)?,
        })
    }
}

fn desktop_work_area(values: &[u32], desktop: u32) -> Option<ScreenRect> {
    let start = usize::try_from(desktop).ok()?.checked_mul(4)?;
    let values = values.get(start..start.checked_add(4)?)?;
    Some(ScreenRect {
        // EWMH origins are signed coordinates carried in 32-bit CARDINALs.
        x: values[0] as i32,
        y: values[1] as i32,
        width: values[2],
        height: values[3],
    })
}

fn frame(area: ScreenRect, width: u32, height: u32, gap: u32) -> ScreenRect {
    let width = width.min(area.width);
    let gap = gap.min(area.height);
    let height = height.min(area.height - gap);
    ScreenRect {
        x: (i64::from(area.x) + i64::from((area.width - width) / 2)) as i32,
        y: (i64::from(area.y) + i64::from(area.height - gap - height)) as i32,
        width,
        height,
    }
}

struct DisplayConnection {
    connection: RustConnection,
    root: u32,
    atoms: Atoms,
}

impl DisplayConnection {
    fn open() -> io::Result<Self> {
        let (connection, screen) = x11rb::connect(None).map_err(io::Error::other)?;
        let root = connection.setup().roots[screen].root;
        let atoms = Atoms::new(&connection)
            .map_err(io::Error::other)?
            .reply()
            .map_err(io::Error::other)?;
        let version = connection
            .randr_query_version(1, 5)
            .map_err(io::Error::other)?
            .reply()
            .map_err(io::Error::other)?;
        if (version.major_version, version.minor_version) < (1, 5) {
            return Err(io::Error::other(
                "recording overlay requires RandR 1.5 monitor discovery",
            ));
        }
        connection
            .randr_select_input(
                root,
                NotifyMask::SCREEN_CHANGE
                    | NotifyMask::CRTC_CHANGE
                    | NotifyMask::OUTPUT_CHANGE
                    | NotifyMask::RESOURCE_CHANGE,
            )
            .map_err(io::Error::other)?
            .check()
            .map_err(io::Error::other)?;
        connection
            .change_window_attributes(
                root,
                &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
            )
            .map_err(io::Error::other)?
            .check()
            .map_err(io::Error::other)?;
        Ok(Self {
            connection,
            root,
            atoms,
        })
    }

    fn property(&self, atom: u32) -> io::Result<Vec<u32>> {
        let reply = self
            .connection
            .get_property(false, self.root, atom, AtomEnum::CARDINAL, 0, 4096)
            .map_err(io::Error::other)?
            .reply()
            .map_err(io::Error::other)?;
        Ok(reply.value32().map(Iterator::collect).unwrap_or_default())
    }

    fn work_area(&self) -> io::Result<ScreenRect> {
        let reply = self
            .connection
            .randr_get_monitors(self.root, true)
            .map_err(io::Error::other)?
            .reply()
            .map_err(io::Error::other)?;
        let monitors: Vec<_> = reply
            .monitors
            .iter()
            .filter(|m| m.width > 0 && m.height > 0)
            .collect();
        // Some compositors expose no primary flag. RandR's first active monitor
        // is the fallback, never the combined root-screen dimensions.
        let primary = monitors
            .iter()
            .find(|m| m.primary)
            .or(monitors.first())
            .ok_or_else(|| io::Error::other("no active monitor for recording overlay"))?;
        let monitor = ScreenRect {
            x: primary.x.into(),
            y: primary.y.into(),
            width: primary.width.into(),
            height: primary.height.into(),
        };
        let desktop = self
            .property(self.atoms._NET_CURRENT_DESKTOP)?
            .first()
            .copied()
            .unwrap_or(0);
        let work_area = desktop_work_area(&self.property(self.atoms._NET_WORKAREA)?, desktop);
        Ok(work_area
            .and_then(|area| monitor.intersect(area))
            .unwrap_or(monitor))
    }

    fn place(&self, window: u32, size: [u32; 3], work_area: ScreenRect) -> io::Result<ScreenRect> {
        let bounds = frame(work_area, size[0], size[1], size[2]);
        if bounds.width == 0 || bounds.height == 0 {
            return Err(io::Error::other(
                "primary monitor work area cannot fit the recording overlay",
            ));
        }
        self.connection
            .configure_window(
                window,
                &ConfigureWindowAux::new()
                    .x(bounds.x)
                    .y(bounds.y)
                    .width(bounds.width)
                    .height(bounds.height),
            )
            .map_err(io::Error::other)?
            .check()
            .map_err(io::Error::other)?;
        self.connection.flush().map_err(io::Error::other)?;
        tracing::info!(
            window,
            ?work_area,
            ?bounds,
            "recording overlay positioned on primary monitor"
        );
        Ok(bounds)
    }
}

/// Owns monitor observation for one helper window. Dropping it wakes and joins
/// the worker; no native event subscription survives the helper session.
pub struct OverlayPlacementWatcher {
    stop: UnixStream,
    worker: Option<JoinHandle<()>>,
}

impl OverlayPlacementWatcher {
    pub fn start(
        window: u32,
        scale: f32,
        logical_size: [u32; 3],
        on_error: impl FnOnce(io::Error) + Send + 'static,
    ) -> io::Result<Self> {
        if !scale.is_finite() || scale <= 0.0 {
            return Err(io::Error::other("invalid overlay scale factor"));
        }
        let size = logical_size.map(|n| (n as f32 * scale).round() as u32);
        let display = DisplayConnection::open()?;
        let area = display.work_area()?;
        display.place(window, size, area)?;
        let (stop, stopped) = UnixStream::pair()?;
        let worker = std::thread::Builder::new()
            .name("agentdictate-overlay-placement".into())
            .spawn(move || {
                if let Err(error) = watch(display, window, size, area, stopped) {
                    on_error(error);
                }
            })?;
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for OverlayPlacementWatcher {
    fn drop(&mut self) {
        let _ = self.stop.write_all(&[1]);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn watch(
    display: DisplayConnection,
    window: u32,
    size: [u32; 3],
    mut area: ScreenRect,
    stopped: UnixStream,
) -> io::Result<()> {
    loop {
        let mut changed = false;
        while let Some(event) = display
            .connection
            .poll_for_event()
            .map_err(io::Error::other)?
        {
            changed |= match event {
                Event::RandrNotify(_) | Event::RandrScreenChangeNotify(_) => true,
                Event::PropertyNotify(event) => {
                    event.atom == display.atoms._NET_WORKAREA
                        || event.atom == display.atoms._NET_CURRENT_DESKTOP
                }
                _ => false,
            };
        }
        if changed {
            let current = display.work_area()?;
            if current != area {
                display.place(window, size, current)?;
                area = current;
            }
            // Replies can queue newer events inside x11rb. Drain them before
            // waiting on the socket, which may already have no unread bytes.
            continue;
        }
        let mut fds = [
            libc::pollfd {
                fd: display.connection.stream().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: stopped.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: both descriptors are owned for this loop; poll retains no pointers.
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, -1) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if fds[1].revents != 0 {
            return Ok(());
        }
        if fds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            return Err(io::Error::other("overlay display connection closed"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_uses_primary_monitor_and_current_desktop_work_area() {
        let primary = ScreenRect {
            x: 1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let desktop = desktop_work_area(&[0, 0, 5760, 1200, 0, 0, 5760, 1032], 1).unwrap();
        let area = primary.intersect(desktop).unwrap();
        assert_eq!(
            frame(area, 143, 56, 72),
            ScreenRect {
                x: 2808,
                y: 904,
                width: 143,
                height: 56
            }
        );
        let moved = ScreenRect {
            x: -1920,
            y: -200,
            width: 1920,
            height: 1000,
        };
        assert_eq!(
            frame(moved, 286, 112, 144),
            ScreenRect {
                x: -1103,
                y: 544,
                width: 286,
                height: 112
            }
        );
    }

    #[test]
    fn placement_preserves_bottom_gap_and_fits_constrained_areas() {
        for (area, size, expected) in [
            ((1920, 0, 1440, 860), [336, 64, 24], (2472, 772, 336, 64)),
            ((-280, 24, 280, 48), [336, 64, 24], (-280, 24, 280, 24)),
            ((0, 0, 1920, 1040), [143, 56, 72], (888, 912, 143, 56)),
        ] {
            let rect = |(x, y, width, height)| ScreenRect {
                x,
                y,
                width,
                height,
            };
            assert_eq!(frame(rect(area), size[0], size[1], size[2]), rect(expected));
        }
    }

    #[test]
    fn missing_or_stale_desktop_area_cannot_move_the_overlay_off_monitor() {
        assert_eq!(desktop_work_area(&[0, 0, 1920], 0), None);
        assert_eq!(desktop_work_area(&[0, 0, 1920, 1080], u32::MAX), None);
        let monitor = ScreenRect {
            x: 1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let old = ScreenRect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1200,
        };
        assert_eq!(monitor.intersect(old), None);
    }
}
