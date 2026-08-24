use agentdictate_ui::LogicalRect;

use super::protocol::{OVERLAY_HELPER_ARGUMENT, OVERLAY_WORK_AREA, parse_work_area};

#[cfg(feature = "desktop")]
use std::{
    io::{self, BufRead, BufReader, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[cfg(feature = "desktop")]
use agentdictate_ui::run_recording_overlay_with_ready;

#[cfg(feature = "desktop")]
use super::protocol::{OverlayHelperStatus, OverlayUpdate};

pub fn is_overlay_helper_argument(argument: Option<&str>) -> bool {
    argument == Some(OVERLAY_HELPER_ARGUMENT)
}

pub fn overlay_work_area_from_environment() -> Option<LogicalRect> {
    std::env::var(OVERLAY_WORK_AREA)
        .ok()
        .as_deref()
        .and_then(parse_work_area)
}

#[cfg(feature = "desktop")]
pub fn run_overlay_helper() -> anyhow::Result<()> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("agentdictate-overlay-input".into())
        .spawn(move || {
            let input = std::io::stdin();
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
                if sender.send(update.presentation()).is_err() {
                    return;
                }
            }
        })?;
    let initial = receiver
        .recv()
        .map_err(|_| anyhow::anyhow!("overlay helper received no initial update"))?;
    let ready = Arc::new(AtomicBool::new(false));
    let ready_callback = Arc::clone(&ready);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_recording_overlay_with_ready(
            initial,
            receiver,
            overlay_work_area_from_environment(),
            move || {
                write_overlay_helper_status(&OverlayHelperStatus::Ready)
                    .expect("overlay helper readiness should be writable");
                ready_callback.store(true, Ordering::Release);
            },
        );
    }));
    if let Err(payload) = result {
        let message = panic_message(payload.as_ref());
        if !ready.load(Ordering::Acquire) {
            let _ = write_overlay_helper_status(&OverlayHelperStatus::Error {
                message: message.clone(),
            });
        }
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
