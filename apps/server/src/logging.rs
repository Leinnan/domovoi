//! Logging: stderr always, plus a daily-rotated file when the server runs
//! (`<home>/userdata/logs/server.<date>.log`, a week kept). Pairing codes
//! are printed to stdout, never logged, so they stay out of the file.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the protocol log: provider messages as sent and received")]

use std::path::Path;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// Keeps the file writer alive; dropping it flushes buffered lines.
pub struct Guard(#[allow(dead_code)] Option<tracing_appender::non_blocking::WorkerGuard>);

fn filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())
}

/// `dir` is `None` for one-shot commands (`pair`, `remote …`), which only
/// write to stderr.
pub fn init(dir: Option<&Path>) -> Guard {
    // Colour only for a person at a terminal, not for a captured log.
    let stderr = tracing_subscriber::fmt::layer().with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr())).with_writer(std::io::stderr).with_filter(filter());
    let file = dir.and_then(|dir| {
        let appender = tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("server")
            .filename_suffix("log")
            .max_log_files(7)
            .build(dir)
            .ok()?;
        Some(tracing_appender::non_blocking(appender))
    });
    match file {
        Some((writer, guard)) => {
            let file = tracing_subscriber::fmt::layer().with_ansi(false).with_writer(writer).with_filter(filter());
            let _ = tracing_subscriber::registry().with(stderr).with(file).try_init();
            Guard(Some(guard))
        }
        None => {
            let _ = tracing_subscriber::registry().with(stderr).try_init();
            Guard(None)
        }
    }
}

/// Provider protocol logs (`claude-<session>.ndjson`, `codex-app-server.ndjson`):
/// one line per message, written by a background thread through one open,
/// buffered file, so a streamed token costs the caller a channel send rather
/// than opening, writing and closing the file. Past `PROTOCOL_LOG_MAX_BYTES`
/// the file moves to `<name>.1` (replacing the previous one) and starts over.
pub fn protocol_log(path: std::path::PathBuf) -> impl Fn(&str, &serde_json::Value) + Send + Sync + 'static {
    let (lines, queued) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || write_protocol_log(&path, &queued, PROTOCOL_LOG_MAX_BYTES));
    let lines = std::sync::Mutex::new(lines);
    move |direction: &str, msg: &serde_json::Value| {
        let line = serde_json::json!({ "t": domovoi_contracts::iso_now(), "direction": direction, "payload": msg }).to_string();
        let _ = crate::util::locked(&lines).send(line);
    }
}

const PROTOCOL_LOG_MAX_BYTES: u64 = 32 * 1024 * 1024;

fn write_protocol_log(path: &Path, queued: &std::sync::mpsc::Receiver<String>, max_bytes: u64) {
    use std::io::Write;
    let open = || std::fs::OpenOptions::new().create(true).append(true).open(path).ok().map(std::io::BufWriter::new);
    let mut file = open();
    let mut size = std::fs::metadata(path).map_or(0, |m| m.len());
    // Blocks for the next line; stops when every sender is gone. The file is
    // flushed whenever the queue runs dry, so a line is on disk as soon as
    // the burst it came in is written (crash recovery reads these logs).
    while let Ok(first) = queued.recv() {
        for line in std::iter::once(first).chain(std::iter::from_fn(|| queued.try_recv().ok())) {
            if size + line.len() as u64 + 1 > max_bytes && size > 0 {
                if let Some(mut f) = file.take() {
                    let _ = f.flush();
                }
                let _ = std::fs::rename(path, path.with_extension("ndjson.1"));
                file = open();
                size = 0;
            }
            if let Some(f) = &mut file {
                if writeln!(f, "{line}").is_ok() {
                    size += line.len() as u64 + 1;
                }
            }
        }
        if let Some(f) = &mut file {
            let _ = f.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_lines_are_kept_in_order_and_the_file_rolls_over() {
        let dir = std::env::temp_dir().join(format!("domovoi-protolog-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.ndjson");
        let (tx, rx) = std::sync::mpsc::channel();
        for i in 0..100 {
            tx.send(format!("{{\"n\":{i}}}")).unwrap();
        }
        drop(tx);
        write_protocol_log(&path, &rx, 500);
        let current = std::fs::read_to_string(&path).unwrap();
        let previous = std::fs::read_to_string(path.with_extension("ndjson.1")).unwrap();
        assert!(current.len() <= 500 && previous.len() <= 500);
        let last: Vec<&str> = current.lines().collect();
        assert_eq!(last.last(), Some(&"{\"n\":99}"), "the newest line is last");
        let first_kept: u32 = previous.lines().next().unwrap()[5..].trim_end_matches('}').parse().unwrap();
        assert!(previous.lines().chain(current.lines()).map(|l| l[5..].trim_end_matches('}').parse::<u32>().unwrap()).eq(first_kept..100));
        let _ = std::fs::remove_dir_all(dir);
    }
}
