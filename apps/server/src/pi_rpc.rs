//! The pi coding agent's `pi --mode rpc` protocol: JSONL commands on stdin,
//! responses (`{type: "response", id, command, success, data | error}`) and
//! events on stdout. After t3code `provider-pi` (`rpc.ts`, `mcpInjection.ts`).
//!
//! Records are split on LF only (a trailing CR is dropped): pi's own
//! framing, which a line reader that also splits on U+2028/U+2029 would break.
//! stdin stays open until the process is stopped: on EOF pi quits without
//! running its SIGTERM handler.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "pi's RPC commands, responses and events")]

use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{Mutex, mpsc, oneshot};

pub const PI_TOOL: crate::providers::Tool = crate::providers::Tool { program: "pi", label: "Pi", env: "DOMOVOI_PI_BINARY" };
/// Extra arguments for every `pi --mode rpc` Domovoi starts (t3code's `launchArgs`).
pub const PI_ARGS_ENV: &str = "DOMOVOI_PI_ARGS";
/// What the bundled extension reads (see `adapters/pi_extension.ts`).
pub const RUNTIME_MODE_ENV: &str = "DOMOVOI_PI_RUNTIME_MODE";
pub const MCP_URL_ENV: &str = "DOMOVOI_MCP_URL";
pub const MCP_TOKEN_ENV: &str = "DOMOVOI_MCP_TOKEN";

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
/// `switch_session`, `new_session`, `fork`: extensions may run their hooks.
pub const LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(60);
/// `get_session_stats`, `abort`, probes that must not hold a turn up.
pub const SHORT_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(unix)]
const TERMINATION_GRACE: Duration = Duration::from_secs(1);
/// A record longer than this is dropped. MCP image results are carried twice
/// (model content and the tool result), so leave room for several images.
const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;

const RESERVED: &[&str] = &[
    "--continue", "-c", "--export", "--fork", "--help", "-h", "--list-models", "--mode", "--no-session", "--print", "-p", "--resume", "-r", "--session",
    "--session-id", "--version", "-v",
];
const WITH_VALUES: &[&str] = &[
    "--api-key", "--append-system-prompt", "--exclude-tools", "-xt", "--extension", "-e", "--model", "--models", "--name", "-n", "--prompt-template",
    "--provider", "--session-dir", "--skill", "--system-prompt", "--theme", "--thinking", "--tools", "-t", "--tui-mode", "--use-theme",
];
const WITHOUT_VALUES: &[&str] = &[
    "--approve", "-a", "--no-approve", "-na", "--no-builtin-tools", "-nbt", "--no-context-files", "-nc", "--no-extensions", "-ne", "--no-prompt-templates",
    "-np", "--no-skills", "-ns", "--no-themes", "--no-tools", "-nt", "--offline", "--verbose",
];

/// Splits a command line as a shell would: whitespace separates, single
/// quotes are literal, double quotes and backslashes escape.
pub fn tokenize(line: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur: Option<String> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if let Some(t) = cur.take() {
                    out.push(t);
                }
            }
            '\'' => {
                let t = cur.get_or_insert_with(String::new);
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    t.push(c);
                }
            }
            '"' => {
                let t = cur.get_or_insert_with(String::new);
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => t.extend(chars.next()),
                        c => t.push(c),
                    }
                }
            }
            '\\' => cur.get_or_insert_with(String::new).extend(chars.next()),
            c => cur.get_or_insert_with(String::new).push(c),
        }
    }
    out.extend(cur);
    out
}

/// The user's extra launch arguments, checked: Domovoi owns the session and
/// mode flags, and a prompt can't be passed this way. pi reads an
/// `--option=value` it doesn't know as an extension flag, so only its own
/// options are split at `=`.
pub fn resolve_launch_args(line: &str) -> Result<Vec<String>, String> {
    let args: Vec<String> = tokenize(line)
        .into_iter()
        .flat_map(|a| match a.split_once('=') {
            Some((opt, value)) if !opt.is_empty() && WITH_VALUES.contains(&opt) => vec![opt.to_owned(), value.to_owned()],
            _ => vec![a],
        })
        .collect();
    let (mut provider, mut model) = (false, false);
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if let Some(r) = RESERVED.iter().find(|r| a == **r || (r.starts_with("--") && a.starts_with(&format!("{r}=")))) {
            return Err(format!("Pi launch argument '{r}' is controlled by Domovoi and cannot be overridden."));
        }
        if a == "--" {
            return Err("Pi launch arguments cannot include positional prompts.".into());
        }
        if WITH_VALUES.contains(&a) {
            if i + 1 >= args.len() {
                return Err(format!("Pi launch argument '{a}' requires a value."));
            }
            provider |= a == "--provider";
            model |= a == "--model";
            i += 2;
            continue;
        }
        if WITHOUT_VALUES.contains(&a) || (a.starts_with("--") && a.contains('=')) {
            i += 1;
            continue;
        }
        if a.starts_with("--") {
            // An extension flag may take one value that follows it.
            let takes_value = args.get(i + 1).is_some_and(|n| !n.starts_with('-') && !n.starts_with('@'));
            i += if takes_value { 2 } else { 1 };
            continue;
        }
        if a.starts_with('-') {
            return Err(format!("Pi launch argument '{a}' is not supported by Domovoi."));
        }
        return Err(format!("Pi launch arguments cannot include positional prompt '{a}'."));
    }
    if provider && !model {
        return Err("Pi launch argument '--provider' requires '--model'.".into());
    }
    Ok(args)
}

/// [`PI_ARGS_ENV`], checked.
pub fn user_launch_args() -> Result<Vec<String>, String> {
    resolve_launch_args(&std::env::var(PI_ARGS_ENV).unwrap_or_default())
}

/// A command running pi. An npm install of pi is a Node script
/// (`#!/usr/bin/env node`), and the server's PATH (launchd's, or a version
/// manager's folder gone since) may have no `node`: the `node` of pi's own
/// npm prefix (`<prefix>/bin/node`, beside `<prefix>/lib/node_modules`) goes first.
pub fn command(exe: &Path) -> tokio::process::Command {
    let mut cmd = crate::process::command(exe);
    if let Some(bin) = exe.ancestors().skip(1).take(8).map(|a| a.join("bin")).find(|b| b.join("node").is_file()) {
        let path = std::env::var_os("PATH").unwrap_or_default();
        if let Ok(joined) = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&path))) {
            cmd.env("PATH", joined);
        }
    }
    cmd
}

/// How one `pi --mode rpc` is started.
#[derive(Debug, Clone, Default)]
pub struct Launch {
    /// `--no-session`: nothing is written (probes, text generation).
    pub ephemeral: bool,
    /// `--offline`: no startup network work (probes, forks, text generation).
    pub short_lived: bool,
    pub no_extensions: bool,
    pub no_tools: bool,
    /// `--fork <session file>`: a new session copied from this one.
    pub fork: Option<String>,
    /// The bundled extension (`--extension`).
    pub extension: Option<PathBuf>,
    pub user_args: Vec<String>,
}

impl Launch {
    /// The argv after `pi`. The disabling flags follow the user's arguments
    /// so those can't turn tools back on.
    pub fn args(&self) -> Vec<String> {
        let mut a = vec!["--mode".to_owned(), "rpc".to_owned()];
        if self.ephemeral {
            a.push("--no-session".into());
        }
        if let Some(f) = &self.fork {
            a.extend(["--fork".to_owned(), f.clone()]);
        }
        a.extend(self.user_args.iter().cloned());
        if self.short_lived {
            a.push("--offline".into());
        }
        if self.no_extensions {
            a.push("--no-extensions".into());
        }
        if self.no_tools {
            a.push("--no-tools".into());
        }
        if let Some(e) = &self.extension
            && !self.no_extensions
            && !self.user_args.windows(2).any(|w| matches!(w[0].as_str(), "--extension" | "-e") && Path::new(&w[1]) == e.as_path())
        {
            a.extend(["--extension".to_owned(), e.to_string_lossy().into_owned()]);
        }
        a
    }
}

/// Splits complete records off `buf` (consumed): LF-terminated, a trailing CR
/// dropped, blank and non-JSON lines skipped. A record over the limit is
/// dropped as it arrives (`dropping` carries that across chunks); returns the
/// records and the lengths of those dropped.
pub fn take_records(buf: &mut Vec<u8>, dropping: &mut bool) -> (Vec<Value>, Vec<usize>) {
    let (mut records, mut dropped) = (vec![], vec![]);
    let mut start = 0;
    while let Some(nl) = memchr::memchr(b'\n', &buf[start..]) {
        let line = &buf[start..start + nl];
        if *dropping {
            *dropping = false;
        } else {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if !line.is_empty()
                && let Ok(v) = serde_json::from_slice::<Value>(line)
            {
                records.push(v);
            }
        }
        start += nl + 1;
    }
    buf.drain(..start);
    if buf.len() > MAX_RECORD_BYTES {
        if !*dropping {
            dropped.push(buf.len());
        }
        buf.clear();
        *dropping = true;
    }
    (records, dropped)
}

/// pi provider names and model ids may both contain `/`, so a slug is split
/// through the `{provider, id}` pairs pi reported when it can be.
pub fn resolve_model_slug(slug: &str, reported: &[Value]) -> Option<(String, String)> {
    if let Some(m) = reported.iter().find(|m| m["provider"].as_str().zip(m["id"].as_str()).is_some_and(|(p, id)| format!("{p}/{id}") == slug)) {
        return Some((m["provider"].as_str()?.to_owned(), m["id"].as_str()?.to_owned()));
    }
    let (provider, id) = slug.split_once('/')?;
    (!provider.is_empty() && !id.is_empty()).then(|| (provider.to_owned(), id.to_owned()))
}

#[derive(Debug)]
pub enum PiEvent {
    /// Any record that isn't the response to a request of ours.
    Record(Value),
    /// stdout closed; the exit code if the process ended soon after.
    Exited(Option<i32>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum RpcError {
    /// pi answered `success: false`.
    Refused(String),
    Timeout(String),
    /// The process is gone (or its stdin is).
    Gone(String),
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(m) | Self::Timeout(m) | Self::Gone(m) => f.write_str(m),
        }
    }
}

impl From<RpcError> for String {
    fn from(e: RpcError) -> Self {
        e.to_string()
    }
}

/// Environment for a pi process: what the bundled extension reads, and
/// nothing of the same names inherited from Domovoi's own environment.
pub fn environment(runtime_mode: &str, mcp: Option<(String, String)>) -> Vec<(String, Option<String>)> {
    let mut env: Vec<(String, Option<String>)> =
        std::env::vars().map(|(k, _)| k).filter(|k| k.starts_with("DOMOVOI_PI_") || k.starts_with("DOMOVOI_MCP_")).map(|k| (k, None)).collect();
    let mode = if runtime_mode == "auto" { "approval-required" } else { runtime_mode };
    env.push((RUNTIME_MODE_ENV.into(), Some(mode.into())));
    if let Some((url, token)) = mcp {
        env.push((MCP_URL_ENV.into(), Some(url)));
        env.push((MCP_TOKEN_ENV.into(), Some(token.strip_prefix("Bearer ").unwrap_or(&token).to_owned())));
        // What the tools are for, for the extension to add to pi's system prompt.
        env.push(("DOMOVOI_MCP_INSTRUCTIONS".into(), Some(crate::agent_instructions::system_prompt())));
    }
    env
}

pub struct PiRpc {
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    /// Pi and what it starts (Windows; Unix signals the process group).
    tree: crate::process::Tree,
    pending: Mutex<HashMap<String, oneshot::Sender<Value>>>,
    next: AtomicU64,
    exited: AtomicBool,
}

type Log = dyn Fn(&str, &Value) + Send + Sync;

impl PiRpc {
    /// Starts `exe` with `launch` in `cwd`, in a process group of its own.
    pub fn spawn(
        exe: &Path,
        launch: &Launch,
        cwd: &str,
        env: &[(String, Option<String>)],
        log: impl Fn(&str, &Value) + Send + Sync + 'static,
    ) -> std::io::Result<(Arc<Self>, mpsc::UnboundedReceiver<PiEvent>)> {
        let mut cmd = command(exe);
        cmd.args(launch.args()).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        for (k, v) in env {
            match v {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
            };
        }
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let mut stdout = child.stdout.take().expect("piped stdout");
        #[expect(clippy::disallowed_methods, reason = "the reader also delivers the answers a turn waits for: made to wait for room here, it could never read the answer that frees the turn")]
        let (tx, rx) = mpsc::unbounded_channel();
        let tree = crate::process::Tree::of(child.id());
        let rpc = Arc::new(PiRpc { stdin: Mutex::new(stdin), child: Mutex::new(child), tree, pending: Mutex::default(), next: AtomicU64::new(1), exited: AtomicBool::new(false) });
        let weak: Weak<PiRpc> = Arc::downgrade(&rpc);
        let log: Box<Log> = Box::new(log);
        tokio::spawn(async move {
            let (mut buf, mut dropping) = (Vec::with_capacity(64 * 1024), false);
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                let n = match stdout.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                buf.extend_from_slice(&chunk[..n]);
                let (records, dropped) = take_records(&mut buf, &mut dropping);
                for len in dropped {
                    tracing::warn!(len, "dropped an oversized pi record");
                }
                for record in records {
                    log("incoming", &record);
                    let waiter = match (record["type"].as_str(), record["id"].as_str(), weak.upgrade()) {
                        (Some("response"), Some(id), Some(rpc)) => rpc.pending.lock().await.remove(id),
                        _ => None,
                    };
                    match waiter {
                        Some(w) => {
                            let _ = w.send(record);
                        }
                        None => {
                            let _ = tx.send(PiEvent::Record(record));
                        }
                    }
                }
            }
            let code = match weak.upgrade() {
                Some(rpc) => {
                    rpc.exited.store(true, Ordering::SeqCst);
                    // Requests still waiting will never be answered.
                    rpc.pending.lock().await.clear();
                    let mut child = rpc.child.lock().await;
                    tokio::time::timeout(Duration::from_millis(250), child.wait()).await.ok().and_then(Result::ok).and_then(|s| s.code())
                }
                None => None,
            };
            let _ = tx.send(PiEvent::Exited(code));
        });
        Ok((rpc, rx))
    }

    pub fn exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    /// Writes one record (an `extension_ui_response`, which pi doesn't answer).
    pub async fn send(&self, record: &Value) -> Result<(), RpcError> {
        let mut line = serde_json::to_vec(record).expect("serializable");
        line.push(b'\n');
        let mut s = self.stdin.lock().await;
        s.write_all(&line).await.map_err(|e| RpcError::Gone(format!("Pi is not running ({e}).")))?;
        s.flush().await.map_err(|e| RpcError::Gone(format!("Pi is not running ({e}).")))
    }

    /// Sends a command and waits for its response; its `data` on success.
    pub async fn request(&self, mut command: Value, timeout: Duration) -> Result<Value, RpcError> {
        let response = self.request_raw(&mut command, timeout).await?;
        if response["success"] == true {
            Ok(response["data"].clone())
        } else {
            Err(RpcError::Refused(response["error"].as_str().unwrap_or("Pi refused the request.").to_owned()))
        }
    }

    async fn request_raw(&self, command: &mut Value, timeout: Duration) -> Result<Value, RpcError> {
        let name = command["type"].as_str().unwrap_or("request").to_owned();
        if self.exited() {
            return Err(RpcError::Gone("Pi is not running.".into()));
        }
        let id = format!("domovoi-{}", self.next.fetch_add(1, Ordering::SeqCst));
        command["id"] = json!(id);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), tx);
        if let Err(e) = self.send(command).await {
            self.pending.lock().await.remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => Err(RpcError::Gone(format!("Pi exited before answering `{name}`."))),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(RpcError::Timeout(format!("Pi did not answer `{name}` within {}s.", timeout.as_secs())))
            }
        }
    }

    /// SIGTERM to the process group, then SIGKILL after a grace period. A
    /// process already reaped is never signalled (its pid may be reused).
    pub async fn terminate(&self) {
        let mut child = self.child.lock().await;
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        #[cfg(unix)]
        if let Some(pid) = child.id() {
            // SAFETY: signalling the process group of a child this server started (`process_group(0)`).
            unsafe { libc::killpg(pid as i32, libc::SIGTERM) };
            if tokio::time::timeout(TERMINATION_GRACE, child.wait()).await.is_ok() {
                return;
            }
            if child.try_wait().ok().flatten().is_none() {
                // SAFETY: as above; still not reaped, so the group is still ours.
                unsafe { libc::killpg(pid as i32, libc::SIGKILL) };
            }
        }
        // Windows has no signal to ask with: the tree ends at once.
        self.tree.kill();
        let _ = child.kill().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_split_on_lf_only() {
        let (mut buf, mut dropping) = (Vec::new(), false);
        buf.extend_from_slice("{\"a\":1}\r\n{\"t\":\"line\u{2028}sep\"}\nnot json\n\n{\"b\":".as_bytes());
        let (r, d) = take_records(&mut buf, &mut dropping);
        assert_eq!(r, vec![json!({"a": 1}), json!({"t": "line\u{2028}sep"})]);
        assert!(d.is_empty());
        buf.extend_from_slice(b"2}\n");
        assert_eq!(take_records(&mut buf, &mut dropping).0, vec![json!({"b": 2})]);
        assert!(buf.is_empty());
    }

    #[test]
    fn an_oversized_record_is_dropped_up_to_its_newline() {
        let (mut buf, mut dropping) = (vec![b'x'; MAX_RECORD_BYTES + 1], false);
        let (r, d) = take_records(&mut buf, &mut dropping);
        assert!(r.is_empty() && d == [MAX_RECORD_BYTES + 1] && dropping);
        buf.extend_from_slice(b"tail of it\n{\"ok\":true}\n");
        let (r, d) = take_records(&mut buf, &mut dropping);
        assert_eq!(r, vec![json!({"ok": true})]);
        assert!(d.is_empty() && !dropping);
    }

    #[test]
    fn launch_args_are_checked() {
        assert_eq!(resolve_launch_args("").unwrap(), Vec::<String>::new());
        assert_eq!(resolve_launch_args("--provider=anthropic --model 'claude x' -nc --my-flag v").unwrap(), ["--provider", "anthropic", "--model", "claude x", "-nc", "--my-flag", "v"]);
        assert!(resolve_launch_args("--session=/x").unwrap_err().contains("'--session'"));
        assert!(resolve_launch_args("-c").unwrap_err().contains("'-c'"));
        assert!(resolve_launch_args("--provider openai").unwrap_err().contains("requires '--model'"));
        assert!(resolve_launch_args("--model").unwrap_err().contains("requires a value"));
        assert!(resolve_launch_args("hello").unwrap_err().contains("positional prompt 'hello'"));
        assert!(resolve_launch_args("-- hi").unwrap_err().contains("positional"));
        assert!(resolve_launch_args("-q").unwrap_err().contains("not supported"));
    }

    #[test]
    fn launch_puts_disabling_flags_after_the_users() {
        let l = Launch { ephemeral: true, short_lived: true, no_tools: true, user_args: vec!["--tools".into(), "bash".into()], extension: Some("/c/ext.ts".into()), ..Launch::default() };
        assert_eq!(l.args(), ["--mode", "rpc", "--no-session", "--tools", "bash", "--offline", "--no-tools", "--extension", "/c/ext.ts"]);
        let f = Launch { fork: Some("/s/a.jsonl".into()), no_extensions: true, extension: Some("/c/ext.ts".into()), ..Launch::default() };
        assert_eq!(f.args(), ["--mode", "rpc", "--fork", "/s/a.jsonl", "--no-extensions"]);
    }

    #[test]
    fn slugs_resolve_through_reported_models() {
        let reported = [json!({ "provider": "openrouter/x", "id": "a/b" }), json!({ "provider": "anthropic", "id": "claude" })];
        assert_eq!(resolve_model_slug("openrouter/x/a/b", &reported), Some(("openrouter/x".into(), "a/b".into())));
        assert_eq!(resolve_model_slug("ollama/qwen:4b", &reported), Some(("ollama".into(), "qwen:4b".into())));
        assert_eq!(resolve_model_slug("nope", &reported), None);
        assert_eq!(resolve_model_slug("/x", &reported), None);
    }

    #[test]
    fn the_environment_names_what_the_extension_reads() {
        let env = environment("auto", Some(("http://127.0.0.1:1/mcp".into(), "Bearer t0k".into())));
        let get = |k: &str| env.iter().rev().find(|(n, _)| n == k).and_then(|(_, v)| v.clone());
        assert_eq!(get(RUNTIME_MODE_ENV).as_deref(), Some("approval-required"));
        assert_eq!(get(MCP_TOKEN_ENV).as_deref(), Some("t0k"));
        assert_eq!(get(MCP_URL_ENV).as_deref(), Some("http://127.0.0.1:1/mcp"));
    }

    #[test]
    fn pi_runs_with_the_node_of_its_npm_prefix() {
        let prefix = std::env::temp_dir().join(format!("domovoi-pi-prefix-{}", crate::util::new_uuid()));
        let cli = prefix.join("lib/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js");
        std::fs::create_dir_all(cli.parent().unwrap()).unwrap();
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        std::fs::write(prefix.join("bin/node"), "").unwrap();
        let cmd = command(&cli);
        let path = cmd.as_std().get_envs().find(|(k, _)| *k == "PATH").and_then(|(_, v)| v).map(|v| v.to_string_lossy().into_owned()).unwrap();
        assert!(path.starts_with(&prefix.join("bin").to_string_lossy().into_owned()), "{path}");
        let _ = std::fs::remove_dir_all(prefix);
    }

    /// A stand-in speaking the protocol: answers requests, emits an event, exits 3 on `abort`.
    #[tokio::test]
    async fn requests_are_correlated_and_an_exit_is_reported() {
        let dir = std::env::temp_dir().join(format!("domovoi-pi-rpc-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("pi");
        std::fs::write(&exe, "#!/usr/bin/python3\nimport json,sys\nfor l in sys.stdin:\n  m=json.loads(l)\n  if m['type']=='abort': sys.exit(3)\n  print(json.dumps({'type':'agent_start'}),flush=True)\n  ok=m['type']!='bad'\n  r={'type':'response','id':m['id'],'command':m['type'],'success':ok}\n  r['data' if ok else 'error']={'echo':m['type']} if ok else 'no such command'\n  sys.stdout.write(json.dumps(r)+'\\r\\n');sys.stdout.flush()\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Windows runs no shebang: a batch file starts Python on it.
        #[cfg(windows)]
        let exe = {
            let cmd = dir.join("pi.cmd");
            std::fs::write(&cmd, "@python \"%~dp0pi\" %*\r\n").unwrap();
            cmd
        };
        let (rpc, mut events) = PiRpc::spawn(&exe, &Launch::default(), dir.to_str().unwrap(), &[], |_, _| {}).unwrap();
        assert_eq!(rpc.request(json!({ "type": "get_state" }), DEFAULT_TIMEOUT).await.unwrap(), json!({ "echo": "get_state" }));
        assert_eq!(rpc.request(json!({ "type": "bad" }), DEFAULT_TIMEOUT).await, Err(RpcError::Refused("no such command".into())));
        assert!(matches!(events.recv().await, Some(PiEvent::Record(r)) if r["type"] == "agent_start"));
        let _ = rpc.send(&json!({ "type": "abort" })).await;
        loop {
            match events.recv().await {
                Some(PiEvent::Exited(code)) => {
                    assert_eq!(code, Some(3));
                    break;
                }
                Some(_) => {}
                None => panic!("no exit"),
            }
        }
        assert!(matches!(rpc.request(json!({ "type": "get_state" }), DEFAULT_TIMEOUT).await, Err(RpcError::Gone(_))));
        rpc.terminate().await;
        let _ = std::fs::remove_dir_all(dir);
    }
}
