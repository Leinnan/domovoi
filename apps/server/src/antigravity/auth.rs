//! Signing an Antigravity instance in and out (t3code `AntigravityAuth.ts`,
//! `antigravityCallback.ts`): `provider.auth.start`, `complete`, `cancel`,
//! `logout` and `subscribe`.
//!
//! Sign-in runs a throwaway agent in a scratch folder: `initialize`,
//! `authenticate` with the instance's method, then `session/new` to check
//! access and load models. A browser method makes the agent print a Google
//! link that redirects to a listener on `127.0.0.1` of the server's machine;
//! a browser there finishes by itself, and from another device the user
//! pastes the redirect URL, which the server delivers to that listener
//! (`complete`). A flow belongs to the client session that started it:
//! others see that one is running, not its link.
//!
//! The native CLI keeps the Google sign-in in the user's keychain, shared
//! with the `agy` command and the IDE: Domovoi never signs it out (see
//! [`logout`]), and only the built-in instance signs in with Google.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "Antigravity's ACP answers, read as its agent sends them")]

use super::{AuthMethod, launch};
use crate::config_failure::Failure;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const AUTH_TIMEOUT: Duration = Duration::from_secs(300);
const STARTUP_HINT: Duration = Duration::from_secs(90);
const NO_RESPONSE_HINT: Duration = Duration::from_secs(30);
const FORWARD_TIMEOUT: Duration = Duration::from_secs(10);
const NOT_STARTED: &str = "Antigravity has not started after 90 seconds. Its runtime may be stuck starting on this machine.";
const NOT_RESPONDING: &str = "Antigravity did not respond. Its runtime may not run on this machine.";
const NO_RESPONSE: &str = "Antigravity has not reached Google after 30 seconds. Check this machine's network connection. Broken IPv6 is a common cause.";
const UNREACHABLE: &str = "Antigravity could not reach Google. Check this machine's network connection, then start sign-in again.";
const FORWARDING_FAILED: &str = "Could not deliver the sign-in response. Start sign-in again.";
const EXPIRED: &str = "Google sign-in expired. Start sign-in again.";

/// `ProviderAuthState`; `subscribe` adds who holds the credential and the
/// interaction to show.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthState {
    pub instance_id: String,
    pub phase: &'static str,
    pub flow_id: Option<String>,
    pub authorization_url: Option<String>,
    pub expires_at: Option<String>,
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interaction: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_owner: Option<&'static str>,
}

impl AuthState {
    fn idle(instance: &str) -> Self {
        AuthState { instance_id: instance.to_owned(), phase: "idle", flow_id: None, authorization_url: None, expires_at: None, message: None, interaction: None, credential_owner: None }
    }

    fn busy(&self) -> bool {
        matches!(self.phase, "starting" | "waiting" | "verifying")
    }
}

/// The link Google sends the user to, and where it returns.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthorizationUrl {
    pub url: String,
    pub redirect_uri: String,
    pub state: String,
}

fn single<'a>(url: &'a url::Url, key: &str) -> Option<std::borrow::Cow<'a, str>> {
    let mut values = url.query_pairs().filter(|(k, _)| k == key).map(|(_, v)| v);
    let first = values.next()?;
    values.next().is_none().then_some(first)
}

fn count(url: &url::Url, key: &str) -> usize {
    url.query_pairs().filter(|(k, _)| k == key).count()
}

/// Checks a sign-in link the agent gave (t3code `parseAntigravityAuthorizationUrl`):
/// Google's authorization endpoint (`/o/oauth2/v2/auth`, or `/o/oauth2/auth`
/// as the native CLI uses), a code flow, one state, and a redirect to an
/// unprivileged loopback port (`127.0.0.1`, or `localhost` with the CLI's
/// `/auth/callback` path).
pub fn parse_authorization_url(url: &str) -> Result<AuthorizationUrl, String> {
    let invalid = || "Antigravity returned an invalid Google sign-in URL.".to_owned();
    if url.len() > 16_384 || url.chars().any(char::is_whitespace) {
        return Err(invalid());
    }
    let parsed = url::Url::parse(url).map_err(|_| invalid())?;
    let state = single(&parsed, "state").ok_or_else(invalid)?;
    let redirect = single(&parsed, "redirect_uri").ok_or_else(invalid)?;
    let ok = parsed.scheme() == "https"
        && parsed.host_str() == Some("accounts.google.com")
        && parsed.port().is_none()
        && (parsed.path() == "/o/oauth2/v2/auth" || parsed.path() == "/o/oauth2/auth")
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.fragment().is_none()
        && single(&parsed, "response_type").as_deref() == Some("code")
        && !state.is_empty()
        && state.len() <= 512
        && !state.chars().any(char::is_whitespace)
        && regex::Regex::new(r"^http://(?:127\.0\.0\.1|localhost):[1-9][0-9]{0,4}/(?:[A-Za-z0-9_-]+(?:/[A-Za-z0-9_-]+)*)?$").is_ok_and(|r| r.is_match(&redirect));
    if !ok {
        return Err(invalid());
    }
    let port = url::Url::parse(&redirect).ok().and_then(|r| r.port()).ok_or_else(invalid)?;
    if port < 1024 {
        return Err(invalid());
    }
    Ok(AuthorizationUrl { url: url.to_owned(), redirect_uri: redirect.into_owned(), state: state.into_owned() })
}

/// Checks a pasted redirect URL against the flow (t3code
/// `validateAntigravityCallbackUrl`): the flow's listener, its state, one
/// code or error, and Google as issuer if any.
pub fn validate_callback(pending: &AuthorizationUrl, callback: &str) -> Result<url::Url, String> {
    if callback.len() > 16_384 {
        return Err("The sign-in response URL is too long.".into());
    }
    let url = url::Url::parse(callback.trim()).map_err(|_| "Paste the complete redirect URL from the Google sign-in page.".to_owned())?;
    let expected = url::Url::parse(&pending.redirect_uri).map_err(|_| "This redirect URL does not belong to the current sign-in.".to_owned())?;
    let foreign = || "This redirect URL does not belong to the current sign-in.".to_owned();
    if url.scheme() != "http" || !matches!(url.host_str(), Some("127.0.0.1" | "localhost")) || url.origin() != expected.origin() || url.path() != expected.path() || !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(foreign());
    }
    if single(&url, "state").as_deref() != Some(pending.state.as_str()) {
        return Err(foreign());
    }
    let (codes, errors) = (count(&url, "code"), count(&url, "error"));
    let code = single(&url, "code").is_some_and(|c| !c.is_empty());
    let error = single(&url, "error").is_some_and(|e| !e.is_empty());
    if !((codes == 1 && code && errors == 0) || (errors == 1 && error && codes == 0)) {
        return Err("The redirect URL must contain one Google sign-in response.".into());
    }
    let issuers = count(&url, "iss");
    if issuers > 1 || (issuers == 1 && single(&url, "iss").as_deref() != Some("https://accounts.google.com")) {
        return Err("The redirect URL is not a Google sign-in response.".into());
    }
    Ok(url)
}

/// Delivers a redirect to the agent's listener on this machine: a plain GET,
/// no proxy, no redirects followed, a 2xx answer within 10 seconds.
async fn forward(callback: &url::Url) -> Result<(), ()> {
    let client = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none()).timeout(FORWARD_TIMEOUT).build().map_err(|_| ())?;
    let answer = client.get(callback.as_str()).send().await.map_err(|_| ())?;
    if !answer.status().is_success() {
        return Err(());
    }
    let _ = answer.bytes().await;
    Ok(())
}

/// The reason a sign-in failed that is safe to show (never a code or token).
fn safe_failure(message: &str, browser: bool) -> String {
    if message.contains("SUBSCRIPTION_REQUIRED") || message.contains("eligible Antigravity subscription") {
        return "Google requires an eligible Antigravity subscription for this account.".into();
    }
    if regex::Regex::new(r"(?i)access_denied|denied access|cancelled").is_ok_and(|r| r.is_match(message)) {
        return "Google sign-in was not approved. Start sign-in again.".into();
    }
    if message.starts_with("NEW-32603") {
        return "Antigravity authenticated, but could not initialize a session or load models.".into();
    }
    if !browser && message.starts_with("AUTH-32602") {
        return "Antigravity rejected the configured credentials. Check the provider settings.".into();
    }
    // A message Domovoi wrote itself (a timeout, a setting) is safe as it is.
    if let Some(own) = message.strip_prefix("SETUP:") {
        return own.to_owned();
    }
    if browser { "Google sign-in failed. Start sign-in again.".into() } else { "Antigravity could not authenticate with the configured credentials.".into() }
}

/// A flow under way.
struct Flow {
    id: String,
    owner: String,
    expires: tokio::time::Instant,
    pending: Option<AuthorizationUrl>,
    callback_sent: bool,
    task: Option<tokio::task::AbortHandle>,
    /// The flow's agent, killed when the flow is stopped.
    rpc: Option<Arc<crate::jsonrpc::RpcClient>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    Idle,
    Auth,
}

struct Inner {
    operation: Operation,
    flow: Option<Flow>,
}

/// One instance's sign-in.
struct Controller {
    instance: String,
    inner: Mutex<Inner>,
    /// The latest state and the session it belongs to.
    shown: tokio::sync::watch::Sender<(Option<String>, AuthState)>,
}

static CONTROLLERS: Mutex<BTreeMap<String, Arc<Controller>>> = Mutex::new(BTreeMap::new());

fn controller(instance: &str) -> Arc<Controller> {
    crate::util::locked(&CONTROLLERS)
        .entry(instance.to_owned())
        .or_insert_with(|| {
            Arc::new(Controller {
                instance: instance.to_owned(),
                inner: Mutex::new(Inner { operation: Operation::Idle, flow: None }),
                shown: tokio::sync::watch::Sender::new((None, AuthState::idle(instance))),
            })
        })
        .clone()
}

/// The state as session `viewer` may see it.
fn visible(owner: Option<&str>, state: &AuthState, viewer: &str) -> AuthState {
    if owner.is_none_or(|o| o == viewer) {
        return state.clone();
    }
    let mut s = state.clone();
    let busy = s.busy();
    s.flow_id = None;
    s.authorization_url = None;
    s.interaction = None;
    s.expires_at = None;
    if busy {
        s.message = Some("Sign-in is in progress in another client.".into());
    }
    s
}

fn error(instance: &str, operation: &'static str, detail: &str) -> Failure {
    Failure::setup(instance, operation, detail)
}

impl Controller {
    fn browser(&self) -> bool {
        super::config(&self.instance).auth_method.uses_browser()
    }

    fn current(&self) -> AuthState {
        self.shown.borrow().1.clone()
    }

    /// Publishes a change of the flow `flow_id` (when it is still the flow).
    fn publish(&self, flow_id: &str, change: impl FnOnce(&mut AuthState)) -> Option<AuthState> {
        let owner = {
            let inner = crate::util::locked(&self.inner);
            let flow = inner.flow.as_ref().filter(|f| f.id == flow_id)?;
            flow.owner.clone()
        };
        let mut state = self.current();
        change(&mut state);
        self.shown.send_replace((Some(owner), state.clone()));
        Some(state)
    }

    fn finish(&self, flow_id: &str, result: Result<(), String>) {
        let browser = self.browser();
        let (owner, rpc) = {
            let mut inner = crate::util::locked(&self.inner);
            if inner.flow.as_ref().is_none_or(|f| f.id != flow_id) {
                return;
            }
            inner.operation = Operation::Idle;
            let flow = inner.flow.take();
            (flow.as_ref().map(|f| f.owner.clone()), flow.and_then(|f| f.rpc))
        };
        let mut state = self.current();
        state.phase = if result.is_ok() { "succeeded" } else { "failed" };
        state.authorization_url = None;
        state.expires_at = None;
        state.message = Some(match result {
            Ok(()) if browser => "Signed in with Google.".into(),
            Ok(()) => "Connected to Antigravity.".into(),
            Err(e) => safe_failure(&e, browser),
        });
        self.shown.send_replace((owner, state));
        if let Some(rpc) = rpc {
            tokio::spawn(async move { rpc.kill().await });
        }
    }

    /// Ends the flow as `phase` with `message`, stopping its agent.
    async fn stop(&self, flow_id: &str, phase: &'static str, message: &str) -> Option<AuthState> {
        let flow = {
            let mut inner = crate::util::locked(&self.inner);
            if inner.flow.as_ref().is_none_or(|f| f.id != flow_id) {
                return None;
            }
            inner.operation = Operation::Idle;
            inner.flow.take()
        }?;
        let mut state = self.current();
        state.phase = phase;
        state.authorization_url = None;
        state.expires_at = None;
        state.message = Some(message.to_owned());
        self.shown.send_replace((Some(flow.owner.clone()), state.clone()));
        if let Some(task) = &flow.task {
            task.abort();
        }
        if let Some(rpc) = &flow.rpc {
            rpc.kill().await;
        }
        Some(state)
    }
}

async fn stop_sessions(instance: &str) {
    if let Some(host) = crate::adapters::HOST.get() {
        host.stop_instance(instance).await;
    }
}

/// `provider.auth.start`: begins a sign-in for session `owner` (the one it
/// runs already, if it started it).
pub async fn start(instance: &str, owner: &str) -> Result<AuthState, Failure> {
    let c = controller(instance);
    let config = super::config(instance);
    if !config.enabled {
        return Err(error(instance, "start", "Antigravity is disabled in Domovoi's settings."));
    }
    if let Some(issue) = config.issue() {
        return Err(error(instance, "start", &issue));
    }
    let browser = config.auth_method.uses_browser();
    let flow_id = crate::util::new_uuid();
    {
        let mut inner = crate::util::locked(&c.inner);
        if inner.operation == Operation::Auth && inner.flow.as_ref().is_some_and(|f| f.owner == owner) {
            return Ok(c.current());
        }
        if inner.operation != Operation::Idle {
            return Err(error(instance, "start", "Antigravity setup is already in progress."));
        }
        inner.operation = Operation::Auth;
        inner.flow = Some(Flow { id: flow_id.clone(), owner: owner.to_owned(), expires: tokio::time::Instant::now() + AUTH_TIMEOUT, pending: None, callback_sent: false, task: None, rpc: None });
    }
    let expires_at = crate::time_fmt::iso_from_millis(crate::time_fmt::now_millis() + AUTH_TIMEOUT.as_millis() as i64);
    let state = AuthState {
        instance_id: instance.to_owned(),
        phase: "starting",
        flow_id: Some(flow_id.clone()),
        authorization_url: None,
        expires_at: Some(expires_at),
        message: Some(if browser { "Starting Google sign-in." } else { "Checking credentials." }.into()),
        interaction: None,
        credential_owner: None,
    };
    c.shown.send_replace((Some(owner.to_owned()), state.clone()));
    let task = tokio::spawn({
        let (c, flow_id) = (c.clone(), flow_id.clone());
        async move {
            let run = sign_in(&c, &flow_id, config.auth_method);
            let result = match tokio::time::timeout(AUTH_TIMEOUT, run).await {
                Ok(r) => r,
                Err(_) => {
                    let phase = c.current().phase;
                    let initialized = c.current().message.as_deref() == Some(NO_RESPONSE) || phase != "starting";
                    Err(format!("SETUP:{}", if phase != "starting" { EXPIRED } else if initialized { UNREACHABLE } else { NOT_RESPONDING }))
                }
            };
            c.finish(&flow_id, result);
        }
    });
    if let Some(flow) = crate::util::locked(&c.inner).flow.as_mut().filter(|f| f.id == flow_id) {
        flow.task = Some(task.abort_handle());
    }
    Ok(state)
}

/// The sign-in itself: a throwaway agent, its link published, then access checked.
async fn sign_in(c: &Arc<Controller>, flow_id: &str, method: AuthMethod) -> Result<(), String> {
    stop_sessions(&c.instance).await;
    let scratch = std::env::temp_dir().join(format!("domovoi-antigravity-setup-{}", crate::util::new_uuid()));
    std::fs::create_dir_all(&scratch).map_err(|e| format!("SETUP:Could not create an Antigravity setup workspace: {e}"))?;
    let cwd = scratch.to_string_lossy().into_owned();
    let hint = |delay: Duration, message: &'static str| {
        let (c, flow_id) = (c.clone(), flow_id.to_owned());
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            if c.current().phase == "starting" {
                c.publish(&flow_id, |s| s.message = Some(message.into()));
            }
        })
    };
    let startup_hint = hint(STARTUP_HINT, NOT_STARTED);
    let started = launch::start(&c.instance, &cwd, vec![], |_, _| {}).await;
    startup_hint.abort();
    let l = match started {
        Ok(l) => l,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&scratch);
            return Err(format!("SETUP:{e}"));
        }
    };
    if let Some(flow) = crate::util::locked(&c.inner).flow.as_mut().filter(|f| f.id == flow_id) {
        flow.rpc = Some(l.rpc.clone());
    }
    let reach_hint = hint(NO_RESPONSE_HINT, NO_RESPONSE);
    // The link, when the agent prints one, is shown while `authenticate` waits for Google.
    let watcher = {
        let (c, flow_id, url) = (c.clone(), flow_id.to_owned(), l.auth_url.clone());
        tokio::spawn(async move {
            let link = url.wait().await;
            let Ok(parsed) = parse_authorization_url(&link) else { return };
            {
                let mut inner = crate::util::locked(&c.inner);
                let Some(flow) = inner.flow.as_mut().filter(|f| f.id == flow_id) else { return };
                if flow.pending.is_some() {
                    return;
                }
                flow.pending = Some(parsed.clone());
            }
            c.publish(&flow_id, |s| {
                s.phase = "waiting";
                s.authorization_url = Some(parsed.url.clone());
                s.message = Some("Open the Google sign-in link. If you are remote, paste the redirect URL here.".into());
            });
        })
    };
    let result = async {
        let answer = l.rpc.request("authenticate", json!({ "methodId": method.id() })).await;
        reach_hint.abort();
        match answer {
            Ok(answer) => super::authenticated(&c.instance, &answer),
            Err(crate::jsonrpc::RpcError::Remote(e)) if e["code"] == -32602 => return Err(format!("AUTH-32602 {e}")),
            Err(e) => return Err(launch::explain(&e)),
        }
        if let Some(flow) = crate::util::locked(&c.inner).flow.as_mut().filter(|f| f.id == flow_id) {
            flow.pending = None;
        }
        c.publish(flow_id, |s| {
            s.phase = "verifying";
            s.authorization_url = None;
            s.message = Some("Checking Antigravity access and models.".into());
        });
        let params = json!({ "cwd": cwd, "mcpServers": [] });
        let session = match l.rpc.request("session/new", params).await {
            Ok(s) => s,
            Err(crate::jsonrpc::RpcError::Remote(e)) if e["code"] == -32603 => return Err(format!("NEW-32603 {e}")),
            Err(e) => return Err(launch::explain(&e)),
        };
        publish_catalog(&c.instance, &l, &session).await;
        Ok(session["sessionId"].as_str().map(str::to_owned))
    }
    .await;
    watcher.abort();
    l.kill().await;
    let session_id = result.as_ref().ok().cloned().flatten();
    super::remove_session_files(&l.profile, session_id.as_deref(), &cwd);
    let _ = std::fs::remove_dir_all(&scratch);
    result.map(|_| ())
}

/// The account's models, each with its thinking levels: those are listed
/// only for the selected model, so each is selected once, then the
/// session's own again (t3code `publishCatalog`).
async fn publish_catalog(instance: &str, l: &launch::Launch, session: &Value) {
    super::session_started(instance, l.method(), l.exe.version.as_deref(), session, None);
    let options = &session["configOptions"];
    let (Some(model), Some(current)) = (super::acp::model_option(options), super::acp::model_option(options).and_then(|m| m["currentValue"].as_str())) else { return };
    if super::acp::thinking_option(options).is_none() {
        return;
    }
    let id = model["id"].as_str().unwrap_or("model").to_owned();
    let Some(session_id) = session["sessionId"].as_str() else { return };
    let values: Vec<String> = super::acp::flatten_choices(&model["options"]).iter().filter_map(|c| c["value"].as_str().map(str::to_owned)).chain([current.to_owned()]).collect();
    for value in values {
        if let Ok(answer) = l.rpc.request("session/set_config_option", json!({ "sessionId": session_id, "configId": id, "value": value })).await
            && answer["configOptions"].is_array()
        {
            super::config_options_changed(instance, &answer["configOptions"]);
        }
    }
}

/// `provider.auth.complete`: delivers the pasted redirect of the flow.
pub async fn complete(instance: &str, owner: &str, flow_id: &str, callback: &str) -> Result<AuthState, Failure> {
    let c = controller(instance);
    let callback = {
        let mut inner = crate::util::locked(&c.inner);
        let flow = inner.flow.as_mut().filter(|f| f.id == flow_id && f.owner == owner).ok_or_else(|| error(instance, "complete", "This sign-in is no longer active in this client."))?;
        if tokio::time::Instant::now() >= flow.expires {
            return Err(error(instance, "complete", EXPIRED));
        }
        if flow.callback_sent {
            return Err(error(instance, "complete", "The sign-in response was already sent. Wait for Google to finish."));
        }
        let pending = flow.pending.clone().ok_or_else(|| error(instance, "complete", "Wait for the Google sign-in link before you send a redirect URL."))?;
        let callback = validate_callback(&pending, callback).map_err(|e| error(instance, "complete", &e))?;
        flow.callback_sent = true;
        callback
    };
    c.publish(flow_id, |s| {
        s.phase = "verifying";
        s.authorization_url = None;
        s.message = Some("Waiting for Google to finish sign-in.".into());
    });
    if forward(&callback).await.is_err() {
        c.stop(flow_id, "failed", FORWARDING_FAILED).await;
        return Err(error(instance, "complete", FORWARDING_FAILED));
    }
    Ok(visible(Some(owner), &c.current(), owner))
}

/// `provider.auth.cancel`.
pub async fn cancel(instance: &str, owner: &str, flow_id: &str) -> Result<AuthState, Failure> {
    let c = controller(instance);
    let known = crate::util::locked(&c.inner).flow.as_ref().is_some_and(|f| f.id == flow_id && f.owner == owner);
    if !known {
        return Err(error(instance, "cancel", "This sign-in is no longer active in this client."));
    }
    c.stop(flow_id, "cancelled", "Google sign-in was cancelled.").await.ok_or_else(|| error(instance, "cancel", "This sign-in is no longer active in this client."))
}

/// `provider.auth.logout`: nothing to sign out here. A Google account is the
/// machine's shared `agy` sign-in (signing it out would sign out the user's
/// terminal and IDE too), and a key or project is the instance's settings.
/// The answer says where to change them; the instance's sessions and
/// account are left as they are.
pub async fn logout(instance: &str) -> Result<AuthState, Failure> {
    Err(error(instance, "logout", &sign_out_answer(instance)))
}

/// What signing out of instance `instance` from Domovoi tells the user.
pub fn sign_out_answer(instance: &str) -> String {
    if super::config(instance).auth_method.uses_browser() {
        super::SIGN_OUT_ELSEWHERE.to_owned()
    } else {
        "This Antigravity instance connects with its API key or GCP project: change or remove them in its settings.".to_owned()
    }
}

/// `provider.auth.subscribe`: the state as session `viewer` sees it, now
/// and at each change.
pub fn subscribe(instance: &str, viewer: String) -> impl futures::Stream<Item = AuthState> + Send + 'static {
    let mut rx = controller(instance).shown.subscribe();
    async_stream::stream! {
        loop {
            let (owner, state) = rx.borrow_and_update().clone();
            let mut s = visible(owner.as_deref(), &state, &viewer);
            s.credential_owner = Some("provider");
            s.interaction = Some(match (&s.flow_id, &s.authorization_url) {
                (Some(id), Some(url)) if s.phase == "waiting" => json!({ "type": "browser", "id": id, "url": url, "requiresConsent": false, "acceptsCallback": true }),
                _ => Value::Null,
            });
            yield s;
            if rx.changed().await.is_err() {
                break;
            }
        }
    }
}

/// Whether a message is a sign-out (`/logout` alone).
pub fn is_logout_prompt(text: &str, attachments: bool) -> bool {
    !attachments && text.trim() == "/logout"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(port: u16, state: &str) -> String {
        format!("https://accounts.google.com/o/oauth2/v2/auth?client_id=x&redirect_uri=http%3A%2F%2F127.0.0.1%3A{port}%2F&response_type=code&state={state}")
    }

    #[test]
    fn only_googles_own_sign_in_links_are_shown() {
        let ok = parse_authorization_url(&link(4567, "s1")).unwrap();
        assert_eq!((ok.redirect_uri.as_str(), ok.state.as_str()), ("http://127.0.0.1:4567/", "s1"));
        assert!(parse_authorization_url(&link(80, "s1")).is_err(), "a privileged port");
        assert!(parse_authorization_url(&link(4567, "s1").replace("accounts.google.com", "accounts.google.com.evil.example")).is_err());
        assert!(parse_authorization_url(&format!("{}&state=again", link(4567, "s1"))).is_err(), "two states");
        // The native CLI's own form: `/o/oauth2/auth`, PKCE, `localhost` and a callback path.
        let native = "https://accounts.google.com/o/oauth2/auth?access_type=offline&client_id=x&code_challenge=c&code_challenge_method=S256&prompt=consent&redirect_uri=http%3A%2F%2Flocalhost%3A51567%2Fauth%2Fcallback&response_type=code&scope=openid&state=S1";
        let ok = parse_authorization_url(native).unwrap();
        assert_eq!(ok.redirect_uri, "http://localhost:51567/auth/callback");
        assert!(validate_callback(&ok, "http://localhost:51567/auth/callback?state=S1&code=4%2Fx&scope=openid").is_ok());
        assert!(validate_callback(&ok, "http://127.0.0.1:51567/auth/callback?state=S1&code=x").is_err(), "another origin than the redirect's");
        assert!(validate_callback(&ok, "http://localhost:51567/?state=S1&code=x").is_err(), "another path");
        assert!(parse_authorization_url(&link(4567, "s1").replace("127.0.0.1", "evil.example")).is_err());
        assert!(parse_authorization_url(&native.replace("%2Fauth%2Fcallback", "%2F..%2Fx")).is_err());
        assert!(parse_authorization_url(&link(4567, "s1").replace("response_type=code", "response_type=token")).is_err());
        assert!(parse_authorization_url("https://accounts.google.com/o/oauth2/v2/auth?x=1 y").is_err());
    }

    #[test]
    fn pasted_redirects_must_belong_to_the_flow() {
        let pending = parse_authorization_url(&link(4567, "s1")).unwrap();
        assert!(validate_callback(&pending, "http://127.0.0.1:4567/?state=s1&code=abc&scope=x").is_ok());
        assert!(validate_callback(&pending, "http://127.0.0.1:4567/?state=s1&error=access_denied").is_ok());
        assert!(validate_callback(&pending, "http://127.0.0.1:4568/?state=s1&code=abc").is_err(), "another port");
        assert!(validate_callback(&pending, "http://127.0.0.1:4567/?state=s2&code=abc").is_err(), "another flow");
        assert!(validate_callback(&pending, "http://127.0.0.1:4567/?state=s1&code=a&code=b").is_err());
        assert!(validate_callback(&pending, "http://127.0.0.1:4567/?state=s1&code=a&error=b").is_err());
        assert!(validate_callback(&pending, "http://127.0.0.1:4567/?state=s1&code=a&iss=https://evil.example").is_err());
        assert!(validate_callback(&pending, "http://127.0.0.1:4567/cb?state=s1&code=a").is_err(), "another path");
        assert!(validate_callback(&pending, "not a url").unwrap_err().contains("complete redirect URL"));
    }

    #[test]
    fn another_clients_flow_shows_only_that_it_runs() {
        let state = AuthState { phase: "waiting", flow_id: Some("f".into()), authorization_url: Some("https://x".into()), expires_at: Some("t".into()), message: Some("m".into()), ..AuthState::idle("ag") };
        assert_eq!(visible(Some("me"), &state, "me"), state);
        let other = visible(Some("me"), &state, "you");
        assert_eq!((other.flow_id, other.authorization_url, other.expires_at), (None, None, None));
        assert_eq!(other.message.as_deref(), Some("Sign-in is in progress in another client."));
    }

    #[test]
    fn failures_shown_never_carry_the_agents_text() {
        assert_eq!(safe_failure("boom code=secret", true), "Google sign-in failed. Start sign-in again.");
        assert!(safe_failure("Error: SUBSCRIPTION_REQUIRED", true).contains("eligible"));
        assert!(safe_failure("access_denied", true).contains("not approved"));
        assert!(safe_failure("AUTH-32602 bad key", false).contains("rejected the configured credentials"));
        assert_eq!(safe_failure("SETUP:Antigravity sign-out timed out.", true), "Antigravity sign-out timed out.");
        let json = serde_json::to_value(AuthState::idle("ag")).unwrap();
        assert_eq!(json, json!({ "instanceId": "ag", "phase": "idle", "flowId": null, "authorizationUrl": null, "expiresAt": null, "message": null }));
    }
}
