//! GitLab through the GitLab CLI (`glab`), after t3code's `GitLabCli` and the
//! `api` passthrough of `GitLabPullRequestCli` (`source-control-gitlab`):
//! running a command against a host, sorting its failures, GitLab's REST and
//! GraphQL APIs through `glab api`, and who is signed in.
//!
//! The host is named in `GITLAB_HOST`, as upstream does, so a self-managed
//! GitLab is read with whatever `glab auth login --hostname` stored for it.
//! A JSON body goes on stdin (`--input -` with a `Content-Type` header), never
//! in argv. `DOMOVOI_GLAB_BINARY` overrides the executable.
//!
//! Failures are `crate::gh`'s [`GhError`]: the same `PullRequestProviderError`
//! reasons, with what `glab` said (its first stderr line) as the detail, or
//! upstream's fixed sentence for a missing tool, a missing login and a rate
//! limit.
//!
//! Limits: upstream blanks `GITLAB_TOKEN` and its kin when a caller names a
//! host other than glab's default (old `glab` sent that token to any host);
//! here the host always comes from a project's remote, so they are passed on.
//! Output is not capped by size.

use crate::gh::lenient::{Fields, Loose};
use crate::gh::{GhError, GhReason};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

const TIMEOUT: Duration = Duration::from_secs(30);
/// Diffs and file contents (upstream's `DIFF_TIMEOUT_MS`).
pub const SLOW_TIMEOUT: Duration = Duration::from_secs(60);
const VIEWER_TTL: Duration = Duration::from_secs(600);

/// t3code's GitLab `UNAVAILABLE_REQUIREMENT`.
pub const REQUIREMENT_MISSING: &str =
    "GitLab CLI (`glab`) is required to browse change requests on this host. Install it from https://gitlab.com/gitlab-org/cli and reload.";
pub const REQUIREMENT_UNAUTHENTICATED: &str = "GitLab CLI is not authenticated. Run `glab auth login` and retry.";

/// `GitLabCliUnavailableError.detail` and its kin.
const MISSING_DETAIL: &str = "GitLab CLI (`glab`) is required but not available on PATH.";
const RATE_LIMIT_DETAIL: &str = "GitLab API rate limit exceeded.";
const FAILED_DETAIL: &str = "GitLab CLI command failed.";

fn binary() -> String {
    std::env::var("DOMOVOI_GLAB_BINARY").unwrap_or_else(|_| "glab".into())
}

/// Sorts a failed `glab` run by its stderr (upstream's `classifyGlabFailure`,
/// with the process runner's rate limit and 401 readings).
pub fn classify(stderr: &str) -> GhError {
    let lower = stderr.to_lowercase();
    let reason = if lower.contains("glab auth login") || lower.contains("invalid_grant") || lower.contains("401") || lower.contains("unauthorized") {
        GhReason::Unauthenticated
    } else if lower.contains("rate limit") || lower.contains("429") || lower.contains("too many requests") {
        GhReason::RateLimited
    } else if lower.contains("not found") || lower.contains("404") {
        GhReason::NotFound
    } else if lower.contains("403") || lower.contains("forbidden") {
        GhReason::Forbidden
    } else {
        GhReason::Failed
    };
    let detail = match reason {
        GhReason::Unauthenticated => REQUIREMENT_UNAUTHENTICATED.to_owned(),
        GhReason::RateLimited => RATE_LIMIT_DETAIL.to_owned(),
        _ => stderr
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(|l| l.trim_start_matches("glab: ").trim_start_matches("ERROR: ").chars().take(500).collect::<String>())
            .unwrap_or_else(|| FAILED_DETAIL.into()),
    };
    GhError { reason, detail }
}

/// Runs `glab` against `host`; returns stdout.
pub async fn run(cwd: Option<&str>, host: &str, args: &[&str], stdin: Option<&str>, timeout: Duration) -> Result<String, GhError> {
    let mut cmd = crate::process::command(binary());
    cmd.args(args)
        .env("GITLAB_HOST", host)
        .env("NO_PROMPT", "1")
        .env("GLAB_CHECK_UPDATE", "false")
        .env("GLAB_SEND_TELEMETRY", "false")
        .env("NO_COLOR", "1")
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = cwd.filter(|c| std::path::Path::new(c).is_dir()) {
        cmd.current_dir(cwd);
    }
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => GhError { reason: GhReason::MissingTool, detail: MISSING_DETAIL.into() },
        _ => GhError::failed(format!("The GitLab CLI could not be started: {e}")),
    })?;
    let _tree = crate::process::Tree::of(child.id());
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(input.as_bytes()).await;
        drop(pipe);
    }
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| GhError::failed("The GitLab CLI did not answer in time."))?
        .map_err(|e| GhError::failed(e.to_string()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(classify(&String::from_utf8_lossy(&out.stderr)))
    }
}

/// A request of `glab api`: its path below `/api/v4/` (query included), and
/// optionally a method and a JSON body.
pub struct Api<'a> {
    pub path: &'a str,
    pub method: Option<&'a str>,
    pub body: Option<String>,
    pub headers: Vec<String>,
    pub timeout: Duration,
}

impl<'a> Api<'a> {
    pub fn get(path: &'a str) -> Self {
        Api { path, method: None, body: None, headers: vec![], timeout: TIMEOUT }
    }

    /// A write with no body (an approval, an award by query parameter, a delete).
    pub fn call(method: &'a str, path: &'a str) -> Self {
        Api { method: Some(method), ..Api::get(path) }
    }

    /// A write with a JSON body.
    pub fn send<B: Serialize>(method: &'a str, path: &'a str, body: &B) -> Result<Self, GhError> {
        let body = serde_json::to_string(body).map_err(|e| GhError::failed(format!("The request could not be written: {e}")))?;
        Ok(Api { method: Some(method), body: Some(body), ..Api::get(path) })
    }

    pub fn slow(mut self) -> Self {
        self.timeout = SLOW_TIMEOUT;
        self
    }
}

/// `glab api`; returns what GitLab answered.
pub async fn api(cwd: Option<&str>, host: &str, request: &Api<'_>) -> Result<String, GhError> {
    let mut args: Vec<&str> = vec!["api", request.path];
    if let Some(method) = request.method {
        args.extend(["--method", method]);
    }
    // `glab api --input` sends no Content-Type of its own, which GitLab answers with HTTP 415.
    if request.body.is_some() {
        args.extend(["--input", "-", "--header", "Content-Type: application/json"]);
    }
    for header in &request.headers {
        args.extend(["--header", header.as_str()]);
    }
    run(cwd, host, &args, request.body.as_deref(), request.timeout).await
}

/// `glab api` read as a `T`: the caller's description of the answer.
pub async fn api_json<T: DeserializeOwned>(cwd: Option<&str>, host: &str, request: &Api<'_>) -> Result<T, GhError> {
    let out = api(cwd, host, request).await?;
    read_json(&out)
}

/// GitLab's JSON as a `T`, or the failure upstream calls unreadable.
pub fn read_json<T: DeserializeOwned>(out: &str) -> Result<T, GhError> {
    serde_json::from_str(out.trim()).map_err(|_| GhError::failed("GitLab CLI returned an unreadable response."))
}

#[derive(Serialize)]
struct GraphqlRequest<'a, V: Serialize> {
    query: &'a str,
    variables: V,
}

#[derive(Deserialize)]
#[serde(default, bound(deserialize = "T: Deserialize<'de> + Default"))]
struct GraphqlAnswer<T> {
    errors: Loose<Vec<Fields<GraphqlError>>>,
    data: Fields<T>,
}

impl<T: Default> Default for GraphqlAnswer<T> {
    fn default() -> Self {
        GraphqlAnswer { errors: Loose(None), data: Fields(T::default()) }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GraphqlError {
    message: Loose<String>,
}

/// `POST /api/graphql` with its variables; the answer's `data` is read as a `T`.
/// An answer with errors is the first error's failure.
pub async fn graphql<T: DeserializeOwned + Default, V: Serialize>(cwd: Option<&str>, host: &str, query: &str, variables: V) -> Result<T, GhError> {
    let request = Api::send("POST", "graphql", &GraphqlRequest { query, variables })?;
    let answer: Fields<GraphqlAnswer<T>> = api_json(cwd, host, &request).await?;
    let GraphqlAnswer { errors, data } = answer.0;
    if let Some(err) = errors.0.and_then(|errors| errors.into_iter().next()) {
        let message = err.message.as_deref().unwrap_or("GitLab refused the request.");
        let mut e = classify(message);
        if e.reason == GhReason::Failed {
            e.detail = format!("GitLab refused it: {message}");
        }
        return Err(e);
    }
    Ok(data.0)
}

/// The variables of a query about one merge request or issue (`fullPath`, `iid`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OfIid<'a> {
    pub full_path: &'a str,
    pub iid: String,
}

/// The REST API addresses a project by its URL-encoded full path (upstream's `projectPath`).
pub fn project_path(repository: &str) -> String {
    encode(repository.trim())
}

/// `encodeURIComponent`.
pub fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A query string of `key=value` pairs, each value encoded.
pub fn query(params: &[(&str, &str)]) -> String {
    params.iter().map(|(k, v)| format!("{k}={}", encode(v))).collect::<Vec<_>>().join("&")
}

/// `GET /user`, as far as it is read.
#[derive(Deserialize, Default)]
#[serde(default)]
struct User {
    id: Loose<u64>,
    username: Loose<String>,
}

type ViewerCache = Option<HashMap<String, (Instant, String, String)>>;
static VIEWERS: Mutex<ViewerCache> = Mutex::new(None);

/// `(accountId, username)` of the signed-in account (cached per host).
pub async fn identity(cwd: Option<&str>, host: &str) -> Result<(String, String), GhError> {
    if let Some((at, id, login)) = VIEWERS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_or_insert_with(HashMap::new).get(host).cloned() {
        if at.elapsed() < VIEWER_TTL {
            return Ok((id, login));
        }
    }
    let (id, login) = read_identity(cwd, host).await?;
    VIEWERS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_or_insert_with(HashMap::new).insert(host.to_owned(), (Instant::now(), id.clone(), login.clone()));
    Ok((id, login))
}

/// `GET /user` as it is now, never cached (the issue tracker's `getViewer`).
pub async fn read_identity(cwd: Option<&str>, host: &str) -> Result<(String, String), GhError> {
    let user: Fields<User> = api_json(cwd, host, &Api::get("user")).await?;
    let login = user.username.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
    match (user.id.0.map(|id| id.to_string()), login) {
        (Some(id), Some(login)) => Ok((id, login)),
        // `GitLabViewerUnavailableError`.
        _ => Err(GhError { reason: GhReason::Unauthenticated, detail: "GitLab CLI returned no username for the authenticated account.".into() }),
    }
}

/// Forgets cached viewers (`pullRequests.invalidate` without a reference).
pub fn forget_identities() {
    *VIEWERS.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

/// Whether a remote's host is GitLab's by its name (the `recognizeRemote` of
/// t3code's GitLab definition): gitlab.com, or any host with a `gitlab` label
/// (`gitlab.example.com`, `code.gitlab.acme.io`), which t3code calls GitLab Self-Hosted.
pub fn recognizes_host(hostname: &str) -> bool {
    let hostname = hostname.trim().to_lowercase();
    let hostname = hostname.split(':').next().unwrap_or("");
    hostname == "gitlab.com" || hostname.split('.').any(|label| label == "gitlab")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_glab_failures() {
        assert_eq!(classify("glab: To get started with GitLab CLI, please run: glab auth login").reason, GhReason::Unauthenticated);
        assert_eq!(classify("oauth2: \"invalid_grant\"").reason, GhReason::Unauthenticated);
        assert_eq!(classify("ERROR: 401 Unauthorized").detail, REQUIREMENT_UNAUTHENTICATED);
        assert_eq!(classify("glab: 404 Not Found").reason, GhReason::NotFound);
        assert_eq!(classify("glab: 404 Not Found").detail, "404 Not Found");
        assert_eq!(classify("429 Too Many Requests").reason, GhReason::RateLimited);
        assert_eq!(classify("403 Forbidden").reason, GhReason::Forbidden);
        assert_eq!(classify("").detail, FAILED_DETAIL);
    }

    #[test]
    fn paths_and_queries_are_encoded_like_encode_uri_component() {
        assert_eq!(project_path(" group/sub/repo "), "group%2Fsub%2Frepo");
        assert_eq!(query(&[("labels", "a b,c"), ("approved_by_ids[]", "None")]), "labels=a%20b%2Cc&approved_by_ids[]=None");
        assert_eq!(encode("ü"), "%C3%BC");
    }

    #[test]
    fn recognizes_gitlab_hosts_by_name() {
        assert!(recognizes_host("gitlab.com"));
        assert!(recognizes_host("GitLab.example.com"));
        assert!(recognizes_host("code.gitlab.acme.io:8443"));
        assert!(!recognizes_host("github.com"));
        assert!(!recognizes_host("mygitlab.example.com"));
    }
}
