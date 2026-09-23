//! Drives the production selection owner for
//! `scripts/test-overlay-desktop.py` on its private desktop. Never point
//! it at a real session: publishing takes that session's clipboard.
//!
//! Reads one command per line on stdin and answers each on stdout:
//! - `publish clipboard <text>` or `publish both <text>`: answers `published`
//!   once AgentDictate owns the selections, or `error <reason>`.
//! - `pressed`: marks now as the paste key press; answers `marked`.
//! - `requested <ms>`: answers `requested <latency ms>` when an application
//!   requested the text after the mark, waiting up to `<ms>` after it, or
//!   `not-requested`.
//!
//! It exits at the end of its input.

use std::{
    io::{self, BufRead, Write},
    time::{Duration, Instant},
};

use agentdictate_linux::clipboard::{ClipboardSelection, SelectionOwner};

fn main() -> io::Result<()> {
    let mut owner = SelectionOwner::new();
    let mut pressed = Instant::now();
    let mut stdout = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let line = line?;
        let (command, argument) = line.split_once(' ').unwrap_or((&line, ""));
        let answer = match command {
            "publish" => {
                let (scope, text) = argument.split_once(' ').unwrap_or((argument, ""));
                let selections: &[ClipboardSelection] = match scope {
                    "both" => &[ClipboardSelection::Primary, ClipboardSelection::Clipboard],
                    _ => &[ClipboardSelection::Clipboard],
                };
                match owner.publish(text, selections, Instant::now() + Duration::from_secs(5)) {
                    Ok(()) => "published".to_owned(),
                    Err(error) => format!("error {error}"),
                }
            }
            "pressed" => {
                pressed = Instant::now();
                "marked".to_owned()
            }
            "requested" => {
                let window = Duration::from_millis(argument.parse().unwrap_or(0));
                match owner.text_requested_since(pressed, pressed + window) {
                    Some(requested) => {
                        format!(
                            "requested {}",
                            requested.duration_since(pressed).as_millis()
                        )
                    }
                    None => "not-requested".to_owned(),
                }
            }
            _ => format!("error unknown command {command}"),
        };
        writeln!(stdout, "{answer}")?;
        stdout.flush()?;
    }
    Ok(())
}
