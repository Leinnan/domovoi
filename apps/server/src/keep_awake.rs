//! Keeps the machine awake while agents work (t3code background/KeepAwake,
//! the `keepAwakeWhileAgentsWork` setting): with a provider turn running,
//! the server holds `caffeinate -i -w <pid>` on macOS (`systemd-inhibit`
//! on Linux, when present), and lets it go once the work is done. A phone
//! following a turn over iroh would otherwise lose the Mac to idle sleep.

use std::sync::Arc;
use std::time::Duration;

const CHECK: Duration = Duration::from_secs(5);

/// The command that holds a sleep assertion until it is killed (or this
/// process exits), if the system has one.
fn inhibitor() -> Option<tokio::process::Command> {
    let pid = std::process::id().to_string();
    if cfg!(target_os = "macos") {
        let mut c = crate::process::command("/usr/bin/caffeinate");
        c.args(["-i", "-w", &pid]);
        return Some(c);
    }
    if cfg!(target_os = "linux") {
        let path = ["/usr/bin/systemd-inhibit", "/bin/systemd-inhibit"].into_iter().find(|p| std::path::Path::new(p).exists())?;
        let mut c = crate::process::command(path);
        // `tail --pid` ends with this process, as `caffeinate -w` does, so a
        // crash cannot leave the machine held awake.
        c.args(["--what=idle:sleep", "--who=domovoi-server", "--why=Agents are working", "--mode=block", "tail", &format!("--pid={pid}"), "-f", "/dev/null"]);
        return Some(c);
    }
    None
}

/// Whether to hold the machine awake now.
pub fn wanted(enabled: bool, live_turns: usize) -> bool {
    enabled && live_turns > 0
}

pub fn spawn(state: Arc<crate::state::AppState>) {
    tokio::spawn(async move {
        let mut held: Option<tokio::process::Child> = None;
        loop {
            let enabled = state.settings.read().await.keep_awake_while_agents_work != Some(false);
            let live = match crate::adapters::HOST.get() {
                Some(h) => h.live_turns(),
                None => 0,
            };
            let want = wanted(enabled, live);
            // A holder that exited (killed from outside) is started again.
            if held.as_mut().is_some_and(|c| c.try_wait().ok().flatten().is_some()) {
                held = None;
            }
            match (want, held.is_some()) {
                (true, false) => {
                    if let Some(mut cmd) = inhibitor() {
                        match cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true).spawn() {
                            Ok(c) => {
                                tracing::info!("keeping the machine awake while agents work");
                                held = Some(c);
                            }
                            Err(e) => tracing::warn!("could not keep the machine awake: {e}"),
                        }
                    }
                }
                (false, true) => {
                    if let Some(mut c) = held.take() {
                        let _ = c.kill().await;
                        tracing::info!("agents are idle; the machine may sleep");
                    }
                }
                _ => {}
            }
            tokio::time::sleep(CHECK).await;
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn held_only_while_turns_run_and_the_setting_allows() {
        assert!(super::wanted(true, 1));
        assert!(!super::wanted(true, 0));
        assert!(!super::wanted(false, 3));
    }
}
