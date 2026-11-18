//! GitHub through the GitHub CLI (`gh`), after t3code's `source-control-github`
//! package: running a command, sorting its failures, and who is signed in.
//!
//! t3code talks to GitHub's GraphQL and REST APIs with a credential it resolves
//! itself (settings token, `GH_TOKEN`, `gh auth token` or its GitHub App). Domovoi
//! always goes through `gh`, so the credential is whatever `gh` is signed in
//! with, and only github.com-style hosts that `gh` knows are served.
//! `DOMOVOI_GH_BINARY` overrides the executable.
//!
//! What `gh` prints is GitHub's JSON, not ours: [`run_json`] and [`graphql`]
//! read it into the caller's own description of the answer, and [`lenient`]
//! has the pieces such a description is made of. What GitHub's pull requests
//! become for a client is in `rpc::source_control::github`.

use lenient::{Fields, Loose};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use domovoi_model::typed::PullRequestState;
use tokio::io::AsyncWriteExt;

const TIMEOUT: Duration = Duration::from_secs(60);
const VIEWER_TTL: Duration = Duration::from_secs(600);

/// `PullRequestProviderError.reason`, as far as `gh`'s output tells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhReason {
    MissingTool,
    Unauthenticated,
    NotFound,
    RateLimited,
    Forbidden,
    Failed,
}

/// A `gh` command that did not answer, with what it said.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{detail}")]
pub struct GhError {
    pub reason: GhReason,
    pub detail: String,
}

impl GhError {
    pub fn failed(detail: impl Into<String>) -> Self {
        GhError { reason: GhReason::Failed, detail: detail.into() }
    }
    pub fn is_unusable(&self) -> bool {
        matches!(self.reason, GhReason::MissingTool | GhReason::Unauthenticated)
    }
}

/// t3code's `UNAVAILABLE_REQUIREMENT` for GitHub.
pub const REQUIREMENT_MISSING: &str =
    "No GitHub credential on the server. Set GH_TOKEN, or install the GitHub CLI (https://cli.github.com/) and run `gh auth login`.";
pub const REQUIREMENT_UNAUTHENTICATED: &str =
    "GitHub has no working credential for this host. Run `gh auth login`, or check the account and hosts in Settings → Source Control.";

fn binary() -> String {
    std::env::var("DOMOVOI_GH_BINARY").unwrap_or_else(|_| "gh".into())
}

/// Sorts a failed `gh` run by its stderr.
pub fn classify(stderr: &str) -> GhError {
    let lower = stderr.to_lowercase();
    let reason = if lower.contains("gh auth login")
        || lower.contains("not logged in")
        || lower.contains("authentication required")
        || lower.contains("bad credentials")
        || lower.contains("http 401")
    {
        GhReason::Unauthenticated
    } else if lower.contains("rate limit") || lower.contains("http 429") {
        GhReason::RateLimited
    } else if lower.contains("http 404") || lower.contains("could not resolve to a") || lower.contains("not found") {
        GhReason::NotFound
    } else if lower.contains("http 403") || lower.contains("forbidden") {
        GhReason::Forbidden
    } else {
        GhReason::Failed
    };
    let detail = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.trim_start_matches("gh: ").chars().take(500).collect::<String>())
        .unwrap_or_else(|| "The GitHub CLI command failed.".into());
    GhError { reason, detail }
}

/// Runs `gh` against `host`; returns stdout.
pub async fn run(cwd: Option<&str>, host: &str, args: &[&str], stdin: Option<&str>) -> Result<String, GhError> {
    let mut cmd = crate::process::command(binary());
    cmd.args(args)
        .env("GH_HOST", host)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GH_SPINNER_DISABLED", "1")
        .env("NO_COLOR", "1")
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = cwd.filter(|c| std::path::Path::new(c).is_dir()) {
        cmd.current_dir(cwd);
    }
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => GhError { reason: GhReason::MissingTool, detail: "The GitHub CLI (gh) is not installed.".into() },
        _ => GhError::failed(format!("The GitHub CLI could not be started: {e}")),
    })?;
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(input.as_bytes()).await;
        drop(pipe);
    }
    let out = tokio::time::timeout(TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| GhError::failed("The GitHub CLI did not answer in time."))?
        .map_err(|e| GhError::failed(e.to_string()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(classify(&String::from_utf8_lossy(&out.stderr)))
    }
}

/// Runs `gh` and reads the JSON it prints as a `T`: the caller's description
/// of that command's answer.
pub async fn run_json<T: DeserializeOwned>(cwd: Option<&str>, host: &str, args: &[&str]) -> Result<T, GhError> {
    let out = run(cwd, host, args, None).await?;
    serde_json::from_str(&out).map_err(|_| GhError::failed("The GitHub CLI answered with unreadable JSON."))
}

/// A variable of a GraphQL request, as `gh api graphql` is given it.
#[derive(Debug, Clone)]
pub enum Var<'a> {
    /// Sent as a string (`-f`).
    Text(&'a str),
    /// Sent as `gh` reads it (`-F`): a number, `true`, `false` or `null`.
    Raw(String),
}

impl Var<'_> {
    pub fn number(n: u64) -> Self {
        Var::Raw(n.to_string())
    }

    /// For a value the caller was not given.
    pub fn null() -> Self {
        Var::Raw("null".into())
    }
}

/// What a GraphQL request is answered with.
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
    #[serde(rename = "type")]
    kind: Loose<String>,
}

/// `gh api graphql` with its variables; the answer's `data` is read as a `T`.
pub async fn graphql<T: DeserializeOwned + Default>(cwd: Option<&str>, host: &str, query: &str, vars: &[(&str, Var<'_>)]) -> Result<T, GhError> {
    let mut args: Vec<String> = vec!["api".into(), "graphql".into(), "-f".into(), format!("query={query}")];
    for (k, v) in vars {
        match v {
            Var::Text(s) => args.extend(["-f".into(), format!("{k}={s}")]),
            Var::Raw(raw) => args.extend(["-F".into(), format!("{k}={raw}")]),
        }
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let answer: Fields<GraphqlAnswer<T>> = run_json(cwd, host, &refs).await?;
    let GraphqlAnswer { errors, data } = answer.0;
    if let Some(err) = errors.0.and_then(|errors| errors.into_iter().next()) {
        let mut e = classify(err.message.as_deref().unwrap_or("GitHub refused the request."));
        match err.kind.as_deref() {
            Some("NOT_FOUND") => e.reason = GhReason::NotFound,
            Some("FORBIDDEN") => e.reason = GhReason::Forbidden,
            _ => {}
        }
        return Err(e);
    }
    Ok(data.0)
}

type ViewerCache = Option<HashMap<String, (Instant, String, String)>>;
static VIEWERS: Mutex<ViewerCache> = Mutex::new(None);

/// `gh api user`, as far as it is read.
#[derive(Deserialize, Default)]
#[serde(default)]
struct User {
    login: Loose<String>,
    id: Loose<UserId>,
}

/// GitHub's REST API numbers its accounts; a string is taken as well.
#[derive(Deserialize)]
#[serde(untagged)]
enum UserId {
    Number(serde_json::Number),
    Text(String),
}

/// `(accountId, viewer)` of the signed-in account (cached per host).
pub async fn identity(cwd: Option<&str>, host: &str) -> Result<(String, String), GhError> {
    if let Some((at, id, login)) = VIEWERS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_or_insert_with(HashMap::new).get(host).cloned() {
        if at.elapsed() < VIEWER_TTL {
            return Ok((id, login));
        }
    }
    let user: Fields<User> = run_json(cwd, host, &["api", "user"]).await?;
    let login = user.login.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
    let id = match &user.id.0 {
        Some(UserId::Number(n)) => Some(n.to_string()),
        Some(UserId::Text(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    };
    let (Some(id), Some(login)) = (id, login) else {
        return Err(GhError { reason: GhReason::Unauthenticated, detail: "The signed-in account could not be verified.".into() });
    };
    VIEWERS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_or_insert_with(HashMap::new).insert(host.to_owned(), (Instant::now(), id.clone(), login.clone()));
    Ok((id, login))
}

/// Forgets cached viewers (`pullRequests.invalidate` without a reference).
pub fn forget_identities() {
    *VIEWERS.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

/// Whether a pull request is open, closed or merged, from GitHub's `state`
/// and `mergedAt`: one that was merged at some time is merged whatever its
/// state says.
pub fn state(state: Option<&str>, merged_at: Option<&str>) -> PullRequestState {
    if merged_at.is_some_and(|at| !at.trim().is_empty()) {
        return PullRequestState::Merged;
    }
    match state.unwrap_or("").trim().to_uppercase().as_str() {
        "MERGED" => PullRequestState::Merged,
        "CLOSED" => PullRequestState::Closed,
        _ => PullRequestState::Open,
    }
}

/// Reading JSON the way `value["field"].as_str()` reads it: nothing in it is
/// required, and a value of another type than the one asked for is as good
/// as absent. GitHub's answers are read like that, and so are the fields of a
/// request the server has always taken as they came.
pub mod lenient {
    use serde::de::{Deserialize, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
    use std::marker::PhantomData;
    use std::ops::Deref;

    /// A value of type `T`, or nothing where the JSON has no value, `null` or
    /// a value of another type.
    #[derive(Debug, Clone, PartialEq)]
    pub struct Loose<T>(pub Option<T>);

    impl<T> Default for Loose<T> {
        fn default() -> Self {
            Loose(None)
        }
    }

    impl<T> Deref for Loose<T> {
        type Target = Option<T>;
        fn deref(&self) -> &Option<T> {
            &self.0
        }
    }

    impl<'de, T: Deserialize<'de>> Deserialize<'de> for Loose<T> {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            #[derive(serde::Deserialize)]
            #[serde(untagged)]
            enum Either<T> {
                Is(T),
                Not(IgnoredAny),
            }
            Ok(Loose(match Either::<T>::deserialize(deserializer)? {
                Either::Is(value) => Some(value),
                Either::Not(_) => None,
            }))
        }
    }

    /// The fields of a JSON object as a `T`, and a `T` with no field set for
    /// anything that is not an object.
    #[derive(Debug, Clone, PartialEq, Default)]
    pub struct Fields<T>(pub T);

    impl<T> Deref for Fields<T> {
        type Target = T;
        fn deref(&self) -> &T {
            &self.0
        }
    }

    impl<'de, T: Deserialize<'de> + Default> Deserialize<'de> for Fields<T> {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct ObjectOrNothing<T>(PhantomData<T>);

            impl<'de, T: Deserialize<'de> + Default> Visitor<'de> for ObjectOrNothing<T> {
                type Value = T;

                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("any JSON value")
                }
                fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<T, A::Error> {
                    T::deserialize(serde::de::value::MapAccessDeserializer::new(map))
                }
                fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<T, A::Error> {
                    while seq.next_element::<IgnoredAny>()?.is_some() {}
                    Ok(T::default())
                }
                fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<T, D::Error> {
                    deserializer.deserialize_any(self)
                }
                fn visit_newtype_struct<D: Deserializer<'de>>(self, deserializer: D) -> Result<T, D::Error> {
                    deserializer.deserialize_any(self)
                }
                fn visit_bool<E>(self, _: bool) -> Result<T, E> {
                    Ok(T::default())
                }
                fn visit_i64<E>(self, _: i64) -> Result<T, E> {
                    Ok(T::default())
                }
                fn visit_u64<E>(self, _: u64) -> Result<T, E> {
                    Ok(T::default())
                }
                fn visit_f64<E>(self, _: f64) -> Result<T, E> {
                    Ok(T::default())
                }
                fn visit_str<E>(self, _: &str) -> Result<T, E> {
                    Ok(T::default())
                }
                fn visit_bytes<E>(self, _: &[u8]) -> Result<T, E> {
                    Ok(T::default())
                }
                fn visit_none<E>(self) -> Result<T, E> {
                    Ok(T::default())
                }
                fn visit_unit<E>(self) -> Result<T, E> {
                    Ok(T::default())
                }
            }

            deserializer.deserialize_any(ObjectOrNothing(PhantomData)).map(Fields)
        }
    }

    /// A list of objects, each read by its fields; no list at all where the
    /// JSON has anything else.
    pub type Rows<T> = Loose<Vec<Fields<T>>>;

    /// The rows of a list, and none where there was no list.
    pub fn rows<T>(list: &Rows<T>) -> impl Iterator<Item = &T> {
        list.iter().flatten().map(|row| &row.0)
    }

    /// For a field whose presence matters: `Some` when the JSON has the key,
    /// whatever its value (`null` included), and the field's default (`None`)
    /// when it has not.
    pub fn present<'de, T: Deserialize<'de>, D: Deserializer<'de>>(deserializer: D) -> Result<Option<T>, D::Error> {
        T::deserialize(deserializer).map(Some)
    }

    /// A string, a number or a boolean, each kept as it is: for a value that
    /// is handed on rather than read. Anything else is no scalar (in a
    /// [`Loose`], nothing).
    #[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
    #[serde(untagged)]
    pub enum Scalar {
        Text(String),
        Number(serde_json::Number),
        Flag(bool),
    }

    /// A string without the white space around it, or nothing for a blank one.
    pub fn trimmed(text: &Loose<String>) -> Option<String> {
        text.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::lenient::{Fields, Loose, Rows, present, rows};
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_gh_failures() {
        assert_eq!(classify("To get started with GitHub CLI, please run:  gh auth login").reason, GhReason::Unauthenticated);
        assert_eq!(classify("GraphQL: Could not resolve to a PullRequest with the number of 9.").reason, GhReason::NotFound);
        assert_eq!(classify("HTTP 403: Resource not accessible by integration").reason, GhReason::Forbidden);
        assert_eq!(classify("API rate limit exceeded for user").reason, GhReason::RateLimited);
        assert_eq!(classify("something odd\nmore").detail, "something odd");
    }

    #[test]
    fn a_merged_pull_request_is_merged_whatever_its_state() {
        assert_eq!(state(Some("OPEN"), Some("2026-01-03T00:00:00Z")), PullRequestState::Merged);
        assert_eq!(state(Some(" closed "), Some("  ")), PullRequestState::Closed);
        assert_eq!(state(Some("merged"), None), PullRequestState::Merged);
        assert_eq!(state(None, None), PullRequestState::Open);
    }

    #[derive(Deserialize, Default, Debug, PartialEq)]
    #[serde(default, rename_all = "camelCase")]
    struct Row {
        name: Loose<String>,
        count: Loose<u64>,
        on: Loose<bool>,
        #[serde(deserialize_with = "present")]
        closed_at: Option<Loose<String>>,
        inner: Fields<Inner>,
        list: Rows<Inner>,
    }

    #[derive(Deserialize, Default, Debug, PartialEq)]
    #[serde(default)]
    struct Inner {
        login: Loose<String>,
    }

    fn read(value: serde_json::Value) -> Row {
        serde_json::from_value::<Fields<Row>>(value).unwrap().0
    }

    #[test]
    fn json_is_read_as_indexing_reads_it() {
        let row = read(json!({ "name": "a", "count": 3, "on": true, "closedAt": null, "inner": { "login": "x", "more": 1 }, "list": [{ "login": "y" }, "z", null] }));
        assert_eq!(row.name.as_deref(), Some("a"));
        assert_eq!((row.count.0, row.on.0), (Some(3), Some(true)));
        assert_eq!(row.closed_at, Some(Loose(None)), "the key is there, its value is null");
        assert_eq!(row.inner.login.as_deref(), Some("x"));
        assert_eq!(rows(&row.list).map(|inner| inner.login.as_deref()).collect::<Vec<_>>(), [Some("y"), None, None]);

        // Values of other types are as good as absent, as `as_str` and its like have it.
        let odd = read(json!({ "name": 5, "count": -1, "on": "yes", "inner": [1, 2], "list": { "login": "y" } }));
        assert_eq!(odd, Row::default());
        assert_eq!(read(json!({ "count": 1.0 })).count.0, None, "a float is no count");
        // Anything but an object has no field.
        for value in [json!(null), json!("text"), json!(5), json!(["a", 1, true]), json!(true)] {
            assert_eq!(read(value), Row::default());
        }
    }

    #[test]
    fn a_scalar_is_handed_on_as_it_came() {
        let read = |value: serde_json::Value| serde_json::from_value::<Loose<lenient::Scalar>>(value).unwrap().0;
        for value in [json!("PRRT_1"), json!(5), json!(1.5), json!(true)] {
            assert_eq!(json!(read(value.clone())), value);
        }
        for value in [json!(null), json!([1]), json!({ "id": 1 })] {
            assert_eq!(read(value), None);
        }
    }

    #[test]
    fn a_graphql_answer_carries_its_data_or_its_first_error() {
        let read = |value: serde_json::Value| serde_json::from_value::<Fields<GraphqlAnswer<Inner>>>(value).unwrap().0;
        assert_eq!(read(json!({ "data": { "login": "x" } })).data.login.as_deref(), Some("x"));
        let refused = read(json!({ "errors": [{ "type": "NOT_FOUND", "message": "gone" }, { "message": "second" }], "data": null }));
        let first = refused.errors.0.unwrap().remove(0);
        assert_eq!((first.kind.as_deref(), first.message.as_deref()), (Some("NOT_FOUND"), Some("gone")));
        assert!(read(json!([1])).errors.is_none());
    }

    #[test]
    fn an_account_id_is_a_number_or_a_string() {
        let id = |value: serde_json::Value| match serde_json::from_value::<Fields<User>>(value).unwrap().0.id.0 {
            Some(UserId::Number(n)) => n.to_string(),
            Some(UserId::Text(s)) => s,
            None => "none".into(),
        };
        assert_eq!(id(json!({ "id": 583231 })), "583231");
        assert_eq!(id(json!({ "id": "U_1" })), "U_1");
        assert_eq!(id(json!({ "id": null })), "none");
    }
}
