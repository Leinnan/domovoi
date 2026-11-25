//! Workspace files for the composer's `@` mentions, the file tree, content
//! search and agent-written files (t3code `WorkspaceEntries`,
//! `WorkspaceSearchIndex`, `WorkspaceFileSystem`).
//!
//! t3code indexes with a native finder; here ripgrep's own crates do the same
//! jobs: `ignore` walks the tree honoring `.gitignore`, `grep-regex` and
//! `grep-searcher` search contents, and `nucleo-matcher` (Helix's fuzzy
//! matcher) ranks paths. Index entries are cached briefly per workspace.

use grep_matcher::Matcher;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, sinks::UTF8};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use domovoi_model::typed::{ProjectEntriesResult, ProjectEntry, ProjectEntryKind};

pub const MAX_ENTRIES: usize = 25_000;
const INDEX_TTL: Duration = Duration::from_secs(5);
const IGNORED_FILES_LIMIT: usize = 200;
const CONTENT_SEARCH_BUDGET: Duration = Duration::from_millis(250);
const CONTENT_MAX_MATCHES_PER_FILE: usize = 100;
const LINE_CONTENT_CAP: usize = 512;

/// `WORKSPACE_IMAGE_PREVIEW_EXTENSIONS` plus `.icns` (`isProjectFaviconPath`).
const IMAGE_EXTENSIONS: [&str; 14] =
    [".avif", ".bmp", ".gif", ".ico", ".heic", ".heif", ".jpeg", ".jpg", ".png", ".svg", ".tif", ".tiff", ".webp", ".icns"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub dir: bool,
}

impl Entry {
    /// The entry as a client reads it (`ProjectEntry`), not marked ignored.
    pub fn record(&self) -> ProjectEntry {
        let kind = if self.dir { ProjectEntryKind::Directory } else { ProjectEntryKind::File };
        ProjectEntry { path: self.path.clone(), kind, ignored: None }
    }
}

pub struct Index {
    pub entries: Vec<Entry>,
    /// The walk stopped at `MAX_ENTRIES`.
    pub truncated: bool,
}

/// Why a workspace's entries were not listed or searched (`ProjectEntriesFailure`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntriesFailure {
    WorkspaceRootNotFound,
    WorkspaceRootStatFailed,
    WorkspaceRootNotDirectory,
    DirectoryListFailed,
}

/// A workspace whose entries were not listed or searched: why, and in words.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct EntriesError {
    pub failure: EntriesFailure,
    pub message: String,
}

impl EntriesError {
    fn new(failure: EntriesFailure, message: impl Into<String>) -> Self {
        EntriesError { failure, message: message.into() }
    }
}

/// Why a workspace file was not read or written (`ProjectFileFailure`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileFailure {
    WorkspacePathOutsideRoot,
    ResolvedPathOutsideRoot,
    PathNotFile,
    BinaryFile,
    OperationFailed,
}

/// The step of a write that failed (`ProjectFileOperation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileOperation {
    RealpathWorkspaceRoot,
    RealpathTarget,
    MakeDirectory,
    WriteFile,
}

/// A workspace file that was not written: why, at which step, and in words.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct WriteError {
    pub failure: FileFailure,
    pub operation: Option<FileOperation>,
    pub message: String,
}

/// Why `filesystem.browse` listed nothing (`FilesystemBrowseFailure`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowseFailure {
    CurrentProjectRequired,
    ReadDirectoryFailed,
}

/// A failed call about a workspace's files or the folders of the machine,
/// written as its tagged error (t3code's `ProjectListEntriesError` and the
/// rest), with the fields that error carries.
#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "_tag")]
pub enum Failure {
    #[error("{message}")]
    #[serde(rename = "ProjectListEntriesError", rename_all = "camelCase")]
    ListEntries {
        cwd: String,
        failure: EntriesFailure,
        message: String,
        /// The folder asked for, `null` when the whole tree was. Left out by
        /// `projects.listIgnoredFiles`, which names none.
        #[serde(skip_serializing_if = "Option::is_none")]
        directory_path: Option<Option<String>>,
    },

    #[error("{message}")]
    #[serde(rename = "ProjectSearchEntriesError", rename_all = "camelCase")]
    SearchEntries {
        cwd: String,
        failure: EntriesFailure,
        message: String,
        /// In UTF-16 units, as the client counts.
        query_length: usize,
        limit: usize,
    },

    #[error("{message}")]
    #[serde(rename = "ProjectSearchContentsError", rename_all = "camelCase")]
    SearchContents {
        cwd: String,
        failure: EntriesFailure,
        message: String,
        /// In UTF-16 units, as the client counts.
        query_length: usize,
        limit: usize,
    },

    #[error("{message}")]
    #[serde(rename = "ProjectReadFileError", rename_all = "camelCase")]
    ReadFile { cwd: String, relative_path: String, failure: FileFailure, message: String },

    #[error("{message}")]
    #[serde(rename = "ProjectWriteFileError", rename_all = "camelCase")]
    WriteFile {
        cwd: String,
        relative_path: String,
        failure: FileFailure,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        operation: Option<FileOperation>,
    },

    #[error("{message}")]
    #[serde(rename = "FilesystemBrowseError", rename_all = "camelCase")]
    Browse {
        partial_path: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// Present, and `true`, when the folder does not exist; then there is no `failure`.
        #[serde(skip_serializing_if = "Option::is_none")]
        path_missing: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        failure: Option<BrowseFailure>,
    },
}

/// `normalizeWorkspaceRoot`: an existing absolute directory.
pub fn normalize_root(cwd: &str) -> Result<PathBuf, EntriesError> {
    let p = PathBuf::from(crate::util::expand_home(cwd));
    match std::fs::metadata(&p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(EntriesError::new(EntriesFailure::WorkspaceRootNotFound, format!("Workspace root does not exist: {}", p.display())))
        }
        Err(e) => Err(EntriesError::new(EntriesFailure::WorkspaceRootStatFailed, e.to_string())),
        Ok(m) if !m.is_dir() => Err(EntriesError::new(EntriesFailure::WorkspaceRootNotDirectory, format!("Workspace root is not a directory: {}", p.display()))),
        Ok(_) => Ok(p),
    }
}

fn cache() -> &'static Mutex<HashMap<PathBuf, (Instant, Arc<Index>)>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, (Instant, Arc<Index>)>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Drops the cached index (after a write created a file).
pub fn invalidate(root: &Path) {
    cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(root);
}

fn walker(root: &Path) -> ignore::Walk {
    ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .require_git(false)
        .parents(true)
        .filter_entry(|e| e.file_name() != ".git")
        .build()
}

fn relative(root: &Path, p: &Path) -> Option<String> {
    let rel = p.strip_prefix(root).ok()?.to_string_lossy().replace('\\', "/");
    (!rel.is_empty()).then_some(rel)
}

/// Every visible (not git-ignored) file and directory under `root`.
pub fn index(root: &Path) -> Arc<Index> {
    if let Some((at, idx)) = cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(root) {
        if at.elapsed() < INDEX_TTL {
            return idx.clone();
        }
    }
    let mut entries = vec![];
    let mut truncated = false;
    for e in walker(root).flatten() {
        let Some(rel) = relative(root, e.path()) else { continue };
        if entries.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
        entries.push(Entry { path: rel, dir: e.file_type().is_some_and(|t| t.is_dir()) });
    }
    let idx = Arc::new(Index { entries, truncated });
    cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(root.to_owned(), (Instant::now(), idx.clone()));
    idx
}

/// `localeCompare`-like ordering: case-insensitive first, then exact.
fn path_order(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b))
}

/// The whole tree (`WorkspaceSearchIndex.list`): entries plus their ancestor
/// directories, sorted by path.
pub fn list(root: &Path) -> ProjectEntriesResult {
    let idx = index(root);
    let mut by_path: BTreeMap<String, bool> = BTreeMap::new();
    for e in &idx.entries {
        by_path.insert(e.path.clone(), e.dir);
        let mut p = e.path.as_str();
        while let Some((parent, _)) = p.rsplit_once('/') {
            by_path.entry(parent.to_owned()).or_insert(true);
            p = parent;
        }
    }
    let mut all: Vec<Entry> = by_path.into_iter().map(|(path, dir)| Entry { path, dir }).collect();
    all.sort_by(|a, b| path_order(&a.path, &b.path));
    let truncated = idx.truncated || all.len() > MAX_ENTRIES;
    all.truncate(MAX_ENTRIES);
    ProjectEntriesResult { entries: all.iter().map(Entry::record).collect(), truncated }
}

/// `normalizeSearchQuery` with t3code's mention prefix trimming (`^[@./]+`).
fn normalize_query(q: &str) -> String {
    q.trim().trim_start_matches(['@', '.', '/']).to_lowercase()
}

pub fn is_image_path(p: &str) -> bool {
    let lower = p.to_lowercase();
    IMAGE_EXTENSIONS.iter().any(|e| lower.ends_with(e))
}

/// Fuzzy path search (`WorkspaceEntries.search`). An empty query lists the
/// index in walk order.
pub fn search(root: &Path, query: &str, limit: usize, kind: Option<&ProjectEntryKind>, image_only: bool) -> ProjectEntriesResult {
    let idx = index(root);
    let q = normalize_query(query);
    let eligible = |e: &&Entry| match (kind, image_only) {
        (_, true) => !e.dir && is_image_path(&e.path),
        (Some(ProjectEntryKind::File), _) => !e.dir,
        (Some(ProjectEntryKind::Directory), _) => e.dir,
        // A kind this build does not know filters nothing.
        _ => true,
    };
    let mut ranked: Vec<(&Entry, u32)> = if q.is_empty() {
        idx.entries.iter().filter(eligible).map(|e| (e, 0)).collect()
    } else {
        let mut matcher = nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT.match_paths());
        let pattern = Pattern::parse(&q, CaseMatching::Ignore, Normalization::Smart);
        let mut buf = vec![];
        let mut scored: Vec<(&Entry, u32)> = idx
            .entries
            .iter()
            .filter(eligible)
            .filter_map(|e| {
                let hay = nucleo_matcher::Utf32Str::new(&e.path, &mut buf);
                pattern.score(hay, &mut matcher).map(|s| (e, s))
            })
            .collect();
        scored.sort_by(|(a, sa), (b, sb)| sb.cmp(sa).then(a.path.len().cmp(&b.path.len())).then_with(|| a.path.cmp(&b.path)));
        scored
    };
    let mut entries: Vec<ProjectEntry> = vec![];
    // Files in nested worktrees are not indexed; a typed relative path still finds them.
    if kind != Some(&ProjectEntryKind::Directory) && !image_only {
        entries.extend(nested_worktree_files(root, query, &ranked).into_iter().map(|path| ProjectEntry { path, kind: ProjectEntryKind::File, ignored: None }));
    }
    let total = entries.len() + ranked.len();
    entries.extend(ranked.drain(..).take(limit).map(|(e, _)| e.record()));
    entries.truncate(limit);
    ProjectEntriesResult { entries, truncated: total > limit || idx.truncated }
}

/// `findNestedWorktreeFiles`: `<worktree>/<query>` for git worktrees inside the root.
fn nested_worktree_files(root: &Path, query: &str, indexed: &[(&Entry, u32)]) -> Vec<String> {
    let rel = query.trim().replace('\\', "/");
    if !rel.contains('/')
        || rel.ends_with('/')
        || rel.starts_with('/')
        || rel.split('/').any(|s| s == "..")
        || indexed.iter().any(|(e, _)| !e.dir && (e.path == rel || e.path.ends_with(&format!("/{rel}"))))
    {
        return vec![];
    }
    let Ok(out) = crate::process::output_blocking(crate::process::blocking_command("git").args(["worktree", "list", "--porcelain"]).current_dir(root), crate::process::QUICK) else {
        return vec![];
    };
    if !out.status.success() {
        return vec![];
    }
    let real = std::fs::canonicalize(root).unwrap_or(root.to_owned());
    String::from_utf8_lossy(&out.stdout)
        .split("\n\n")
        .filter(|rec| !rec.lines().any(|l| l.starts_with("prunable")))
        .filter_map(|rec| rec.lines().find_map(|l| l.strip_prefix("worktree ")).map(PathBuf::from))
        .filter_map(|wt| wt.strip_prefix(&real).ok().map(|r| r.to_string_lossy().replace('\\', "/")))
        .filter(|r| !r.is_empty())
        .map(|r| format!("{r}/{rel}"))
        .filter(|candidate| real.join(candidate).is_file())
        .collect()
}

// ---- content search ----

/// Where a line matches (`ProjectContentMatchRange`), in UTF-16 units.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MatchRange {
    pub start: usize,
    pub end: usize,
}

/// A line of a file that matches a content search (`ProjectContentMatch`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentMatch {
    pub path: String,
    pub line_number: u64,
    /// The line, to its first `LINE_CONTENT_CAP` characters.
    pub line_content: String,
    pub match_ranges: Vec<MatchRange>,
}

/// What `projects.searchContents` answers (`ProjectSearchContentsResult`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentSearchResult {
    pub matches: Vec<ContentMatch>,
    pub truncated: bool,
    /// Why the query, asked for as a pattern, was searched as text instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regex_fallback_error: Option<String>,
}

pub struct ContentQuery<'a> {
    pub query: &'a str,
    pub limit: usize,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub use_regex: bool,
}

/// Byte offset in `line` to a JS (UTF-16) string index.
fn utf16_index(line: &str, byte: usize) -> usize {
    line[..byte.min(line.len())].encode_utf16().count()
}

fn is_word_char(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_alphanumeric() || c == '_')
}

/// `isWholeWordRange`, on byte offsets.
fn is_whole_word(line: &str, start: usize, end: usize) -> bool {
    if end <= start {
        return false;
    }
    let before = line[..start].chars().next_back();
    let first = line[start..].chars().next();
    let last = line[..end].chars().next_back();
    let after = line[end..].chars().next();
    let left = start == 0 || !is_word_char(before) || !is_word_char(first);
    let right = end >= line.len() || !is_word_char(after) || !is_word_char(last);
    left && right
}

fn build_matcher(q: &ContentQuery, regex: bool) -> Result<grep_regex::RegexMatcher, String> {
    let mut b = RegexMatcherBuilder::new();
    b.case_insensitive(!q.case_sensitive).fixed_strings(!regex).line_terminator(Some(b'\n'));
    b.build(q.query).map_err(|e| e.to_string())
}

/// Content search (`WorkspaceSearchIndex.searchContents`): line matches with
/// UTF-16 match ranges, within a 250 ms budget like t3code's.
pub fn search_contents(root: &Path, q: &ContentQuery) -> ContentSearchResult {
    let mut regex_fallback_error = None;
    let matcher = if q.use_regex {
        match build_matcher(q, true) {
            Ok(m) => m,
            Err(e) => {
                regex_fallback_error = Some(e);
                build_matcher(q, false).expect("literal pattern")
            }
        }
    } else {
        build_matcher(q, false).expect("literal pattern")
    };
    let deadline = Instant::now() + CONTENT_SEARCH_BUDGET;
    let mut searcher: Searcher = SearcherBuilder::new().binary_detection(BinaryDetection::quit(b'\0')).line_number(true).build();
    let mut matches: Vec<ContentMatch> = vec![];
    let mut stopped_early = false;
    let mut files: Vec<String> = index(root).entries.iter().filter(|e| !e.dir).map(|e| e.path.clone()).collect();
    files.sort_by(|a, b| path_order(a, b));
    for (i, rel) in files.iter().enumerate() {
        if matches.len() > q.limit || Instant::now() > deadline {
            stopped_early = i < files.len();
            break;
        }
        let mut per_file = 0usize;
        let _ = searcher.search_path(
            &matcher,
            root.join(rel),
            UTF8(|line_number, line| {
                let line = line.trim_end_matches(['\n', '\r']);
                let mut ranges = vec![];
                let _ = matcher.find_iter(line.as_bytes(), |m| {
                    if !q.whole_word || is_whole_word(line, m.start(), m.end()) {
                        ranges.push((m.start(), m.end()));
                    }
                    true
                });
                if ranges.is_empty() {
                    return Ok(true);
                }
                let shown: String = line.chars().take(LINE_CONTENT_CAP).collect();
                let match_ranges: Vec<MatchRange> = ranges
                    .into_iter()
                    .filter(|(s, _)| *s <= shown.len())
                    .map(|(s, e)| MatchRange { start: utf16_index(line, s), end: utf16_index(line, e.min(shown.len())) })
                    .collect();
                matches.push(ContentMatch { path: rel.clone(), line_number, line_content: shown, match_ranges });
                per_file += 1;
                Ok(per_file < CONTENT_MAX_MATCHES_PER_FILE && matches.len() <= q.limit)
            }),
        );
    }
    let truncated = matches.len() > q.limit || stopped_early;
    matches.truncate(q.limit);
    ContentSearchResult { matches, truncated, regex_fallback_error }
}

// ---- directory listing ----

fn git_stdin(cwd: &Path, args: &[&str], stdin: &[u8]) -> Option<(i32, Vec<u8>)> {
    use std::io::Write;
    let mut child = crate::process::blocking_command("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut input = child.stdin.take()?;
    let data = stdin.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = input.write_all(&data);
    });
    let out = child.wait_with_output().ok()?;
    let _ = writer.join();
    Some((out.status.code().unwrap_or(-1), out.stdout))
}

/// One directory's children (`WorkspaceEntries.list` with `directoryPath`),
/// git-ignored ones marked. `.git` is never listed or entered.
pub fn list_directory(root: &Path, directory: &str) -> Result<ProjectEntriesResult, EntriesError> {
    let fail = |m: String| EntriesError::new(EntriesFailure::DirectoryListFailed, m);
    let real_root = std::fs::canonicalize(root).map_err(|e| fail(e.to_string()))?;
    let dir_rel = directory.trim_matches('/');
    if dir_rel.split('/').any(|s| s == ".." || s == ".git") {
        return Err(fail("Directory must be inside the workspace and outside .git.".into()));
    }
    let dir = std::fs::canonicalize(root.join(dir_rel)).map_err(|e| fail(e.to_string()))?;
    if !dir.starts_with(&real_root) {
        return Err(fail("Directory must be inside the workspace and outside .git.".into()));
    }
    let mut entries = vec![];
    for child in std::fs::read_dir(&dir).map_err(|e| fail(e.to_string()))?.flatten() {
        let name = child.file_name().to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
        let ft = child.file_type().map_err(|e| fail(e.to_string()))?;
        let is_dir = if ft.is_symlink() {
            // Symlinks count only when they resolve inside the workspace and do not loop back.
            match std::fs::canonicalize(child.path()) {
                Ok(target) if target.starts_with(&real_root) && !target.components().any(|c| c.as_os_str() == ".git") => {
                    if target.is_file() {
                        false
                    } else if target.is_dir() && !dir.starts_with(&target) {
                        true
                    } else {
                        continue;
                    }
                }
                _ => continue,
            }
        } else if ft.is_dir() {
            true
        } else if ft.is_file() {
            false
        } else {
            continue;
        };
        let path = if dir_rel.is_empty() { name } else { format!("{dir_rel}/{name}") };
        entries.push(Entry { path, dir: is_dir });
    }
    entries.sort_by(|a, b| path_order(&a.path, &b.path));
    let mut ignored = std::collections::HashSet::new();
    for chunk in entries.chunks(1000) {
        let stdin: Vec<u8> = chunk.iter().flat_map(|e| e.path.bytes().chain([0])).collect();
        match git_stdin(root, &["-c", "core.fsmonitor=false", "check-ignore", "-z", "--stdin"], &stdin) {
            Some((0 | 1, out)) => ignored.extend(out.split(|b| *b == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned())),
            _ => break,
        }
    }
    let entries = entries.iter().map(|e| ProjectEntry { ignored: ignored.contains(&e.path).then_some(true), ..e.record() }).collect();
    Ok(ProjectEntriesResult { entries, truncated: false })
}

/// A file git ignores (`ProjectIgnoredFile`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IgnoredFile {
    pub path: String,
    pub byte_length: u64,
}

/// What `projects.listIgnoredFiles` answers (`ProjectListIgnoredFilesResult`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IgnoredFiles {
    pub files: Vec<IgnoredFile>,
    /// Whether there are more than the `IGNORED_FILES_LIMIT` listed.
    pub truncated: bool,
}

/// `listIgnoredFiles`: git-ignored files (not directories), with sizes.
pub fn list_ignored_files(root: &Path) -> IgnoredFiles {
    let out = crate::process::output_blocking(
        crate::process::blocking_command("git")
            .args(["-c", "core.fsmonitor=false", "ls-files", "--others", "--ignored", "--exclude-standard", "--directory", "--no-empty-directory", "-z"])
            .current_dir(root),
        crate::process::QUICK,
    );
    let Some(out) = out.ok().filter(|o| o.status.success()) else {
        return IgnoredFiles { files: vec![], truncated: false };
    };
    let mut paths: Vec<String> = out
        .stdout
        .split(|b| *b == 0)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .filter(|p| !p.is_empty() && !p.ends_with('/'))
        .collect();
    paths.sort();
    let files = paths
        .iter()
        .take(IGNORED_FILES_LIMIT)
        .filter_map(|p| {
            let m = std::fs::symlink_metadata(root.join(p)).ok().filter(|m| m.is_file())?;
            Some(IgnoredFile { path: p.clone(), byte_length: m.len() })
        })
        .collect();
    IgnoredFiles { files, truncated: paths.len() > IGNORED_FILES_LIMIT }
}

// ---- writes and metadata ----

/// Whether `p` (a normalized absolute path) is inside git metadata.
fn in_git_folder(p: &Path) -> bool {
    if p.components().any(|c| c.as_os_str() == ".git") {
        return true;
    }
    let mut folder = p.parent();
    while let Some(f) = folder {
        let has = |n: &str| f.join(n).symlink_metadata().is_ok();
        if has("HEAD") && (has("commondir") || (has("objects") && has("refs"))) {
            return true;
        }
        if f.join(".git").is_file() {
            return false;
        }
        folder = f.parent();
    }
    false
}

/// `WorkspaceFileSystem.writeFile`: a workspace-relative path that stays
/// inside the root (symlinks included) and outside git metadata.
pub fn write_file(cwd: &str, relative_path: &str, contents: &str) -> Result<String, WriteError> {
    let outside = |m: &str| WriteError { failure: FileFailure::WorkspacePathOutsideRoot, operation: None, message: m.into() };
    let op = |operation: FileOperation, e: std::io::Error| WriteError { failure: FileFailure::OperationFailed, operation: Some(operation), message: e.to_string() };
    let rel = relative_path.replace('\\', "/");
    if rel.is_empty() || rel.starts_with('/') || rel.split('/').any(|s| s == "..") {
        return Err(outside("Path must be relative to the workspace."));
    }
    let root = std::fs::canonicalize(cwd).map_err(|e| op(FileOperation::RealpathWorkspaceRoot, e))?;
    let target = root.join(&rel);
    // Resolve the deepest existing ancestor to catch symlinks pointing out of the root.
    let mut existing = target.clone();
    while !existing.exists() {
        match existing.parent() {
            Some(p) => existing = p.to_owned(),
            None => break,
        }
    }
    let real_existing = std::fs::canonicalize(&existing).map_err(|e| op(FileOperation::RealpathTarget, e))?;
    // Joining an empty rest would end the path of a file that exists with a separator.
    let rest = target.strip_prefix(&existing).unwrap_or(Path::new(""));
    let real_target = if rest.as_os_str().is_empty() { real_existing } else { real_existing.join(rest) };
    if !real_target.starts_with(&root) {
        return Err(WriteError { failure: FileFailure::ResolvedPathOutsideRoot, operation: None, message: "Path resolves outside the workspace.".into() });
    }
    if in_git_folder(&real_target) {
        return Err(outside("Git metadata cannot be written."));
    }
    if real_target.is_dir() {
        return Err(WriteError { failure: FileFailure::PathNotFile, operation: None, message: "Path is a directory.".into() });
    }
    let replaces = real_target.exists();
    if let Some(parent) = real_target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| op(FileOperation::MakeDirectory, e))?;
    }
    {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        // Never write through a symlink planted at the target.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc_nofollow());
        }
        #[cfg(windows)]
        if crate::util::is_link(&real_target) {
            return Err(op(FileOperation::WriteFile, std::io::Error::other("the target is a link")));
        }
        let mut f = options.open(&real_target).map_err(|e| op(FileOperation::WriteFile, e))?;
        f.write_all(contents.as_bytes()).map_err(|e| op(FileOperation::WriteFile, e))?;
    }
    if !replaces {
        invalidate(Path::new(cwd));
        invalidate(&root);
    }
    Ok(rel)
}

#[cfg(unix)]
fn libc_nofollow() -> i32 {
    #[cfg(target_os = "macos")]
    {
        0x0100
    }
    #[cfg(target_os = "linux")]
    {
        0o400000
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        0
    }
}

/// `fileHeaderMimeType`: sniffs common formats from a file's first bytes.
pub fn sniff_mime(head: &[u8]) -> Option<&'static str> {
    let starts = |sig: &[u8]| head.starts_with(sig);
    Some(if starts(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if starts(b"\xff\xd8\xff") {
        "image/jpeg"
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        "image/gif"
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        "image/webp"
    } else if starts(b"%PDF-") {
        "application/pdf"
    } else if starts(b"PK\x03\x04") {
        "application/zip"
    } else {
        return None;
    })
}

/// What a path on the machine is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    File,
    Directory,
    Other,
}

/// What `filesystem.getMetadata` says of one path (`FilesystemEntryMetadata`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryMetadata {
    pub kind: PathKind,
    /// A file's size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byte_length: Option<u64>,
    /// What a file without an extension holds, when its first bytes tell.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<&'static str>,
}

/// `filesystem.getMetadata` for one path: only absolute paths (`/HOME` in one
/// stands for the home folder; a leading `~/` is not expanded, so such a path
/// counts as relative); the server's own state directory answers like a
/// missing path.
pub fn metadata(requested: &str, server_home: &Path) -> Option<EntryMetadata> {
    let expanded = PathBuf::from(crate::util::expand_home(requested));
    // A network share is never asked; a local path written `\\?\C:\…` is.
    let verbatim_local = requested.strip_prefix(r"\\?\").is_some_and(|rest| rest.as_bytes().get(1) == Some(&b':'));
    if !expanded.is_absolute() || requested.starts_with("//") || requested.starts_with(r"\\") && !verbatim_local {
        return None;
    }
    let m = std::fs::metadata(&expanded).ok()?;
    if std::fs::canonicalize(&expanded).is_ok_and(|p| std::fs::canonicalize(server_home).is_ok_and(|h| p.starts_with(h))) {
        return None;
    }
    if m.is_dir() {
        return Some(EntryMetadata { kind: PathKind::Directory, byte_length: None, mime_type: None });
    }
    if !m.is_file() {
        return Some(EntryMetadata { kind: PathKind::Other, byte_length: None, mime_type: None });
    }
    let mut mime_type = None;
    if expanded.extension().is_none() && m.len() > 0 {
        use std::io::Read;
        let mut head = [0u8; 512];
        if let Ok(n) = std::fs::File::open(&expanded).and_then(|mut f| f.read(&mut head)) {
            mime_type = sniff_mime(&head[..n]);
        }
    }
    Some(EntryMetadata { kind: PathKind::File, byte_length: Some(m.len()), mime_type })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn repo() -> PathBuf {
        let d = std::env::temp_dir().join(format!("domovoi-ws-{}-{}", std::process::id(), crate::util::new_uuid()));
        std::fs::create_dir_all(d.join("src/nested")).unwrap();
        std::fs::write(d.join(".gitignore"), "build/\n*.log\n").unwrap();
        std::fs::write(d.join("src/main.rs"), "fn main() {\n    let add = 1; // addition\n}\n").unwrap();
        std::fs::write(d.join("src/nested/lib.rs"), "pub fn add_two() {}\nconst ÉTÉ: &str = \"été add\";\n").unwrap();
        std::fs::write(d.join("README.md"), "Add things\n").unwrap();
        std::fs::write(d.join("logo.PNG"), b"\x89PNG\r\n\x1a\nxxxx").unwrap();
        std::fs::create_dir_all(d.join("build")).unwrap();
        std::fs::write(d.join("build/out.txt"), "add").unwrap();
        std::fs::write(d.join("debug.log"), "add add").unwrap();
        std::process::Command::new("git").args(["init", "-q"]).current_dir(&d).status().unwrap();
        d.canonicalize().unwrap()
    }

    #[test]
    fn index_respects_gitignore_and_lists_ancestors() {
        let d = repo();
        let paths: Vec<String> = index(&d).entries.iter().map(|e| e.path.clone()).collect();
        assert!(paths.contains(&"src/nested/lib.rs".into()) && paths.contains(&".gitignore".into()));
        assert!(!paths.iter().any(|p| p.starts_with("build") || p.ends_with(".log") || p.starts_with(".git/")));
        let l = json!(list(&d));
        let listed: Vec<&str> = l["entries"].as_array().unwrap().iter().map(|e| e["path"].as_str().unwrap()).collect();
        assert!(listed.windows(2).all(|w| path_order(w[0], w[1]).is_le()), "sorted: {listed:?}");
        assert_eq!(l["entries"].as_array().unwrap().iter().find(|e| e["path"] == "src").unwrap()["kind"], "directory");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn fuzzy_search_ranks_and_filters() {
        let d = repo();
        let r = json!(search(&d, "@./lib", 10, None, false));
        assert_eq!(r["entries"][0]["path"], "src/nested/lib.rs", "{r}");
        let r = json!(search(&d, "src", 10, Some(&ProjectEntryKind::Directory), false));
        assert!(r["entries"].as_array().unwrap().iter().all(|e| e["kind"] == "directory"));
        let r = json!(search(&d, "", 10, None, true));
        assert_eq!(r["entries"], json!([{ "path": "logo.PNG", "kind": "file" }]));
        let r = json!(search(&d, "", 2, None, false));
        assert_eq!((r["entries"].as_array().unwrap().len(), r["truncated"].as_bool()), (2, Some(true)));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn content_search_ranges_are_utf16_and_whole_word_filters() {
        let d = repo();
        fn q(query: &str, whole_word: bool, use_regex: bool, case_sensitive: bool) -> ContentQuery<'_> {
            ContentQuery { query, limit: 50, case_sensitive, whole_word, use_regex }
        }
        let r = json!(search_contents(&d, &q("add", false, false, false)));
        let paths: Vec<&str> = r["matches"].as_array().unwrap().iter().map(|m| m["path"].as_str().unwrap()).collect();
        assert!(paths.contains(&"README.md") && paths.contains(&"src/main.rs"), "{r}");
        assert!(!paths.iter().any(|p| p.ends_with(".log") || p.starts_with("build")), "ignored files are not searched");
        let lib = r["matches"].as_array().unwrap().iter().find(|m| m["path"] == "src/nested/lib.rs" && m["lineNumber"] == 2).unwrap();
        let line = lib["lineContent"].as_str().unwrap();
        let start = line.find("add").unwrap();
        assert_eq!(lib["matchRanges"][0]["start"], json!(line[..start].encode_utf16().count()));
        let r = json!(search_contents(&d, &q("add", true, false, false)));
        assert!(r["matches"].as_array().unwrap().iter().all(|m| m["path"] != "src/nested/lib.rs" || m["lineNumber"] == 2), "add_two is not a whole word: {r}");
        let r = json!(search_contents(&d, &q("Add", false, false, true)));
        assert_eq!(r["matches"].as_array().unwrap().len(), 1);
        let r = json!(search_contents(&d, &q("add(", false, true, false)));
        assert!(r["regexFallbackError"].is_string() && r["matches"].as_array().unwrap().is_empty());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn directory_listing_marks_ignored_entries() {
        let d = repo();
        let r = json!(list_directory(&d, "").unwrap());
        let get = |p: &str| r["entries"].as_array().unwrap().iter().find(|e| e["path"] == p).cloned();
        assert_eq!(get("build").unwrap()["ignored"], true);
        assert_eq!(get("debug.log").unwrap()["ignored"], true);
        assert!(get("src").unwrap().get("ignored").is_none());
        assert!(get(".git").is_none());
        assert!(list_directory(&d, "../").is_err() && list_directory(&d, ".git").is_err());
        let ignored = json!(list_ignored_files(&d));
        assert_eq!(ignored["files"], json!([{ "path": "debug.log", "byteLength": 7 }]), "directories are left out");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn writes_stay_inside_the_workspace() {
        let d = repo();
        let root = d.to_str().unwrap();
        assert_eq!(write_file(root, "docs/new.md", "hi").ok(), Some("docs/new.md".into()));
        assert_eq!(std::fs::read_to_string(d.join("docs/new.md")).unwrap(), "hi");
        assert_eq!(write_file(root, "docs/new.md", "again").ok(), Some("docs/new.md".into()), "a file that exists is replaced");
        assert_eq!(std::fs::read_to_string(d.join("docs/new.md")).unwrap(), "again");
        assert_eq!(write_file(root, "../escape.txt", "x").err().map(|e| e.failure), Some(FileFailure::WorkspacePathOutsideRoot));
        assert_eq!(write_file(root, ".git/config", "x").err().map(|e| e.failure), Some(FileFailure::WorkspacePathOutsideRoot));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(std::env::temp_dir(), d.join("out")).unwrap();
            assert_eq!(write_file(root, "out/x.txt", "x").err().map(|e| e.failure), Some(FileFailure::ResolvedPathOutsideRoot));
        }
        assert_eq!(write_file(root, "src", "x").err().map(|e| e.failure), Some(FileFailure::PathNotFile));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn metadata_answers_only_absolute_paths() {
        let d = repo();
        let home = d.join("domovoi-home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(d.join("blob"), b"\x89PNG\r\n\x1a\n....").unwrap();
        assert_eq!(json!(metadata("relative.txt", &home)), Value::Null);
        assert_eq!(json!(metadata(d.join("src").to_str().unwrap(), &home)), json!({ "kind": "directory" }));
        assert_eq!(json!(metadata(d.join("blob").to_str().unwrap(), &home)), json!({ "kind": "file", "byteLength": 12, "mimeType": "image/png" }));
        assert_eq!(json!(metadata(d.join("README.md").to_str().unwrap(), &home)), json!({ "kind": "file", "byteLength": 11 }));
        assert_eq!(json!(metadata(home.to_str().unwrap(), &home)), Value::Null, "server state is hidden");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn failures_are_written_as_the_tagged_errors_of_t3code() {
        let entries = [
            (EntriesFailure::WorkspaceRootNotFound, "workspace_root_not_found"),
            (EntriesFailure::WorkspaceRootStatFailed, "workspace_root_stat_failed"),
            (EntriesFailure::WorkspaceRootNotDirectory, "workspace_root_not_directory"),
            (EntriesFailure::DirectoryListFailed, "directory_list_failed"),
        ];
        for (failure, spelled) in entries {
            assert_eq!(
                json!(Failure::ListEntries { cwd: "/w".into(), failure, message: "no".into(), directory_path: Some(None) }),
                json!({ "_tag": "ProjectListEntriesError", "cwd": "/w", "failure": spelled, "message": "no", "directoryPath": null })
            );
        }
        assert_eq!(
            json!(Failure::ListEntries { cwd: "/w".into(), failure: EntriesFailure::DirectoryListFailed, message: "no".into(), directory_path: Some(Some("src".into())) }),
            json!({ "_tag": "ProjectListEntriesError", "cwd": "/w", "failure": "directory_list_failed", "message": "no", "directoryPath": "src" })
        );
        assert_eq!(
            json!(Failure::ListEntries { cwd: "/w".into(), failure: EntriesFailure::WorkspaceRootNotFound, message: "no".into(), directory_path: None }),
            json!({ "_tag": "ProjectListEntriesError", "cwd": "/w", "failure": "workspace_root_not_found", "message": "no" })
        );
        assert_eq!(
            json!(Failure::SearchEntries { cwd: "/w".into(), failure: EntriesFailure::WorkspaceRootNotFound, message: "no".into(), query_length: 3, limit: 5 }),
            json!({ "_tag": "ProjectSearchEntriesError", "cwd": "/w", "failure": "workspace_root_not_found", "message": "no", "queryLength": 3, "limit": 5 })
        );
        assert_eq!(
            json!(Failure::SearchContents { cwd: "/w".into(), failure: EntriesFailure::WorkspaceRootStatFailed, message: "no".into(), query_length: 0, limit: 1 }),
            json!({ "_tag": "ProjectSearchContentsError", "cwd": "/w", "failure": "workspace_root_stat_failed", "message": "no", "queryLength": 0, "limit": 1 })
        );
        let files = [
            (FileFailure::WorkspacePathOutsideRoot, "workspace_path_outside_root"),
            (FileFailure::ResolvedPathOutsideRoot, "resolved_path_outside_root"),
            (FileFailure::PathNotFile, "path_not_file"),
            (FileFailure::BinaryFile, "binary_file"),
            (FileFailure::OperationFailed, "operation_failed"),
        ];
        for (failure, spelled) in files {
            assert_eq!(
                json!(Failure::ReadFile { cwd: "/w".into(), relative_path: "a".into(), failure, message: "no".into() }),
                json!({ "_tag": "ProjectReadFileError", "cwd": "/w", "relativePath": "a", "failure": spelled, "message": "no" })
            );
            assert_eq!(
                json!(Failure::WriteFile { cwd: "/w".into(), relative_path: "a".into(), failure, message: "no".into(), operation: None }),
                json!({ "_tag": "ProjectWriteFileError", "cwd": "/w", "relativePath": "a", "failure": spelled, "message": "no" })
            );
        }
        let operations = [
            (FileOperation::RealpathWorkspaceRoot, "realpath-workspace-root"),
            (FileOperation::RealpathTarget, "realpath-target"),
            (FileOperation::MakeDirectory, "make-directory"),
            (FileOperation::WriteFile, "write-file"),
        ];
        for (operation, spelled) in operations {
            assert_eq!(
                json!(Failure::WriteFile { cwd: "/w".into(), relative_path: "a".into(), failure: FileFailure::OperationFailed, message: "no".into(), operation: Some(operation) }),
                json!({ "_tag": "ProjectWriteFileError", "cwd": "/w", "relativePath": "a", "failure": "operation_failed", "message": "no", "operation": spelled })
            );
        }
        assert_eq!(
            json!(Failure::Browse { partial_path: "sr".into(), message: "Relative paths need a current project.".into(), cwd: None, path_missing: None, failure: Some(BrowseFailure::CurrentProjectRequired) }),
            json!({ "_tag": "FilesystemBrowseError", "partialPath": "sr", "failure": "current_project_required", "message": "Relative paths need a current project." })
        );
        assert_eq!(
            json!(Failure::Browse { partial_path: "/x/".into(), message: "Directory does not exist.".into(), cwd: Some("/w".into()), path_missing: Some(true), failure: None }),
            json!({ "_tag": "FilesystemBrowseError", "partialPath": "/x/", "message": "Directory does not exist.", "cwd": "/w", "pathMissing": true })
        );
        assert_eq!(
            json!(Failure::Browse { partial_path: "/x/".into(), message: "denied".into(), cwd: None, path_missing: None, failure: Some(BrowseFailure::ReadDirectoryFailed) }),
            json!({ "_tag": "FilesystemBrowseError", "partialPath": "/x/", "message": "denied", "failure": "read_directory_failed" })
        );
    }
}
