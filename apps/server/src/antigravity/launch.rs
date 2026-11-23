//! One `agy acp` process of an instance: in the instance's profile, with
//! its own environment, initialized and signed in (t3code
//! `makeAntigravityAcpRuntime` and the driver's `makeRuntime`).
//!
//! The agent prints a Google sign-in link when it needs one (on stdout,
//! where it is taken out of the protocol, or on stderr through `BROWSER`).
//! A thread's session can't sign in: the link means "sign in first".

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "Antigravity's ACP messages, read as its agent sends them")]

use super::{AuthMethod, Config, Executable, Profile};
use crate::jsonrpc::{Incoming, RpcClient, RpcError, SpawnOptions};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Notify, mpsc};

/// How long a launch waits for `authenticate` when no link appears.
const AUTHENTICATE_TIMEOUT: Duration = Duration::from_secs(90);

/// The sign-in link the agent printed, once it has.
#[derive(Default)]
pub struct AuthUrl {
    url: std::sync::Mutex<Option<String>>,
    seen: Notify,
}

impl AuthUrl {
    fn found(&self, url: String) {
        let mut slot = crate::util::locked(&self.url);
        if slot.is_none() {
            *slot = Some(url);
            drop(slot);
            self.seen.notify_waiters();
            self.seen.notify_one();
        }
    }

    pub fn get(&self) -> Option<String> {
        crate::util::locked(&self.url).clone()
    }

    /// Waits for the link.
    pub async fn wait(&self) -> String {
        loop {
            let notified = self.seen.notified();
            if let Some(url) = self.get() {
                return url;
            }
            notified.await;
        }
    }
}

/// Takes a sign-in link from `file` (the launch's `open` writes it there)
/// while the launch lives.
fn watch_link_file(file: PathBuf, slot: std::sync::Weak<AuthUrl>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let Some(slot) = slot.upgrade() else { return };
            if slot.get().is_some() {
                return;
            }
            let Ok(text) = std::fs::read_to_string(&file) else { continue };
            if let Some(url) = text.lines().find_map(super::acp::auth_url_in) {
                slot.found(url);
                return;
            }
        }
    });
}

/// A started agent.
pub struct Launch {
    pub rpc: Arc<RpcClient>,
    pub events: mpsc::UnboundedReceiver<Incoming>,
    /// Its `initialize` answer.
    pub init: Value,
    pub config: Config,
    pub exe: Executable,
    pub profile: Profile,
    /// This launch's own temp folder, removed when it is killed or dropped.
    pub temp: TempDir,
    /// Held while it runs from Domovoi's install (which then stays).
    pub lease: Option<super::install::Lease>,
    pub auth_url: Arc<AuthUrl>,
}

/// Why signing in at launch did not work.
#[derive(Debug, PartialEq, Eq)]
pub enum SignIn {
    /// The account needs signing in (in Settings).
    Required,
    Failed(String),
}

impl SignIn {
    pub fn message(&self) -> String {
        match self {
            SignIn::Required => super::SIGN_IN_REQUIRED.to_owned(),
            SignIn::Failed(m) => m.clone(),
        }
    }
}

/// Whether an agent's error answer means "sign in first" (ACP's -32000).
pub fn sign_in_required(e: &RpcError) -> bool {
    matches!(e, RpcError::Remote(v) if v["code"] == -32000)
}

/// Readable text of an agent's error answer, with Google's account errors explained.
pub fn explain(e: &RpcError) -> String {
    let text = match e {
        RpcError::Remote(v) => [v["message"].as_str(), v["data"]["details"].as_str(), v["data"].as_str()].into_iter().flatten().collect::<Vec<_>>().join(": "),
        other => other.to_string(),
    };
    if text.contains("SUBSCRIPTION_REQUIRED") {
        return "Google requires an eligible Antigravity subscription for this account. Check the account's plan at https://antigravity.google/docs/plans, then sign in again.".into();
    }
    if text.is_empty() { e.to_string() } else { text }
}

/// Instances whose temp folder was swept since the server started.
static SWEPT: std::sync::Mutex<std::collections::BTreeSet<String>> = std::sync::Mutex::new(std::collections::BTreeSet::new());

/// Starts instance `instance`'s agent in `cwd` and sends `initialize`. `env`
/// is added (the thread id); `log` sees every message.
pub async fn start(instance: &str, cwd: &str, env: Vec<(String, String)>, log: impl Fn(&str, &Value) + Send + Sync + 'static) -> Result<Launch, String> {
    let config = super::config(instance);
    if let Some(issue) = config.issue() {
        return Err(issue);
    }
    let exe = super::resolve(&config).await?;
    let profile = super::profile(instance);
    super::prepare(&profile, &config)?;
    if crate::util::locked(&SWEPT).insert(instance.to_owned()) {
        super::sweep_temp(&profile);
    }
    let temp = TempDir(super::launch_temp(&profile)?);
    let browser = super::browser_command()?;
    let mut vars = super::environment(&profile, &config, temp.path(), &browser);
    for v in crate::providers::instance(instance).and_then(|c| c.environment).unwrap_or_default() {
        if let Some(value) = v.value {
            vars.push((v.name, value));
        }
    }
    vars.extend(env);
    // The native CLI opens its sign-in link with `open` / `xdg-open`: ours come first.
    #[cfg(unix)]
    {
        let shims = super::write_open_shims(temp.path())?;
        let rest = vars.iter().rev().find(|(k, _)| k == "PATH").map(|(_, v)| v.clone()).or_else(|| std::env::var("PATH").ok()).unwrap_or_default();
        vars.retain(|(k, _)| k != "PATH");
        vars.push(("PATH".into(), format!("{}:{rest}", shims.to_string_lossy())));
    }
    let auth_url = Arc::new(AuthUrl::default());
    watch_link_file(super::signin_link_file(temp.path()), Arc::downgrade(&auth_url));
    let (on_stdout, on_stderr) = (auth_url.clone(), auth_url.clone());
    let options = SpawnOptions {
        env: vars,
        remove_env: super::REMOVED_ENV.iter().map(|s| (*s).to_owned()).collect(),
        jsonrpc2: true,
        stderr: Some(Box::new(move |line: &str| {
            if let Some(url) = super::acp::auth_url_in(line) {
                on_stderr.found(url);
            }
        })),
        stdout_line: Some(Box::new(move |line: &str| {
            if line.starts_with(super::AUTH_STDOUT_PREFIX) {
                if let Some(url) = super::acp::auth_url_in(line) {
                    on_stdout.found(url);
                }
                return false;
            }
            true
        })),
    };
    let args = super::acp_args(&profile);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let program = exe.path.to_string_lossy().into_owned();
    let spawned = RpcClient::spawn_opts(&program, &args, Some(cwd), options, log);
    let (rpc, events) = spawned.map_err(|e| format!("Could not start Antigravity: {e}"))?;
    let lease = super::install::acquire(&exe.path);
    let launch = |init| Launch { rpc: rpc.clone(), events, init, config: config.clone(), exe: exe.clone(), profile: profile.clone(), temp, lease, auth_url };
    match tokio::time::timeout(AUTHENTICATE_TIMEOUT, rpc.request("initialize", super::acp::initialize_params(exe.version.as_deref()))).await {
        Ok(Ok(init)) => Ok(launch(init)),
        Ok(Err(e)) => {
            let l = launch(Value::Null);
            l.kill().await;
            Err(format!("Antigravity did not initialize: {}", explain(&e)))
        }
        Err(_) => {
            let l = launch(Value::Null);
            l.kill().await;
            Err("Antigravity did not answer `initialize` within 90 seconds.".into())
        }
    }
}

impl Launch {
    pub fn method(&self) -> AuthMethod {
        self.config.auth_method
    }

    /// `authenticate` with the instance's method, sent at every launch. A
    /// sign-in link in the meantime means the account must sign in first.
    pub async fn authenticate(&self) -> Result<Value, SignIn> {
        let request = self.rpc.request("authenticate", json!({ "methodId": self.method().id() }));
        tokio::select! {
            answer = tokio::time::timeout(AUTHENTICATE_TIMEOUT, request) => match answer {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) if sign_in_required(&e) => Err(SignIn::Required),
                Ok(Err(e)) => Err(SignIn::Failed(format!("Antigravity did not sign in: {}", explain(&e)))),
                Err(_) => Err(SignIn::Failed("Antigravity did not finish signing in within 90 seconds.".into())),
            },
            _ = self.auth_url.wait() => Err(SignIn::Required),
        }
    }

    /// `session/new` or `session/resume` parameters: the folder, Domovoi's
    /// tools, the attachments folder, and `_meta` when there is any.
    pub fn session_params(&self, cwd: &str, mcp_servers: Value, attachments: &Path, meta: Option<Value>) -> Value {
        let mut p = json!({ "cwd": cwd, "mcpServers": mcp_servers, "additionalDirectories": [attachments.to_string_lossy()] });
        if let Some(meta) = meta {
            p["_meta"] = meta;
        }
        p
    }

    pub async fn kill(&self) {
        self.rpc.kill().await;
        self.temp.remove();
    }
}

/// A launch's temp folder (the agent unpacks itself there): removed when
/// the launch ends, or else at the instance's next start.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn remove(&self) {
        if self.0.file_name().is_some_and(|n| n.to_string_lossy().starts_with("run-")) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        self.remove();
    }
}
