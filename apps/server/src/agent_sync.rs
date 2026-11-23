//! Keeps the server in step with Claude Code and Codex on this machine.
//!
//! - **Automatic import** (at start, then every 10 minutes): every folder with
//!   sessions from the last 14 days becomes a project with its recent sessions
//!   ([`agent_sessions::scan`], [`agent_sessions::import_recent`]), unless a
//!   project for it was removed before (`agent_session_roots`).
//! - **Sync** (every minute): when a transcript of an imported folder changes,
//!   that folder is imported again, which adds new sessions and the new
//!   messages of threads Domovoi has not run.
//!
//! `--no-session-import` turns both off; the app's import picker still works.

use crate::agent_sessions::{self, Failure, ScanEnv};
use crate::state::SharedState;
use crate::store::Store;
use rusqlite::params;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Folders active this recently are imported without asking.
const AUTO_WINDOW_MS: i64 = 14 * 24 * 60 * 60 * 1000;
const TICK: Duration = Duration::from_secs(60);
/// Ticks between full scans.
const FULL_EVERY: u32 = 10;

pub fn spawn(state: SharedState) {
    tokio::spawn(async move {
        let mut seen = Seen::default();
        let mut tick: u32 = 0;
        loop {
            let settings = state.settings.read().await.clone();
            let env = ScanEnv::new(&settings, &state.paths.home);
            let store = state.store.clone();
            let full = tick % FULL_EVERY == 0;
            let moved = std::mem::take(&mut seen);
            let result = tokio::task::spawn_blocking(move || {
                let mut seen = moved;
                let report = if full { auto_import(&env, &store) } else { Report::default() };
                let synced = sync(&env, &store, &mut seen);
                (seen, report, synced)
            })
            .await;
            match result {
                Ok((s, report, synced)) => {
                    seen = s;
                    if report.projects > 0 || report.sessions > 0 {
                        tracing::info!("imported {} project(s) and {} session(s) from Claude Code and Codex", report.projects, report.sessions);
                    }
                    if let Some(e) = report.error {
                        tracing::warn!("session import: {e}");
                    }
                    if synced > 0 {
                        tracing::info!("synced {synced} folder(s) with new Claude Code or Codex activity");
                    }
                }
                Err(e) => tracing::warn!("session import stopped: {e}"),
            }
            tick = tick.wrapping_add(1);
            tokio::select! {
                () = state.shutdown.cancelled() => return,
                () = tokio::time::sleep(TICK) => {}
            }
        }
    });
}

#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub projects: usize,
    pub sessions: usize,
    pub error: Option<String>,
}

fn now_iso() -> String {
    domovoi_contracts::iso_now()
}

/// Records which roots have a project now and which lost theirs. Returns the
/// roots marked removed.
fn reconcile_roots(store: &Store) -> rusqlite::Result<HashSet<String>> {
    let present: HashSet<String> = store.list_projects().map(|ps| ps.into_iter().map(|p| norm(&p.workspace_root)).collect()).unwrap_or_default();
    store.db().with(|c| {
        let tx = c.savepoint()?;
        let known: Vec<(String, String)> = {
            let mut st = tx.prepare("SELECT root, state FROM agent_session_roots")?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
        };
        let now = now_iso();
        let mut removed = HashSet::new();
        for (root, state) in &known {
            match (state.as_str(), present.contains(root)) {
                ("present", false) => {
                    tx.execute("UPDATE agent_session_roots SET state = 'removed', updated_at = ?2 WHERE root = ?1", params![root, now])?;
                    removed.insert(root.clone());
                }
                // Added again by hand: it counts as present again.
                ("removed", true) => {
                    tx.execute("UPDATE agent_session_roots SET state = 'present', updated_at = ?2 WHERE root = ?1", params![root, now])?;
                }
                ("removed", false) => {
                    removed.insert(root.clone());
                }
                _ => {}
            }
        }
        let known_roots: HashSet<&String> = known.iter().map(|(r, _)| r).collect();
        for root in present.iter().filter(|r| !known_roots.contains(r)) {
            tx.execute("INSERT INTO agent_session_roots (root, state, updated_at) VALUES (?1, 'present', ?2)", params![root, now])?;
        }
        tx.commit()?;
        Ok(removed)
    })
}

fn norm(path: &str) -> String {
    let t = path.trim();
    let trimmed = t.trim_end_matches('/');
    if trimmed.is_empty() { t.to_owned() } else { trimmed.to_owned() }
}

/// Rounds of a full pass: a big Claude folder cuts the scan short (it reads a
/// bounded amount), and transcripts already imported are not read again, so
/// each round reaches further.
const MAX_ROUNDS: usize = 6;

/// One full pass: new projects for recently active folders.
pub fn auto_import(env: &ScanEnv, store: &Store) -> Report {
    let mut total = Report::default();
    for _ in 0..MAX_ROUNDS {
        let (round, truncated) = auto_import_round(env, store);
        let progressed = round.projects + round.sessions > 0;
        total.projects += round.projects;
        total.sessions += round.sessions;
        total.error = round.error.or(total.error);
        if !truncated || !progressed {
            break;
        }
    }
    total
}

/// One scan's worth; also says whether the scan was cut short.
fn auto_import_round(env: &ScanEnv, store: &Store) -> (Report, bool) {
    let mut report = Report::default();
    let removed = match reconcile_roots(store) {
        Ok(r) => r,
        Err(e) => {
            report.error = Some(e.to_string());
            return (report, false);
        }
    };
    let scanned = match agent_sessions::scan(env, store) {
        Ok(s) => s,
        Err(e) => {
            report.error = Some(match e {
                Failure::Scan { cause, .. } => cause,
                _ => "the scan failed".to_owned(),
            });
            return (report, false);
        }
    };
    // Same format on both sides, so the strings compare as times.
    let cutoff = domovoi_contracts::time_fmt::iso_from_millis(domovoi_contracts::time_fmt::now_millis() - AUTO_WINDOW_MS);
    let with_imports: HashSet<String> = imported_roots(store).into_iter().map(|(_, r)| norm(&r)).collect();
    for c in &scanned.candidates {
        let path = c.path.as_str();
        let recent = c.last_active_at.as_ref().is_some_and(|t| t.as_str() >= cutoff.as_str());
        if !recent || c.thread_count == 0 || removed.contains(&norm(path)) {
            continue;
        }
        // A project you added yourself gets its sessions once; sync keeps it current.
        if c.already_imported {
            if let (Some(id), false) = (c.project_id.as_ref(), with_imports.contains(&norm(path))) {
                match agent_sessions::import_recent(env, store, id.as_str(), Some(path)) {
                    Ok(r) => report.sessions += r.imported_count as usize,
                    Err(e) => report.error = Some(e.written()),
                }
            }
            continue;
        }
        if !Path::new(path).is_dir() {
            continue;
        }
        let title = c.title.as_str();
        let project_id = match crate::rpc::projects::bootstrap_in(store, &format!("agent-session-auto-import:{}", crate::util::new_uuid()), title, path) {
            Ok((id, created)) => {
                if created {
                    report.projects += 1;
                }
                id
            }
            Err(e) => {
                report.error = Some(e.to_string());
                continue;
            }
        };
        match agent_sessions::import_recent(env, store, &project_id, Some(path)) {
            Ok(r) => report.sessions += r.imported_count as usize,
            Err(e) => report.error = Some(e.written()),
        }
    }
    // The new projects count as present from now on.
    let _ = reconcile_roots(store);
    (report, scanned.truncated == Some(true))
}

/// Transcript files last seen: path → (size, mtime).
#[derive(Default)]
pub struct Seen {
    files: HashMap<PathBuf, (u64, SystemTime)>,
    primed: bool,
}

/// Claude Code's folder name for a project root: every character other than
/// a letter or digit becomes `-`.
fn claude_folder(root: &str) -> String {
    root.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// The folder pi keeps a cwd's sessions in: `--<path>--`, the leading
/// separator dropped and `/`, `\\`, `:` as `-`.
fn pi_folder(root: &str) -> String {
    let path = root.trim_start_matches(['/', '\\']);
    format!("--{}--", path.chars().map(|c| if matches!(c, '/' | '\\' | ':') { '-' } else { c }).collect::<String>())
}

fn jsonl_files(dir: &Path, out: &mut Vec<(PathBuf, u64, SystemTime)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().is_some_and(|x| x == "jsonl") {
            if let Ok(m) = e.metadata() {
                out.push((path, m.len(), m.modified().unwrap_or(SystemTime::UNIX_EPOCH)));
            }
        }
    }
}

/// Codex's dated session folders for today and yesterday (local time is not
/// known here, so UTC ± a day covers it).
fn codex_recent_dirs(home: &Path) -> Vec<PathBuf> {
    let now = domovoi_contracts::time_fmt::now_millis();
    let day = 24 * 60 * 60 * 1000;
    let mut dirs = vec![];
    for offset in [-day, 0, day] {
        let iso = domovoi_contracts::time_fmt::iso_from_millis(now + offset);
        let (y, m, d) = (&iso[0..4], &iso[5..7], &iso[8..10]);
        let dir = home.join("sessions").join(y).join(m).join(d);
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// Roots with imports and the project each belongs to.
fn imported_roots(store: &Store) -> Vec<(String, String)> {
    let cwds: HashSet<String> = store
        .db()
        .with(|c| {
            let mut st = c.prepare("SELECT DISTINCT cwd FROM agent_session_imports")?;
            st.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<HashSet<_>>>()
        })
        .unwrap_or_default()
        .into_iter()
        .map(|c| norm(&c))
        .collect();
    store
        .list_projects()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| cwds.contains(&norm(&p.workspace_root)))
        .map(|p| (p.id, p.workspace_root))
        .collect()
}

/// One sync pass. Returns how many folders were imported again.
pub fn sync(env: &ScanEnv, store: &Store, seen: &mut Seen) -> usize {
    let roots = imported_roots(store);
    if roots.is_empty() {
        return 0;
    }
    let mut files = vec![];
    let mut claude_by_dir: HashMap<PathBuf, usize> = HashMap::new();
    for (source, _, home) in &env.homes {
        match source {
            agent_sessions::Source::Claude => {
                for (i, (_, root)) in roots.iter().enumerate() {
                    let dir = home.join("projects").join(claude_folder(&norm(root)));
                    claude_by_dir.insert(dir.clone(), i);
                    jsonl_files(&dir, &mut files);
                }
            }
            agent_sessions::Source::Codex => {
                for dir in codex_recent_dirs(home) {
                    jsonl_files(&dir, &mut files);
                }
            }
            agent_sessions::Source::Pi => {
                for (i, (_, root)) in roots.iter().enumerate() {
                    let dir = home.join(pi_folder(&norm(root)));
                    claude_by_dir.insert(dir.clone(), i);
                    jsonl_files(&dir, &mut files);
                }
            }
        }
    }
    let mut dirty: HashSet<usize> = HashSet::new();
    let mut codex_changed = false;
    let mut next = HashMap::new();
    for (path, size, mtime) in files {
        let changed = seen.files.get(&path) != Some(&(size, mtime));
        // A session this server runs (its transcript is named by the session
        // id) grows with every turn, and import never writes into its thread.
        let ours = || path.file_stem().and_then(|s| s.to_str()).is_some_and(|id| agent_sessions::native_thread_owned(store, "claudeAgent", id));
        if changed && seen.primed && !ours() {
            match path.parent().and_then(|p| claude_by_dir.get(p)) {
                Some(&i) => {
                    dirty.insert(i);
                }
                None => codex_changed = true,
            }
        }
        next.insert(path, (size, mtime));
    }
    // A removed file changes nothing here; the thread stays.
    seen.files = next;
    if !seen.primed {
        // The first pass only learns the files; the full import just ran.
        seen.primed = true;
        return 0;
    }
    if codex_changed {
        // A Codex file does not name its folder without a read; check them all.
        dirty.extend(0..roots.len());
    }
    let mut synced = 0;
    for i in dirty {
        let (project_id, root) = &roots[i];
        match agent_sessions::import_recent(env, store, project_id, Some(root)) {
            Ok(_) => synced += 1,
            Err(e) => tracing::warn!(root = %root, "session sync failed: {}", e.written()),
        }
    }
    synced
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_folder_names_match_claude_code() {
        assert_eq!(claude_folder("/Users/me/Documents/IDEAS/t5code"), "-Users-me-Documents-IDEAS-t5code");
        assert_eq!(claude_folder("/Users/me/my_app.v2"), "-Users-me-my-app-v2");
    }

    #[test]
    fn pi_folder_names_match_pi() {
        assert_eq!(pi_folder("/Users/me/Documents/IDEAS/t5code"), "--Users-me-Documents-IDEAS-t5code--");
        assert_eq!(pi_folder("/Users/me/my_app.v2"), "--Users-me-my_app.v2--");
    }

    #[test]
    fn codex_dirs_cover_three_days() {
        let dirs = codex_recent_dirs(Path::new("/h"));
        assert_eq!(dirs.len(), 3);
        assert!(dirs.iter().all(|d| d.starts_with("/h/sessions")));
    }
}
