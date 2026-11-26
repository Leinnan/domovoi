//! Lightweight local codebase indexer backed by SQLite (`repo_index_*`).
//!
//! Reads git tree entries and blobs directly via `git ls-tree` and
//! `git cat-file --batch` without modifying the working tree, extracting:
//! - package manifests and file-to-package ownership
//! - import and test-to-source edges
//! - lightweight top-level symbols (Rust, TS/JS, Go, Python, Ruby, Swift, Kotlin)
//! - repository convention documents (`REVIEW.md`, `AGENTS.md`, `CLAUDE.md`, `CONTRIBUTING.md`)

pub mod imports;
pub mod manifests;
pub mod query;
pub mod symbols;

use crate::db::Db;
use domovoi_model::typed::ProjectIndexStatus;
use rusqlite::{OptionalExtension, params};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

const MAX_BLOB_READ_BYTES: i64 = 2_000_000;
const MAX_PARSE_BYTES: usize = 250_000;

#[derive(Clone, Debug)]
enum LiveState {
    Indexing,
    Failed(String),
}

fn live_states() -> &'static Mutex<HashMap<String, LiveState>> {
    static LIVE: OnceLock<Mutex<HashMap<String, LiveState>>> = OnceLock::new();
    LIVE.get_or_init(Default::default)
}

#[derive(Clone, Debug)]
pub struct IndexedFile {
    pub path: String,
    pub blob_sha: String,
    pub size_bytes: i64,
    pub language: Option<String>,
    pub package_path: Option<String>,
    pub is_test: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct IndexedEdge {
    pub source_path: String,
    pub target_path: String,
    pub kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedSymbol {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub start_line: i64,
    pub end_line: i64,
    pub signature: Option<String>,
    pub is_exported: bool,
}

#[derive(Clone, Debug)]
struct TreeEntry {
    path: String,
    blob_sha: String,
    size_bytes: i64,
}

/// Returns the current index status for `project_id`.
pub fn status(db: &Db, project_id: &str) -> ProjectIndexStatus {
    let live = live_states().lock().unwrap_or_else(|e| e.into_inner()).get(project_id).cloned();
    let stored = read_stored_status(db, project_id).ok().flatten();
    match (live, stored) {
        (Some(LiveState::Indexing), Some(mut s)) => {
            s.status = "indexing".into();
            s.error = None;
            s
        }
        (Some(LiveState::Indexing), None) => ProjectIndexStatus {
            project_id: project_id.to_owned(),
            status: "indexing".into(),
            ..Default::default()
        },
        (Some(LiveState::Failed(err)), Some(mut s)) => {
            s.status = "failed".into();
            s.error = Some(err);
            s
        }
        (Some(LiveState::Failed(err)), None) => ProjectIndexStatus {
            project_id: project_id.to_owned(),
            status: "failed".into(),
            error: Some(err),
            ..Default::default()
        },
        (None, Some(s)) => s,
        (None, None) => ProjectIndexStatus {
            project_id: project_id.to_owned(),
            status: "idle".into(),
            ..Default::default()
        },
    }
}

fn read_stored_status(db: &Db, project_id: &str) -> Result<Option<ProjectIndexStatus>, rusqlite::Error> {
    db.read(|c| {
        c.query_row(
            "SELECT base_ref, commit_sha, indexed_at, file_count, package_count, edge_count, symbol_count, convention_paths_json
             FROM repo_index_state WHERE project_id = ?1",
            params![project_id],
            |row| {
                let conventions_raw: String = row.get(7)?;
                let convention_paths: Vec<String> = serde_json::from_str(&conventions_raw).unwrap_or_default();
                Ok(ProjectIndexStatus {
                    project_id: project_id.to_owned(),
                    status: "ready".into(),
                    base_ref: Some(row.get(0)?),
                    commit_sha: Some(row.get(1)?),
                    indexed_at: Some(row.get(2)?),
                    file_count: row.get::<_, i64>(3)?.max(0) as u64,
                    package_count: row.get::<_, i64>(4)?.max(0) as u64,
                    edge_count: row.get::<_, i64>(5)?.max(0) as u64,
                    symbol_count: row.get::<_, i64>(6)?.max(0) as u64,
                    convention_paths,
                    error: None,
                })
            },
        )
        .optional()
    })
}

/// Indexes `repo_root` at `base_ref` (default `"HEAD"`) directly from git objects into SQLite.
/// Skips work if the same commit SHA is already indexed unless `force` is true.
pub fn index_project(
    db: &Db,
    project_id: &str,
    repo_root: &Path,
    base_ref: Option<&str>,
    force: bool,
    cancel: &AtomicBool,
) -> Result<ProjectIndexStatus, String> {
    if !repo_root.is_dir() {
        return Err("Project directory does not exist.".into());
    }
    {
        let mut states = live_states().lock().unwrap_or_else(|e| e.into_inner());
        states.insert(project_id.to_owned(), LiveState::Indexing);
    }
    let outcome = index_project_inner(db, project_id, repo_root, base_ref, force, cancel);
    {
        let mut states = live_states().lock().unwrap_or_else(|e| e.into_inner());
        match &outcome {
            Ok(_) => {
                states.remove(project_id);
            }
            Err(err) => {
                states.insert(project_id.to_owned(), LiveState::Failed(err.clone()));
            }
        }
    }
    outcome
}

fn index_project_inner(
    db: &Db,
    project_id: &str,
    repo_root: &Path,
    base_ref: Option<&str>,
    force: bool,
    cancel: &AtomicBool,
) -> Result<ProjectIndexStatus, String> {
    check_cancel(cancel)?;
    let target_ref = base_ref.filter(|r| !r.trim().is_empty()).unwrap_or("HEAD");
    let commit_sha = git_output(repo_root, &["rev-parse", "--verify", &format!("{target_ref}^{{commit}}")])?;
    if !force
        && let Ok(Some(existing)) = read_stored_status(db, project_id)
        && existing.commit_sha.as_deref() == Some(commit_sha.as_str())
    {
        return Ok(existing);
    }

    let entries = list_tree(repo_root, &commit_sha)?;
    check_cancel(cancel)?;

    let existing_shas = load_existing_blob_shas(db, project_id).unwrap_or_default();
    let existing_edges = load_existing_edges(db, project_id).unwrap_or_default();
    let existing_symbols = load_existing_symbols(db, project_id).unwrap_or_default();

    let all_paths: BTreeSet<String> = entries.iter().map(|e| e.path.clone()).collect();
    let manifest_entries: Vec<&TreeEntry> = entries.iter().filter(|e| manifests::is_manifest(&e.path)).collect();
    let manifest_blobs = read_blobs(repo_root, &manifest_entries.iter().map(|e| e.blob_sha.as_str()).collect::<Vec<_>>(), cancel)?;
    let mut manifest_contents: BTreeMap<String, String> = BTreeMap::new();
    for entry in manifest_entries {
        if let Some(bytes) = manifest_blobs.get(&entry.blob_sha)
            && let Ok(text) = std::str::from_utf8(bytes)
        {
            manifest_contents.insert(entry.path.clone(), text.to_owned());
        }
    }
    let pkg_map = manifests::build_package_map(&manifest_contents, &all_paths);

    // Determine which source files need blob reads (new or changed blob_sha, or forced).
    let mut to_parse: Vec<&TreeEntry> = Vec::new();
    let mut unchanged_paths: HashSet<String> = HashSet::new();
    for entry in &entries {
        let lang = detect_language(&entry.path);
        if lang.is_none() || entry.size_bytes <= 0 || entry.size_bytes > MAX_BLOB_READ_BYTES {
            continue;
        }
        if !force && existing_shas.get(&entry.path).is_some_and(|sha| sha == &entry.blob_sha) {
            unchanged_paths.insert(entry.path.clone());
        } else {
            to_parse.push(entry);
        }
    }

    let parsed_blobs = read_blobs(repo_root, &to_parse.iter().map(|e| e.blob_sha.as_str()).collect::<Vec<_>>(), cancel)?;
    check_cancel(cancel)?;

    let mut files = Vec::with_capacity(entries.len());
    let mut convention_paths = Vec::new();
    let mut edge_set: HashSet<IndexedEdge> = HashSet::new();
    let mut symbol_list: Vec<IndexedSymbol> = Vec::new();

    for entry in &entries {
        if is_convention_doc(&entry.path) {
            convention_paths.push(entry.path.clone());
        }
        let language = detect_language(&entry.path).map(str::to_owned);
        let package_path = pkg_map.owner_of(&entry.path).map(str::to_owned);
        let is_test = is_test_path(&entry.path);
        files.push(IndexedFile {
            path: entry.path.clone(),
            blob_sha: entry.blob_sha.clone(),
            size_bytes: entry.size_bytes,
            language: language.clone(),
            package_path,
            is_test,
        });

        if unchanged_paths.contains(&entry.path) {
            if let Some(prev_edges) = existing_edges.get(&entry.path) {
                for e in prev_edges {
                    if all_paths.contains(&e.target_path) {
                        edge_set.insert(e.clone());
                    }
                }
            }
            if let Some(prev_syms) = existing_symbols.get(&entry.path) {
                symbol_list.extend(prev_syms.iter().cloned());
            }
            continue;
        }

        if let Some(lang) = language.as_deref()
            && let Some(raw) = parsed_blobs.get(&entry.blob_sha)
        {
            if raw.len() > MAX_PARSE_BYTES || raw.contains(&0) {
                continue;
            }
            if let Ok(content) = std::str::from_utf8(raw) {
                for target in imports::extract_imports(&entry.path, lang, content, &all_paths) {
                    if target != entry.path {
                        edge_set.insert(IndexedEdge {
                            source_path: entry.path.clone(),
                            target_path: target,
                            kind: "import".into(),
                        });
                    }
                }
                symbol_list.extend(symbols::extract_symbols(&entry.path, lang, content));
            }
        }
    }

    // Add package dependency edges and test-to-source heuristic edges.
    for (src, dst) in pkg_map.package_dep_edges() {
        edge_set.insert(IndexedEdge { source_path: src, target_path: dst, kind: "package_dep".into() });
    }
    for (test_path, source_path) in imports::infer_test_edges(&all_paths) {
        edge_set.insert(IndexedEdge { source_path: test_path, target_path: source_path, kind: "test_for".into() });
    }

    convention_paths.sort();
    let indexed_at = crate::time_fmt::iso_now();
    let package_count = pkg_map.package_count() as i64;
    let file_count = files.len() as i64;
    let edge_count = edge_set.len() as i64;
    let symbol_count = symbol_list.len() as i64;
    let conventions_json = serde_json::to_string(&convention_paths).unwrap_or_else(|_| "[]".into());

    db.batch(|| {
        db.with(|c| {
            c.execute("DELETE FROM repo_index_files WHERE project_id = ?1", params![project_id])?;
            c.execute("DELETE FROM repo_index_edges WHERE project_id = ?1", params![project_id])?;
            c.execute("DELETE FROM repo_index_symbols WHERE project_id = ?1", params![project_id])?;

            {
                let mut stmt = c.prepare(
                    "INSERT INTO repo_index_files (project_id, path, blob_sha, size_bytes, language, package_path, is_test)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )?;
                for f in &files {
                    stmt.execute(params![project_id, f.path, f.blob_sha, f.size_bytes, f.language, f.package_path, i64::from(f.is_test)])?;
                }
            }
            {
                let mut stmt = c.prepare(
                    "INSERT OR IGNORE INTO repo_index_edges (project_id, source_path, target_path, kind)
                     VALUES (?1, ?2, ?3, ?4)",
                )?;
                for e in &edge_set {
                    stmt.execute(params![project_id, e.source_path, e.target_path, e.kind])?;
                }
            }
            {
                let mut stmt = c.prepare(
                    "INSERT INTO repo_index_symbols (project_id, path, name, kind, start_line, end_line, signature, is_exported)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )?;
                for s in &symbol_list {
                    stmt.execute(params![project_id, s.path, s.name, s.kind, s.start_line, s.end_line, s.signature, i64::from(s.is_exported)])?;
                }
            }
            c.execute(
                "INSERT OR REPLACE INTO repo_index_state
                 (project_id, base_ref, commit_sha, indexed_at, file_count, package_count, edge_count, symbol_count, convention_paths_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![project_id, target_ref, commit_sha, indexed_at, file_count, package_count, edge_count, symbol_count, conventions_json],
            )?;
            Ok::<_, rusqlite::Error>(())
        })
    })
    .map_err(|e| format!("Could not store codebase index: {e}"))?;

    Ok(ProjectIndexStatus {
        project_id: project_id.to_owned(),
        status: "ready".into(),
        base_ref: Some(target_ref.to_owned()),
        commit_sha: Some(commit_sha),
        indexed_at: Some(indexed_at),
        file_count: file_count as u64,
        package_count: package_count as u64,
        edge_count: edge_count as u64,
        symbol_count: symbol_count as u64,
        convention_paths,
        error: None,
    })
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) { Err("Indexing cancelled.".into()) } else { Ok(()) }
}

fn git_output(repo_root: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = crate::process::blocking_command("git");
    cmd.current_dir(repo_root).args(args);
    let out = crate::process::output_blocking(&mut cmd, crate::process::QUICK).map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

fn list_tree(repo_root: &Path, commit_sha: &str) -> Result<Vec<TreeEntry>, String> {
    let mut cmd = crate::process::blocking_command("git");
    cmd.current_dir(repo_root).args(["ls-tree", "-r", "-l", "-z", commit_sha]);
    let out = crate::process::output_blocking(&mut cmd, crate::process::QUICK).map_err(|e| format!("git ls-tree failed: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    let mut entries = Vec::new();
    for raw in out.stdout.split(|b| *b == 0) {
        if raw.is_empty() {
            continue;
        }
        let Ok(line) = std::str::from_utf8(raw) else { continue };
        let Some((meta, path)) = line.split_once('\t') else { continue };
        if is_ignored_path(path) {
            continue;
        }
        let mut parts = meta.split_whitespace();
        let _mode = parts.next();
        let kind = parts.next();
        let sha = parts.next();
        let size = parts.next();
        if kind != Some("blob") {
            continue;
        }
        let (Some(blob_sha), Some(size_s)) = (sha, size) else { continue };
        let size_bytes = size_s.parse::<i64>().unwrap_or(0);
        entries.push(TreeEntry { path: path.to_owned(), blob_sha: blob_sha.to_owned(), size_bytes });
    }
    Ok(entries)
}

fn read_blobs(repo_root: &Path, shas: &[&str], cancel: &AtomicBool) -> Result<HashMap<String, Vec<u8>>, String> {
    if shas.is_empty() {
        return Ok(HashMap::new());
    }
    let unique: BTreeSet<&str> = shas.iter().copied().collect();
    let mut cmd = crate::process::blocking_command("git");
    cmd.current_dir(repo_root)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| format!("git cat-file --batch failed: {e}"))?;
    let _tree = crate::process::Tree::of(Some(child.id()));

    let mut stdin = child.stdin.take().ok_or("git cat-file stdin unavailable")?;
    let shas_owned: Vec<String> = unique.iter().map(|s| (*s).to_owned()).collect();
    let writer = std::thread::spawn(move || {
        for sha in shas_owned {
            if writeln!(stdin, "{sha}").is_err() {
                break;
            }
        }
    });

    let stdout = child.stdout.take().ok_or("git cat-file stdout unavailable")?;
    let mut reader = BufReader::new(stdout);
    let mut out = HashMap::with_capacity(unique.len());
    let mut header = String::new();
    for _ in 0..unique.len() {
        check_cancel(cancel)?;
        header.clear();
        if reader.read_line(&mut header).map_err(|e| e.to_string())? == 0 {
            break;
        }
        let parts: Vec<&str> = header.trim_end().split_whitespace().collect();
        if parts.len() < 3 || parts[1] != "blob" {
            continue;
        }
        let sha = parts[0].to_owned();
        let size: usize = parts[2].parse().unwrap_or(0);
        let mut buf = vec![0u8; size];
        reader.read_exact(&mut buf).map_err(|e| e.to_string())?;
        // Consume trailing newline emitted by `git cat-file --batch`.
        let mut nl = [0u8; 1];
        let _ = reader.read_exact(&mut nl);
        out.insert(sha, buf);
    }
    let _ = writer.join();
    let _ = child.wait();
    Ok(out)
}

fn load_existing_blob_shas(db: &Db, project_id: &str) -> Result<HashMap<String, String>, rusqlite::Error> {
    db.read(|c| {
        let mut stmt = c.prepare("SELECT path, blob_sha FROM repo_index_files WHERE project_id = ?1")?;
        let rows = stmt.query_map(params![project_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect()
    })
}

fn load_existing_edges(db: &Db, project_id: &str) -> Result<HashMap<String, Vec<IndexedEdge>>, rusqlite::Error> {
    db.read(|c| {
        let mut stmt = c.prepare("SELECT source_path, target_path, kind FROM repo_index_edges WHERE project_id = ?1 AND kind = 'import'")?;
        let rows = stmt.query_map(params![project_id], |r| {
            Ok(IndexedEdge { source_path: r.get(0)?, target_path: r.get(1)?, kind: r.get(2)? })
        })?;
        let mut map: HashMap<String, Vec<IndexedEdge>> = HashMap::new();
        for row in rows {
            let edge = row?;
            map.entry(edge.source_path.clone()).or_default().push(edge);
        }
        Ok(map)
    })
}

fn load_existing_symbols(db: &Db, project_id: &str) -> Result<HashMap<String, Vec<IndexedSymbol>>, rusqlite::Error> {
    db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT path, name, kind, start_line, end_line, signature, is_exported
             FROM repo_index_symbols WHERE project_id = ?1",
        )?;
        let rows = stmt.query_map(params![project_id], |r| {
            Ok(IndexedSymbol {
                path: r.get(0)?,
                name: r.get(1)?,
                kind: r.get(2)?,
                start_line: r.get(3)?,
                end_line: r.get(4)?,
                signature: r.get(5)?,
                is_exported: r.get::<_, i64>(6)? != 0,
            })
        })?;
        let mut map: HashMap<String, Vec<IndexedSymbol>> = HashMap::new();
        for row in rows {
            let sym = row?;
            map.entry(sym.path.clone()).or_default().push(sym);
        }
        Ok(map)
    })
}

pub fn is_ignored_path(path: &str) -> bool {
    path.split('/').any(|seg| {
        matches!(
            seg,
            ".git" | "node_modules" | "target" | "dist" | "build" | ".next" | ".turbo" | ".gradle" | "vendor" | "Pods" | "__pycache__" | ".venv"
        )
    })
}

pub fn detect_language(path: &str) -> Option<&'static str> {
    let ext = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "rs" => Some("rust"),
        "ts" | "mts" | "cts" => Some("typescript"),
        "tsx" => Some("typescriptreact"),
        "js" | "mjs" | "cjs" => Some("javascript"),
        "jsx" => Some("javascriptreact"),
        "go" => Some("go"),
        "py" => Some("python"),
        "rb" => Some("ruby"),
        "swift" => Some("swift"),
        "kt" | "kts" => Some("kotlin"),
        _ => None,
    }
}

pub fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let name = Path::new(&lower).file_name().and_then(|n| n.to_str()).unwrap_or("");
    lower.contains("/tests/")
        || lower.starts_with("tests/")
        || lower.contains("/__tests__/")
        || lower.contains("/test/")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.ends_with("_test.rs")
        || name.ends_with("_tests.rs")
        || name.ends_with("_test.go")
        || name.ends_with("_test.py")
        || name.starts_with("test_")
        || name.ends_with("tests.swift")
        || name.ends_with("test.kt")
}

pub fn is_convention_doc(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "review.md"
            | "agents.md"
            | "claude.md"
            | "contributing.md"
            | "conventions.md"
            | ".github/pull_request_template.md"
            | "docs/review.md"
    ) || (lower.ends_with("/review.md") || lower.ends_with("/agents.md") || lower.ends_with("/claude.md"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_in(dir: &Path, args: &[&str]) {
        let mut cmd = crate::process::blocking_command("git");
        cmd.current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .args(args);
        let out = crate::process::output_blocking(&mut cmd, crate::process::QUICK).unwrap();
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn indexes_git_repo_incrementally_and_resolves_graph_queries() {
        let dir = std::env::temp_dir().join(format!("domovoi-indexer-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("web/src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n").unwrap();
        std::fs::write(dir.join("REVIEW.md"), "# Review Guidelines\nCheck error handling.\n").unwrap();
        std::fs::write(dir.join("AGENTS.md"), "# Agents\nRun cargo test.\n").unwrap();
        std::fs::write(dir.join("src/auth.rs"), "pub fn verify_token(tok: &str) -> bool {\n    !tok.is_empty()\n}\n").unwrap();
        std::fs::write(dir.join("src/main.rs"), "mod auth;\nuse crate::auth::verify_token;\npub fn main() { verify_token(\"x\"); }\n").unwrap();
        std::fs::write(dir.join("src/auth_tests.rs"), "use crate::auth::verify_token;\n#[test]\nfn works() { assert!(verify_token(\"a\")); }\n").unwrap();
        std::fs::write(dir.join("web/package.json"), r#"{"name":"web"}"#).unwrap();
        std::fs::write(dir.join("web/src/api.ts"), "export function fetchPr() { return 1; }\n").unwrap();
        std::fs::write(dir.join("web/src/app.ts"), "import { fetchPr } from './api';\nexport const run = () => fetchPr();\n").unwrap();
        std::fs::write(dir.join("web/src/App.swift"), "public struct ReviewCard {\n    let title: String\n}\n").unwrap();
        std::fs::write(dir.join("web/src/Screen.kt"), "fun renderReview() {\n}\n").unwrap();

        git_in(&dir, &["init", "-b", "main"]);
        git_in(&dir, &["add", "."]);
        git_in(&dir, &["commit", "-m", "init"]);

        let db = Db::open_in_memory().unwrap();
        let cancel = AtomicBool::new(false);
        let st = index_project(&db, "p1", &dir, None, false, &cancel).unwrap();
        assert_eq!(st.status, "ready");
        assert!(st.file_count >= 10);
        assert_eq!(st.package_count, 2);
        assert!(st.edge_count >= 2);
        assert!(st.symbol_count >= 5);
        assert_eq!(st.convention_paths, vec!["AGENTS.md", "REVIEW.md"]);

        let ctx = query::context_for_changed_files(&db, "p1", &["src/auth.rs".into()], 20);
        assert!(ctx.related_files.iter().any(|r| r.path == "src/main.rs"), "main.rs imports auth.rs: {:?}", ctx.related_files);
        assert!(ctx.related_files.iter().any(|r| r.path == "src/auth_tests.rs"), "auth_tests.rs tests auth.rs: {:?}", ctx.related_files);
        assert!(ctx.symbols.iter().any(|s| s.name == "verify_token"));
        assert_eq!(ctx.convention_paths, vec!["AGENTS.md", "REVIEW.md"]);

        // Second commit updates only one file; incremental re-index refreshes commit_sha and keeps symbols.
        std::fs::write(dir.join("src/auth.rs"), "pub fn verify_token(tok: &str) -> bool {\n    tok.len() > 2\n}\npub fn revoke() {}\n").unwrap();
        git_in(&dir, &["add", "src/auth.rs"]);
        git_in(&dir, &["commit", "-m", "update auth"]);
        let st2 = index_project(&db, "p1", &dir, None, false, &cancel).unwrap();
        assert_ne!(st2.commit_sha, st.commit_sha);
        let ctx2 = query::context_for_changed_files(&db, "p1", &["src/auth.rs".into()], 20);
        assert!(ctx2.symbols.iter().any(|s| s.name == "revoke"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
