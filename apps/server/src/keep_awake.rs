//! Keeps the machine awake while agents work (t3code background/KeepAwake,
//! the `keepAwakeWhileAgentsWork` setting): with a provider turn running,
//! the server holds `caffeinate -i -w <pid>` on macOS (`systemd-inhibit`
//! on Linux, when present; on Windows a thread holding
//! `SetThreadExecutionState`), and lets it go once the work is done. A phone
//! following a turn over iroh would otherwise lose the Mac to idle sleep.

use std::sync::Arc;
use std::time::Duration;

const CHECK: Duration = Duration::from_secs(5);

/// The command that holds a sleep assertion until it is killed (or this
/// process exits), if the system has one.
#[cfg(not(windows))]
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

/// What holds the machine awake while it lives.
enum Holder {
    #[cfg(not(windows))]
    Child(tokio::process::Child),
    #[cfg(windows)]
    Thread(crate::win::AwakeGuard),
}

impl Holder {
    fn start() -> Option<std::io::Result<Holder>> {
        #[cfg(windows)]
        return Some(crate::win::AwakeGuard::hold().map(Holder::Thread));
        #[cfg(not(windows))]
        inhibitor().map(|mut cmd| cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true).spawn().map(Holder::Child))
    }

    /// Whether it let go by itself (killed from outside).
    fn ended(&mut self) -> bool {
        match self {
            #[cfg(not(windows))]
            Holder::Child(c) => c.try_wait().ok().flatten().is_some(),
            #[cfg(windows)]
            Holder::Thread(_) => false,
        }
    }

    async fn release(self) {
        match self {
            #[cfg(not(windows))]
            Holder::Child(mut c) => {
                let _ = c.kill().await;
            }
            #[cfg(windows)]
            Holder::Thread(guard) => drop(guard),
        }
    }
}

pub fn spawn(state: Arc<crate::state::AppState>) {
    tokio::spawn(async move {
        let mut held: Option<Holder> = None;
        loop {
            let enabled = state.settings.read().await.keep_awake_while_agents_work != Some(false);
            let live = match crate::adapters::HOST.get() {
                Some(h) => h.live_turns(),
                None => 0,
            };
            let want = wanted(enabled, live);
            // A holder that exited (killed from outside) is started again.
            if held.as_mut().is_some_and(Holder::ended) {
                held = None;
            }
            match (want, held.is_some()) {
                (true, false) => {
                    if let Some(started) = Holder::start() {
                        match started {
                            Ok(c) => {
                                tracing::info!("keeping the machine awake while agents work");
                                held = Some(c);
                            }
                            Err(e) => tracing::warn!("could not keep the machine awake: {e}"),
                        }
                    }
                }
                (false, true) => {
                    if let Some(c) = held.take() {
                        c.release().await;
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
