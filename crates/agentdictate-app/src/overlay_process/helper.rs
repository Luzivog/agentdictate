use super::protocol::OVERLAY_HELPER_ARGUMENT;

#[cfg(feature = "desktop")]
use std::{
    cell::RefCell,
    io::{self, BufRead, BufReader, Write},
    rc::Rc,
};

#[cfg(feature = "desktop")]
use agentdictate_linux::overlay_placement::OverlayPlacementWatcher;
#[cfg(feature = "desktop")]
use agentdictate_ui::run_recording_overlay;

#[cfg(feature = "desktop")]
use super::protocol::{OverlayHelperStatus, OverlayUpdate};

pub fn is_overlay_helper_argument(argument: Option<&str>) -> bool {
    argument == Some(OVERLAY_HELPER_ARGUMENT)
}

#[cfg(feature = "desktop")]
pub fn run_overlay_helper() -> anyhow::Result<()> {
    if std::env::var_os("DISPLAY").is_none() {
        let message = "focus-neutral recording overlay requires X11 or XWayland";
        write_overlay_helper_status(&OverlayHelperStatus::Error {
            message: message.into(),
        })?;
        anyhow::bail!(message);
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("agentdictate-overlay-input".into())
        .spawn(move || {
            let input = std::io::stdin();
            let mut last_phase = None;
            for line in BufReader::new(input).lines() {
                let update = match line {
                    Ok(line) => match serde_json::from_str::<OverlayUpdate>(&line) {
                        Ok(update) => update,
                        Err(error) => {
                            tracing::error!(%error, "invalid overlay snapshot");
                            return;
                        }
                    },
                    Err(error) => {
                        tracing::error!(%error, "could not read overlay snapshot");
                        return;
                    }
                };
                if last_phase != Some(update.workflow.phase) {
                    tracing::info!(phase = ?update.workflow.phase, "recording overlay workflow changed");
                    last_phase = Some(update.workflow.phase);
                }
                if sender.send(update.presentation()).is_err() {
                    return;
                }
            }
        })?;
    let initial = receiver
        .recv()
        .map_err(|_| anyhow::anyhow!("overlay helper received no initial update"))?;
    let placement = Rc::new(RefCell::new(None));
    let placement_owner = Rc::clone(&placement);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_recording_overlay(
            initial,
            receiver,
            move |window, scale| {
                let watcher = OverlayPlacementWatcher::start(
                    window,
                    scale,
                    [
                        agentdictate_ui::OVERLAY_WIDTH,
                        agentdictate_ui::OVERLAY_HEIGHT,
                        agentdictate_ui::OVERLAY_BOTTOM_GAP,
                    ],
                    |error| {
                        tracing::error!(%error, "recording overlay placement failed");
                        let _ = write_overlay_helper_status(&OverlayHelperStatus::Error {
                            message: error.to_string(),
                        });
                    },
                )
                .expect("recording overlay placement should initialize");
                *placement_owner.borrow_mut() = Some(watcher);
                write_overlay_helper_status(&OverlayHelperStatus::WindowCreated)
                    .expect("overlay window creation should be reportable");
            },
            || {
                write_overlay_helper_status(&OverlayHelperStatus::FrameSubmitted)
                    .expect("overlay frame submission should be reportable");
            },
        );
    }));
    if let Err(payload) = result {
        let message = panic_message(payload.as_ref());
        let _ = write_overlay_helper_status(&OverlayHelperStatus::Error {
            message: message.clone(),
        });
        anyhow::bail!("recording overlay panicked: {message}")
    }
    Ok(())
}

#[cfg(feature = "desktop")]
fn write_overlay_helper_status(status: &OverlayHelperStatus) -> io::Result<()> {
    let output = std::io::stdout();
    let mut output = output.lock();
    serde_json::to_writer(&mut output, status).map_err(io::Error::other)?;
    output.write_all(b"\n")?;
    output.flush()
}

#[cfg(feature = "desktop")]
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&str>()
                .map(|message| (*message).to_owned())
        })
        .unwrap_or_else(|| "unknown panic".to_owned())
}
