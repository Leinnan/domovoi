//! Local agent sessions: scan Claude (`<config>/projects/*/*.jsonl`), Codex
//! (`<home>/sessions/YYYY/MM/DD/rollout-*.jsonl`) and Pi
//! (`<sessions>/--<cwd>--/*.jsonl`, trees of entries) transcripts for projects,
//! and import a project's recent sessions as resumable threads. After t3code's
//! `AgentSessionScanner` and `AgentSessionImporter`. An imported Pi thread
//! resumes its session file (its native id), keeping only the active branch.
//!
//! Reads are read-only and bounded. Differences from t3code:
//! - a rewound Codex rollout is imported on its own, without the history it
//!   inherits from earlier rollouts (t3code's fallback when those are missing);
//! - repository keys use the generic `host/path` spelling, without per-host rewrites;
//! - git files are read directly (after checking they are regular files)
//!   instead of through a `cat` with a deadline.

#![expect(clippy::disallowed_types, reason = "other tools' files: Claude and Codex transcript lines and Codex's session index, read inside the parsers")]

use crate::adapters::turn::{Shared, spelled, turn_item};
use crate::orchestration::events::Origin;
use crate::store::Store;
use rusqlite::{OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use crate::time_fmt::iso_from_millis;
use domovoi_model::typed::{
    Actor, AgentSessionImportResult, AgentSessionProjectCandidate, AgentSessionProjectGit, AgentSessionScanResult, AgentSessionSource, AppThread,
    ConversationMessage, CreationSource, EventBody, HistoryOrigin, InputIntent, InteractionMode, MessageId, MessageRole, ModelSelection, NativeRefStrength,
    ProviderRef, ProviderThread, ProviderThreadStatus, Run, RuntimeMode, SettledOverride, ThreadChange, ThreadLineage, Timestamp, TurnItemStatus,
};

/// A pattern compiled once, on first use (not on every call).
macro_rules! regex {
    ($re:literal) => {{
        static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new($re).expect("a valid pattern"));
        &*RE
    }};
}

const MAX_TRANSCRIPT_SCAN_BYTES: u64 = 1024 * 1024;
const MAX_CODEX_SESSION_INDEX_BYTES: u64 = 1024 * 1024;
const MAX_CODEX_PROJECT_ROOT_ROWS: i64 = 100_000;
const MAX_TRANSCRIPTS_PER_SOURCE: usize = 5000;
const MAX_DISCOVERY_OPERATIONS_PER_SOURCE: usize = MAX_TRANSCRIPTS_PER_SOURCE * 4;
const MAX_METADATA_BYTES_PER_SOURCE: u64 = 64 * 1024 * 1024;
const MAX_METADATA_RECORDS_PER_SOURCE: usize = 100_000;
const MAX_METADATA_RECORDS_PER_TRANSCRIPT: usize = 1_000;
const RECENT_THREAD_WINDOW_MS: i64 = 30 * 24 * 60 * 60 * 1000;
const MAX_IMPORTED_TRANSCRIPT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_IMPORTED_MESSAGES: usize = 200;
const MAX_IMPORT_HISTORY_BYTES: usize = 32 * 1024 * 1024;
const MAX_IMPORT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_IMPORT_TRANSCRIPTS: usize = 100;
const MAX_IMPORT_RECORDS: usize = 100_000;
const MAX_GIT_FILE_BYTES: u64 = 1024 * 1024;
const IMPORT_EVENT_PREFIX: &str = "agent-session-import:v2";
const CODEX_STATE_DATABASE: &str = "state_5.sqlite";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    Claude,
    Codex,
    Pi,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Claude => "claudeAgent",
            Source::Codex => "codex",
            Source::Pi => "pi",
        }
    }

    /// The source as a scan names it to the client.
    fn named(self) -> AgentSessionSource {
        match self {
            Source::Claude => AgentSessionSource::ClaudeAgent,
            Source::Codex => AgentSessionSource::Codex,
            Source::Pi => AgentSessionSource::Pi,
        }
    }
}

/// What a scan or an import could not read (`AgentSessionScanError`'s
/// `operation`). t3code's schema also has `read-settings`, a step Domovoi cannot
/// fail in: settings that cannot be read are no settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScanOperation {
    /// The projects, or the imports recorded for them.
    ReadProjects,
}

impl ScanOperation {
    fn as_str(self) -> &'static str {
        match self {
            ScanOperation::ReadProjects => "read-projects",
        }
    }
}

/// Why `agentSessions.scan` or `agentSessions.import` failed, as the client is
/// told: each a tagged error of t3code's `agentSessions.ts`, with its message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "_tag")]
pub enum Failure {
    /// The store or the record of imports could not be read; `cause` is what it said.
    #[error("Failed to scan agent sessions during {}.", operation.as_str())]
    #[serde(rename = "AgentSessionScanError")]
    Scan { operation: ScanOperation, cause: String },

    #[error("Project '{project_id}' does not exist.")]
    #[serde(rename = "AgentSessionImportProjectNotFoundError", rename_all = "camelCase")]
    ProjectNotFound { project_id: String },

    #[error("Project '{project_id}' changed directories. Scan for projects again before importing history.")]
    #[serde(rename = "AgentSessionImportProjectChangedError", rename_all = "camelCase")]
    ProjectChanged { project_id: String },
}

impl Failure {
    /// The projects or the imports recorded for them could not be read.
    pub fn read_projects(cause: impl std::fmt::Display) -> Self {
        Failure::Scan { operation: ScanOperation::ReadProjects, cause: cause.to_string() }
    }

    /// The failure as the client gets it, for a log or a report.
    pub fn written(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| self.to_string())
    }
}

/// Reads a setting as the scan did before it had a record for them: a value
/// of another type than the setting's counts as not set.
fn lenient<'de, T: serde::de::DeserializeOwned, D: serde::Deserializer<'de>>(set: D) -> Result<Option<T>, D::Error> {
    Ok(serde_json::from_value(Value::deserialize(set)?).ok())
}

/// What a scan reads of the server's settings.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScanSettings {
    #[serde(default, deserialize_with = "lenient")]
    provider_instances: Option<ScanInstances>,
    #[serde(default, deserialize_with = "lenient")]
    worktrees_directory: Option<String>,
}

/// The provider instances whose sessions are scanned.
#[derive(Debug, Default, Deserialize)]
struct ScanInstances {
    #[serde(default, rename = "claudeAgent", deserialize_with = "lenient")]
    claude_agent: Option<ScanInstance>,
    #[serde(default, deserialize_with = "lenient")]
    codex: Option<ScanInstance>,
    #[serde(default, deserialize_with = "lenient")]
    pi: Option<ScanInstance>,
}

#[derive(Debug, Default, Deserialize)]
struct ScanInstance {
    #[serde(default, deserialize_with = "lenient")]
    enabled: Option<bool>,
    #[serde(default, deserialize_with = "lenient")]
    config: Option<ScanInstanceConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScanInstanceConfig {
    #[serde(default, deserialize_with = "lenient")]
    enabled: Option<bool>,
    #[serde(default, deserialize_with = "lenient")]
    home_path: Option<String>,
}

/// Where to look, and what never counts as a project.
#[derive(Debug, Clone)]
pub struct ScanEnv {
    pub home: PathBuf,
    /// (source, provider instance id, config / home directory)
    pub homes: Vec<(Source, String, PathBuf)>,
    /// The server's own data directory.
    pub base_dir: PathBuf,
    pub worktrees_dir: PathBuf,
    pub temp_dir: PathBuf,
}

impl ScanEnv {
    /// From settings and the environment. `DOMOVOI_AGENT_SESSIONS_HOME` replaces
    /// the home directory (and the `CLAUDE_CONFIG_DIR` / `CODEX_HOME`
    /// overrides), so tests never read a real one.
    pub fn new(settings: &crate::settings::Settings, base_dir: &Path) -> ScanEnv {
        // Its own view of the settings: a provider instance is read whether or not it names a driver.
        let settings: ScanSettings = settings.view().unwrap_or_default();
        let instances = settings.provider_instances.unwrap_or_default();
        let isolated = std::env::var_os("DOMOVOI_AGENT_SESSIONS_HOME").filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = isolated.clone().unwrap_or_else(crate::util::home_dir);
        let env_home = |name: &str| if isolated.is_some() { None } else { std::env::var(name).ok().filter(|v| !v.trim().is_empty()) };
        let mut homes = vec![];
        let pi_instance = instances.pi;
        for (source, instance, env_name, default_dir) in
            [(Source::Claude, instances.claude_agent, "CLAUDE_CONFIG_DIR", ".claude"), (Source::Codex, instances.codex, "CODEX_HOME", ".codex")]
        {
            let id = source.as_str();
            let instance = instance.unwrap_or_default();
            let config = instance.config.unwrap_or_default();
            let enabled = instance.enabled.or(config.enabled).unwrap_or(true);
            if !enabled {
                continue;
            }
            let configured = config.home_path.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
            let dir = configured
                .or_else(|| env_home(env_name))
                .map(|p| resolve(&expand_home(p.trim(), &home)))
                .unwrap_or_else(|| home.join(default_dir));
            homes.push((source, id.to_owned(), dir));
        }
        // Pi's sessions folder (t3code `resolvePiSessionsDir`): `--session-dir`
        // in Domovoi's pi arguments, else PI_CODING_AGENT_SESSION_DIR, else
        // `sessions` in PI_CODING_AGENT_DIR (default `~/.pi/agent`).
        let pi_instance = pi_instance.unwrap_or_default();
        let pi_config = pi_instance.config.unwrap_or_default();
        if pi_instance.enabled.or(pi_config.enabled).unwrap_or(true) {
            let args = crate::pi_rpc::tokenize(&std::env::var(crate::pi_rpc::PI_ARGS_ENV).unwrap_or_default());
            let from_args = (isolated.is_none()).then(|| args.windows(2).find(|w| w[0] == "--session-dir").map(|w| w[1].clone())).flatten()
                .or_else(|| args.iter().find_map(|a| a.strip_prefix("--session-dir=").map(str::to_owned)).filter(|_| isolated.is_none()));
            let dir = pi_config.home_path.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|p| resolve(&expand_home(p, &home)))
                .or_else(|| from_args.map(|p| resolve(&expand_home(p.trim(), &home))))
                .or_else(|| env_home("PI_CODING_AGENT_SESSION_DIR").map(|p| resolve(&expand_home(p.trim(), &home))))
                .or_else(|| env_home("PI_CODING_AGENT_DIR").map(|p| resolve(&expand_home(p.trim(), &home)).join("sessions")))
                .unwrap_or_else(|| home.join(".pi").join("agent").join("sessions"));
            homes.push((Source::Pi, Source::Pi.as_str().to_owned(), dir));
        }
        let worktrees = settings.worktrees_directory.as_deref().map(str::trim).filter(|s| !s.is_empty());
        ScanEnv {
            worktrees_dir: worktrees.map(|w| resolve(&expand_home(w, &home))).unwrap_or_else(|| base_dir.join("worktrees")),
            base_dir: resolve(&base_dir.to_string_lossy()),
            temp_dir: std::env::temp_dir(),
            homes,
            home,
        }
    }

    fn excluded(&self, candidate: &Path) -> bool {
        let c = candidate.to_string_lossy();
        let norm = normalize_for_comparison(&c);
        let roots = [self.home.as_path(), self.temp_dir.as_path(), Path::new("/tmp"), Path::new("/private/tmp")];
        if roots.iter().any(|r| normalize_for_comparison(&resolve(&r.to_string_lossy()).to_string_lossy()) == norm) {
            return true;
        }
        let under = |ancestor: &Path| with_slash(&c).starts_with(&with_slash(&ancestor.to_string_lossy()));
        under(&self.home.join("Downloads"))
            // App data (the Claude app's scratch workspaces among it), not projects.
            || under(&self.home.join("Library"))
            || under(&self.home.join("Documents").join("Codex"))
            || under(&self.base_dir)
            || under(&self.worktrees_dir)
            || with_slash(&c).contains("/.t3/worktrees/")
    }
}

// ---- paths ----

fn with_slash(p: &str) -> String {
    format!("{}/", p.replace('\\', "/"))
}

/// `normalizeProjectPathForComparison` (POSIX): trimmed, without trailing separators.
fn normalize_for_comparison(p: &str) -> String {
    let t = p.trim();
    if t == "/" || t.is_empty() {
        return t.to_owned();
    }
    let trimmed = t.trim_end_matches('/');
    if trimmed.is_empty() { t.to_owned() } else { trimmed.to_owned() }
}

/// `expandHomePath`.
fn expand_home(value: &str, home: &Path) -> String {
    if value == "~" {
        return home.to_string_lossy().into_owned();
    }
    match value.strip_prefix("~/").or_else(|| value.strip_prefix("~\\")) {
        Some(rest) => home.join(rest).to_string_lossy().into_owned(),
        None => value.to_owned(),
    }
}

/// `path.resolve`: absolute and lexically normalized.
fn resolve(p: &str) -> PathBuf {
    let path = Path::new(p);
    let abs = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn millis(t: std::io::Result<SystemTime>) -> Option<i64> {
    t.ok()?.duration_since(UNIX_EPOCH).ok().map(|d| d.as_millis() as i64)
}

/// A transcript as it was when read; an import is redone only if it changes.
#[derive(Debug, Clone, PartialEq, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub file_path: String,
    pub size: u64,
    pub mtime_ms: Option<i64>,
    pub device: u64,
    pub inode: Option<String>,
    pub birthtime_ms: Option<i64>,
}

/// (device, inode); Windows has no stable equivalent in `std`, so files are
/// told apart by path there.
#[cfg(unix)]
fn device_inode(meta: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

#[cfg(not(unix))]
fn device_inode(_: &std::fs::Metadata) -> (u64, u64) {
    (0, 0)
}

fn identity(path: &Path, meta: &std::fs::Metadata) -> Identity {
    let (device, inode) = device_inode(meta);
    Identity {
        file_path: path.to_string_lossy().into_owned(),
        size: meta.len(),
        mtime_ms: millis(meta.modified()),
        device,
        inode: (inode > 0).then(|| inode.to_string()),
        birthtime_ms: millis(meta.created()),
    }
}

/// `directoryIdentity`: the same directory under any spelling or symlink.
fn directory_identity(path: &Path) -> String {
    match std::fs::metadata(path).map(|m| device_inode(&m)) {
        Ok((device, inode)) if inode > 0 => format!("inode:{device}:{inode}"),
        _ => {
            let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            format!("path:{}", normalize_for_comparison(&real.to_string_lossy()))
        }
    }
}

// ---- transcript records ----

#[derive(Debug, Clone, Default, Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Blocks(Vec<Block>),
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Message {
    content: Option<Content>,
    model: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Payload {
    id: Option<String>,
    session_id: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    role: Option<String>,
    message: Option<String>,
    model: Option<String>,
    cwd: Option<String>,
    content: Option<Vec<Block>>,
    internal_chat_message_metadata_passthrough: Option<Value>,
}

/// t3code's `TranscriptRecord`: the fields of Claude and Codex lines it reads.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    cwd: Option<String>,
    session_id: Option<String>,
    ai_title: Option<String>,
    custom_title: Option<String>,
    is_sidechain: Option<bool>,
    is_meta: Option<bool>,
    is_compact_summary: Option<bool>,
    /// Claude Code: who wrote a user record (`{"kind": "human"}` for the
    /// person; `task-notification` and others for the harness).
    origin: Option<Value>,
    message: Option<Message>,
    payload: Option<Payload>,
}

impl Record {
    fn kind(&self) -> &str {
        self.kind.as_deref().unwrap_or("")
    }
    fn payload_kind(&self) -> &str {
        self.payload.as_ref().and_then(|p| p.kind.as_deref()).unwrap_or("")
    }
    fn payload_role(&self) -> &str {
        self.payload.as_ref().and_then(|p| p.role.as_deref()).unwrap_or("")
    }
    fn hidden_claude(&self) -> bool {
        self.is_sidechain == Some(true) || self.is_meta == Some(true) || self.is_compact_summary == Some(true)
    }
    /// A user record the harness wrote (a background task finishing), not the person.
    fn from_harness(&self) -> bool {
        self.kind() == "user" && self.origin.as_ref().and_then(|o| o["kind"].as_str()).is_some_and(|k| k != "human")
    }
    fn cwd(&self) -> Option<String> {
        let c = self.cwd.as_deref().map(str::trim).filter(|s| !s.is_empty());
        c.or_else(|| self.payload.as_ref().and_then(|p| p.cwd.as_deref()).map(str::trim).filter(|s| !s.is_empty())).map(str::to_owned)
    }
}

fn is_text_block(b: &Block) -> bool {
    matches!(b.kind.as_deref(), Some("text" | "input_text" | "output_text"))
}

fn blocks_text(blocks: &[Block]) -> String {
    blocks
        .iter()
        .filter(|b| is_text_block(b))
        .map(|b| b.text.as_deref().unwrap_or("").trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn extract_text(content: Option<&Content>) -> String {
    match content {
        Some(Content::Text(s)) => s.trim().to_owned(),
        Some(Content::Blocks(b)) => blocks_text(b),
        None => String::new(),
    }
}

/// User text without the `<system-reminder>` blocks Claude Code adds to it.
fn without_reminders(text: &str) -> String {
    const OPEN: &str = "<system-reminder>";
    const CLOSE: &str = "</system-reminder>";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        match rest[start..].find(CLOSE) {
            Some(end) => rest = &rest[start + end + CLOSE.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_owned()
}

fn normalize_timestamp(value: Option<&str>, fallback: &str) -> String {
    value.and_then(|v| v.parse::<jiff::Timestamp>().ok()).map(|t| iso_from_millis(t.as_millisecond())).unwrap_or_else(|| fallback.to_owned())
}

fn codex_turn_id(metadata: Option<&Value>) -> Option<String> {
    metadata?["turn_id"].as_str().filter(|s| !s.trim().is_empty()).map(str::to_owned)
}

#[derive(Debug, Clone, Default)]
struct ClaudeFields {
    session_id: Option<String>,
    title: Option<String>,
    custom_title: Option<Option<String>>,
    model: Option<String>,
}

fn fold_claude_fields(mut f: ClaudeFields, r: &Record) -> ClaudeFields {
    if r.hidden_claude() {
        return f;
    }
    let nonblank = |s: &Option<String>| s.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
    if let Some(id) = nonblank(&r.session_id) {
        f.session_id = Some(id);
    }
    if let Some(t) = nonblank(&r.ai_title) {
        f.title = Some(t);
    }
    // Claude flushes the AI title after the user's rename; an empty rename clears it.
    if let Some(c) = &r.custom_title {
        f.custom_title = Some(Some(c.trim().to_owned()).filter(|s| !s.is_empty()));
    }
    // `<synthetic>` marks local error responses, not a selectable model.
    if let Some(m) = r.message.as_ref().and_then(|m| m.model.as_deref()).map(str::trim).filter(|m| !m.is_empty() && *m != "<synthetic>") {
        f.model = Some(m.to_owned());
    }
    f
}

/// Codex setup context ahead of the real request, skipped when deriving a title.
fn strip_codex_preamble(text: &str) -> &str {
    const TAGS: [&str; 5] = ["recommended_plugins", "environment_context", "user_instructions", "INSTRUCTIONS", "in-app-browser-context"];
    let mut rest = text;
    loop {
        let trimmed = rest.trim_start();
        let mut advanced = None;
        for tag in TAGS {
            let open = format!("<{tag}");
            if let Some(after) = trimmed.strip_prefix(&open) {
                if after.starts_with('>') || after.starts_with(char::is_whitespace) {
                    let close = format!("</{tag}>");
                    if let Some(end) = after.find(&close) {
                        advanced = Some(&after[end + close.len()..]);
                        break;
                    }
                }
            }
        }
        if advanced.is_none() {
            if let Some(after) = trimmed.strip_prefix("# AGENTS.md instructions") {
                if after.is_empty() || !after.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                    advanced = Some(after.find('\n').map_or("", |i| &after[i..]));
                }
            } else if let Some(after) = trimmed.strip_prefix("## My request for Codex:") {
                advanced = Some(after);
            }
        }
        match advanced {
            Some(next) => rest = next,
            None => return if rest.len() == text.len() { text } else { rest },
        }
    }
}

fn derive_title(source: Source, text: &str) -> Option<String> {
    let request = if source == Source::Codex { strip_codex_preamble(text) } else { text };
    let first = request.trim().split('\n').next().unwrap_or("");
    let t: String = first.chars().take(100).collect::<String>().trim().to_owned();
    (!t.is_empty()).then_some(t)
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ThreadMessage {
    pub role: String,
    pub text: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedThread {
    pub source: Source,
    pub provider_instance_id: String,
    pub provider_session_id: String,
    /// What the provider resumes, when not the session id (Pi: its session file).
    pub native_id: Option<String>,
    pub title: String,
    pub model: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub messages: Vec<ThreadMessage>,
    pub omitted_message_count: usize,
}

pub struct TranscriptMeta<'a> {
    pub source: Source,
    pub provider_instance_id: &'a str,
    pub fallback_session_id: &'a str,
    pub last_active_ms: i64,
}

/// `parseAgentSessionTranscript`: a whole transcript's contents.
#[cfg(test)]
fn parse_transcript(meta: &TranscriptMeta<'_>, contents: &str) -> Option<ParsedThread> {
    let body = contents.strip_suffix('\n').unwrap_or(contents);
    let lines: Vec<&str> = body.split('\n').take(MAX_IMPORT_RECORDS + 1).collect();
    if lines.len() > MAX_IMPORT_RECORDS {
        return None;
    }
    let records: Vec<Record> = lines.iter().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let fields = if meta.source == Source::Claude { records.iter().fold(ClaudeFields::default(), fold_claude_fields) } else { ClaudeFields::default() };
    parse_records(meta, &records, &fields, 0)
}

struct Kept {
    role: String,
    text: String,
    created_at: String,
    codex_response_user: bool,
    seq: usize,
}

/// `parseAgentSessionRecords` for Claude and Codex.
fn parse_records(meta: &TranscriptMeta<'_>, records: &[Record], fields: &ClaudeFields, dropped: usize) -> Option<ParsedThread> {
    let fallback = iso_from_millis(meta.last_active_ms);
    let mut session_id = match meta.source {
        Source::Codex | Source::Pi => String::new(),
        Source::Claude => fields.session_id.clone().unwrap_or_else(|| meta.fallback_session_id.to_owned()),
    };
    let mut model = fields.model.clone();
    let mut derived_title: Option<String> = None;
    let mut has_codex_session = false;

    // Codex records a prompt both as a user event and as a response item; a
    // response copy whose turn also has the event is not a separate message.
    let mut canonical_response_users: HashSet<usize> = HashSet::new();
    if meta.source == Source::Codex {
        let mut texts_in_turn: HashSet<String> = HashSet::new();
        let mut users_in_turn: Vec<(usize, String, String)> = vec![];
        let mut finish = |texts: &mut HashSet<String>, users: &mut Vec<(usize, String, String)>| {
            let turns: HashSet<String> = users.iter().filter(|(_, _, t)| texts.contains(t)).map(|(_, id, _)| id.clone()).collect();
            for (i, id, _) in users.iter() {
                if turns.contains(id) {
                    canonical_response_users.insert(*i);
                }
            }
            texts.clear();
            users.clear();
        };
        for (i, r) in records.iter().enumerate() {
            if r.kind() == "response_item" && r.payload_kind() == "message" && r.payload_role() == "assistant" {
                finish(&mut texts_in_turn, &mut users_in_turn);
            } else if r.kind() == "event_msg" && r.payload_kind() == "user_message" {
                let t = r.payload.as_ref().and_then(|p| p.message.as_deref()).unwrap_or("").trim().to_owned();
                if !t.is_empty() {
                    texts_in_turn.insert(t);
                }
            } else if r.kind() == "response_item" && r.payload_kind() == "message" && r.payload_role() == "user" {
                let p = r.payload.as_ref().expect("a message record has a payload");
                let text = blocks_text(p.content.as_deref().unwrap_or(&[]));
                if let Some(turn) = codex_turn_id(p.internal_chat_message_metadata_passthrough.as_ref()) {
                    if !text.is_empty() {
                        users_in_turn.push((i, turn, text));
                    }
                }
            }
        }
        finish(&mut texts_in_turn, &mut users_in_turn);
    }

    let mut messages: VecDeque<Kept> = VecDeque::new();
    let mut first_user: Option<(usize, String, String)> = None; // (seq, text, createdAt)
    let mut count = 0usize;
    let mut seq = 0usize;
    let retain = |messages: &mut VecDeque<Kept>, first_user: &mut Option<(usize, String, String)>, derived: &mut Option<String>, count: &mut usize, k: Kept| {
        if first_user.is_none() && k.role == "user" {
            *first_user = Some((k.seq, k.text.clone(), k.created_at.clone()));
        }
        if derived.is_none() && k.role == "user" {
            *derived = derive_title(meta.source, &k.text);
        }
        *count += 1;
        messages.push_back(k);
        if messages.len() > MAX_IMPORTED_MESSAGES {
            messages.pop_front();
        }
    };

    for (i, r) in records.iter().enumerate() {
        if meta.source == Source::Claude {
            if r.hidden_claude() || r.from_harness() || (r.kind() != "user" && r.kind() != "assistant") {
                continue;
            }
            let mut text = extract_text(r.message.as_ref().and_then(|m| m.content.as_ref()));
            if r.kind() == "user" {
                text = without_reminders(&text);
            }
            if text.is_empty() {
                continue;
            }
            seq += 1;
            let k = Kept { role: r.kind().to_owned(), text, created_at: normalize_timestamp(r.timestamp.as_deref(), &fallback), codex_response_user: false, seq };
            retain(&mut messages, &mut first_user, &mut derived_title, &mut count, k);
            continue;
        }
        match r.kind() {
            "session_meta" => {
                let p = r.payload.as_ref();
                let id = p.and_then(|p| p.id.as_deref()).map(str::trim).filter(|s| !s.is_empty())
                    .or_else(|| p.and_then(|p| p.session_id.as_deref()).map(str::trim).filter(|s| !s.is_empty()));
                if let (false, Some(id)) = (has_codex_session, id) {
                    session_id = id.to_owned();
                    has_codex_session = true;
                }
                continue;
            }
            "turn_context" => {
                if let Some(m) = r.payload.as_ref().and_then(|p| p.model.as_deref()).map(str::trim).filter(|m| !m.is_empty()) {
                    model = Some(m.to_owned());
                    continue;
                }
            }
            _ => {}
        }
        if r.kind() == "event_msg" && r.payload_kind() == "user_message" {
            let text = r.payload.as_ref().and_then(|p| p.message.clone()).unwrap_or_default();
            if text.trim().is_empty() {
                continue;
            }
            // Remove only a matching response copy within this turn.
            for idx in (0..messages.len()).rev() {
                if messages[idx].role == "assistant" {
                    break;
                }
                if messages[idx].codex_response_user && messages[idx].text.trim() == text.trim() {
                    if first_user.as_ref().is_some_and(|f| f.0 == messages[idx].seq) {
                        first_user = None;
                    }
                    messages.remove(idx);
                    count -= 1;
                    break;
                }
            }
            seq += 1;
            let k = Kept { role: "user".into(), text, created_at: normalize_timestamp(r.timestamp.as_deref(), &fallback), codex_response_user: false, seq };
            retain(&mut messages, &mut first_user, &mut derived_title, &mut count, k);
            continue;
        }
        let role = r.payload_role();
        if r.kind() != "response_item" || r.payload_kind() != "message" || (role != "user" && role != "assistant") {
            continue;
        }
        let text = blocks_text(r.payload.as_ref().and_then(|p| p.content.as_deref()).unwrap_or(&[]));
        if text.is_empty() {
            continue;
        }
        if role == "user" && canonical_response_users.contains(&i) {
            continue;
        }
        if role == "user" {
            let has_event = messages.iter().rev().take_while(|m| m.role != "assistant").any(|m| m.role == "user" && !m.codex_response_user && m.text.trim() == text.trim());
            if has_event {
                continue;
            }
        }
        seq += 1;
        let k = Kept { role: role.to_owned(), text, created_at: normalize_timestamp(r.timestamp.as_deref(), &fallback), codex_response_user: role == "user", seq };
        retain(&mut messages, &mut first_user, &mut derived_title, &mut count, k);
    }

    let (first_seq, first_text, first_at) = first_user?;
    if session_id.trim().is_empty() {
        return None;
    }
    let visible = |k: &Kept| ThreadMessage { role: k.role.clone(), text: k.text.clone(), created_at: k.created_at.clone() };
    let retained: Vec<ThreadMessage> = if messages.iter().any(|m| m.seq == first_seq) {
        messages.iter().map(visible).collect()
    } else {
        let skip = messages.len().saturating_sub(MAX_IMPORTED_MESSAGES - 1);
        std::iter::once(ThreadMessage { role: "user".into(), text: first_text, created_at: first_at })
            .chain(messages.iter().skip(skip).map(visible))
            .collect()
    };
    let title = match &fields.custom_title {
        Some(Some(c)) => c.clone(),
        _ => fields.title.clone().or(derived_title).unwrap_or_else(|| "Imported thread".into()),
    };
    Some(ParsedThread {
        source: meta.source,
        provider_instance_id: meta.provider_instance_id.to_owned(),
        provider_session_id: session_id,
        native_id: None,
        title,
        model,
        created_at: retained.first().map(|m| m.created_at.clone()).unwrap_or_else(|| fallback.clone()),
        updated_at: fallback,
        omitted_message_count: (count + dropped).saturating_sub(retained.len()),
        messages: retained,
    })
}

/// `shouldRetainDecodedRecord`: the records an import parse needs.
fn should_retain(source: Source, r: &Record) -> bool {
    match source {
        Source::Claude => {
            if r.hidden_claude() || (r.kind() != "user" && r.kind() != "assistant") {
                return false;
            }
            !extract_text(r.message.as_ref().and_then(|m| m.content.as_ref())).is_empty()
        }
        Source::Pi => false,
        Source::Codex => {
            r.cwd().is_some()
                || matches!(r.kind(), "session_meta" | "turn_context")
                || (r.kind() == "event_msg" && r.payload_kind() == "user_message")
                || (r.kind() == "response_item" && r.payload_kind() == "message" && matches!(r.payload_role(), "user" | "assistant"))
        }
    }
}

struct Snapshot {
    records: Vec<Record>,
    dropped: usize,
    cwd: Option<String>,
    fields: ClaudeFields,
}

/// `createClaudeHistoryWindow`: the first user message and the latest
/// messages; everything else counts as dropped.
#[derive(Default)]
struct ClaudeWindow {
    recent: VecDeque<(usize, Record, usize)>,
    first_user: Option<(usize, Record)>,
    dropped: usize,
    next: usize,
}

impl ClaudeWindow {
    /// Returns the bytes released.
    fn push(&mut self, r: Record, bytes: usize) -> usize {
        let idx = self.next;
        self.next += 1;
        if self.first_user.is_none() && r.kind() == "user" {
            self.first_user = Some((idx, r.clone()));
        }
        self.recent.push_back((idx, r, bytes));
        if self.recent.len() <= MAX_IMPORTED_MESSAGES {
            return 0;
        }
        let (evicted, _, bytes) = self.recent.pop_front().expect("more than the limit are kept");
        if self.first_user.as_ref().is_some_and(|(i, _)| *i == evicted) {
            return 0;
        }
        self.dropped += 1;
        bytes
    }

    fn records(self) -> Vec<Record> {
        let held_first = self.first_user.as_ref().is_none_or(|(i, _)| self.recent.iter().any(|(j, _, _)| j == i));
        let held = self.recent.into_iter().map(|(_, r, _)| r);
        match self.first_user {
            Some((_, f)) if !held_first => std::iter::once(f).chain(held).collect(),
            _ => held.collect(),
        }
    }
}

/// `readTranscript`: streams the file, keeping only what the parse needs.
/// None when the file changed while being read or exceeds the budgets.
fn read_transcript(path: &Path, expected: &Identity, source: Source) -> Option<Snapshot> {
    let file = std::fs::File::open(path).ok()?;
    if identity(path, &file.metadata().ok()?) != *expected {
        return None;
    }
    let mut reader = BufReader::new(&file);
    let mut records = vec![];
    let mut window = ClaudeWindow::default();
    let (mut history_bytes, mut count) = (0usize, 0usize);
    let mut cwd = None;
    let mut fields = ClaudeFields::default();
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line).ok()?;
        if n == 0 {
            break;
        }
        count += 1;
        // Claude keeps a bounded window, so only the byte budget limits it.
        if count > MAX_IMPORT_RECORDS && source != Source::Claude {
            return None;
        }
        let text = String::from_utf8_lossy(&line);
        let Ok(r) = serde_json::from_str::<Record>(text.trim_end_matches(['\n', '\r'])) else { continue };
        if cwd.is_none() {
            cwd = r.cwd();
        }
        if source == Source::Claude {
            fields = fold_claude_fields(fields, &r);
        }
        if !should_retain(source, &r) {
            continue;
        }
        history_bytes += n;
        if source == Source::Claude {
            history_bytes -= window.push(r, n);
        } else {
            records.push(r);
        }
        if history_bytes > MAX_IMPORT_HISTORY_BYTES {
            return None;
        }
    }
    if identity(path, &file.metadata().ok()?) != *expected {
        return None;
    }
    let dropped = window.dropped;
    if source == Source::Claude {
        records = window.records();
    }
    Some(Snapshot { records, dropped, cwd, fields })
}

// ---- discovery ----

#[derive(Debug, Clone)]
struct Transcript {
    path: PathBuf,
    mtime_ms: i64,
    instance: String,
    home: PathBuf,
    identity: Identity,
    project_roots: Option<Vec<String>>,
}

struct Discovery {
    ops: usize,
    truncated: bool,
}

impl Discovery {
    fn list(&mut self, dir: &Path) -> Vec<String> {
        if self.ops == 0 {
            self.truncated = true;
            return vec![];
        }
        self.ops -= 1;
        std::fs::read_dir(dir)
            .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default()
    }

    fn stat(&mut self, path: PathBuf, instance: &str, home: &Path, out: &mut Vec<Transcript>) {
        if self.ops == 0 {
            self.truncated = true;
            return;
        }
        self.ops -= 1;
        let Ok(meta) = std::fs::metadata(&path) else { return };
        let Some(mtime_ms) = millis(meta.modified()).filter(|_| meta.is_file()) else { return };
        let identity = identity(&path, &meta);
        out.push(Transcript { path, mtime_ms, instance: instance.to_owned(), home: home.to_path_buf(), identity, project_roots: None });
    }
}

fn discover_claude(home: &Path, instance: &str, ops: usize) -> (Vec<Transcript>, bool) {
    let mut d = Discovery { ops, truncated: false };
    let projects = home.join("projects");
    let mut out = vec![];
    for project in d.list(&projects) {
        if d.ops == 0 {
            d.truncated = true;
            break;
        }
        let dir = projects.join(&project);
        for entry in d.list(&dir).into_iter().filter(|e| e.ends_with(".jsonl")) {
            d.stat(dir.join(entry), instance, home, &mut out);
        }
    }
    (out, d.truncated)
}

/// Pi's sessions: `*.jsonl` in the folder, or one folder down (pi groups them by cwd).
fn discover_pi(home: &Path, instance: &str, ops: usize) -> (Vec<Transcript>, bool) {
    let mut d = Discovery { ops, truncated: false };
    let mut out = vec![];
    for entry in d.list(home) {
        if d.ops == 0 {
            d.truncated = true;
            break;
        }
        let path = home.join(&entry);
        if entry.ends_with(".jsonl") {
            d.stat(path, instance, home, &mut out);
        } else if path.is_dir() {
            for file in d.list(&path).into_iter().filter(|e| e.ends_with(".jsonl")) {
                d.stat(path.join(file), instance, home, &mut out);
            }
        }
    }
    (out, d.truncated)
}

/// The text of a pi message's content (a string or text blocks).
fn pi_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.trim().to_owned(),
        Value::Array(blocks) => blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n").trim().to_owned(),
        _ => String::new(),
    }
}

/// A pi session file as an importable thread, with its cwd: the header's
/// session id and cwd, the messages of the active branch (from the last
/// entry back through parents), titled by its last name. `None` if it
/// changed while read, exceeds the budgets, or has no user message.
fn read_pi_session(path: &Path, expected: &Identity, instance: &str, last_active_ms: i64) -> Option<(String, ParsedThread)> {
    let file = std::fs::File::open(path).ok()?;
    if identity(path, &file.metadata().ok()?) != *expected {
        return None;
    }
    let mut reader = BufReader::new(&file);
    let (mut session_id, mut cwd, mut name) = (None, None, None);
    // id -> (parent, role, text, timestamp, model): what the branch walk needs.
    let mut entries: HashMap<String, (Option<String>, String, String, String, Option<String>)> = HashMap::new();
    let mut last: Option<String> = None;
    let (mut line, mut count, mut bytes) = (Vec::new(), 0usize, 0usize);
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line).ok()?;
        if n == 0 {
            break;
        }
        count += 1;
        bytes += n;
        if count > MAX_IMPORT_RECORDS || bytes > MAX_IMPORT_HISTORY_BYTES.saturating_mul(4) {
            return None;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&line) else { continue };
        match v["type"].as_str() {
            Some("session") => {
                session_id = v["id"].as_str().map(str::to_owned);
                cwd = v["cwd"].as_str().filter(|c| !c.trim().is_empty()).map(str::to_owned);
                continue;
            }
            Some("session_info") => name = v["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).map(str::to_owned).or(name),
            _ => {}
        }
        let Some(id) = v["id"].as_str() else { continue };
        let m = &v["message"];
        let role = if v["type"] == "message" { m["role"].as_str().unwrap_or_default().to_owned() } else { String::new() };
        let text = if matches!(role.as_str(), "user" | "assistant") { pi_text(&m["content"]) } else { String::new() };
        let model = m["provider"].as_str().zip(m["model"].as_str()).map(|(p, id)| format!("{p}/{id}"));
        let at = v["timestamp"].as_str().unwrap_or_default().to_owned();
        entries.insert(id.to_owned(), (v["parentId"].as_str().map(str::to_owned), role, text, at, model));
        last = Some(id.to_owned());
    }
    if identity(path, &file.metadata().ok()?) != *expected {
        return None;
    }
    // The active branch, newest first; a cycle ends it.
    let mut seen = HashSet::new();
    let mut branch = vec![];
    let mut current = last;
    while let Some(id) = current {
        if !seen.insert(id.clone()) {
            break;
        }
        let Some(e) = entries.get(&id) else { break };
        branch.push(e);
        current = e.0.clone();
    }
    branch.reverse();
    let fallback = iso_from_millis(last_active_ms);
    let said: Vec<ThreadMessage> = branch
        .iter()
        .filter(|e| matches!(e.1.as_str(), "user" | "assistant") && !e.2.is_empty())
        .map(|e| ThreadMessage { role: e.1.clone(), text: e.2.clone(), created_at: normalize_timestamp(Some(&e.3).filter(|t| !t.is_empty()).map(|t| t.as_str()), &fallback) })
        .collect();
    let first_user = said.iter().position(|m| m.role == "user")?;
    let total = said.len();
    // The first message and the latest ones, as for Claude.
    let messages: Vec<ThreadMessage> = if total <= MAX_IMPORTED_MESSAGES {
        said.clone()
    } else {
        let tail = &said[total - (MAX_IMPORTED_MESSAGES - 1)..];
        if tail.iter().any(|m| std::ptr::eq(m, &said[first_user])) { tail.to_vec() } else { std::iter::once(said[first_user].clone()).chain(tail.iter().cloned()).collect() }
    };
    let title = name.or_else(|| derive_title(Source::Pi, &said[first_user].text)).unwrap_or_else(|| "Imported thread".into());
    let model = branch.iter().rev().find_map(|e| e.4.clone());
    let thread = ParsedThread {
        source: Source::Pi,
        provider_instance_id: instance.to_owned(),
        provider_session_id: session_id?,
        native_id: Some(path.to_string_lossy().into_owned()),
        title,
        model,
        created_at: messages.first().map(|m| m.created_at.clone()).unwrap_or_else(|| fallback.clone()),
        updated_at: fallback,
        omitted_message_count: total - messages.len(),
        messages,
    };
    Some((cwd?, thread))
}

fn sorted_desc(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v.reverse();
    v
}

/// Codex chat id (and, for a rewound rollout, its rollout id) from a rollout file name.
fn codex_rollout_ids(path: &Path) -> Option<(String, Option<String>)> {
    let rollout = regex!(r"(?i)-([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})(?:_([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}))?\.jsonl$");
    let name = path.to_string_lossy();
    let c = rollout.captures(&name)?;
    Some((c[1].to_owned(), c.get(2).map(|m| m.as_str().to_owned())))
}

/// Codex records a chat's project only in its state database.
fn read_codex_project_roots(home: &Path) -> HashMap<String, Vec<String>> {
    let mut roots: HashMap<String, Vec<String>> = HashMap::new();
    let path = home.join(CODEX_STATE_DATABASE);
    if !path.is_file() {
        return roots;
    }
    let Ok(conn) = rusqlite::Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX) else {
        return roots;
    };
    let _ = conn.busy_timeout(std::time::Duration::from_millis(100));
    let rows: rusqlite::Result<Vec<(Option<String>, Option<String>)>> = conn
        .prepare(
            "SELECT threads.id AS id, project_roots.path AS path
             FROM threads JOIN project_roots ON project_roots.project_id = threads.project_id
             ORDER BY threads.rowid DESC, project_roots.position LIMIT ?1",
        )
        .and_then(|mut st| st.query_map(params![MAX_CODEX_PROJECT_ROOT_ROWS], |r| Ok((r.get(0).ok(), r.get(1).ok())))?.collect());
    for (id, root) in rows.unwrap_or_default() {
        if let (Some(id), Some(root)) = (id, root.filter(|r| !r.trim().is_empty())) {
            roots.entry(id).or_default().push(root);
        }
    }
    roots
}

fn discover_codex(home: &Path, instance: &str, ops: usize) -> (Vec<Transcript>, bool) {
    let mut d = Discovery { ops, truncated: false };
    let sessions = home.join("sessions");
    let mut out = vec![];
    'outer: for year in sorted_desc(d.list(&sessions)) {
        for month in sorted_desc(d.list(&sessions.join(&year))) {
            for day in sorted_desc(d.list(&sessions.join(&year).join(&month))) {
                if d.ops == 0 {
                    d.truncated = true;
                    break 'outer;
                }
                let dir = sessions.join(&year).join(&month).join(&day);
                for entry in sorted_desc(d.list(&dir)).into_iter().filter(|e| e.starts_with("rollout-") && e.ends_with(".jsonl")) {
                    d.stat(dir.join(entry), instance, home, &mut out);
                }
            }
        }
    }
    let roots = read_codex_project_roots(home);
    for t in &mut out {
        t.project_roots = codex_rollout_ids(&t.path).and_then(|(id, _)| roots.get(&id).cloned());
    }
    (out, d.truncated)
}

struct MetadataBudget {
    bytes: u64,
    records: usize,
    truncated: bool,
}

enum CwdRead {
    Found(String),
    /// A Codex subagent rollout, never offered.
    Excluded,
    Missing,
}

/// `extractCwd` on one raw line.
fn extract_cwd(line: &str, source: Source) -> CwdRead {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return CwdRead::Missing };
    if !v.is_object() {
        return CwdRead::Missing;
    }
    if let Some(c) = v["cwd"].as_str().filter(|c| !c.trim().is_empty()) {
        return CwdRead::Found(c.to_owned());
    }
    let payload = &v["payload"];
    if payload.is_object() {
        if source == Source::Codex && v["type"] == "session_meta" {
            if let Some(src) = payload["source"].as_object() {
                if src.contains_key("subagent") || src.contains_key("subAgent") {
                    return CwdRead::Excluded;
                }
            }
        }
        if let Some(c) = payload["cwd"].as_str().filter(|c| !c.trim().is_empty()) {
            return CwdRead::Found(c.to_owned());
        }
    }
    CwdRead::Missing
}

/// `readCwd`: the first cwd within the transcript's first MiB.
fn read_cwd(t: &Transcript, source: Source, budget: &mut MetadataBudget) -> CwdRead {
    if t.identity.size == 0 {
        return CwdRead::Missing;
    }
    if budget.bytes == 0 || budget.records == 0 {
        budget.truncated = true;
        return CwdRead::Missing;
    }
    let want = MAX_TRANSCRIPT_SCAN_BYTES.min(t.identity.size).min(budget.bytes);
    let mut buf = Vec::with_capacity(want as usize);
    let Ok(file) = std::fs::File::open(&t.path) else { return CwdRead::Missing };
    if file.take(want).read_to_end(&mut buf).is_err() {
        return CwdRead::Missing;
    }
    budget.bytes -= buf.len() as u64;
    let text = String::from_utf8_lossy(&buf);
    let mut parts: Vec<&str> = text.split('\n').collect();
    let remaining = parts.pop().unwrap_or("");
    let mut records = 0usize;
    let mut reserve = |budget: &mut MetadataBudget| {
        if records == MAX_METADATA_RECORDS_PER_TRANSCRIPT || budget.records == 0 {
            budget.truncated = true;
            return false;
        }
        records += 1;
        budget.records -= 1;
        true
    };
    for line in parts {
        if !reserve(budget) {
            return CwdRead::Missing;
        }
        match extract_cwd(line.trim(), source) {
            CwdRead::Missing => {}
            found => return found,
        }
    }
    if (buf.len() as u64) < t.identity.size {
        budget.truncated = true;
        return CwdRead::Missing;
    }
    if remaining.is_empty() || !reserve(budget) {
        return CwdRead::Missing;
    }
    extract_cwd(remaining.trim(), source)
}

/// Sessions grouped by provider instance and working directory.
#[derive(Debug, Clone)]
struct RawCandidate {
    cwd: String,
    source: Source,
    instance: String,
    home: PathBuf,
    last_active_ms: i64,
    transcripts: Vec<Transcript>,
}

/// One transcript an import came from.
#[derive(Debug, Clone, PartialEq, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSource {
    #[serde(flatten)]
    pub identity: Identity,
    pub provider: String,
    pub provider_instance_id: String,
    pub provider_session_id: String,
}

/// Recorded imports: (thread id, cwd, source).
fn recorded_imports(store: &Store) -> rusqlite::Result<Vec<(String, String, ImportSource)>> {
    let rows: Vec<(String, String, String)> = store.db().with(|c| {
        let mut st = c.prepare("SELECT thread_id, cwd, sources FROM agent_session_imports")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect()
    })?;
    Ok(rows
        .into_iter()
        .flat_map(|(tid, cwd, sources)| {
            serde_json::from_str::<Vec<ImportSource>>(&sources).unwrap_or_default().into_iter().map(move |s| (tid.clone(), cwd.clone(), s))
        })
        .collect())
}

fn record_import(store: &Store, thread_id: &str, cwd: &str, source: &ImportSource) -> rusqlite::Result<()> {
    store.db().with(|c| {
        let existing: Option<String> =
            c.query_row("SELECT sources FROM agent_session_imports WHERE thread_id = ?1", params![thread_id], |r| r.get(0)).optional()?;
        let mut sources: Vec<ImportSource> = existing.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        if !sources.iter().any(|s| s == source) {
            sources.push(source.clone());
        }
        c.execute(
            "INSERT INTO agent_session_imports (thread_id, cwd, sources) VALUES (?1, ?2, ?3)
             ON CONFLICT (thread_id) DO UPDATE SET sources = excluded.sources",
            params![thread_id, cwd, serde_json::to_string(&sources).unwrap_or_default()],
        )?;
        Ok(())
    })
}

struct Collected {
    candidates: Vec<RawCandidate>,
    truncated: bool,
}

/// `collectCandidates`.
fn collect_candidates(env: &ScanEnv, store: &Store) -> Result<Collected, Failure> {
    let imported = recorded_imports(store).map_err(Failure::read_projects)?;
    let mut imported_by_file: HashMap<(String, String), Vec<(String, ImportSource)>> = HashMap::new();
    for (_, cwd, s) in imported {
        imported_by_file.entry((s.provider_instance_id.clone(), s.identity.file_path.clone())).or_default().push((cwd, s));
    }
    let mut raw = vec![];
    let mut truncated = false;
    for source in [Source::Claude, Source::Codex, Source::Pi] {
        let mut seen = HashSet::new();
        let homes: Vec<&(Source, String, PathBuf)> =
            env.homes.iter().filter(|(s, _, p)| *s == source && seen.insert(directory_identity(p))).collect();
        let mut transcripts = vec![];
        let n = homes.len().max(1);
        for (i, (_, instance, home)) in homes.iter().enumerate() {
            let ops = MAX_DISCOVERY_OPERATIONS_PER_SOURCE / n + usize::from(i < MAX_DISCOVERY_OPERATIONS_PER_SOURCE % n);
            let (found, cut) = match source {
                Source::Claude => discover_claude(home, instance, ops),
                Source::Codex => discover_codex(home, instance, ops),
                Source::Pi => discover_pi(home, instance, ops),
            };
            truncated |= cut;
            transcripts.extend(found);
        }
        transcripts.sort_by(|a, b| b.mtime_ms.cmp(&a.mtime_ms).then_with(|| a.path.cmp(&b.path)));
        if transcripts.len() > MAX_TRANSCRIPTS_PER_SOURCE {
            truncated = true;
            transcripts.truncate(MAX_TRANSCRIPTS_PER_SOURCE);
        }
        let mut budget = MetadataBudget { bytes: MAX_METADATA_BYTES_PER_SOURCE, records: MAX_METADATA_RECORDS_PER_SOURCE, truncated: false };
        let mut groups: Vec<RawCandidate> = vec![];
        for t in transcripts {
            // Unchanged imported Claude transcripts keep their import cwd without a read.
            let known = (source != Source::Codex)
                .then(|| imported_by_file.get(&(t.instance.clone(), t.identity.file_path.clone())))
                .flatten()
                .and_then(|list| list.iter().find(|(_, s)| s.provider == source.as_str() && s.identity == t.identity).map(|(c, _)| c.clone()));
            let cwd = match known {
                Some(c) => c,
                None => match read_cwd(&t, source, &mut budget) {
                    CwdRead::Found(c) => c,
                    CwdRead::Excluded | CwdRead::Missing => continue,
                },
            };
            let cwd = project_cwd(env, &cwd, t.project_roots.as_deref());
            match groups.iter_mut().find(|g| g.instance == t.instance && g.cwd == cwd) {
                Some(g) => {
                    g.last_active_ms = g.last_active_ms.max(t.mtime_ms);
                    g.transcripts.push(t);
                }
                None => groups.push(RawCandidate {
                    cwd,
                    source,
                    instance: t.instance.clone(),
                    home: t.home.clone(),
                    last_active_ms: t.mtime_ms,
                    transcripts: vec![t],
                }),
            }
        }
        truncated |= budget.truncated;
        raw.extend(groups);
    }
    Ok(Collected { candidates: raw, truncated })
}

/// Only chats left in an excluded folder (Codex's scratch folder) take their project's root.
fn project_cwd(env: &ScanEnv, cwd: &str, roots: Option<&[String]>) -> String {
    match roots.and_then(|r| r.first()) {
        Some(root) if env.excluded(&resolve(&expand_home(cwd.trim(), &env.home))) => root.clone(),
        _ => cwd.to_owned(),
    }
}

// ---- git identity ----

fn read_small_file(path: &Path) -> String {
    match std::fs::metadata(path) {
        // Only regular files: a FIFO would block the read.
        Ok(m) if m.is_file() => {
            let mut s = String::new();
            std::fs::File::open(path).and_then(|f| f.take(MAX_GIT_FILE_BYTES).read_to_string(&mut s)).map(|_| s).unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// `parseGitConfigValue`.
fn git_config_value(raw: &str) -> String {
    let mut out = String::new();
    let mut quoted = false;
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(n) => out.push(n),
                None => out.push('\\'),
            },
            '"' => quoted = !quoted,
            '#' | ';' if !quoted => break,
            c => out.push(c),
        }
    }
    out.trim().to_owned()
}

/// `parseOriginUrlFromGitConfig`: origin's URL, else the first remote's.
fn origin_url_from_git_config(config: &str) -> Option<String> {
    let joined = regex::Regex::new(r"\\\r?\n[ \t]*").ok()?.replace_all(config, "").into_owned();
    let header = regex::Regex::new(r#"(?i)^\[\s*remote(?:\s+"([^"]+)"|\.([^\]\s]+))\s*\](.*)$"#).ok()?;
    let url_re = regex::Regex::new(r"(?i)^url\s*=\s*(.*)$").ok()?;
    let (mut section, mut origin, mut first): (Option<String>, Option<String>, Option<String>) = (None, None, None);
    for raw in joined.lines() {
        let mut line = raw.trim().to_owned();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(c) = header.captures(&line.clone()) {
            section = c.get(1).map(|m| m.as_str().to_owned()).or_else(|| c.get(2).map(|m| m.as_str().to_lowercase()));
            line = c[3].trim().to_owned();
        } else if line.starts_with('[') {
            section = None;
            continue;
        }
        let Some(s) = &section else { continue };
        let Some(m) = url_re.captures(&line) else { continue };
        let url = git_config_value(&m[1]);
        if url.is_empty() {
            continue;
        }
        if s == "origin" {
            origin.get_or_insert(url);
        } else {
            first.get_or_insert(url);
        }
    }
    origin.or(first)
}

/// `isBareGitConfig` (single-line values).
fn is_bare_git_config(config: &str) -> bool {
    let header = regex!(r"^\[\s*([^\]]*?)\s*\](.*)$");
    let bare_re = regex!(r"(?i)^bare\s*(?:=(.*)|[#;].*)?$");
    let (mut in_core, mut bare) = (false, false);
    for raw in config.lines() {
        let mut line = raw.trim().to_owned();
        if let Some(c) = header.captures(&line.clone()) {
            in_core = c[1].to_lowercase() == "core";
            line = c[2].trim().to_owned();
        }
        if !in_core {
            continue;
        }
        let Some(m) = bare_re.captures(&line) else { continue };
        let value = m.get(1).map(|v| git_config_value(v.as_str()).to_lowercase()).unwrap_or_else(|| "true".into());
        bare = matches!(value.as_str(), "true" | "yes" | "on") || value.parse::<i64>().is_ok_and(|n| n != 0);
    }
    bare
}

fn lower_case_remote(v: &str) -> String {
    let t = v.trim().trim_end_matches('/');
    let t = if t.len() >= 4 && t.is_char_boundary(t.len() - 4) && t[t.len() - 4..].eq_ignore_ascii_case(".git") { &t[..t.len() - 4] } else { t };
    t.to_lowercase()
}

/// `parseGitRemoteRepository`: host and path, lower case.
fn parse_git_remote(value: &str) -> Option<(String, String)> {
    let normalized = lower_case_remote(value);
    let scheme = regex!(r"(?i)^(?:ssh|https?|git)://");
    if scheme.is_match(&normalized) {
        let lower = value.trim().to_lowercase();
        let rest = lower.split_once("://")?.1;
        let rest = rest.split(['?', '#']).next().unwrap_or("");
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let host = authority.rsplit('@').next().unwrap_or("").split(':').next().unwrap_or("").to_owned();
        let path = path.trim_end_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        return (!host.is_empty() && segments.len() > 1).then(|| (host, segments.join("/")));
    }
    if regex!(r"(?i)^[a-z]:").is_match(&normalized) {
        return None;
    }
    let scp = regex!(r"(?i)^(?:[a-zA-Z0-9._-]+@)?([^@:/\\\s]+):/?([^/\s]+(?:/[^/\s]+)+)$");
    let c = scp.captures(&normalized)?;
    Some((c[1].to_owned(), c[2].to_owned()))
}

/// `normalizeGitRemoteUrl`.
fn remote_key(url: &str) -> String {
    match parse_git_remote(url) {
        Some((host, path)) => format!("{host}/{path}"),
        None => lower_case_remote(url),
    }
}

/// `repositoryPathFromRemoteUrl`: `owner/name` (or deeper) as written.
fn repository_path(url: &str) -> Option<String> {
    let remote = url.trim();
    let scheme = regex!(r"(?i)^(?:ssh|https?|git)://");
    let trimmed = if scheme.is_match(remote) { remote.split(['?', '#']).next().unwrap_or("") } else { remote };
    if trimmed.is_empty() {
        return None;
    }
    let re = regex!(r"(?i)^(?:[^@/\s]+@[^:/\s]+:|([^@:/\\\s]{2,}):|(?:ssh|https?|git)://[^/]+/)((?:[^/\s]+/)+[^/\s]+?)(?:\.git)?/?$");
    let c = re.captures(trimmed)?;
    let path = c[2].trim();
    // `host:path`, but not `host::path` (git's transport syntax).
    if c.get(1).is_some() && path.starts_with(':') {
        return None;
    }
    (!path.is_empty()).then(|| path.to_owned())
}

enum GitIdentity {
    Repository(AgentSessionProjectGit),
    /// A linked worktree; `git` is its repository's, when that could be read.
    Worktree { main_checkout: Option<PathBuf>, git: Option<AgentSessionProjectGit> },
    NotGit,
}

fn git_object(config: &str) -> AgentSessionProjectGit {
    let origin = origin_url_from_git_config(config);
    AgentSessionProjectGit { remote_key: origin.as_deref().map(remote_key), repository: origin.as_deref().and_then(repository_path) }
}

/// `readGitIdentities` for one directory.
fn git_identity(dir: &Path) -> GitIdentity {
    let git_path = dir.join(".git");
    let Ok(meta) = std::fs::metadata(&git_path) else { return GitIdentity::NotGit };
    let (git_dir, linked) = if meta.is_dir() {
        (git_path, false)
    } else {
        let text = read_small_file(&git_path);
        let target = text.lines().find_map(|l| l.strip_prefix("gitdir:")).map(str::trim).filter(|t| !t.is_empty());
        let Some(target) = target else { return GitIdentity::NotGit };
        let git_dir = resolve(&dir.join(target).to_string_lossy());
        let is_worktree = regex!(r"[\\/]worktrees[\\/][^\\/]+[\\/]?$").is_match(&git_dir.to_string_lossy());
        if is_worktree {
            let common = read_small_file(&git_dir.join("commondir")).trim().to_owned();
            if common.is_empty() {
                return GitIdentity::Worktree { main_checkout: None, git: None };
            }
            (resolve(&git_dir.join(common).to_string_lossy()), true)
        } else {
            (git_dir, false)
        }
    };
    let config = read_small_file(&git_dir.join("config"));
    let git = git_object(&config);
    // Bare repositories have no main checkout; their worktrees stand alone.
    if linked && !is_bare_git_config(&config) {
        let main = (!config.is_empty() && git_dir.file_name().is_some_and(|n| n == ".git")).then(|| git_dir.parent().map(Path::to_path_buf)).flatten();
        return GitIdentity::Worktree { main_checkout: main, git: Some(git) };
    }
    GitIdentity::Repository(git)
}

/// `projectDirectoryStats`: an existing, non-excluded directory (symlink targets re-checked).
fn project_directory(env: &ScanEnv, dir: &Path) -> bool {
    if env.excluded(dir) || !std::fs::metadata(dir).is_ok_and(|m| m.is_dir()) {
        return false;
    }
    let real = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    !env.excluded(&real)
}

// ---- scan ----

struct Merged {
    path: PathBuf,
    sources: Vec<Source>,
    thread_count: usize,
    last_active_ms: i64,
    git: Option<AgentSessionProjectGit>,
}

/// The project a session directory belongs to: (identity key, path, git).
fn session_project(env: &ScanEnv, resolved: &Path) -> Option<(String, PathBuf, Option<AgentSessionProjectGit>)> {
    if !project_directory(env, resolved) {
        return None;
    }
    match git_identity(resolved) {
        // A linked worktree's sessions count toward its main checkout's project.
        GitIdentity::Worktree { main_checkout, git } => {
            let main = main_checkout.filter(|m| project_directory(env, m))?;
            Some((directory_identity(&main), main, git))
        }
        GitIdentity::Repository(git) => Some((directory_identity(resolved), resolved.to_path_buf(), Some(git))),
        GitIdentity::NotGit => Some((directory_identity(resolved), resolved.to_path_buf(), None)),
    }
}

/// `scan`: candidate projects, newest activity first.
pub fn scan(env: &ScanEnv, store: &Store) -> Result<AgentSessionScanResult, Failure> {
    let Collected { candidates: raw, truncated } = collect_candidates(env, store)?;
    // Counted as import takes them: the last 30 days, at the project root
    // itself (sessions in a linked worktree show the project but are not imported).
    let now = crate::time_fmt::now_millis();
    let cutoff = now - RECENT_THREAD_WINDOW_MS;
    let mut merged: Vec<(String, Merged)> = vec![];
    let mut projects: HashMap<PathBuf, Option<(String, PathBuf, Option<AgentSessionProjectGit>)>> = HashMap::new();
    for c in &raw {
        let expanded = expand_home(c.cwd.trim(), &env.home);
        if !Path::new(&expanded).is_absolute() {
            continue;
        }
        let resolved = resolve(&expanded);
        let project = projects.entry(resolved.clone()).or_insert_with(|| session_project(env, &resolved)).clone();
        let Some((key, path, git)) = project else { continue };
        let importable = if directory_identity(&resolved) == key {
            c.transcripts.iter().filter(|t| t.mtime_ms >= cutoff && t.mtime_ms <= now).count().min(MAX_IMPORT_TRANSCRIPTS)
        } else {
            0
        };
        match merged.iter_mut().find(|(k, _)| *k == key) {
            Some((_, m)) => {
                if !m.sources.contains(&c.source) {
                    m.sources.push(c.source);
                }
                m.thread_count = (m.thread_count + importable).min(MAX_IMPORT_TRANSCRIPTS);
                m.last_active_ms = m.last_active_ms.max(c.last_active_ms);
            }
            None => merged.push((key, Merged { path, sources: vec![c.source], thread_count: importable, last_active_ms: c.last_active_ms, git })),
        }
    }
    // Persisted roots may use other symlinks to the same directory.
    let shells = store.list_projects().map_err(Failure::read_projects)?;
    let mut by_root: HashMap<String, &crate::wire::shell::ProjectShell> = HashMap::new();
    for p in &shells {
        let root = resolve(&expand_home(&p.workspace_root, &env.home));
        by_root.insert(normalize_for_comparison(&root.to_string_lossy()), p);
        by_root.insert(directory_identity(&root), p);
    }
    let mut candidates: Vec<AgentSessionProjectCandidate> = merged
        .into_iter()
        .map(|(key, m)| {
            let project = by_root.get(&normalize_for_comparison(&m.path.to_string_lossy())).or_else(|| by_root.get(&key));
            let path = project.map(|p| p.workspace_root.clone()).unwrap_or_else(|| m.path.to_string_lossy().into_owned());
            let title = Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| path.clone());
            AgentSessionProjectCandidate {
                path,
                title,
                project_id: project.map(|p| p.id.as_str().into()),
                sources: m.sources.iter().map(|s| s.named()).collect(),
                thread_count: m.thread_count as u64,
                last_active_at: Some(iso_from_millis(m.last_active_ms).into()),
                already_imported: project.is_some(),
                // Always written: `null` outside a repository.
                git: Some(m.git),
            }
        })
        .collect();
    candidates.sort_by(|a, b| b.last_active_at.cmp(&a.last_active_at).then_with(|| a.path.cmp(&b.path)));
    Ok(AgentSessionScanResult { candidates, scanned_at: crate::time_fmt::iso_now().into(), truncated: truncated.then_some(true) })
}

// ---- import ----

enum Recent {
    Importable(Box<ParsedThread>, ImportSource),
    AlreadyImported(ImportSource),
    Duplicate(ImportSource),
    Skipped,
}

/// Codex's saved chat names (`session_index.jsonl`; the newest nonempty name wins).
fn read_codex_thread_names(home: &Path) -> HashMap<String, String> {
    let mut names = HashMap::new();
    let Ok(mut file) = std::fs::File::open(home.join("session_index.jsonl")) else { return names };
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = size.saturating_sub(MAX_CODEX_SESSION_INDEX_BYTES);
    let mut buf = Vec::new();
    if file.seek(SeekFrom::Start(start)).is_err() || file.read_to_end(&mut buf).is_err() {
        return names;
    }
    let text = String::from_utf8_lossy(&buf);
    // A tail read can start mid-record.
    let text = if start > 0 { text.split_once('\n').map(|(_, r)| r).unwrap_or("") } else { &text };
    for line in text.split('\n') {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if let (Some(id), Some(name)) = (v["id"].as_str(), v["thread_name"].as_str().map(str::trim).filter(|n| !n.is_empty())) {
            names.insert(id.to_owned(), name.to_owned());
        }
    }
    names
}

/// `recentThreads`: the project's sessions from the last 30 days, newest first.
fn recent_threads(env: &ScanEnv, store: &Store, workspace_root: &str, completed: &[ImportSource]) -> Result<Vec<Recent>, Failure> {
    let root = resolve(&expand_home(workspace_root, &env.home));
    let real_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
    if env.excluded(&root) || env.excluded(&real_root) {
        return Ok(vec![]);
    }
    let root_identity = directory_identity(&root);
    let now = crate::time_fmt::now_millis();
    let cutoff = now - RECENT_THREAD_WINDOW_MS;
    let Collected { candidates, .. } = collect_candidates(env, store)?;
    let mut eligible: Vec<(&RawCandidate, &Transcript)> = vec![];
    for c in &candidates {
        let expanded = expand_home(c.cwd.trim(), &env.home);
        if !Path::new(&expanded).is_absolute() || directory_identity(&resolve(&expanded)) != root_identity {
            continue;
        }
        eligible.extend(c.transcripts.iter().filter(|t| t.mtime_ms >= cutoff && t.mtime_ms <= now).map(|t| (c, t)));
    }
    eligible.sort_by(|a, b| b.1.mtime_ms.cmp(&a.1.mtime_ms).then_with(|| a.1.path.cmp(&b.1.path)));

    let mut seen_sessions: HashSet<(String, String)> = HashSet::new();
    let mut names: HashMap<PathBuf, HashMap<String, String>> = HashMap::new();
    let (mut bytes_left, mut transcripts_left) = (MAX_IMPORT_BYTES, MAX_IMPORT_TRANSCRIPTS);
    let mut out = vec![];
    for (c, t) in eligible {
        let file_path = t.path.to_string_lossy();
        let done: Vec<&ImportSource> = completed.iter().filter(|s| s.provider_instance_id == c.instance && s.identity.file_path == file_path).collect();
        if done.is_empty() && (transcripts_left == 0 || bytes_left == 0) {
            out.push(Recent::Skipped);
            continue;
        }
        let Some(meta) = std::fs::metadata(&t.path).ok().filter(|m| m.is_file()) else {
            out.push(Recent::Skipped);
            continue;
        };
        let ident = identity(&t.path, &meta);
        if let Some(s) = done.into_iter().find(|s| s.provider == c.source.as_str() && s.identity == ident) {
            if seen_sessions.insert((s.provider_instance_id.clone(), s.provider_session_id.clone())) {
                out.push(Recent::AlreadyImported(s.clone()));
            }
            continue;
        }
        if transcripts_left == 0 || ident.size > MAX_IMPORTED_TRANSCRIPT_BYTES || ident.size > bytes_left {
            out.push(Recent::Skipped);
            continue;
        }
        transcripts_left -= 1;
        bytes_left -= ident.size;
        if c.source == Source::Pi {
            let thread = read_pi_session(&t.path, &ident, &c.instance, t.mtime_ms);
            let Some((cwd, thread)) = thread else {
                out.push(Recent::Skipped);
                continue;
            };
            let expanded = expand_home(project_cwd(env, &cwd, None).trim(), &env.home);
            if !Path::new(&expanded).is_absolute() || directory_identity(&resolve(&expanded)) != root_identity {
                out.push(Recent::Skipped);
                continue;
            }
            let source = ImportSource { identity: ident, provider: c.source.as_str().into(), provider_instance_id: c.instance.clone(), provider_session_id: thread.provider_session_id.clone() };
            if !seen_sessions.insert((thread.provider_instance_id.clone(), thread.provider_session_id.clone())) {
                out.push(Recent::Duplicate(source));
                continue;
            }
            out.push(Recent::Importable(Box::new(thread), source));
            continue;
        }
        let Some(snap) = read_transcript(&t.path, &ident, c.source) else {
            out.push(Recent::Skipped);
            continue;
        };
        let Some(cwd) = snap.cwd.as_deref() else {
            out.push(Recent::Skipped);
            continue;
        };
        let expanded = expand_home(project_cwd(env, cwd, t.project_roots.as_deref()).trim(), &env.home);
        if !Path::new(&expanded).is_absolute() || directory_identity(&resolve(&expanded)) != root_identity {
            out.push(Recent::Skipped);
            continue;
        }
        let fallback = t.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let tm = TranscriptMeta { source: c.source, provider_instance_id: &c.instance, fallback_session_id: &fallback, last_active_ms: t.mtime_ms };
        let Some(mut thread) = parse_records(&tm, &snap.records, &snap.fields, snap.dropped) else {
            out.push(Recent::Skipped);
            continue;
        };
        if c.source == Source::Codex {
            let saved = names.entry(c.home.clone()).or_insert_with(|| read_codex_thread_names(&c.home));
            if let Some(name) = saved.get(&thread.provider_session_id) {
                thread.title = name.clone();
            }
        }
        let source = ImportSource {
            identity: ident,
            provider: c.source.as_str().into(),
            provider_instance_id: c.instance.clone(),
            provider_session_id: thread.provider_session_id.clone(),
        };
        if !seen_sessions.insert((thread.provider_instance_id.clone(), thread.provider_session_id.clone())) {
            out.push(Recent::Duplicate(source));
            continue;
        }
        out.push(Recent::Importable(Box::new(thread), source));
    }
    Ok(out)
}

fn is_claude_session_id(id: &str) -> bool {
    regex!(r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$").is_match(id)
}

/// t3code's `DEFAULT_MODEL_BY_PROVIDER`.
fn default_model(source: Source) -> &'static str {
    match source {
        Source::Claude => "claude-opus-5-5",
        Source::Codex => "gpt-6-astra",
        Source::Pi => crate::providers::PI_DEFAULT_MODEL,
    }
}

/// The fields every imported turn item has: of no run, complete when it was said.
fn imported_item(id: String, thread_id: &str, ordinal: u64, at: &Timestamp) -> Shared {
    Shared {
        id: id.into(),
        thread_id: thread_id.into(),
        run_id: None,
        node_id: None,
        provider_thread_id: None,
        provider_turn_id: None,
        native_item_ref: None,
        parent_item_id: None,
        ordinal,
        status: TurnItemStatus::Completed,
        title: None,
        started_at: Some(at.clone()),
        completed_at: Some(at.clone()),
        updated_at: at.clone(),
    }
}

/// `messageEvents`: a message of the transcript and the run-less history item that shows it.
fn message_events(thread_id: &str, index: usize, m: &ThreadMessage) -> [EventBody; 2] {
    let suffix = format!("{index:06}");
    let message_id = MessageId::from(format!("{thread_id}:{suffix}"));
    let at = Timestamp::from(m.created_at.as_str());
    let user = m.role == "user";
    let shared = imported_item(format!("{IMPORT_EVENT_PREFIX}:turn-item:{thread_id}:{suffix}"), thread_id, index as u64 + 1, &at);
    let item = if user {
        turn_item!(UserMessage(UserMessageItem), shared, {
            created_by: Actor::User,
            creation_source: CreationSource::Server,
            message_id: message_id.clone(),
            scheduled_task_id: None,
            sender_thread_id: None,
            linked_sender: None,
            idle_compaction: None,
            workflow_script: None,
            fresh_context: None,
            input_intent: InputIntent::TurnStart,
            text: m.text.clone(),
            text_omitted: None,
            context: None,
            attachments: vec![],
            model_switch: None,
        })
    } else {
        turn_item!(AssistantMessage(AssistantMessageItem), shared, {
            phase: None,
            message_id: message_id.clone(),
            text: m.text.clone(),
            text_omitted: None,
            attachments: None,
            streaming: false,
        })
    };
    let message = ConversationMessage {
        notification: None,
        created_by: if user { Actor::User } else { Actor::Agent },
        creation_source: CreationSource::Server,
        scheduled_task_id: None,
        sender_thread_id: None,
        linked_sender: None,
        idle_compaction: None,
        workflow_script: None,
        id: message_id,
        thread_id: thread_id.into(),
        run_id: None,
        node_id: None,
        role: spelled(&m.role).unwrap_or(MessageRole::Assistant),
        text: m.text.clone(),
        context: None,
        attachments: vec![],
        streaming: false,
        created_at: at.clone(),
        updated_at: at,
        delegated_completion: None,
    };
    [EventBody::MessageUpdated(Box::new(message)), EventBody::TurnItemUpdated(item)]
}

/// Whether another thread already holds this native session.
pub(crate) fn native_thread_owned(store: &Store, driver: &str, native_id: &str) -> bool {
    store
        .db()
        .with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM thread_entities WHERE kind = 'providerThreads'
                   AND json_extract(record, '$.driver') = ?1 AND json_extract(record, '$.nativeThreadRef.nativeId') = ?2",
                params![driver, native_id],
                |r| r.get::<_, i64>(0),
            )
        })
        .unwrap_or(0)
        > 0
}

/// Adds the messages of a re-read transcript that the imported thread does not
/// have yet. `record` is the thread's own record as it was before.
/// `false` when Domovoi has run the thread (it is no longer import-only).
fn append_new_messages(store: &Store, thread_id: &str, mut record: AppThread, thread: &ParsedThread) -> Result<bool, String> {
    if !store.records::<Run>(thread_id).map_err(|e| e.to_string())?.is_empty() {
        return Ok(false);
    }
    let stored = store.records::<ConversationMessage>(thread_id).map_err(|e| e.to_string())?;
    let same = |m: &ConversationMessage, t: &ThreadMessage| m.role.as_str() == t.role && m.text == t.text;
    // New messages follow the stored ones. Times can't place them: a record
    // without its own timestamp takes the file's, which moves as it grows.
    // The last few stored messages are found in the re-read list, and what
    // comes after them is new.
    let window = stored.len().min(3);
    let tail = &stored[stored.len() - window..];
    let after = (window..=thread.messages.len())
        .rev()
        .find(|&end| tail.iter().zip(&thread.messages[end - window..end]).all(|(m, t)| same(m, t)));
    let fresh: Vec<&ThreadMessage> = match after {
        Some(end) => thread.messages[end..].iter().collect(),
        // Not found (history cut differently): whatever is not stored yet.
        None => thread.messages.iter().filter(|t| !stored.iter().any(|m| same(m, t))).collect(),
    };
    // Message ids end in a running index; new ones continue after the highest.
    let next = stored
        .iter()
        .filter_map(|m| m.id.as_str().rsplit(':').next()?.parse::<usize>().ok())
        .max()
        .map_or(0, |i| i + 1);
    if fresh.is_empty() {
        return Ok(true);
    }
    let instance = &thread.provider_instance_id;
    let origin = Origin { driver: None, provider_instance_id: Some(instance.clone()), command_id: None };
    let events = fresh.iter().enumerate()
        .flat_map(|(i, m)| message_events(thread_id, next + i, m))
        .map(|event| (event, origin.clone()))
        .collect();
    store.append_events(thread_id, events).map_err(|e| e.to_string())?;
    // The thread's time is its newest message, not the moment of the sync.
    let updated = Timestamp::from(thread.updated_at.as_str());
    if record.settled_override == Some(Some(SettledOverride::Settled)) {
        record.settled_at = Some(Some(updated.clone()));
    }
    record.updated_at = updated;
    store.append_event(thread_id, EventBody::Thread { change: ThreadChange::MetadataUpdated, thread: Box::new(record) }, &origin).map_err(|e| e.to_string())?;
    Ok(true)
}

/// Writes one imported thread. `Ok(Some(counted))`, or `Ok(None)` when
/// another thread already owns the session.
fn write_thread(store: &Store, project_id: &str, cwd: &str, thread: &ParsedThread, source: &ImportSource, runtime_mode: RuntimeMode) -> Result<Option<bool>, String> {
    if thread.source == Source::Claude && !is_claude_session_id(&thread.provider_session_id) {
        return Err(format!("Session '{}' from '{}' cannot be resumed.", thread.provider_session_id, thread.source.as_str()));
    }
    let thread_id = format!("import:{}:{}", source.provider_instance_id, source.provider_session_id);
    let driver = thread.source.as_str();
    let native = thread.native_id.as_ref().unwrap_or(&thread.provider_session_id);
    if let Some(existing) = store.app_thread(&thread_id).map_err(|e| e.to_string())? {
        if existing.project_id.as_str() != project_id {
            return Err(format!("Imported thread '{thread_id}' belongs to project '{}', not '{project_id}'.", existing.project_id));
        }
        if existing.history_origin != Some(HistoryOrigin::V1Import) {
            return Err(format!("Imported thread '{thread_id}' already contains non-imported activity."));
        }
        // Other recorded transcripts mean this one changed after the import:
        // work went on in the terminal. While Domovoi has not run the thread, the
        // new messages are added; once it has, Domovoi's own turns land in the same
        // transcript and the Domovoi copy is the live one.
        let recorded: Vec<ImportSource> =
            recorded_imports(store).map_err(|e| e.to_string())?.into_iter().filter(|(t, _, _)| *t == thread_id).map(|(_, _, s)| s).collect();
        if !recorded.is_empty() && !recorded.iter().any(|s| s.identity == source.identity) {
            if !append_new_messages(store, &thread_id, existing, thread)? {
                return Ok(Some(false));
            }
        }
        record_import(store, &thread_id, cwd, source).map_err(|e| e.to_string())?;
        return Ok(Some(true));
    }
    if native_thread_owned(store, driver, native) {
        return Ok(None);
    }
    let instance = &thread.provider_instance_id;
    let model = thread.model.clone().unwrap_or_else(|| default_model(thread.source).to_owned());
    let enc = crate::util::encode_uri_component;
    let pt_id = format!("provider-thread:provider:{}:provider-instance:{}:native-thread:{}", enc(driver), enc(instance), enc(native));
    let title = if thread.title.trim().is_empty() { "Untitled thread" } else { thread.title.as_str() };
    let (created, updated) = (Timestamp::from(thread.created_at.as_str()), Timestamp::from(thread.updated_at.as_str()));
    let app_thread = AppThread {
        created_by: Actor::System,
        creation_source: CreationSource::Server,
        id: thread_id.as_str().into(),
        project_id: project_id.into(),
        title: title.to_owned(),
        provider_instance_id: instance.as_str().into(),
        model_selection: ModelSelection { instance_id: instance.as_str().into(), model, options: None },
        runtime_mode,
        interaction_mode: InteractionMode::Default,
        branch: None,
        worktree_path: None,
        linked_pull_request: None,
        pull_requests: Some(vec![]),
        issues: None,
        branch_pull_request: None,
        active_provider_thread_id: Some(pt_id.as_str().into()),
        history_origin: Some(HistoryOrigin::V1Import),
        link_origin: None,
        delegated_from: None,
        lineage: ThreadLineage { parent_thread_id: None, relationship_to_parent: None, root_thread_id: thread_id.as_str().into(), side_chat: None },
        forked_from: None,
        created_at: created.clone(),
        updated_at: updated.clone(),
        archived_at: None,
        // An imported thread is done until someone takes it up.
        settled_override: Some(Some(SettledOverride::Settled)),
        settled_at: Some(Some(updated.clone())),
        unsettled_at: Some(None),
        snoozed_until: Some(None),
        snoozed_at: Some(None),
        last_snooze_wake_at: None,
        limit_recovery: None,
        guest_share: None,
        pinned_at: Some(None),
        auto_settle_disabled_at: None,
        pin_order_key: Some(None),
        active_order_key: None,
        tags: None,
        last_visited_at: Some(None),
        opens: None,
        title_regeneration: None,
        last_title_regeneration: None,
        rollback_request_id: None,
        rollback_failure: None,
        deleted_at: None,
    };
    let provider_thread = ProviderThread {
        id: pt_id.as_str().into(),
        driver: driver.into(),
        provider_instance_id: instance.as_str().into(),
        provider_session_id: None,
        app_thread_id: Some(thread_id.as_str().into()),
        owner_node_id: None,
        native_thread_ref: Some(ProviderRef { driver: driver.into(), native_id: Some(native.clone()), strength: NativeRefStrength::Strong, fingerprint: None, ordinal: None }),
        native_conversation_head_ref: None,
        status: ProviderThreadStatus::Idle,
        first_run_ordinal: None,
        last_run_ordinal: None,
        handoff_ids: vec![],
        forked_from: None,
        pending_background_tasks: Some(vec![]),
        context_usage: None,
        native_metadata: None,
        goal: None,
        created_at: created.clone(),
        updated_at: updated,
    };
    let mut events = vec![EventBody::Thread { change: ThreadChange::Created, thread: Box::new(app_thread) }];
    let omitted = thread.omitted_message_count;
    if omitted > 0 {
        let what = if omitted == 1 { "message was" } else { "messages were" };
        let shared = imported_item(format!("{IMPORT_EVENT_PREFIX}:history-truncated:{thread_id}"), &thread_id, 0, &created);
        events.push(EventBody::TurnItemUpdated(turn_item!(SystemNotice(SystemNoticeItem), shared, {
            message: format!("{omitted} earlier {what} not imported. This thread keeps the first user message and the most recent history. The original session transcript is unchanged."),
        })));
    }
    for (i, m) in thread.messages.iter().enumerate() {
        events.extend(message_events(&thread_id, i, m));
    }
    events.push(EventBody::ProviderThreadUpdated(Box::new(provider_thread)));
    // The provider thread's event is the provider's; the history is nobody's.
    let events = events.into_iter().map(|event| {
        let origin = Origin { driver: matches!(event, EventBody::ProviderThreadUpdated(_)).then(|| driver.to_owned()), provider_instance_id: Some(instance.clone()), command_id: None };
        (event, origin)
    }).collect();
    store.append_events(&thread_id, events).map_err(|e| e.to_string())?;
    record_import(store, &thread_id, cwd, source).map_err(|e| e.to_string())?;
    Ok(Some(true))
}

/// `importRecentAgentThreads`.
pub fn import_recent(env: &ScanEnv, store: &Store, project_id: &str, expected_root: Option<&str>) -> Result<AgentSessionImportResult, Failure> {
    let project = store
        .get_project(project_id)
        .map_err(Failure::read_projects)?
        .ok_or_else(|| Failure::ProjectNotFound { project_id: project_id.to_owned() })?;
    if expected_root.is_some_and(|r| normalize_for_comparison(r) != normalize_for_comparison(&project.workspace_root)) {
        return Err(Failure::ProjectChanged { project_id: project_id.to_owned() });
    }
    let root = &project.workspace_root;
    let completed: Vec<ImportSource> = recorded_imports(store)
        .map_err(Failure::read_projects)?
        .into_iter()
        .filter(|(_, cwd, _)| normalize_for_comparison(cwd) == normalize_for_comparison(root))
        .map(|(_, _, s)| s)
        .collect();
    let thread_id_of = |s: &ImportSource| format!("import:{}:{}", s.provider_instance_id, s.provider_session_id);
    let mut imported_threads: HashSet<String> = HashSet::new();
    let (mut imported, mut skipped) = (0usize, 0usize);
    for outcome in recent_threads(env, store, root, &completed)? {
        match outcome {
            Recent::Skipped => skipped += 1,
            Recent::AlreadyImported(s) => {
                imported_threads.insert(thread_id_of(&s));
                imported += 1;
            }
            Recent::Duplicate(s) => {
                let tid = thread_id_of(&s);
                if imported_threads.contains(&tid) {
                    let _ = record_import(store, &tid, root, &s);
                }
            }
            // Imports run in full access (t3code's default without a policy ceiling).
            Recent::Importable(thread, s) => match write_thread(store, project_id, root, &thread, &s, RuntimeMode::FullAccess) {
                Ok(Some(true)) => {
                    imported_threads.insert(thread_id_of(&s));
                    imported += 1;
                }
                Ok(Some(false)) => skipped += 1,
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(provider = thread.source.as_str(), session = %thread.provider_session_id, error = %e, "could not import an agent session");
                    skipped += 1;
                }
            },
        }
    }
    Ok(AgentSessionImportResult { imported_count: imported as u64, skipped_count: skipped as u64 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use serde_json::json;

    const CLAUDE_SESSION: &str = "0b5d6a8e-1c2f-4a3b-9d4e-5f6a7b8c9d0e";
    const CODEX_SESSION: &str = "019a1b2c-3d4e-7f60-8a1b-2c3d4e5f6a7b";

    fn meta(source: Source) -> TranscriptMeta<'static> {
        TranscriptMeta { source, provider_instance_id: source.as_str(), fallback_session_id: "fallback", last_active_ms: 1_794_583_650_000 }
    }

    fn lines(records: &[Value]) -> String {
        records.iter().map(|r| format!("{r}\n")).collect()
    }

    fn claude_transcript(cwd: &str) -> String {
        lines(&[
            json!({ "type": "summary", "summary": "ignored" }),
            json!({ "type": "user", "cwd": cwd, "sessionId": CLAUDE_SESSION, "timestamp": "2026-11-13T10:00:00Z",
                "message": { "role": "user", "content": "Where is add defined?\nSecond line" } }),
            json!({ "type": "assistant", "isSidechain": true, "message": { "role": "assistant", "content": [{ "type": "text", "text": "sidechain" }] } }),
            json!({ "type": "assistant", "timestamp": "2026-11-13T10:00:05Z",
                "message": { "role": "assistant", "model": "claude-haiku-5-5", "content": [
                    { "type": "thinking", "thinking": "hidden" }, { "type": "text", "text": "In calc.py." }, { "type": "tool_use", "id": "t1" }] } }),
            json!({ "type": "assistant", "message": { "role": "assistant", "model": "<synthetic>", "content": [{ "type": "text", "text": "API error" }] } }),
            json!({ "type": "ai-title", "aiTitle": "Find add" }),
            json!({ "type": "custom-title", "customTitle": "  My rename  " }),
        ])
    }

    fn codex_rollout(cwd: &str, subagent: bool) -> String {
        let source = if subagent { json!({ "subagent": { "parent": "x" } }) } else { json!("cli") };
        lines(&[
            json!({ "timestamp": "2026-11-13T09:00:00Z", "type": "session_meta", "payload": { "id": CODEX_SESSION, "cwd": cwd, "source": source } }),
            json!({ "type": "turn_context", "payload": { "cwd": cwd, "model": "gpt-5-codex" } }),
            json!({ "timestamp": "2026-11-13T09:00:01Z", "type": "response_item", "payload": { "type": "message", "role": "user",
                "content": [{ "type": "input_text", "text": "<environment_context>\n  <cwd>/x</cwd>\n</environment_context>" }] } }),
            json!({ "timestamp": "2026-11-13T09:00:02Z", "type": "response_item", "payload": { "type": "message", "role": "user",
                "content": [{ "type": "input_text", "text": "Fix the tests" }], "internal_chat_message_metadata_passthrough": { "turn_id": "turn-1" } } }),
            json!({ "timestamp": "2026-11-13T09:00:02Z", "type": "event_msg", "payload": { "type": "user_message", "message": "Fix the tests" } }),
            json!({ "type": "response_item", "payload": { "type": "reasoning", "summary": [], "content": null } }),
            json!({ "timestamp": "2026-11-13T09:00:09Z", "type": "response_item", "payload": { "type": "message", "role": "assistant",
                "content": [{ "type": "output_text", "text": "Done." }] } }),
        ])
    }

    #[test]
    fn parses_claude_transcripts() {
        let t = parse_transcript(&meta(Source::Claude), &claude_transcript("/w")).unwrap();
        assert_eq!(t.provider_session_id, CLAUDE_SESSION);
        assert_eq!(t.title, "My rename", "a rename beats the AI title");
        assert_eq!(t.model.as_deref(), Some("claude-haiku-5-5"), "<synthetic> is not a model");
        let texts: Vec<(&str, &str)> = t.messages.iter().map(|m| (m.role.as_str(), m.text.as_str())).collect();
        assert_eq!(texts, [("user", "Where is add defined?\nSecond line"), ("assistant", "In calc.py."), ("assistant", "API error")]);
        assert_eq!(t.messages[0].created_at, "2026-11-13T10:00:00.000Z");
        assert_eq!(t.created_at, "2026-11-13T10:00:00.000Z");
        assert_eq!(t.updated_at, "2026-11-13T15:27:30.000Z", "the file's mtime");
        assert_eq!(t.omitted_message_count, 0);

        // Without titles the first user line is the title; no user message, no thread.
        let plain = lines(&[json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "text", "text": "  Hello there  " }] } })]);
        let t = parse_transcript(&meta(Source::Claude), &plain).unwrap();
        assert_eq!((t.title.as_str(), t.provider_session_id.as_str()), ("Hello there", "fallback"));
        let only_assistant = lines(&[json!({ "type": "assistant", "message": { "content": "hi" } })]);
        assert!(parse_transcript(&meta(Source::Claude), &only_assistant).is_none());
    }

    #[test]
    fn parses_codex_rollouts_without_duplicate_prompts() {
        let t = parse_transcript(&meta(Source::Codex), &codex_rollout("/w", false)).unwrap();
        assert_eq!(t.provider_session_id, CODEX_SESSION);
        assert_eq!(t.model.as_deref(), Some("gpt-5-codex"));
        let texts: Vec<&str> = t.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts.len(), 3, "the response copy of the prompt is dropped: {texts:?}");
        assert_eq!(texts[1..], ["Fix the tests", "Done."]);
        // The setup preamble is kept as a message but skipped for the title.
        assert!(texts[0].starts_with("<environment_context>"));
        assert_eq!(t.title, "Fix the tests");
        assert_eq!(derive_title(Source::Codex, "# AGENTS.md instructions for /x\n\n## My request for Codex:\nDo it"), Some("Do it".into()));
        assert_eq!(derive_title(Source::Codex, "# AGENTS.md instructions for /x\nbody\n## My request for Codex: later"), Some("body".into()));
        // A rollout without session metadata cannot be resumed.
        let headless = lines(&[json!({ "type": "event_msg", "payload": { "type": "user_message", "message": "hi" } })]);
        assert!(parse_transcript(&meta(Source::Codex), &headless).is_none());
    }

    #[test]
    fn long_claude_sessions_keep_the_first_prompt_and_latest_history() {
        let mut records = vec![json!({ "type": "user", "sessionId": CLAUDE_SESSION, "message": { "content": "first" } })];
        records.extend((0..250).map(|i| json!({ "type": "assistant", "message": { "content": format!("reply {i}") } })));
        let dir = std::env::temp_dir().join(format!("domovoi-claude-window-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{CLAUDE_SESSION}.jsonl"));
        std::fs::write(&path, lines(&records)).unwrap();
        let ident = identity(&path, &std::fs::metadata(&path).unwrap());
        let snap = read_transcript(&path, &ident, Source::Claude).unwrap();
        let t = parse_records(&meta(Source::Claude), &snap.records, &snap.fields, snap.dropped).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(t.messages.len(), MAX_IMPORTED_MESSAGES);
        assert_eq!(t.messages[0].text, "first");
        assert_eq!(t.messages[1].text, "reply 51");
        assert_eq!(t.messages.last().unwrap().text, "reply 249");
        assert_eq!(t.omitted_message_count, 51);
        // Same result as parsing the whole file.
        assert_eq!(parse_transcript(&meta(Source::Claude), &lines(&records)).unwrap().messages, t.messages);
    }

    #[test]
    fn git_config_and_remotes() {
        let config = "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = git@github.com:other/fork.git\n[remote \"origin\"]\n\turl = \"https://github.com/Acme/Calc.git\" # main\n";
        assert_eq!(origin_url_from_git_config(config).as_deref(), Some("https://github.com/Acme/Calc.git"));
        assert!(!is_bare_git_config(config) && is_bare_git_config("[core]\n\tbare\n"));
        assert_eq!(remote_key("https://github.com/Acme/Calc.git"), "github.com/acme/calc");
        assert_eq!(remote_key("git@gitlab.com:group/sub/repo.git"), "gitlab.com/group/sub/repo");
        assert_eq!(repository_path("https://github.com/Acme/Calc.git").as_deref(), Some("Acme/Calc"));
        assert_eq!(repository_path("git@github.com:acme/calc").as_deref(), Some("acme/calc"));
        assert_eq!(repository_path("/srv/repos/calc"), None);
    }

    struct Fixture {
        root: PathBuf,
        env: ScanEnv,
        project: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn fixture(name: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!("domovoi-agent-sessions-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let project = root.join("work").join("calc");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        std::fs::write(project.join(".git/config"), "[remote \"origin\"]\n\turl = git@github.com:acme/calc.git\n").unwrap();
        let cwd = project.to_string_lossy().into_owned();
        let claude_dir = home.join(".claude/projects/-work-calc");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(claude_dir.join(format!("{CLAUDE_SESSION}.jsonl")), claude_transcript(&cwd)).unwrap();
        // Sessions in the home directory itself, or in app data, are never a project.
        std::fs::write(claude_dir.join("other.jsonl"), claude_transcript(&home.to_string_lossy())).unwrap();
        let app_data = home.join("Library/Application Support/Claude/scratch-workspaces/s1");
        std::fs::create_dir_all(&app_data).unwrap();
        std::fs::write(claude_dir.join("scratch.jsonl"), claude_transcript(&app_data.to_string_lossy())).unwrap();
        let day = home.join(".codex/sessions/2026/11/13");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(day.join(format!("rollout-2026-11-13T09-00-00-{CODEX_SESSION}.jsonl")), codex_rollout(&cwd, false)).unwrap();
        std::fs::write(day.join("rollout-2026-11-13T09-30-00-019a1b2c-3d4e-7f60-8a1b-ffffffffffff.jsonl"), codex_rollout(&cwd, true)).unwrap();
        std::fs::write(home.join(".codex/session_index.jsonl"), format!("{}\n", json!({ "id": CODEX_SESSION, "thread_name": "Codex rename" }))).unwrap();
        let env = ScanEnv {
            homes: vec![(Source::Claude, "claudeAgent".into(), home.join(".claude")), (Source::Codex, "codex".into(), home.join(".codex"))],
            base_dir: root.join("domovoi-home"),
            worktrees_dir: root.join("domovoi-home/worktrees"),
            temp_dir: root.join("tmp"),
            home,
        };
        Fixture { root, env, project }
    }

    fn project_shell(id: &str, root: &Path) -> crate::wire::shell::ProjectShell {
        crate::wire::shell::ProjectShell {
            id: id.into(), title: "calc".into(), has_custom_title: false, workspace_root: root.to_string_lossy().into_owned(),
            repository_identity: None, default_model_selection: None, default_thread_env_mode: None, auto_pull: false,
            favicon_path: None, project_icon: None, scripts: vec![], pinned_at: None, archived_at: None,
            created_at: "t".into(), updated_at: "t".into(),
        }
    }

    #[test]
    fn failures_are_written_as_the_tagged_errors_of_t3code() {
        let scan = Failure::read_projects("no such table: agent_session_imports");
        assert_eq!(json!(scan), json!({ "_tag": "AgentSessionScanError", "operation": "read-projects", "cause": "no such table: agent_session_imports" }));
        assert_eq!(scan.to_string(), "Failed to scan agent sessions during read-projects.");
        let missing = Failure::ProjectNotFound { project_id: "p1".into() };
        assert_eq!(json!(missing), json!({ "_tag": "AgentSessionImportProjectNotFoundError", "projectId": "p1" }));
        assert_eq!(missing.to_string(), "Project 'p1' does not exist.");
        let moved = Failure::ProjectChanged { project_id: "p1".into() };
        assert_eq!(json!(moved), json!({ "_tag": "AgentSessionImportProjectChangedError", "projectId": "p1" }));
        assert_eq!(moved.to_string(), "Project 'p1' changed directories. Scan for projects again before importing history.");
        // A log or a report gets the failure as the client does, keys in this order.
        assert_eq!(scan.written(), r#"{"_tag":"AgentSessionScanError","operation":"read-projects","cause":"no such table: agent_session_imports"}"#);
        assert_eq!(missing.written(), r#"{"_tag":"AgentSessionImportProjectNotFoundError","projectId":"p1"}"#);
    }

    // The settings name Unix paths.
    #[cfg(unix)]
    #[test]
    fn the_scan_reads_settings_leniently() {
        let of = |settings: Value| crate::settings::Settings::from_document(settings.as_object().cloned().unwrap_or_default());
        let all = |settings: Value| -> Vec<(Source, PathBuf)> { ScanEnv::new(&of(settings), Path::new("/domovoi")).homes.into_iter().map(|(source, _, home)| (source, home)).collect() };
        let homes = |settings: Value| -> Vec<(Source, PathBuf)> { all(settings).into_iter().filter(|(source, _)| *source != Source::Pi).collect() };
        let default = homes(json!({}));
        assert_eq!(all(json!({})).iter().map(|(source, _)| *source).collect::<Vec<_>>(), [Source::Claude, Source::Codex, Source::Pi]);
        assert_eq!(all(json!({ "providerInstances": { "pi": { "config": { "homePath": "/s/pi" } } } })).last(), Some(&(Source::Pi, PathBuf::from("/s/pi"))));
        assert!(all(json!({ "providerInstances": { "pi": { "enabled": false } } })).iter().all(|(source, _)| *source != Source::Pi));
        // Settings that are no object, or hold other types than expected, are no settings.
        for odd in [json!(null), json!([1]), json!({ "providerInstances": 7 }), json!({ "providerInstances": { "codex": "on", "claudeAgent": { "enabled": "yes", "config": [] } } })] {
            assert_eq!(homes(odd.clone()), default, "{odd}");
        }
        let set = homes(json!({ "providerInstances": {
            "claudeAgent": { "config": { "enabled": false } },
            "codex": { "enabled": true, "config": { "enabled": false, "homePath": " /elsewhere/codex " } },
        } }));
        assert_eq!(set, [(Source::Codex, PathBuf::from("/elsewhere/codex"))], "the instance's own switch wins over its config's");
        assert_eq!(homes(json!({ "providerInstances": { "codex": { "enabled": false } } })).len(), 1);
        let env = ScanEnv::new(&of(json!({ "worktreesDirectory": " /wt " })), Path::new("/domovoi"));
        assert_eq!((env.worktrees_dir, ScanEnv::new(&of(json!({ "worktreesDirectory": 3 })), Path::new("/domovoi")).worktrees_dir), (PathBuf::from("/wt"), PathBuf::from("/domovoi/worktrees")));
    }

    #[test]
    fn scans_projects_from_both_providers() {
        let f = fixture("scan");
        let store = Store::new(Db::open_in_memory().unwrap());
        let r = json!(scan(&f.env, &store).unwrap());
        let candidates = r["candidates"].as_array().unwrap();
        assert_eq!(candidates.len(), 1, "{r}");
        let c = &candidates[0];
        assert_eq!(c["path"], json!(f.project.to_string_lossy()));
        assert_eq!((c["title"].as_str(), c["threadCount"].as_u64()), (Some("calc"), Some(2)), "the subagent rollout is not counted");
        assert_eq!(c["sources"], json!(["claudeAgent", "codex"]));
        assert_eq!(c["git"], json!({ "remoteKey": "github.com/acme/calc", "repository": "acme/calc" }));
        assert_eq!(c["alreadyImported"], false);
        assert!(c.get("projectId").is_none() && r.get("truncated").is_none());
    }

    #[test]
    fn imports_recent_sessions_once() {
        let f = fixture("import");
        let store = Store::new(Db::open_in_memory().unwrap());
        store.create_project(project_shell("p1", &f.project), None).unwrap();
        let err = import_recent(&f.env, &store, "nope", None).unwrap_err();
        assert_eq!(err, Failure::ProjectNotFound { project_id: "nope".into() });
        let err = import_recent(&f.env, &store, "p1", Some("/elsewhere")).unwrap_err();
        assert_eq!(err, Failure::ProjectChanged { project_id: "p1".into() });

        let root = f.project.to_string_lossy().into_owned();
        let r = import_recent(&f.env, &store, "p1", Some(&format!("{root}/"))).unwrap();
        assert_eq!(json!(r), json!({ "importedCount": 2, "skippedCount": 0 }));
        let claude = store.load_thread(&format!("import:claudeAgent:{CLAUDE_SESSION}")).unwrap().unwrap();
        assert_eq!((claude.record.title.as_str(), claude.record.history_origin.as_ref()), ("My rename", Some(&HistoryOrigin::V1Import)));
        assert_eq!(json!(claude.record.model_selection), json!({ "instanceId": "claudeAgent", "model": "claude-haiku-5-5" }));
        assert_eq!((claude.messages.len(), claude.turn_items.len()), (3, 3));
        let pt = claude.provider_threads.first().unwrap();
        assert_eq!(pt.native_thread_ref.as_ref().and_then(|native| native.native_id.as_deref()), Some(CLAUDE_SESSION));
        assert_eq!(claude.record.active_provider_thread_id.as_ref(), Some(&pt.id));
        assert!(crate::orchestration::projection::thread_projection(&store, &format!("import:claudeAgent:{CLAUDE_SESSION}")).unwrap().is_some());
        let codex = store.app_thread(&format!("import:codex:{CODEX_SESSION}")).unwrap().unwrap();
        assert_eq!(codex.title, "Codex rename", "Codex's saved name wins");

        // A second import finds both already imported and writes nothing new.
        let before = store.latest_sequence().unwrap();
        assert_eq!(import_recent(&f.env, &store, "p1", None).unwrap(), AgentSessionImportResult { imported_count: 2, skipped_count: 0 });
        assert_eq!(store.latest_sequence().unwrap(), before);
        let scanned = json!(scan(&f.env, &store).unwrap());
        assert_eq!((scanned["candidates"][0]["alreadyImported"].clone(), scanned["candidates"][0]["projectId"].clone()), (json!(true), json!("p1")));
    }

    /// A pi session: `u1 a1`, then an abandoned branch `u2`, and the active
    /// one `u3 a3` from `a1`, named by its last `session_info`.
    fn pi_session(cwd: &str) -> String {
        let m = |id: &str, parent: Option<&str>, role: &str, text: &str, at: &str| {
            json!({ "type": "message", "id": id, "parentId": parent, "timestamp": at, "message": { "role": role, "content": [{ "type": "text", "text": text }], "provider": "zai", "model": "glm-5.3" } })
        };
        lines(&[
            json!({ "type": "session", "version": 3, "id": PI_SESSION, "timestamp": "2026-11-13T09:00:00.000Z", "cwd": cwd }),
            json!({ "type": "model_change", "id": "e0", "parentId": null, "timestamp": "2026-11-13T09:00:00.000Z", "provider": "zai", "modelId": "glm-5.3" }),
            m("u1", Some("e0"), "user", "Add a divide button", "2026-11-13T09:00:01.000Z"),
            m("a1", Some("u1"), "assistant", "Added.", "2026-11-13T09:00:02.000Z"),
            m("u2", Some("a1"), "user", "Abandoned idea", "2026-11-13T09:00:03.000Z"),
            m("u3", Some("a1"), "user", "Now handle division by zero", "2026-11-13T09:00:04.000Z"),
            m("a3", Some("u3"), "assistant", "Handled.", "2026-11-13T09:00:05.000Z"),
            json!({ "type": "session_info", "id": "n1", "parentId": "a3", "timestamp": "2026-11-13T09:00:06.000Z", "name": "Calculator fixes" }),
        ])
    }

    const PI_SESSION: &str = "01a1efd6-a147-743b-a32e-01e227f1cd2d";

    #[test]
    fn imports_pi_sessions_on_their_active_branch() {
        let mut f = fixture("pi");
        let sessions = f.env.home.join(".pi/agent/sessions");
        let dir = sessions.join("--work-calc--");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("2026-11-13T09-00-00-000Z_{PI_SESSION}.jsonl"));
        std::fs::write(&file, pi_session(&f.project.to_string_lossy())).unwrap();
        f.env.homes = vec![(Source::Pi, "pi".into(), sessions)];
        let store = Store::new(Db::open_in_memory().unwrap());
        let scanned = json!(scan(&f.env, &store).unwrap());
        assert_eq!(scanned["candidates"][0]["sources"], json!(["pi"]), "{scanned}");
        store.create_project(project_shell("p1", &f.project), None).unwrap();
        let r = import_recent(&f.env, &store, "p1", None).unwrap();
        assert_eq!(json!(r), json!({ "importedCount": 1, "skippedCount": 0 }));
        let thread = store.load_thread(&format!("import:pi:{PI_SESSION}")).unwrap().unwrap();
        assert_eq!(thread.record.title, "Calculator fixes");
        let said: Vec<&str> = thread.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(said, ["Add a divide button", "Added.", "Now handle division by zero", "Handled."], "only the active branch");
        assert_eq!(json!(thread.record.model_selection), json!({ "instanceId": "pi", "model": "zai/glm-5.3" }));
        let pt = thread.provider_threads.first().unwrap();
        assert_eq!(pt.driver.as_str(), "pi");
        assert_eq!(pt.native_thread_ref.as_ref().and_then(|n| n.native_id.as_deref()), Some(file.to_string_lossy().as_ref()), "pi resumes its session file");
        assert_eq!(import_recent(&f.env, &store, "p1", None).unwrap().imported_count, 1, "already imported, not again");
    }

    fn append_lines(path: &Path, values: &[Value]) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(lines(values).as_bytes()).unwrap();
    }

    fn turn(at_user: &str, at_reply: &str, text: &str) -> Vec<Value> {
        vec![
            json!({ "type": "user", "sessionId": CLAUDE_SESSION, "timestamp": at_user, "message": { "role": "user", "content": text } }),
            json!({ "type": "assistant", "timestamp": at_reply,
                "message": { "role": "assistant", "model": "claude-haiku-5-5", "content": [{ "type": "text", "text": format!("Re: {text}") }] } }),
        ]
    }

    #[test]
    fn terminal_messages_reach_an_import_until_domovoi_runs_it() {
        let f = fixture("follow");
        let store = Store::new(Db::open_in_memory().unwrap());
        store.create_project(project_shell("p1", &f.project), None).unwrap();
        import_recent(&f.env, &store, "p1", None).unwrap();
        let tid = format!("import:claudeAgent:{CLAUDE_SESSION}");
        let count = |store: &Store| store.records::<ConversationMessage>(&tid).unwrap().len();
        assert_eq!(count(&store), 3);

        let transcript = f.env.home.join(format!(".claude/projects/-work-calc/{CLAUDE_SESSION}.jsonl"));
        append_lines(&transcript, &turn("2026-11-13T11:00:00Z", "2026-11-13T11:00:04Z", "And mul?"));
        assert_eq!(import_recent(&f.env, &store, "p1", None).unwrap().imported_count, 2);
        assert_eq!(count(&store), 5);
        let messages = store.records::<ConversationMessage>(&tid).unwrap();
        let last = messages.last().unwrap();
        assert_eq!((last.text.as_str(), last.id.as_str()), ("Re: And mul?", format!("{tid}:000004").as_str()));
        let thread = store.app_thread(&tid).unwrap().unwrap();
        assert_eq!(Some(Some(thread.updated_at)), thread.settled_at, "the thread's time is its newest message");

        // Read again with nothing new: no duplicates.
        let before = store.latest_sequence().unwrap();
        import_recent(&f.env, &store, "p1", None).unwrap();
        assert_eq!((count(&store), store.latest_sequence().unwrap()), (5, before));

        // Once Domovoi has run the thread, the terminal no longer feeds it.
        crate::test_records::append(&store, &tid, "run.created", crate::test_records::run(&tid, "r1", "completed"), &Origin::default()).unwrap();
        append_lines(&transcript, &turn("2026-11-13T12:00:00Z", "2026-11-13T12:00:04Z", "From Domovoi"));
        import_recent(&f.env, &store, "p1", None).unwrap();
        assert_eq!(count(&store), 5);
    }

    #[test]
    fn automatic_import_adds_recent_folders_once() {
        let f = fixture("auto");
        let store = Store::new(Db::open_in_memory().unwrap());
        let r = crate::agent_sync::auto_import(&f.env, &store);
        assert_eq!((r.projects, r.sessions, r.error.as_deref()), (1, 2, None));
        let project = store.list_projects().unwrap().pop().unwrap();
        assert_eq!((project.title.as_str(), project.workspace_root.as_str()), ("calc", f.project.to_str().unwrap()));
        assert_eq!(crate::agent_sync::auto_import(&f.env, &store), crate::agent_sync::Report::default(), "nothing new the second time");

        // A removed project stays removed.
        store.delete_project(&project.id, None).unwrap();
        assert_eq!(crate::agent_sync::auto_import(&f.env, &store).projects, 0);
        assert!(store.list_projects().unwrap().is_empty());
    }

    #[test]
    fn automatic_import_skips_quiet_folders_and_fills_your_own_projects() {
        let f = fixture("quiet");
        let store = Store::new(Db::open_in_memory().unwrap());
        // A project you added yourself gets its sessions.
        store.create_project(project_shell("mine", &f.project), None).unwrap();
        let r = crate::agent_sync::auto_import(&f.env, &store);
        assert_eq!((r.projects, r.sessions), (0, 2));

        // Folders quiet for more than 14 days are left to the picker (and past
        // 30 days the import takes nothing from them).
        let g = fixture("quiet2");
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(40 * 24 * 3600);
        for dir in [g.env.home.join(".claude/projects/-work-calc"), g.env.home.join(".codex/sessions/2026/11/13")] {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                std::fs::File::options().write(true).open(e.path()).unwrap().set_modified(old).unwrap();
            }
        }
        let fresh = Store::new(Db::open_in_memory().unwrap());
        assert_eq!(crate::agent_sync::auto_import(&g.env, &fresh), crate::agent_sync::Report::default());
        assert_eq!(scan(&g.env, &fresh).unwrap().candidates[0].thread_count, 0, "older than import takes");
    }

    #[test]
    fn sync_imports_new_terminal_sessions() {
        let f = fixture("sync");
        let store = Store::new(Db::open_in_memory().unwrap());
        crate::agent_sync::auto_import(&f.env, &store);
        let mut seen = crate::agent_sync::Seen::default();
        assert_eq!(crate::agent_sync::sync(&f.env, &store, &mut seen), 0, "the first pass learns the files");
        assert_eq!(crate::agent_sync::sync(&f.env, &store, &mut seen), 0, "nothing changed");

        let root = f.project.to_string_lossy().into_owned();
        let folder: String = root.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        let dir = f.env.home.join(".claude/projects").join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        let session = "1c2d3e4f-5a6b-4c7d-8e9f-0a1b2c3d4e5f";
        std::fs::write(
            dir.join(format!("{session}.jsonl")),
            lines(&[json!({ "type": "user", "cwd": root, "sessionId": session, "timestamp": "2026-11-14T09:00:00Z",
                "message": { "role": "user", "content": "New from the terminal" } })]),
        )
        .unwrap();
        assert_eq!(crate::agent_sync::sync(&f.env, &store, &mut seen), 1);
        let t = store.app_thread(&format!("import:claudeAgent:{session}")).unwrap().unwrap();
        assert_eq!(t.title, "New from the terminal");
    }

    #[test]
    fn harness_messages_are_not_yours() {
        assert_eq!(without_reminders("<system-reminder>ctx</system-reminder>\nFix the test"), "Fix the test");
        assert_eq!(without_reminders("a <system-reminder>x</system-reminder> b <system-reminder>y"), "a  b");
        let cwd = "/w/calc";
        let text = lines(&[
            json!({ "type": "user", "cwd": cwd, "sessionId": CLAUDE_SESSION, "timestamp": "2026-11-13T10:00:00Z",
                "origin": { "kind": "human" }, "message": { "role": "user", "content": "<system-reminder>ctx</system-reminder>Add mul" } }),
            json!({ "type": "user", "timestamp": "2026-11-13T10:01:00Z", "origin": { "kind": "task-notification" },
                "message": { "role": "user", "content": "<task-notification>done</task-notification>" } }),
            json!({ "type": "user", "timestamp": "2026-11-13T10:02:00Z", "message": { "role": "user", "content": "<system-reminder>only</system-reminder>" } }),
        ]);
        let thread = parse_transcript(&meta(Source::Claude), &text).unwrap();
        let texts: Vec<&str> = thread.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["Add mul"]);
    }
}


