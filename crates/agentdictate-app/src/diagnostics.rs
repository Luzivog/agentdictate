use std::{fs, io, path::Path, str::FromStr};

use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{Builder, Rotation},
};
use tracing_subscriber::{
    Layer, filter::ParseError, filter::Targets, fmt, layer::SubscriberExt, util::SubscriberInitExt,
};

/// Log levels used unless `RUST_LOG` replaces them. The overlay helper logs
/// into the daemon's file, and its GPU stack is chatty at info level: GPUI's
/// renderer lists every adapter each time a helper opens its window.
const DEFAULT_LOG_FILTER: &str = "info,naga=warn,wgpu_core=warn,wgpu_hal=warn,\
     gpui_wgpu=warn,zed_xim=warn,gpui=warn";
/// Daily log files kept per file prefix; older ones are deleted.
const RETAINED_LOG_FILES: usize = 14;

/// Installs process-wide structured logging and returns the flush guard that
/// must live for the remainder of the process.
pub fn init_file_logging(directory: &Path, file_prefix: &str) -> io::Result<WorkerGuard> {
    fs::create_dir_all(directory)?;
    let appender = Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix(file_prefix)
        .max_log_files(RETAINED_LOG_FILES)
        .build(directory)
        .map_err(io::Error::other)?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let (filter, rejected) = log_filter(std::env::var("RUST_LOG").ok().as_deref());
    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_ansi(false)
                .with_target(true)
                .with_thread_ids(true)
                .with_thread_names(true)
                .with_writer(writer)
                .with_filter(filter),
        )
        .try_init()
        .map_err(|error| io::Error::other(error.to_string()))?;
    if let Some(error) = rejected {
        tracing::warn!(%error, "RUST_LOG is invalid; using the default log levels");
    }
    std::panic::set_hook(Box::new(|panic| {
        tracing::error!(panic = %panic, "process panicked");
    }));
    Ok(guard)
}

/// Chooses the log filter: a non-empty `RUST_LOG` when it parses, otherwise
/// the defaults plus the parse error to report.
fn log_filter(rust_log: Option<&str>) -> (Targets, Option<ParseError>) {
    let defaults =
        || Targets::from_str(DEFAULT_LOG_FILTER).expect("the default log filter is valid");
    match rust_log.map(str::trim).filter(|value| !value.is_empty()) {
        None => (defaults(), None),
        Some(value) => match Targets::from_str(value) {
            Ok(filter) => (filter, None),
            Err(error) => (defaults(), Some(error)),
        },
    }
}

#[cfg(test)]
mod tests {
    use tracing::Level;

    use super::log_filter;

    #[test]
    fn rust_log_replaces_the_defaults_and_an_invalid_value_falls_back_to_them() {
        let (custom, rejected) = log_filter(Some("debug"));
        assert!(rejected.is_none());
        assert!(custom.would_enable("naga::back", &Level::DEBUG));

        let (fallback, rejected) = log_filter(Some("naga=loud"));
        assert!(rejected.is_some());
        assert!(!fallback.would_enable("naga::back", &Level::INFO));
        assert!(fallback.would_enable("agentdictate_app::daemon", &Level::INFO));
    }
}
