//! Local dev-server discovery for the preview panel, after t3code's
//! `PortDiscovery` (`preview/PortScanner.ts`): `subscribeDiscoveredLocalServers`
//! and `preview.stopLocalServer`.
//!
//! Like t3code, a listener is listed only when this server can attribute it:
//! its process descends from a running Domovoi terminal, or its environment carries
//! `T3CODE_THREAD_ID` (set for agent tool shells). Configured URLs (loopback
//! only) are listed when an HTTP request to them answers with HTML or a
//! redirect. Scans repeat every 3 seconds and a subscriber hears only changes.
//! Stopping signals the listener's process group (SIGTERM, then SIGKILL after
//! 3 seconds), and only for a listed server whose group holds neither this
//! server, its ancestors, nor its direct children. Windows has no process
//! groups: there the listener and what it started are ended, under the same
//! rule.
//!
//! Limits:
//! - listeners are read with `lsof` (macOS and Linux) or the IP Helper table
//!   (Windows); t3code's `/proc/net/tcp` walk for hosts without lsof is not
//!   ported;
//! - configured `https:` URLs are not probed (no TLS client here), so they are
//!   listed only through an owned listener on the same port;
//! - Domovoi's Claude adapter does not yet export `T3CODE_THREAD_ID` to its tool
//!   shells, so agent attribution applies to processes started by terminals or
//!   by other tools that set it.

use super::Never;
use super::admin_failure::{Failure, StopLocalServerReason};
use crate::state::{AppState, ConnCtx};
use domovoi_rpc::Router;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::broadcast;

const POLL_INTERVAL: Duration = Duration::from_secs(3);
#[cfg(not(windows))]
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const WEB_PROBE_TIMEOUT: Duration = Duration::from_secs(1);
const WEB_PROBE_CACHE_TTL: Duration = Duration::from_secs(15);
#[cfg(unix)]
const STOP_GRACE: Duration = Duration::from_secs(3);
#[cfg(unix)]
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(250);
const PREVIEW_URL_MAX_LENGTH: usize = 2_048;
const CONFIGURED_LOCAL_SERVER_URLS_MAX_ITEMS: usize = 32;
const AGENT_THREAD_ENV: &str = crate::terminal::THREAD_ENV;
/// What it was called: a process started before the update carries only this.
const OLD_AGENT_THREAD_ENV: &str = crate::terminal::OLD_THREAD_ENV;
#[cfg(any(not(windows), test))]
const LSOF_LOCAL_HOST_TOKENS: &[&str] = &["localhost", "127.0.0.1", "0.0.0.0", "::1", "*", "[::]", "[::1]"];

/// Wakes subscribers right after a stop, so the server leaves now rather than on the next poll.
static RESCAN: LazyLock<broadcast::Sender<()>> = LazyLock::new(|| broadcast::channel(4).0);

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "0.0.0.0" | "::1" | "[::1]")
}

fn local_server_key(host: &str, port: u16) -> String {
    if is_loopback_host(host) { format!("loopback:{port}") } else { format!("{}:{port}", host.to_lowercase()) }
}

fn url_port(url: &url::Url) -> u16 {
    url.port_or_known_default().unwrap_or(80)
}

fn web_probe_cache_key(raw: &str) -> String {
    url::Url::parse(raw)
        .map(|mut u| {
            u.set_fragment(None);
            u.to_string()
        })
        .unwrap_or_else(|_| raw.to_owned())
}

/// `normalizeConfiguredUrls`: loopback http(s) URLs, `0.0.0.0` read as localhost, deduplicated.
pub fn normalize_configured_urls(urls: &[String]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for raw in urls.iter().take(CONFIGURED_LOCAL_SERVER_URLS_MAX_ITEMS) {
        if raw.encode_utf16().count() > PREVIEW_URL_MAX_LENGTH {
            continue;
        }
        let Ok(mut url) = url::Url::parse(raw) else { continue };
        if !matches!(url.scheme(), "http" | "https") || !url.host_str().is_some_and(is_loopback_host) {
            continue;
        }
        if url.host_str() == Some("0.0.0.0") {
            let _ = url.set_host(Some("localhost"));
        }
        let href = url.to_string();
        if href.len() <= PREVIEW_URL_MAX_LENGTH && !out.contains(&href) {
            out.push(href);
        }
    }
    out
}

// ---- process listings ----

#[derive(Debug, Clone, PartialEq)]
pub struct Listener {
    pub pid: Option<u32>,
    pub process_name: Option<String>,
    pub port: u16,
}

#[cfg(any(not(windows), test))]
fn port_from_lsof_name(name: &str) -> Option<u16> {
    let first = name.split(' ').next()?.trim();
    let (host, port) = first.rsplit_once(':')?;
    if !LSOF_LOCAL_HOST_TOKENS.contains(&host) {
        return None;
    }
    port.parse::<u16>().ok().filter(|p| *p > 0)
}

#[cfg(any(not(windows), test))]
/// Loopback or wildcard listeners in `lsof -F pcn` output, in output order.
pub fn parse_lsof(raw: &str) -> Vec<Listener> {
    let (mut pid, mut name) = (None, None);
    let mut out = vec![];
    for line in raw.lines() {
        let Some(tag) = line.chars().next() else { continue };
        let value = &line[1..];
        match tag {
            'p' => {
                pid = value.parse::<u32>().ok().filter(|p| *p > 0);
                name = None;
            }
            'c' => name = Some(value.trim().to_owned()).filter(|n| !n.is_empty()),
            'n' => {
                if let Some(port) = port_from_lsof_name(value) {
                    out.push(Listener { pid, process_name: name.clone(), port });
                }
            }
            _ => {}
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcessRow {
    pub pid: i32,
    pub ppid: i32,
    pub pgid: i32,
}

#[cfg(any(not(windows), test))]
/// `ps -A -o pid=,ppid=,pgid=`.
pub fn parse_process_table(raw: &str) -> HashMap<i32, ProcessRow> {
    raw.lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace().map(str::parse::<i32>);
            let (pid, ppid, pgid) = (it.next()?.ok()?, it.next()?.ok()?, it.next()?.ok()?);
            Some((pid, ProcessRow { pid, ppid, pgid }))
        })
        .collect()
}

/// The process group stopping `pid` signals, or `None` when that group also
/// holds this server, one of its ancestors, or a direct child of it (agent
/// CLIs and terminal shells).
#[cfg(any(unix, test))]
pub fn stoppable_process_group(table: &HashMap<i32, ProcessRow>, pid: i32, server_pid: i32) -> Option<i32> {
    let group = table.get(&pid)?.pgid;
    if group <= 1 {
        return None;
    }
    let mut guarded = std::collections::HashSet::from([server_pid]);
    let mut parent = table.get(&server_pid).map(|r| r.ppid);
    while let Some(p) = parent.filter(|p| *p > 1 && !guarded.contains(p)) {
        guarded.insert(p);
        parent = table.get(&p).map(|r| r.ppid);
    }
    let shared = table.values().any(|r| r.pgid == group && (guarded.contains(&r.pid) || r.ppid == server_pid));
    (!shared).then_some(group)
}

#[cfg(any(not(windows), test))]
/// macOS `ps -E -ww -o pid=,command=`: argv followed by the launch environment.
pub fn parse_ps_environment_threads(raw: &str) -> HashMap<u32, Option<String>> {
    let prefixes = [format!("{AGENT_THREAD_ENV}="), format!("{OLD_AGENT_THREAD_ENV}=")];
    raw.lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, rest) = line.split_once(' ')?;
            let pid = pid.parse::<u32>().ok()?;
            let thread = prefixes.iter().find_map(|prefix| rest.split_whitespace().find_map(|w| w.strip_prefix(prefix.as_str()))).map(str::to_owned);
            Some((pid, thread.filter(|t| !t.is_empty())))
        })
        .collect()
}

/// `/proc/<pid>/environ`, NUL-separated.
pub fn parse_proc_environ_thread(raw: &str) -> Option<String> {
    [AGENT_THREAD_ENV, OLD_AGENT_THREAD_ENV]
        .iter()
        .find_map(|name| raw.split('\0').find_map(|e| e.strip_prefix(&format!("{name}="))))
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

#[cfg(not(windows))]
async fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let child = crate::process::command(cmd).args(args).kill_on_drop(true).output();
    let out = tokio::time::timeout(COMMAND_TIMEOUT, child).await.ok()?.ok()?;
    // lsof exits 1 when nothing listens; its output is still the answer.
    (out.status.success() || cmd == "lsof").then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn process_table() -> Option<HashMap<i32, ProcessRow>> {
    #[cfg(windows)]
    return tokio::task::spawn_blocking(windows::process_table).await.ok().flatten();
    #[cfg(not(windows))]
    run("ps", &["-A", "-o", "pid=,ppid=,pgid="]).await.map(|s| parse_process_table(&s))
}

/// What stopping `pid` ends on Windows: it and its descendants, children
/// first. `None` when `pid` is this server, one of its ancestors, or a direct
/// child of it (agent CLIs and terminal shells), as [`stoppable_process_group`]
/// rules on Unix.
#[cfg(any(windows, test))]
pub fn stoppable_tree(table: &HashMap<i32, ProcessRow>, pid: i32, server_pid: i32) -> Option<Vec<i32>> {
    let row = table.get(&pid)?;
    if pid == server_pid || row.ppid == server_pid {
        return None;
    }
    let mut seen = std::collections::HashSet::from([server_pid]);
    let mut ancestor = table.get(&server_pid).map(|r| r.ppid);
    while let Some(p) = ancestor.filter(|p| *p > 0 && seen.insert(*p)) {
        if p == pid {
            return None;
        }
        ancestor = table.get(&p).map(|r| r.ppid);
    }
    let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
    for r in table.values() {
        children.entry(r.ppid).or_default().push(r.pid);
    }
    // Depth first, then reversed: every child comes before its parent.
    let (mut order, mut stack, mut visited) = (vec![], vec![pid], std::collections::HashSet::new());
    while let Some(p) = stack.pop() {
        if p == server_pid || !visited.insert(p) {
            continue;
        }
        order.push(p);
        stack.extend(children.get(&p).into_iter().flatten().copied());
    }
    order.reverse();
    Some(order)
}

/// The process table, listeners and launch environments, from sysinfo and
/// the IP Helper API (Windows has no `ps` or `lsof`).
#[cfg(windows)]
mod windows {
    use super::{Listener, ProcessRow};
    use std::collections::HashMap;
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

    /// Every process, each its own group.
    pub fn process_table() -> Option<HashMap<i32, ProcessRow>> {
        let mut sys = System::new().ok()?;
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
        let rows = sys.processes().values().map(|p| {
            let pid = p.pid().as_u32() as i32;
            (pid, ProcessRow { pid, ppid: p.parent().map_or(0, |x| x.as_u32() as i32), pgid: pid })
        });
        Some(rows.collect())
    }

    /// Loopback and wildcard TCP listeners, with their processes' names.
    pub fn listeners() -> Vec<Listener> {
        let ports = crate::win::local_tcp_listeners();
        let pids: Vec<Pid> = ports.iter().map(|(_, pid)| Pid::from_u32(*pid)).collect();
        let names: HashMap<u32, String> = System::new()
            .map(|mut sys| {
                sys.refresh_processes_specifics(ProcessesToUpdate::Some(&pids), true, ProcessRefreshKind::nothing());
                sys.processes().values().map(|p| (p.pid().as_u32(), p.name().to_string_lossy().into_owned())).collect()
            })
            .unwrap_or_default();
        ports
            .into_iter()
            .filter(|(port, _)| *port > 0)
            .map(|(port, pid)| Listener { pid: (pid > 0).then_some(pid), process_name: names.get(&pid).cloned(), port })
            .collect()
    }

    /// The agent thread in each process's launch environment.
    pub fn agent_threads(pids: &[u32]) -> HashMap<u32, Option<String>> {
        let Ok(mut sys) = System::new() else { return HashMap::new() };
        let wanted: Vec<Pid> = pids.iter().copied().map(Pid::from_u32).collect();
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&wanted), true, ProcessRefreshKind::nothing().with_environ(UpdateKind::Always));
        pids.iter()
            .map(|pid| {
                let env = sys.process(Pid::from_u32(*pid)).map(|p| p.environ().iter().map(|e| e.to_string_lossy()).collect::<Vec<_>>().join("\0")).unwrap_or_default();
                (*pid, super::parse_proc_environ_thread(&env))
            })
            .collect()
    }
}

async fn agent_threads(pids: &[u32]) -> HashMap<u32, Option<String>> {
    if pids.is_empty() {
        return HashMap::new();
    }
    #[cfg(windows)]
    {
        let pids = pids.to_vec();
        tokio::task::spawn_blocking(move || windows::agent_threads(&pids)).await.unwrap_or_default()
    }
    #[cfg(not(windows))]
    {
        if cfg!(target_os = "linux") {
            let mut out = HashMap::new();
            for pid in pids {
                let raw = tokio::fs::read(format!("/proc/{pid}/environ")).await.unwrap_or_default();
                out.insert(*pid, parse_proc_environ_thread(&String::from_utf8_lossy(&raw)));
            }
            return out;
        }
        let list = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        run("ps", &["-E", "-ww", "-o", "pid=,command=", "-p", &list]).await.map(|s| parse_ps_environment_threads(&s)).unwrap_or_default()
    }
}

// ---- scanning ----

#[derive(Debug, Clone, PartialEq)]
pub enum Owner {
    Terminal { thread_id: String, terminal_id: String },
    Agent { thread_id: String },
}

/// The terminal whose shell a local server descends from.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOwner {
    thread_id: String,
    terminal_id: String,
}

/// The thread whose agent started a local server.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentOwner {
    thread_id: String,
}

/// A local server the preview panel can open (`DiscoveredLocalServer`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalServer {
    host: String,
    port: u16,
    url: String,
    process_name: Option<String>,
    pid: Option<u32>,
    terminal: Option<TerminalOwner>,
    /// `null` for a listener no agent started; left out for a configured URL
    /// no listener was found behind.
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<Option<AgentOwner>>,
}

impl LocalServer {
    fn new(host: &str, port: u16, url: &str, name: Option<&str>, pid: Option<u32>, owner: Option<&Owner>) -> Self {
        let (terminal, agent) = match owner {
            Some(Owner::Terminal { thread_id, terminal_id }) => (Some(TerminalOwner { thread_id: thread_id.clone(), terminal_id: terminal_id.clone() }), None),
            Some(Owner::Agent { thread_id }) => (None, Some(AgentOwner { thread_id: thread_id.clone() })),
            None => (None, None),
        };
        LocalServer { host: host.to_owned(), port, url: url.to_owned(), process_name: name.map(str::to_owned), pid, terminal, agent: Some(agent) }
    }

    /// Whether a terminal or an agent of this server started it.
    fn is_owned(&self) -> bool {
        self.terminal.is_some() || matches!(self.agent, Some(Some(_)))
    }

    fn key(&self) -> String {
        local_server_key(&self.host, self.port)
    }
}

/// What `subscribeDiscoveredLocalServers` streams (`DiscoveredLocalServerList`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalServerList {
    servers: Vec<LocalServer>,
    scanned_at: String,
    /// Always `true`: this server probes configured URLs.
    configured_url_probing: bool,
}

impl LocalServerList {
    fn now(servers: &[LocalServer]) -> Self {
        LocalServerList { servers: servers.to_vec(), scanned_at: crate::time_fmt::iso_now(), configured_url_probing: true }
    }
}

/// `parseLsofOutput`: one server per port; a port also held by an unowned process is unowned.
pub fn discovered_servers(listeners: &[Listener], owners: &HashMap<u32, Owner>) -> Vec<LocalServer> {
    let mut seen: BTreeMap<u16, LocalServer> = BTreeMap::new();
    for l in listeners {
        let owner = l.pid.and_then(|p| owners.get(&p));
        if let Some(existing) = seen.get_mut(&l.port) {
            if owner.is_none() {
                existing.terminal = None;
                existing.agent = Some(None);
            }
            continue;
        }
        let url = format!("http://localhost:{}", l.port);
        seen.insert(l.port, LocalServer::new("localhost", l.port, &url, l.process_name.as_deref(), l.pid, owner));
    }
    seen.into_values().collect()
}

/// `projectWebProbeSnapshot`: configured URLs that answered as web pages first,
/// then owned listeners (opened at the caller's configured URL for their port).
pub fn project(discovered: &[LocalServer], configured: &HashMap<String, LocalServer>, configured_urls: &[String]) -> Vec<LocalServer> {
    let mut visible: BTreeMap<String, LocalServer> = BTreeMap::new();
    let mut first_configured: HashMap<String, String> = HashMap::new();
    for raw in configured_urls {
        let Ok(url) = url::Url::parse(raw) else { continue };
        let key = local_server_key(url.host_str().unwrap_or_default(), url_port(&url));
        first_configured.entry(key.clone()).or_insert_with(|| raw.clone());
        if visible.contains_key(&key) {
            continue;
        }
        if let Some(server) = configured.get(&web_probe_cache_key(raw)) {
            visible.insert(key, LocalServer { url: raw.clone(), ..server.clone() });
        }
    }
    for server in discovered.iter().filter(|s| s.is_owned()) {
        let key = server.key();
        if visible.contains_key(&key) {
            continue;
        }
        let mut s = server.clone();
        if let Some(url) = first_configured.get(&key) {
            s.url = url.clone();
        }
        visible.insert(key, s);
    }
    let mut out: Vec<LocalServer> = visible.into_values().collect();
    out.sort_by_key(|s| s.port);
    out
}

/// `probeWebUrl`: a navigation redirect, or a 2xx HTML answer (not 204/205).
pub fn is_web_response(head: &str) -> bool {
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next().and_then(|l| l.split_whitespace().nth(1)).and_then(|s| s.parse().ok()).unwrap_or(0);
    let header = |name: &str| {
        head.split("\r\n").skip(1).find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
        })
    };
    if matches!(status, 301 | 302 | 303 | 307 | 308) && header("location").is_some_and(|l| !l.is_empty()) {
        return true;
    }
    if !(200..300).contains(&status) || matches!(status, 204 | 205) {
        return false;
    }
    let content_type = header("content-type").map(|c| c.split(';').next().unwrap_or("").trim().to_lowercase());
    matches!(content_type.as_deref(), Some("text/html" | "application/xhtml+xml"))
}

async fn probe_web_url(raw: &str) -> bool {
    let Ok(url) = url::Url::parse(raw) else { return false };
    if url.scheme() != "http" {
        return false;
    }
    let host = url.host_str().unwrap_or("localhost").trim_matches(|c| c == '[' || c == ']').to_owned();
    let port = url_port(&url);
    let path = match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_owned(),
    };
    let authority = url[url::Position::BeforeHost..url::Position::AfterPort].to_owned();
    let attempt = async move {
        let mut stream = tokio::net::TcpStream::connect((host.as_str(), port)).await.ok()?;
        let req = format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\nAccept: text/html\r\nConnection: close\r\n\r\n");
        stream.write_all(req.as_bytes()).await.ok()?;
        let mut buf = Vec::with_capacity(4096);
        let mut chunk = [0u8; 4096];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16 * 1024 {
            let n = stream.read(&mut chunk).await.ok()?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        Some(is_web_response(&String::from_utf8_lossy(&buf)))
    };
    tokio::time::timeout(WEB_PROBE_TIMEOUT, attempt).await.ok().flatten().unwrap_or(false)
}

/// Per-subscriber caches (t3code keeps them while any client retains the scanner).
#[derive(Default)]
pub struct Scanner {
    /// Agent thread read from each listener pid's launch environment.
    agent_threads: HashMap<u32, Option<String>>,
    /// Configured URL probes: resource key -> (pid, is web, expiry).
    web_probes: HashMap<String, (Option<u32>, bool, Instant)>,
}

impl Scanner {
    async fn owners(&mut self, state: &AppState, listener_pids: &[u32]) -> HashMap<u32, Owner> {
        let mut owners = HashMap::new();
        let terminals = state.terminals.running_pids();
        if !terminals.is_empty() {
            if let Some(table) = process_table().await {
                let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
                for row in table.values() {
                    children.entry(row.ppid).or_default().push(row.pid);
                }
                for (thread_id, terminal_id, shell) in terminals {
                    let mut stack = vec![shell as i32];
                    while let Some(pid) = stack.pop() {
                        let owner = Owner::Terminal { thread_id: thread_id.clone(), terminal_id: terminal_id.clone() };
                        owners.insert(pid as u32, owner);
                        stack.extend(children.get(&pid).into_iter().flatten().copied());
                    }
                }
            }
        }
        // A process keeps its launch environment, so each listening pid is read once.
        let unread: Vec<u32> =
            listener_pids.iter().copied().filter(|p| !owners.contains_key(p) && !self.agent_threads.contains_key(p)).collect();
        let read = agent_threads(&unread).await;
        self.agent_threads.retain(|pid, _| listener_pids.contains(pid));
        self.agent_threads.extend(read);
        for (pid, thread) in &self.agent_threads {
            if let (Some(thread_id), false) = (thread, owners.contains_key(pid)) {
                owners.insert(*pid, Owner::Agent { thread_id: thread_id.clone() });
            }
        }
        owners
    }

    /// One scan, projected for `configured_urls` (already normalized).
    pub async fn scan(&mut self, state: &AppState, configured_urls: &[String]) -> Vec<LocalServer> {
        #[cfg(windows)]
        let listeners = tokio::task::spawn_blocking(windows::listeners).await.unwrap_or_default();
        #[cfg(not(windows))]
        let listeners = {
            let lsof = if cfg!(unix) { run("lsof", &["-iTCP", "-sTCP:LISTEN", "-P", "-n", "-F", "pcn"]).await } else { None };
            lsof.as_deref().map(parse_lsof).unwrap_or_default()
        };
        let mut pids: Vec<u32> = listeners.iter().filter_map(|l| l.pid).collect();
        pids.sort_unstable();
        pids.dedup();
        let owners = self.owners(state, &pids).await;
        let discovered = discovered_servers(&listeners, &owners);
        let configured = self.probe_configured(&discovered, configured_urls).await;
        project(&discovered, &configured, configured_urls)
    }

    async fn probe_configured(&mut self, discovered: &[LocalServer], configured_urls: &[String]) -> HashMap<String, LocalServer> {
        let now = Instant::now();
        self.web_probes.retain(|_, (_, _, expiry)| *expiry > now);
        let mut configured = HashMap::new();
        for raw in configured_urls {
            let Ok(url) = url::Url::parse(raw) else { continue };
            let host = url.host_str().unwrap_or_default().to_owned();
            let port = url_port(&url);
            let key = local_server_key(&host, port);
            let resource = web_probe_cache_key(raw);
            if configured.contains_key(&resource) {
                continue;
            }
            let server = discovered
                .iter()
                .find(|s| s.key() == key)
                .cloned()
                // No listener was found behind the URL: no process, and nothing said of an agent.
                .unwrap_or_else(|| LocalServer { agent: None, ..LocalServer::new(&host, port, raw, None, None, None) });
            let pid = server.pid;
            let is_web = match self.web_probes.get(&resource) {
                Some((cached_pid, is_web, _)) if *cached_pid == pid => *is_web,
                _ => {
                    let is_web = probe_web_url(raw).await;
                    self.web_probes.insert(resource.clone(), (pid, is_web, Instant::now() + WEB_PROBE_CACHE_TTL));
                    is_web
                }
            };
            if is_web {
                configured.insert(resource, LocalServer { url: raw.clone(), ..server });
            }
        }
        configured
    }
}

// ---- RPCs ----

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct DiscoverInput {
    configured_urls: Option<Vec<String>>,
}

fn discover(ctx: Arc<ConnCtx>, input: DiscoverInput) -> BoxStream<'static, Result<LocalServerList, Never>> {
    let configured = normalize_configured_urls(&input.configured_urls.unwrap_or_default());
    async_stream::stream! {
        let mut scanner = Scanner::default();
        let mut rescan = RESCAN.subscribe();
        let mut last = scanner.scan(&ctx.state, &configured).await;
        yield Ok(LocalServerList::now(&last));
        loop {
            tokio::select! {
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
                _ = rescan.recv() => {}
            }
            let next = scanner.scan(&ctx.state, &configured).await;
            if next != last {
                last = next;
                yield Ok(LocalServerList::now(&last));
            }
        }
    }
    .boxed()
}

#[derive(Deserialize)]
struct StopInput {
    pid: i64,
    port: i64,
}

#[cfg(unix)]
fn signal_group(group: i32, signal: i32) -> Result<bool, Failure> {
    // SAFETY: killpg only sends a signal; a negative return is reported through errno.
    if unsafe { libc::killpg(group, signal) } == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) { Ok(false) } else { Err(Failure::not_stopped(StopLocalServerReason::SignalFailed, Some(err.to_string()))) }
}

async fn stop(state: &AppState, input: StopInput) -> Result<(), Failure> {
    if !cfg!(unix) && !cfg!(windows) {
        return Err(Failure::not_stopped(StopLocalServerReason::Unsupported, None));
    }
    // The pid comes from a client, so only a server this scan attributes to a thread is signaled.
    let listed = Scanner::default().scan(state, &[]).await.iter().any(|s| {
        s.pid.map(i64::from) == Some(input.pid) && i64::from(s.port) == input.port && s.is_owned()
    });
    if !listed {
        return Err(Failure::not_stopped(StopLocalServerReason::NotFound, None));
    }
    stop_listed(input.pid as i32).await?;
    let _ = RESCAN.send(());
    Ok(())
}

#[cfg(unix)]
async fn stop_listed(pid: i32) -> Result<(), Failure> {
    let table = process_table().await.ok_or_else(|| Failure::not_stopped(StopLocalServerReason::SignalFailed, Some("ps failed".into())))?;
    // It exited after the scan, so there is nothing left to stop.
    if !table.contains_key(&pid) {
        return Ok(());
    }
    let group = stoppable_process_group(&table, pid, std::process::id() as i32).ok_or_else(|| Failure::not_stopped(StopLocalServerReason::SharedGroup, None))?;
    if !signal_group(group, libc::SIGTERM)? {
        return Ok(());
    }
    // While an original member is still in the group, it has not been reused.
    let members: Vec<i32> = table.values().filter(|r| r.pgid == group).map(|r| r.pid).collect();
    let deadline = Instant::now() + STOP_GRACE;
    while Instant::now() < deadline {
        tokio::time::sleep(STOP_POLL_INTERVAL).await;
        let running = process_table().await.is_some_and(|t| members.iter().any(|p| t.get(p).is_some_and(|r| r.pgid == group)));
        if !running {
            return Ok(());
        }
    }
    signal_group(group, libc::SIGKILL).map(|_| ())
}

#[cfg(windows)]
async fn stop_listed(pid: i32) -> Result<(), Failure> {
    let table = process_table().await.ok_or_else(|| Failure::not_stopped(StopLocalServerReason::SignalFailed, Some("the process table could not be read".into())))?;
    // It exited after the scan, so there is nothing left to stop.
    if !table.contains_key(&pid) {
        return Ok(());
    }
    let tree = stoppable_tree(&table, pid, std::process::id() as i32).ok_or_else(|| Failure::not_stopped(StopLocalServerReason::SharedGroup, None))?;
    // Windows has no polite stop for another console process: they end now.
    for p in tree {
        if let Err(e) = crate::win::terminate_process(p as u32)
            && p == pid
            && crate::win::process_alive(p as u32)
        {
            return Err(Failure::not_stopped(StopLocalServerReason::SignalFailed, Some(e.to_string())));
        }
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
async fn stop_listed(_pid: i32) -> Result<(), Failure> {
    Err(Failure::not_stopped(StopLocalServerReason::Unsupported, None))
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.stream("subscribeDiscoveredLocalServers", |ctx, input: Option<DiscoverInput>| discover(ctx, input.unwrap_or_default()))
        .unary("preview.stopLocalServer", |ctx: Arc<ConnCtx>, input: StopInput| async move { stop(&ctx.state, input).await })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn configured_urls_keep_loopback_web_urls() {
        let urls = ["http://0.0.0.0:3000", "https://localhost:8443/x#y", "http://example.com", "ftp://localhost", "http://0.0.0.0:3000"]
            .map(String::from);
        assert_eq!(normalize_configured_urls(&urls), vec!["http://localhost:3000/", "https://localhost:8443/x#y"]);
    }

    const LSOF: &str = "p100\ncnode\nf21\nn*:5173\nf22\nn127.0.0.1:5173\np200\ncpostgres\nn[::1]:5432\nn10.0.0.2:9000\np300\ncvite\nnlocalhost:3000 (LISTEN)\n";

    #[test]
    fn parses_lsof_listeners() {
        let l = parse_lsof(LSOF);
        let ports: Vec<(Option<u32>, u16)> = l.iter().map(|l| (l.pid, l.port)).collect();
        assert_eq!(ports, vec![(Some(100), 5173), (Some(100), 5173), (Some(200), 5432), (Some(300), 3000)]);
        assert_eq!(l[0].process_name.as_deref(), Some("node"));
    }

    #[test]
    fn lists_only_attributed_listeners() {
        let owners = HashMap::from([
            (100, Owner::Terminal { thread_id: "t".into(), terminal_id: "term-1".into() }),
            (300, Owner::Agent { thread_id: "t2".into() }),
        ]);
        let found = discovered_servers(&parse_lsof(LSOF), &owners);
        let projected = project(&found, &HashMap::new(), &["http://localhost:3000/app".into()]);
        // As the client is sent them: an agent's listener at its configured URL, then a terminal's.
        assert_eq!(
            json!(projected),
            json!([
                { "host": "localhost", "port": 3000, "url": "http://localhost:3000/app", "processName": "vite", "pid": 300, "terminal": null, "agent": { "threadId": "t2" } },
                { "host": "localhost", "port": 5173, "url": "http://localhost:5173", "processName": "node", "pid": 100,
                  "terminal": { "threadId": "t", "terminalId": "term-1" }, "agent": null },
            ])
        );
        // A port also held by an unattributed process is not listed.
        let mut shared = parse_lsof(LSOF);
        shared.push(Listener { pid: Some(999), process_name: None, port: 5173 });
        let found = discovered_servers(&shared, &owners);
        assert!(project(&found, &HashMap::new(), &[]).iter().all(|s| s.port != 5173));
        let nobodys = found.iter().find(|s| s.port == 5173).unwrap();
        assert_eq!(json!(nobodys)["agent"], json!(null), "said to be no agent's, not left out");
    }

    #[test]
    fn configured_probes_win_over_listeners() {
        let raw = "http://127.0.0.1:4000/".to_owned();
        let probed = LocalServer { agent: None, ..LocalServer::new("127.0.0.1", 4000, &raw, None, None, None) };
        let configured = HashMap::from([(web_probe_cache_key(&raw), probed)]);
        let out = project(&[], &configured, std::slice::from_ref(&raw));
        // A configured URL with no listener found behind it says nothing of an agent.
        assert_eq!(json!(out), json!([{ "host": "127.0.0.1", "port": 4000, "url": raw, "processName": null, "pid": null, "terminal": null }]));
        let list = LocalServerList::now(&out);
        assert_eq!(json!(list), json!({ "servers": out, "scannedAt": list.scanned_at, "configuredUrlProbing": true }));
    }

    #[test]
    fn web_responses() {
        assert!(is_web_response("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\r\n"));
        assert!(is_web_response("HTTP/1.1 302 Found\r\nLocation: /login\r\n\r\n"));
        assert!(!is_web_response("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n"));
        assert!(!is_web_response("HTTP/1.1 204 No Content\r\nContent-Type: text/html\r\n\r\n"));
        assert!(!is_web_response(""));
    }

    #[test]
    fn environment_threads() {
        let ps = "  41 node server.js TERM=xterm T3CODE_THREAD_ID=thread-9 PATH=/bin\n 42 python -m http.server PATH=/bin\n";
        let m = parse_ps_environment_threads(ps);
        assert_eq!(m[&41].as_deref(), Some("thread-9"));
        assert_eq!(m[&42], None);
        assert_eq!(parse_proc_environ_thread("A=1\0T3CODE_THREAD_ID=t-1\0B=2"), Some("t-1".into()));
    }

    #[test]
    fn process_groups_never_include_this_server_or_its_children() {
        // 1 launchd > 10 shell (pgid 10) > 20 domovoi-server (pgid 10) > 30 terminal shell (pgid 30) > 40 vite (pgid 40)
        let table = parse_process_table("1 0 1\n10 1 10\n20 10 10\n30 20 30\n40 30 40\n41 40 40\n");
        assert_eq!(stoppable_process_group(&table, 40, 20), Some(40));
        assert_eq!(stoppable_process_group(&table, 41, 20), Some(40));
        // The terminal shell is a direct child of the server.
        assert_eq!(stoppable_process_group(&table, 30, 20), None);
        // The server's own group.
        assert_eq!(stoppable_process_group(&table, 10, 20), None);
        assert_eq!(stoppable_process_group(&table, 999, 20), None);
    }

    #[test]
    fn a_windows_stop_ends_the_listener_and_what_it_started_but_never_the_server() {
        // explorer 10 > domovoi-server 20 > pwsh 30 > npm 40 > node 41, 42
        let table = parse_process_table("10 1 10\n20 10 20\n30 20 30\n40 30 40\n41 40 41\n42 40 42\n");
        let mut tree = stoppable_tree(&table, 40, 20).expect("the dev server's own tree");
        assert_eq!(tree.pop(), Some(40), "the listener ends last, after its children");
        tree.sort_unstable();
        assert_eq!(tree, [41, 42]);
        assert_eq!(stoppable_tree(&table, 30, 20), None, "a terminal shell, the server's own child");
        assert_eq!(stoppable_tree(&table, 20, 20), None, "the server");
        assert_eq!(stoppable_tree(&table, 10, 20), None, "what started the server");
        assert_eq!(stoppable_tree(&table, 999, 20), None);
    }
}
