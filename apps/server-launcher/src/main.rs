//! `domovoi-server-launcher.exe` (Windows): starts the `domovoi-server.exe`
//! beside it with the same arguments and no console window, and exits with
//! its exit code.
//!
//! `domovoi-server.exe` is a console program, so its subcommands print in
//! the terminal they are run from; started by Windows at sign-in (the Run
//! value, the scheduled task, a Start-menu shortcut) it would open a console
//! window of its own. This launcher is a windowed program that starts it with
//! `CREATE_NO_WINDOW`.
//!
//! - `--supervise` (first): the server is started again after it fails (a
//!   nonzero exit), 5 s later, doubling to five minutes, and back to 5 s once
//!   it has run ten minutes. A clean stop (`domovoi-server stop`) ends it.
//! - The server is held in a job object: ending the launcher (Task Manager,
//!   `schtasks /End`) ends the server too. What the server starts is not held
//!   in it (the server owns its children's lifetimes itself).
//! - `%LOCALAPPDATA%\Domovoi\logs\launcher.log` gets the server's stderr (a
//!   panic before the server's own log starts), each start and exit, and
//!   the launcher's own panic. Over 1 MiB at a start, it becomes
//!   `launcher.old.log`.

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    std::process::exit(windows::run());
    #[cfg(not(windows))]
    {
        eprintln!("domovoi-server-launcher is for Windows; run domovoi-server itself");
        std::process::exit(2);
    }
}

/// The log is started afresh at a launch once it is past this (1 MiB).
#[cfg_attr(not(windows), allow(dead_code, reason = "the launcher only runs on Windows"))]
const LOG_LIMIT: u64 = 1024 * 1024;

/// The time now, UTC, as `2026-11-25T19:17:03Z`.
#[cfg_attr(not(windows), allow(dead_code, reason = "the launcher only runs on Windows"))]
fn utc_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    civil(secs)
}

/// `secs` since 1970 as a UTC time (Howard Hinnant's days-to-civil).
#[cfg_attr(not(windows), allow(dead_code, reason = "the launcher only runs on Windows"))]
fn civil(secs: u64) -> String {
    let (days, rest) = (secs / 86_400, secs % 86_400);
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

/// When to start the server again after it failed.
#[cfg_attr(not(windows), allow(dead_code, reason = "the launcher only runs on Windows"))]
struct Backoff {
    next: std::time::Duration,
}

#[cfg_attr(not(windows), allow(dead_code, reason = "the launcher only runs on Windows"))]
impl Backoff {
    const FIRST: std::time::Duration = std::time::Duration::from_secs(5);
    const MOST: std::time::Duration = std::time::Duration::from_secs(300);
    /// A run this long was a good one: the next failure waits the least again.
    const HEALTHY: std::time::Duration = std::time::Duration::from_secs(600);

    fn new() -> Self {
        Backoff { next: Self::FIRST }
    }

    /// The wait after a run of `ran` that failed.
    fn after(&mut self, ran: std::time::Duration) -> std::time::Duration {
        if ran >= Self::HEALTHY {
            self.next = Self::FIRST;
        }
        let wait = self.next;
        self.next = (self.next * 2).min(Self::MOST);
        wait
    }
}

#[cfg(windows)]
mod windows {
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::time::Instant;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

    /// A job that ends the server when the launcher goes away. Its children
    /// leave it at once (silent breakaway): an editor the server opened stays
    /// open after it exits.
    struct Job(HANDLE);

    impl Job {
        fn new() -> Option<Job> {
            // SAFETY: no attributes and no name: a fresh, private job.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return None;
            }
            let job = Job(handle);
            // SAFETY: an all-zero structure is "no limits"; two are set.
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK;
            // SAFETY: `info` is this information class's structure, with its size.
            let ok = unsafe {
                SetInformationJobObject(job.0, JobObjectExtendedLimitInformation, (&raw const info).cast(), std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32)
            };
            (ok != 0).then_some(job)
        }

        fn hold(&self, pid: u32) {
            // SAFETY: plain calls; the process handle is closed after.
            unsafe {
                let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
                if !process.is_null() {
                    AssignProcessToJobObject(self.0, process);
                    CloseHandle(process);
                }
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle is open and closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    fn server() -> Option<PathBuf> {
        Some(std::env::current_exe().ok()?.with_file_name("domovoi-server.exe")).filter(|p| p.is_file())
    }

    /// The launcher's log, opened for appending; `None` when there is no
    /// writable place for it (the server is started all the same).
    fn log_file() -> Option<std::fs::File> {
        let dir = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Domovoi").join("logs");
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("launcher.log");
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > super::LOG_LIMIT) {
            let _ = std::fs::rename(&path, dir.join("launcher.old.log"));
        }
        std::fs::OpenOptions::new().create(true).append(true).open(path).ok()
    }

    fn note(log: Option<&std::fs::File>, line: &str) {
        use std::io::Write;
        if let Some(mut f) = log {
            let _ = writeln!(f, "{} {line}", super::utc_now());
        }
    }

    pub fn run() -> i32 {
        let mut args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
        let supervise = args.first().is_some_and(|a| a == "--supervise");
        if supervise {
            args.remove(0);
        }
        let log = log_file();
        if let Some(f) = log.as_ref().and_then(|f| f.try_clone().ok()) {
            std::panic::set_hook(Box::new(move |info| note(Some(&f), &format!("launcher panicked: {info}"))));
        }
        let Some(server) = server() else {
            note(log.as_ref(), "domovoi-server.exe is not beside the launcher: nothing to start");
            return 3;
        };
        let job = Job::new();
        let mut backoff = super::Backoff::new();
        loop {
            let started = Instant::now();
            note(log.as_ref(), &format!("starting {} {}", server.display(), args.iter().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" ")));
            // Its stderr goes to the log: a panic before its own log starts is kept.
            let stderr = log.as_ref().and_then(|f| f.try_clone().ok()).map_or_else(std::process::Stdio::null, std::process::Stdio::from);
            #[expect(clippy::disallowed_methods, reason = "the launcher's one child: the server it exists to start")]
            let child = std::process::Command::new(&server)
                .args(&args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(stderr)
                // Warnings only: the server keeps its full log itself.
                .env("DOMOVOI_STDERR_LOG", "warn")
                .creation_flags(CREATE_NO_WINDOW)
                .spawn();
            let code = match child {
                Ok(mut child) => {
                    if let Some(job) = &job {
                        job.hold(child.id());
                    }
                    child.wait().ok().and_then(|s| s.code()).unwrap_or(1)
                }
                Err(e) => {
                    note(log.as_ref(), &format!("could not start it: {e}"));
                    1
                }
            };
            if !supervise || code == 0 {
                note(log.as_ref(), &format!("it exited with code {code}"));
                return code;
            }
            let wait = backoff.after(started.elapsed());
            note(log.as_ref(), &format!("it exited with code {code} after {}s; starting it again in {}s", started.elapsed().as_secs(), wait.as_secs()));
            std::thread::sleep(wait);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Backoff;
    use std::time::Duration;

    #[test]
    fn log_times_are_utc_dates() {
        assert_eq!(super::civil(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::civil(951_782_400), "2000-02-29T00:00:00Z", "a leap day");
        assert_eq!(super::civil(1_795_634_223), "2026-11-25T19:17:03Z");
    }

    #[test]
    fn failures_back_off_and_a_long_run_starts_over() {
        let mut b = Backoff::new();
        let quick = Duration::from_secs(1);
        let waits: Vec<u64> = (0..8).map(|_| b.after(quick).as_secs()).collect();
        assert_eq!(waits, [5, 10, 20, 40, 80, 160, 300, 300]);
        assert_eq!(b.after(Duration::from_secs(700)).as_secs(), 5, "ran ten minutes: a fresh start");
    }
}
