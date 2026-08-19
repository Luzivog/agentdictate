use std::{fs, io, path::Path};

use tracing_appender::non_blocking::WorkerGuard;

/// Installs process-wide structured logging and returns the flush guard that
/// must live for the remainder of the process.
pub fn init_file_logging(directory: &Path, file_prefix: &str) -> io::Result<WorkerGuard> {
    fs::create_dir_all(directory)?;
    let appender = tracing_appender::rolling::daily(directory, file_prefix);
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_target(true)
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_writer(writer)
        .try_init()
        .map_err(|error| io::Error::other(error.to_string()))?;
    std::panic::set_hook(Box::new(|panic| {
        tracing::error!(panic = %panic, "process panicked");
    }));
    Ok(guard)
}
