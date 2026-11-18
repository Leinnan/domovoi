//! macOS can stop Domovoi Server reading a protected folder (~/Documents,
//! ~/Desktop, …) for the rest of the process's life. After a burst of
//! privacy prompts (an agent's `find /` set one off), every read there fails
//! with "Operation not permitted", from the server and from every agent it
//! starts, though the permission is still granted. A new process reads it
//! again.
//!
//! So the server watches its projects' folders. When one it could read is
//! refused, it restarts itself (the menu bar app opens again, and turns cut
//! off carry on). At most once per half hour: a block that is back within
//! that time, or that a restart did not clear, is shown as a notice instead.

use crate::state::AppState;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const CHECK_EVERY: Duration = Duration::from_secs(30);
/// Restarts closer together than this become a notice.
const RESTART_GAP: Duration = Duration::from_secs(30 * 60);
/// When the server last restarted itself over a block (Unix seconds).
const LAST_RESTART: &str = "folderBlockRestartAt";

/// The notice every provider shows while a block lasts.
static NOTICE: RwLock<Option<String>> = RwLock::new(None);

pub fn notice() -> Option<String> {
    NOTICE.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

/// What to do about the project folders' state.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    Restart(PathBuf),
    Notice(PathBuf),
}

/// Remembers which project folders this process could read.
#[derive(Default)]
pub struct Watch {
    readable: HashSet<PathBuf>,
}

impl Watch {
    /// Reads each of `roots` and decides. `recent_restart`: the server
    /// restarted itself over a block less than `RESTART_GAP` ago.
    pub fn check(&mut self, roots: &[PathBuf], recent_restart: bool) -> Action {
        let mut lost = None;
        let mut never = None;
        for root in roots {
            match std::fs::read_dir(root) {
                Ok(_) => {
                    self.readable.insert(root.clone());
                }
                Err(e) if refused(&e) => {
                    if self.readable.contains(root) {
                        lost.get_or_insert_with(|| root.clone());
                    } else {
                        never.get_or_insert_with(|| root.clone());
                    }
                }
                Err(_) => {}
            }
        }
        decide(lost, never, recent_restart)
    }
}

/// `lost`: a folder this process could read and now can't. `never`: one
/// refused since it started.
fn decide(lost: Option<PathBuf>, never: Option<PathBuf>, recent_restart: bool) -> Action {
    match (lost, never) {
        (Some(root), _) if recent_restart => Action::Notice(root),
        (Some(root), _) => Action::Restart(root),
        // Refused since this process started, right after a restart over a
        // block: restarting did not clear it.
        (None, Some(root)) if recent_restart => Action::Notice(root),
        // Never readable and no restart: maybe never granted, not a block.
        _ => Action::Nothing,
    }
}

/// macOS privacy refusals are EPERM; plain Unix permissions are EACCES.
fn refused(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(libc::EPERM)
}

fn message(root: &Path) -> String {
    format!(
        "macOS has stopped Domovoi Server from reading {} (\"Operation not permitted\"), so agents can't work there. Quit Domovoi Server from its menu bar icon and open it again. If it keeps happening, check System Settings › Privacy & Security › Files and Folders.",
        root.display()
    )
}

/// The app bundle this server runs from, which `open` can start again.
fn app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let app = exe.parent()?.parent()?.parent()?;
    (app.extension().is_some_and(|e| e == "app") && exe.parent()?.ends_with("Contents/MacOS")).then(|| app.to_owned())
}

/// Opens `app` again once this process has exited (a fresh process gets a
/// fresh look from macOS).
fn relaunch_after_exit(app: &Path) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    crate::process::blocking_command("/bin/sh")
        .arg("-c")
        .arg(r#"while kill -0 "$0" 2>/dev/null; do sleep 0.2; done; exec /usr/bin/open "$1""#)
        .arg(std::process::id().to_string())
        .arg(app)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        // Not stopped with this process.
        .process_group(0)
        .spawn()
        .map(drop)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Watches the project folders while the server runs (macOS only).
pub fn spawn(state: Arc<AppState>, shutdown: CancellationToken) {
    if !cfg!(target_os = "macos") {
        return;
    }
    tokio::spawn(async move {
        let mut watch = Watch::default();
        let mut tick = tokio::time::interval(CHECK_EVERY);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                _ = tick.tick() => {}
            }
            let roots: Vec<PathBuf> = state.store.list_projects().unwrap_or_default().into_iter().map(|p| PathBuf::from(p.workspace_root)).collect();
            let last = state.store.db().meta_get(LAST_RESTART).ok().flatten().and_then(|s| s.parse::<u64>().ok());
            let recent = last.is_some_and(|t| now_secs().saturating_sub(t) < RESTART_GAP.as_secs());
            // Reading a folder macOS is deciding about can wait on a prompt.
            let Ok((w, action)) = tokio::task::spawn_blocking(move || {
                let action = watch.check(&roots, recent);
                (watch, action)
            })
            .await
            else {
                return;
            };
            watch = w;
            let notice = match action {
                Action::Restart(root) => match app_bundle() {
                    Some(app) => {
                        tracing::warn!(folder = %root.display(), "macOS stopped this process reading a project folder; restarting");
                        let _ = state.store.db().meta_set(LAST_RESTART, &now_secs().to_string());
                        match relaunch_after_exit(&app) {
                            Ok(()) => {
                                shutdown.cancel();
                                return;
                            }
                            Err(e) => {
                                tracing::warn!("could not restart: {e}");
                                Some(message(&root))
                            }
                        }
                    }
                    // Not the app (a terminal's child): it can't open itself again.
                    None => Some(message(&root)),
                },
                Action::Notice(root) => Some(message(&root)),
                Action::Nothing => None,
            };
            let changed = {
                let mut current = NOTICE.write().unwrap_or_else(std::sync::PoisonError::into_inner);
                let changed = *current != notice;
                if changed {
                    if let Some(n) = &notice {
                        tracing::warn!("{n}");
                    }
                    *current = notice;
                }
                changed
            };
            if changed {
                let all = state.providers.snapshot().await;
                let _ = state.provider_updates.send(all);
            }
        }
    });
}

/// Puts the notice on every provider while a block lasts: none can work in
/// the folder, and the composer says why.
pub fn apply(providers: &mut [domovoi_model::typed::ServerProvider]) {
    if let Some(n) = notice() {
        for p in providers {
            p.status = domovoi_model::typed::ServerProviderState::Error;
            p.message = Some(n.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn missing_folders_and_unix_permissions_are_not_blocks() {
        let dir = std::env::temp_dir().join(format!("domovoi-folder-{}", crate::util::new_uuid()));
        let (ok, gone) = (dir.join("ok"), dir.join("gone"));
        std::fs::create_dir_all(&ok).unwrap();
        let mut watch = Watch::default();
        assert_eq!(watch.check(&[ok.clone(), gone.clone()], false), Action::Nothing, "a missing folder is not a block");
        // Plain Unix permissions (EACCES) are not macOS refusing this process.
        std::fs::set_permissions(&ok, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert_eq!(watch.check(std::slice::from_ref(&ok), false), Action::Nothing);
        std::fs::set_permissions(&ok, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn decisions_follow_what_was_readable_and_the_last_restart() {
        let root = || Some(PathBuf::from("/nowhere/project"));
        let p = PathBuf::from("/nowhere/project");
        assert_eq!(decide(root(), None, false), Action::Restart(p.clone()));
        assert_eq!(decide(root(), None, true), Action::Notice(p.clone()), "again within the half hour");
        assert_eq!(decide(None, root(), true), Action::Notice(p.clone()), "a restart did not clear it");
        assert_eq!(decide(None, root(), false), Action::Nothing, "never readable, no recent restart: not ours to judge");
        assert_eq!(decide(None, None, true), Action::Nothing);
    }

    #[test]
    fn the_notice_goes_on_every_provider_while_it_lasts() {
        use domovoi_model::typed::{ServerProvider, ServerProviderState};
        let provider = |instance: &str| -> ServerProvider {
            serde_json::from_value(serde_json::json!({
                "instanceId": instance, "driver": instance, "enabled": true, "installed": true, "version": "1.0.0", "status": "ready",
                "auth": { "status": "authenticated" }, "checkedAt": "2026-01-02T03:04:05.678Z", "models": [],
            }))
            .unwrap()
        };
        let mut providers = vec![provider("claudeAgent"), provider("codex")];
        apply(&mut providers);
        assert!(providers.iter().all(|p| p.status == ServerProviderState::Ready));
        *NOTICE.write().unwrap() = Some(message(Path::new("/Users/me/Documents/app")));
        apply(&mut providers);
        *NOTICE.write().unwrap() = None;
        assert!(providers.iter().all(|p| p.status == ServerProviderState::Error && p.message.as_deref().unwrap().contains("/Users/me/Documents/app")));
    }
}
