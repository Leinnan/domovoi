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
    /// This executable serving `home` on `host:port`, headless. `carried`
    /// are further flags of this run that the service's must have too
    /// (`--allowed-host`, `--web-url`: `main.rs` `carried_flags`).
    pub fn current(home: &Path, host: &str, port: u16, relays: &[String], carried: &[String]) -> std::io::Result<Exec> {
        let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
        let mut args = vec!["--no-startup-code".to_owned(), "--home-dir".to_owned(), home.to_string_lossy().into_owned(), "--port".to_owned(), port.to_string()];
        if host != "127.0.0.1" {
            args.extend(["--host".to_owned(), host.to_owned()]);
        }
        for relay in relays {
            args.extend(["--relay".to_owned(), relay.clone()]);
        }
        args.extend(carried.iter().cloned());
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

/// `doctor`: whether the unit (the scheduled task on Windows) is installed,
/// enabled and running; `None` elsewhere.
pub fn summary() -> Option<String> {
    #[cfg(windows)]
    return Some(windows::summary());
    #[cfg_attr(windows, expect(unreachable_code, reason = "Windows answered above"))]
    if !cfg!(target_os = "linux") {
        return None;
    }
    let path = unit_path()?;
    if !path.exists() {
        return Some("not installed".into());
    }
    Some(format!("installed, {}, {}", state("is-enabled"), state("is-active")))
}

fn lingering() -> Option<bool> {
    let user = std::env::var("USER").ok()?;
    run("loginctl", &["show-user", &user, "--property=Linger", "--value"]).ok().map(|v| v == "yes")
}

pub fn service(base: &crate::Base, exec: &Exec, cmd: &ServiceCmd) -> crate::Fallible {
    #[cfg(windows)]
    return windows::service(base, exec, cmd);
    #[cfg_attr(windows, expect(unreachable_code, reason = "Windows answered above"))]
    if !cfg!(target_os = "linux") {
        return Err("`service` sets up a systemd user service (Linux) or a scheduled task (Windows); on this system use `domovoi-server autostart on`".into());
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

/// Windows: the headless server as a scheduled task that starts when this
/// user signs in. It runs `domovoi-server-launcher.exe --supervise` beside
/// the server, which starts the server with no console window and starts it
/// again after a failure (5 s, backing off to five minutes), as systemd's
/// `Restart=on-failure` does. Ending the task ends the server with it.
#[cfg(any(windows, test))]
pub(crate) mod windows {
    use super::Exec;
    #[cfg(windows)]
    use super::ServiceCmd;
    use std::path::{Path, PathBuf};

    #[cfg(windows)]
    pub const TASK: &str = "Domovoi Server";

    /// What the task runs: the launcher (when it is beside the server) and its arguments.
    pub fn action(exec: &Exec, launcher: Option<PathBuf>) -> (PathBuf, String) {
        match launcher {
            Some(launcher) => (launcher, crate::autostart::command_line(std::iter::once("--supervise".to_owned()).chain(exec.args.iter().cloned()))),
            None => (exec.exe.clone(), crate::autostart::command_line(exec.args.iter().cloned())),
        }
    }

    fn xml_escape(text: &str) -> String {
        text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    }

    /// The task definition (Task Scheduler schema 1.4) for `user`
    /// (`DOMAIN\name`): at their sign-in, in their session, with no time limit.
    pub fn task_xml(user: &str, command: &Path, arguments: &str) -> String {
        let dir = command.parent().map(|d| d.display().to_string()).unwrap_or_default();
        format!(
            r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Domovoi server (headless), started at sign-in. `domovoi-server service uninstall` removes it.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>3</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>"{command}"</Command>
      <Arguments>{arguments}</Arguments>
      <WorkingDirectory>{dir}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>
"#,
            user = xml_escape(user),
            command = xml_escape(&command.display().to_string()),
            arguments = xml_escape(arguments),
            dir = xml_escape(&dir),
        )
    }

    #[cfg(windows)]
    /// `schtasks` reads a definition declared UTF-16 as UTF-16 (with its BOM).
    fn utf16_with_bom(text: &str) -> Vec<u8> {
        std::iter::once(0xFEFF_u16).chain(text.encode_utf16()).flat_map(u16::to_le_bytes).collect()
    }

    #[cfg(windows)]
    fn schtasks(args: &[&str]) -> Result<String, String> {
        super::run("schtasks", args)
    }

    #[cfg(windows)]
    fn installed() -> bool {
        schtasks(&["/Query", "/TN", TASK]).is_ok()
    }

    #[cfg(windows)]
    /// What the task is doing, from what `schtasks` writes the same in every
    /// language (its status column is translated): whether the definition is
    /// on (`/XML`), and the last result code (`/V`).
    fn status() -> Option<String> {
        let xml = schtasks(&["/Query", "/TN", TASK, "/XML"]).ok()?.replace('\0', "");
        if xml.contains("<Enabled>false</Enabled>") {
            return Some("disabled".into());
        }
        let csv = schtasks(&["/Query", "/TN", TASK, "/V", "/FO", "CSV", "/NH"]).ok()?;
        let line = csv.lines().find(|l| !l.trim().is_empty())?;
        Some(last_result(csv_fields(line).get(LAST_RESULT).map(String::as_str)))
    }

    /// The "Last Result" column of `schtasks /Query /V /FO CSV`.
    const LAST_RESULT: usize = 6;

    /// The fields of one CSV line as `schtasks` writes it (each in quotes).
    pub(crate) fn csv_fields(line: &str) -> Vec<String> {
        let (mut fields, mut field, mut quoted) = (vec![], String::new(), false);
        let mut chars = line.trim_end_matches(['\r', '\n']).chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '"' if quoted && chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => quoted = !quoted,
                ',' if !quoted => fields.push(std::mem::take(&mut field)),
                c => field.push(c),
            }
        }
        fields.push(field);
        fields
    }

    /// A task's last result code, as words.
    pub(crate) fn last_result(code: Option<&str>) -> String {
        // SCHED_S_TASK_*: the codes Task Scheduler reports for itself.
        match code.and_then(|c| c.trim().parse::<i64>().ok()) {
            Some(0x41301) => "running".into(),
            Some(0x41300 | 0x41303) => "ready, not run yet".into(),
            Some(0x41306) => "ready, its last run was ended".into(),
            Some(0) => "ready, its last run ended cleanly".into(),
            Some(code) => format!("ready, its last run failed ({:#x})", code as u32),
            None => "state unknown".into(),
        }
    }

    #[cfg(windows)]
    pub fn summary() -> String {
        match status() {
            Some(status) => format!("scheduled task {TASK:?} at sign-in, {status}"),
            None => "not installed".into(),
        }
    }

    #[cfg(windows)]
    fn user() -> String {
        let name = std::env::var("USERNAME").unwrap_or_default();
        match std::env::var("USERDOMAIN") {
            Ok(domain) if !domain.is_empty() => format!("{domain}\\{name}"),
            _ => name,
        }
    }

    #[cfg(windows)]
    pub fn service(base: &crate::Base, exec: &Exec, cmd: &ServiceCmd) -> crate::Fallible {
        match cmd {
            ServiceCmd::Install { linger, no_start, no_remote } => {
                let new = !installed();
                let launcher = crate::autostart::launcher_beside(&exec.exe);
                if launcher.is_none() {
                    println!("warning: domovoi-server-launcher.exe is not beside {}: the task runs the server itself, in a console window, and does not restart it after a failure", exec.exe.display());
                }
                let (command, arguments) = action(exec, launcher);
                let definition = std::env::temp_dir().join(format!("domovoi-task-{}.xml", std::process::id()));
                std::fs::write(&definition, utf16_with_bom(&task_xml(&user(), &command, &arguments)))?;
                let created = schtasks(&["/Create", "/TN", TASK, "/XML", &definition.to_string_lossy(), "/F"]);
                let _ = std::fs::remove_file(&definition);
                created?;
                println!("Scheduled task {TASK:?}: {} {arguments}", command.display());
                if new && !no_remote && !base.remote.settings().enabled {
                    base.remote.save_enabled(true)?;
                    println!("Remote access: on (`domovoi-server remote off` keeps it off across restarts)");
                }
                if !no_start {
                    // A server already running from the task is replaced by the new definition's.
                    let _ = schtasks(&["/End", "/TN", TASK]);
                    schtasks(&["/Run", "/TN", TASK])?;
                }
                if crate::autostart::is_enabled() {
                    println!("warning: launch at login is also on and would start a second server; `domovoi-server autostart off` turns it off");
                }
                if *linger {
                    println!("note: on Windows the task starts when you sign in; starting before anyone signs in needs your password stored with the task (Task Scheduler › Domovoi Server › \"Run whether user is logged on or not\")");
                }
            }
            ServiceCmd::Uninstall => {
                if installed() {
                    let _ = schtasks(&["/End", "/TN", TASK]);
                    schtasks(&["/Delete", "/TN", TASK, "/F"])?;
                    println!("Removed the scheduled task {TASK:?}");
                }
            }
            ServiceCmd::Status => {}
        }
        println!("Service: {}", summary());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A task's state is read from what `schtasks` writes in every language:
    /// the quoted CSV fields and the numeric last result.
    #[test]
    fn the_windows_task_state_reads_the_same_in_every_language() {
        let line = r#""PC","\Domovoi Server","N/A","Gotowe","Interaktywne","25.11.2026 17:26:37","267009","me","""C:\a b\launcher.exe"" --supervise""#;
        let fields = windows::csv_fields(line);
        assert_eq!(fields.len(), 9);
        assert_eq!(fields[3], "Gotowe", "translated, so not read");
        assert_eq!(fields[8], r#""C:\a b\launcher.exe" --supervise"#);
        assert_eq!(windows::last_result(Some(&fields[6])), "running");
        assert_eq!(windows::last_result(Some("0")), "ready, its last run ended cleanly");
        assert_eq!(windows::last_result(Some("267011")), "ready, not run yet");
        assert_eq!(windows::last_result(Some("-2147024894")), "ready, its last run failed (0x80070002)");
        assert_eq!(windows::last_result(Some("N/A")), "state unknown");
    }

    #[test]
    fn the_windows_task_runs_the_launcher_supervising_the_headless_server() {
        let exec = Exec {
            exe: PathBuf::from(r"C:\Users\me\AppData\Local\Programs\Domovoi\domovoi-server.exe"),
            args: vec!["--no-startup-code".into(), "--home-dir".into(), r"C:\Users\me\.domovoi".into(), "--port".into(), "13773".into()],
        };
        let launcher = PathBuf::from(r"C:\Users\me\AppData\Local\Programs\Domovoi\domovoi-server-launcher.exe");
        let (command, arguments) = windows::action(&exec, Some(launcher.clone()));
        assert_eq!(command, launcher);
        assert_eq!(arguments, r"--supervise --no-startup-code --home-dir C:\Users\me\.domovoi --port 13773");
        let xml = windows::task_xml(r"PC\me & co", &command, &arguments);
        assert!(xml.contains(r#"<Command>"C:\Users\me\AppData\Local\Programs\Domovoi\domovoi-server-launcher.exe"</Command>"#), "{xml}");
        assert!(xml.contains(r"<UserId>PC\me &amp; co</UserId>"), "escaped");
        assert!(xml.contains("<LogonTrigger>") && xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(!xml.contains("--tray"), "headless");
        let (bare, _) = windows::action(&exec, None);
        assert_eq!(bare, exec.exe, "without the launcher the server runs itself");
    }

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
