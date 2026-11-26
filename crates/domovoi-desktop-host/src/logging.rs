//! The client's log: stderr, and a file a day in `<data>/logs` (a week
//! kept), since a windowed program's stderr goes nowhere. A panic is logged
//! and also appended to `panic.log` there at once: on Windows a panic inside
//! a WinUI callback aborts the process before the log's writer thread runs.

use std::io::Write;
use std::path::PathBuf;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

use crate::Identity;

/// Keeps the log file's writer running; dropped at the end of `main`, it
/// writes what is still queued.
pub struct Guard(#[allow(dead_code, reason = "held for its Drop")] Option<tracing_appender::non_blocking::WorkerGuard>);

/// Where the log files are: `logs` in the client's data folder.
pub fn logs_dir(identity: Identity) -> PathBuf {
    crate::default_data_dir(identity).join("logs")
}

/// Starts logging for the client `identity`. `default_filter` applies
/// unless `RUST_LOG` is set.
pub fn init(identity: Identity, default_filter: &str) -> Guard {
    let filter = || EnvFilter::try_from_default_env().unwrap_or_else(|_| default_filter.into());
    let stderr = tracing_subscriber::fmt::layer().with_writer(std::io::stderr).with_filter(filter());
    let dir = logs_dir(identity);
    let file = std::fs::create_dir_all(&dir).ok().and_then(|()| {
        tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix(identity.dir_name)
            .filename_suffix("log")
            .max_log_files(7)
            .build(&dir)
            .ok()
    });
    let guard = match file {
        Some(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let file = tracing_subscriber::fmt::layer().with_ansi(false).with_writer(writer).with_filter(filter());
            let _ = tracing_subscriber::registry().with(stderr).with(file).try_init();
            Some(guard)
        }
        None => {
            let _ = tracing_subscriber::registry().with(stderr).try_init();
            None
        }
    };
    log_panics(dir.join("panic.log"));
    Guard(guard)
}

fn log_panics(panic_log: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info.payload().downcast_ref::<&str>().copied().or_else(|| info.payload().downcast_ref::<String>().map(String::as_str)).unwrap_or("(no message)");
        let location = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())).unwrap_or_default();
        let thread = std::thread::current();
        let thread = thread.name().unwrap_or("unnamed");
        tracing::error!(target: "panic", thread, %location, "panicked: {message}");
        let backtrace = std::backtrace::Backtrace::force_capture();
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&panic_log) {
            let _ = writeln!(f, "{} thread '{thread}' panicked at {location}: {message}\n{backtrace}", utc_now());
        }
        previous(info);
    }));
}

/// The time now, UTC, as `2026-11-25T19:17:03Z`.
fn utc_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_time_is_written_as_utc() {
        let now = super::utc_now();
        assert_eq!(now.len(), 20, "{now}");
        assert!(now.starts_with("20") && now.ends_with('Z') && now.as_bytes()[10] == b'T', "{now}");
    }
}
