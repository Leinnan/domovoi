//! `domovoi-server service …`: the headless server as a systemd user service
//! (Linux), for a machine nobody signs in to. Launch at login (`autostart`)
//! needs a graphical session; this does not: with lingering on, systemd
//! starts the user's services at boot.
//!
//! The unit runs this executable with the home directory, port, host and
//! relays it was installed with, and `--no-startup-code`: its stdout goes to
//! the journal, and pairing codes are never written to logs. Remote access is
//! not in the unit (the setting in `remote.json` is): installing a new unit
//! turns it on once, so a later `remote off` stays off across restarts.
//! `DOMOVOI_SYSTEMD_USER_DIR` redirects the unit file (tests).

use std::path::{Path, PathBuf};

pub const UNIT: &str = "domovoi-server.service";

#[derive(clap::Subcommand)]
pub enum ServiceCmd {
    /// Write the unit, enable it and (re)start the server: run again after
    /// replacing the binary.
    Install {
        /// Also start the user's services at boot, before anyone signs in
        /// (`loginctl enable-linger`).
        #[arg(long)]
        linger: bool,
        /// Enable the unit without starting or restarting it now.
        #[arg(long)]
        no_start: bool,
        /// Leave remote access as it is (a new unit otherwise turns it on).
        #[arg(long)]
        no_remote: bool,
    },
    /// Stop the server, disable the unit and remove it.
    Uninstall,
    /// Whether the unit is installed, enabled and running.
    Status,
}

/// What the unit runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exec {
    pub exe: PathBuf,
    pub args: Vec<String>,
}

impl Exec {
    /// This executable serving `home` on `host:port`, headless.
    pub fn current(home: &Path, host: &str, port: u16, relays: &[String]) -> std::io::Result<Exec> {
        let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
        let mut args = vec!["--no-startup-code".to_owned(), "--home-dir".to_owned(), home.to_string_lossy().into_owned(), "--port".to_owned(), port.to_string()];
        if host != "127.0.0.1" {
            args.extend(["--host".to_owned(), host.to_owned()]);
        }
        for relay in relays {
            args.extend(["--relay".to_owned(), relay.clone()]);
        }
        let exe = std::env::current_exe()?;
        Ok(Exec { exe: std::fs::canonicalize(&exe).unwrap_or(exe), args })
    }
}

/// One `ExecStart` word: double-quoted when it needs to be, with systemd's
/// own specifiers (`%`) and variable expansion (`$`) doubled.
fn quote(word: &str) -> String {
    let escaped = word.replace('%', "%%").replace('$', "$$");
    if !escaped.is_empty() && escaped.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@%$+,".contains(c)) {
        return escaped;
    }
    let mut out = String::from("\"");
    for c in escaped.chars() {
        if matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The unit file. Restarted after a failure (backing off to five minutes),
/// not after a clean stop (`domovoi-server stop`); it has 15 s to stop, which
/// is what Fedora allows before it aborts a service.
pub fn unit(exec: &Exec) -> String {
    let words: Vec<String> = std::iter::once(exec.exe.to_string_lossy().into_owned()).chain(exec.args.iter().cloned()).map(|w| quote(&w)).collect();
    format!(
        "[Unit]\n\
         Description=Domovoi Server\n\
         Wants=network-online.target\n\
         After=network-online.target\n\
         StartLimitIntervalSec=0\n\
         \n\
         [Service]\n\
         Type=exec\n\
         ExecStart={}\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         RestartSteps=6\n\
         RestartMaxDelaySec=300\n\
         TimeoutStopSec=15\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        words.join(" ")
    )
}

fn unit_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("DOMOVOI_SYSTEMD_USER_DIR") {
        return Some(PathBuf::from(dir));
    }
    Some(dirs::config_dir()?.join("systemd").join("user"))
}

pub fn unit_path() -> Option<PathBuf> {
    Some(unit_dir()?.join(UNIT))
}

/// Writes `text` to `path` unless it is already there (staged, then
/// renamed). `Ok(true)`: the file changed.
fn write_unit(path: &Path, text: &str) -> std::io::Result<bool> {
    if std::fs::read_to_string(path).is_ok_and(|current| current == text) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let staged = path.with_extension(format!("service.{}", std::process::id()));
    std::fs::write(&staged, text).and_then(|()| std::fs::rename(&staged, path)).inspect_err(|_| {
        let _ = std::fs::remove_file(&staged);
    })?;
    Ok(true)
}

/// Runs `program args`; its trimmed stdout, or why it failed.
fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let shown = format!("{program} {}", args.join(" "));
    let out = crate::process::output_blocking(crate::process::blocking_command(program).args(args), crate::process::QUICK).map_err(|e| format!("{shown}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if out.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    Err(format!("{shown} failed: {}", if stderr.is_empty() { stdout } else { stderr }))
}

fn systemctl(args: &[&str]) -> Result<String, String> {
    let mut all = vec!["--user"];
    all.extend_from_slice(args);
    run("systemctl", &all)
}

/// `systemctl --user is-enabled|is-active`: the state word, even when the
/// answer is "no" (a nonzero exit).
fn state(query: &str) -> String {
    crate::process::output_blocking(crate::process::blocking_command("systemctl").args(["--user", query, UNIT]), crate::process::QUICK)
        .ok()
        .and_then(|out| String::from_utf8_lossy(&out.stdout).lines().next().map(|l| l.trim().to_owned()))
        .filter(|word| !word.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

fn lingering() -> Option<bool> {
    let user = std::env::var("USER").ok()?;
    run("loginctl", &["show-user", &user, "--property=Linger", "--value"]).ok().map(|v| v == "yes")
}

pub fn service(base: &crate::Base, exec: &Exec, cmd: &ServiceCmd) -> crate::Fallible {
    if !cfg!(target_os = "linux") {
        return Err("`service` sets up a systemd user service (Linux); on this system use `domovoi-server autostart on`".into());
    }
    let path = unit_path().ok_or("no config directory")?;
    match cmd {
        ServiceCmd::Install { linger, no_start, no_remote } => {
            let new = !path.exists();
            if write_unit(&path, &unit(exec))? {
                println!("Wrote {}", path.display());
            }
            if new && !no_remote && !base.remote.settings().enabled {
                base.remote.save_enabled(true)?;
                println!("Remote access: on (`domovoi-server remote off` keeps it off across restarts)");
            }
            systemctl(&["daemon-reload"])?;
            systemctl(&["enable", UNIT])?;
            if *linger {
                run("loginctl", &["enable-linger"])?;
            }
            if !no_start {
                systemctl(&["restart", UNIT])?;
            }
            if crate::autostart::is_enabled() {
                println!("warning: launch at login is also on and would start a second server; `domovoi-server autostart off` turns it off");
            }
            if lingering() == Some(false) {
                println!("note: lingering is off, so the server starts when you sign in, not at boot (`service install --linger`)");
            }
        }
        ServiceCmd::Uninstall => {
            if path.exists() {
                systemctl(&["disable", "--now", UNIT])?;
                std::fs::remove_file(&path)?;
                systemctl(&["daemon-reload"])?;
                println!("Removed {}", path.display());
            }
        }
        ServiceCmd::Status => {}
    }
    print_status(&path);
    Ok(())
}

fn print_status(path: &Path) {
    if !path.exists() {
        println!("Service: not installed ({} is missing)", path.display());
        return;
    }
    println!("Service: {}, {} ({})", state("is-enabled"), state("is-active"), path.display());
    match lingering() {
        Some(true) => println!("Starts at boot: yes (lingering)"),
        Some(false) => println!("Starts at boot: no, at sign-in (lingering is off)"),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exec() -> Exec {
        Exec {
            exe: PathBuf::from("/home/me/.local/bin/domovoi-server"),
            args: vec!["--no-startup-code".into(), "--home-dir".into(), "/home/me/My \"Domovoi\" 100%".into(), "--port".into(), "13773".into()],
        }
    }

    #[test]
    fn the_unit_runs_the_headless_server_and_restarts_it_after_a_failure() {
        let text = unit(&exec());
        assert!(text.contains("\nExecStart=/home/me/.local/bin/domovoi-server --no-startup-code --home-dir \"/home/me/My \\\"Domovoi\\\" 100%%\" --port 13773\n"), "{text}");
        assert!(!text.contains("--tray"), "no status bar icon without a desktop");
        assert!(!text.contains("--remote"), "remote access is the saved setting");
        assert!(text.contains("\nRestart=on-failure\n"));
        assert!(text.contains("\nTimeoutStopSec=15\n"));
        assert!(text.contains("\nWantedBy=default.target\n"));
    }

    #[test]
    fn words_are_quoted_only_when_needed() {
        assert_eq!(quote("/usr/bin/x"), "/usr/bin/x");
        assert_eq!(quote("a b"), "\"a b\"");
        assert_eq!(quote("$HOME"), "$$HOME");
        assert_eq!(quote("back\\slash"), "\"back\\\\slash\"");
        assert_eq!(quote(""), "\"\"");
    }

    #[test]
    fn writing_the_same_unit_again_changes_nothing() {
        let dir = std::env::temp_dir().join(format!("domovoi-service-{}", crate::util::new_uuid()));
        let path = dir.join(UNIT);
        let text = unit(&exec());
        assert!(write_unit(&path, &text).expect("first write"));
        assert!(!write_unit(&path, &text).expect("second write"), "unchanged");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), text);
        assert!(write_unit(&path, "changed").expect("third write"));
        assert_eq!(std::fs::read_dir(&dir).expect("dir").count(), 1, "no staged file left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
