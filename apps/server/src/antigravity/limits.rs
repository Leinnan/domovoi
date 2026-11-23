//! An Antigravity account's usage limits (t3code `antigravityUsageLimits.ts`),
//! as Gemini and shared Claude & GPT windows, five-hourly and weekly.
//!
//! The agent's own language server answers first: each running `agy acp`
//! serves it on a local port (named in its log,
//! `<acp>/log/acp-server-<time>-<pid>.log`) and reads the account's quota
//! (`RetrieveUserQuotaSummary`) with the sign-in it keeps, keychain or not,
//! so no credential leaves the agent. With no agent running the last reading
//! stays. Older runtimes, which save the sign-in in the profile
//! (`acp_token.json`), are read directly: the saved refresh token gets an
//! access token, and Google's Cloud Code service reports the quotas
//! (`retrieveUserQuotaSummary`, else `fetchAvailableModels`). Read in the
//! background: never in the way of readiness or a session's start.
//!
//! `DOMOVOI_ANTIGRAVITY_GOOGLE_API` sends every Google request to one base URL
//! instead (`/token`, `/userinfo`, `/v1internal:…`), for tests.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "Google's OAuth and Cloud Code answers, read as Google sends them")]

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;
use domovoi_model::typed::{ForwardList, ServerProviderUsageLimits, ServerProviderUsageUnavailable, ServerProviderUsageUnavailableReason, ServerProviderUsageWindow, ServerProviderUsageWindowKind, Timestamp};

const DAILY: &str = "https://daily-cloudcode-pa.googleapis.com";
const PROD: &str = "https://cloudcode-pa.googleapis.com";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// A token kept between reads, for the refresh token it came from.
struct Cached {
    fingerprint: String,
    account: String,
    access_token: String,
    expires_at_ms: i64,
}

static TOKENS: Mutex<BTreeMap<String, Cached>> = Mutex::new(BTreeMap::new());
static READING: Mutex<std::collections::BTreeSet<String>> = Mutex::new(std::collections::BTreeSet::new());

fn test_base() -> Option<String> {
    std::env::var("DOMOVOI_ANTIGRAVITY_GOOGLE_API").ok().filter(|v| !v.is_empty())
}

/// The agent's own `User-Agent` for Cloud Code.
fn user_agent() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    };
    format!("antigravity/hub/2.24.0 (aidev_client; os_type={os}; arch={arch}; cl=963137146)")
}

fn checked_at() -> Timestamp {
    Timestamp::from(domovoi_contracts::iso_now())
}

fn unavailable(reason: ServerProviderUsageUnavailableReason, message: Option<&str>) -> ServerProviderUsageLimits {
    ServerProviderUsageLimits {
        checked_at: checked_at(),
        windows: ForwardList::default(),
        credential_fingerprint: None,
        reset_credits: None,
        external_usage: None,
        unavailable: Some(ServerProviderUsageUnavailable { reason, message: message.map(str::to_owned) }),
    }
}

fn kind_rank(kind: &ServerProviderUsageWindowKind) -> u8 {
    match kind {
        ServerProviderUsageWindowKind::Session => 0,
        ServerProviderUsageWindowKind::Weekly => 1,
        ServerProviderUsageWindowKind::Monthly => 2,
        _ => 3,
    }
}

fn iso(value: Option<&str>) -> Option<Timestamp> {
    let t = value?.parse::<jiff::Timestamp>().ok()?;
    Some(Timestamp::from(domovoi_contracts::time_fmt::iso_from_millis(t.as_millisecond())))
}

fn window(id: &str, label: String, remaining: Option<f64>, reset: Option<&str>, kind: ServerProviderUsageWindowKind, mins: Option<u64>) -> Option<ServerProviderUsageWindow> {
    let remaining = remaining.filter(|r| r.is_finite())?;
    let used = (1.0 - remaining.clamp(0.0, 1.0)) * 100.0;
    Some(ServerProviderUsageWindow {
        id: id.to_owned(),
        kind,
        label,
        model_ids: None,
        provider_ids: None,
        used_percent: serde_json::Number::from_f64(used).unwrap_or_else(|| serde_json::Number::from(0)),
        resets_at: iso(reset),
        window_duration_mins: mins,
    })
}

/// The windows of a `retrieveUserQuotaSummary` answer requested at `requested_ms`.
pub fn summary_windows(summary: &Value, requested_ms: i64) -> Vec<ServerProviderUsageWindow> {
    let grouped = summary["groups"].as_array().is_some_and(|g| g.iter().any(|g| g["buckets"].as_array().is_some_and(|b| !b.is_empty())));
    let groups: Vec<Value> = if grouped { summary["groups"].as_array().cloned().unwrap_or_default() } else { vec![json!({ "buckets": summary["buckets"] })] };
    let mut out: Vec<ServerProviderUsageWindow> = vec![];
    for group in &groups {
        let group_name = group["displayName"].as_str().map(str::trim).filter(|n| !n.is_empty());
        for bucket in group["buckets"].as_array().into_iter().flatten() {
            let Some(id) = bucket["bucketId"].as_str().filter(|id| !id.is_empty()) else { continue };
            if bucket["disabled"] == true {
                continue;
            }
            // Argon and Carbon are metered apart from the other Gemini buckets.
            let family = if id.starts_with("gemini-argon-") {
                group_name.unwrap_or("Gemini Argon").to_owned()
            } else if id.starts_with("gemini-carbon-") {
                group_name.unwrap_or("Gemini Carbon").to_owned()
            } else if id.starts_with("gemini-") {
                "Gemini".to_owned()
            } else if id.starts_with("3p-") {
                "Claude & GPT (shared)".to_owned()
            } else {
                group_name.unwrap_or(id).to_owned()
            };
            let (weekly, session) = (bucket["window"] == "weekly", bucket["window"] == "5h");
            let mins = if weekly { Some(7 * 24 * 60) } else if session { Some(5 * 60) } else { None };
            let remaining = bucket["remainingFraction"].as_f64();
            // An untouched bucket reports a rolling full-window reset: no reset to show.
            let reset = bucket["resetTime"].as_str();
            let untouched = remaining == Some(1.0)
                && bucket["description"].is_null()
                && mins.is_some_and(|m| reset.and_then(|r| r.parse::<jiff::Timestamp>().ok()).is_some_and(|r| (r.as_millisecond() - requested_ms - m as i64 * 60_000).abs() <= 60_000));
            let period = if weekly { "Weekly".to_owned() } else if session { "5-hour".to_owned() } else { bucket["displayName"].as_str().map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Quota").to_owned() };
            let kind = if weekly { ServerProviderUsageWindowKind::Weekly } else if session { ServerProviderUsageWindowKind::Session } else { ServerProviderUsageWindowKind::Unnamed };
            if let Some(w) = window(id, format!("{family} \u{b7} {period}"), remaining, if untouched { None } else { reset }, kind, mins)
                && !out.iter().any(|o| o.id == w.id)
            {
                out.push(w);
            }
        }
    }
    out
}

/// Older accounts' model quotas (`fetchAvailableModels`), with no window length.
pub fn model_windows(models: &Value) -> Vec<ServerProviderUsageWindow> {
    let mut out: BTreeMap<String, ServerProviderUsageWindow> = BTreeMap::new();
    for (id, model) in models["models"].as_object().into_iter().flatten() {
        let provider = model["modelProvider"].as_str().unwrap_or_default();
        let family = if provider == "MODEL_PROVIDER_GOOGLE" || id.starts_with("gemini-") {
            "Gemini"
        } else if provider == "MODEL_PROVIDER_ANTHROPIC" || id.starts_with("claude-") {
            "Claude"
        } else if provider == "MODEL_PROVIDER_OPENAI" || id.starts_with("gpt-") {
            "GPT"
        } else {
            continue;
        };
        for (suffix, quota) in [("quota", &model["quotaInfo"]), ("weekly", &model["weeklyQuotaInfo"])] {
            if quota.is_null() {
                continue;
            }
            let weekly = suffix == "weekly";
            let wid = format!("{}-{suffix}", family.to_lowercase());
            let label = format!("{family} \u{b7} {}", if weekly { "Weekly" } else { "Reported quota" });
            let kind = if weekly { ServerProviderUsageWindowKind::Weekly } else { ServerProviderUsageWindowKind::Unnamed };
            let Some(w) = window(&wid, label, quota["remainingFraction"].as_f64(), quota["resetTime"].as_str(), kind, weekly.then_some(7 * 24 * 60)) else { continue };
            let used = |w: &ServerProviderUsageWindow| w.used_percent.as_f64().unwrap_or(0.0);
            if out.get(&wid).is_none_or(|old| used(&w) > used(old)) {
                out.insert(wid, w);
            }
        }
    }
    out.into_values().collect()
}

fn client() -> Option<reqwest::Client> {
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(REQUEST_TIMEOUT).build().ok()
}

async fn json_of(r: reqwest::RequestBuilder) -> Option<Value> {
    let answer = r.send().await.ok()?;
    if !answer.status().is_success() {
        return None;
    }
    answer.json::<Value>().await.ok()
}

/// How many of the newest agent logs to look in for a running agent.
const AGENT_LOGS: usize = 8;

/// The language server's plain HTTP port a `agy acp` log names.
fn http_port_in(log: &str) -> Option<u16> {
    log.lines().find_map(|line| {
        let (_, rest) = line.split_once("Language server listening on random port at ")?;
        let (port, kind) = rest.trim().split_once(' ')?;
        kind.trim().eq_ignore_ascii_case("for HTTP").then(|| port.parse().ok()).flatten()
    })
}

/// The agent process a log belongs to (`acp-server-<time>-<pid>.log`).
fn pid_of(name: &str) -> Option<u32> {
    name.strip_prefix("acp-server-")?.strip_suffix(".log")?.rsplit('-').next()?.parse().ok()
}

#[cfg(unix)]
fn running(pid: u32) -> bool {
    // Signal 0 only asks whether the process is there.
    i32::try_from(pid).is_ok_and(|pid| unsafe { libc::kill(pid, 0) } == 0)
}

#[cfg(not(unix))]
fn running(_pid: u32) -> bool {
    true
}

/// The language server ports of the instance's running agents, newest first.
fn agent_ports(acp: &std::path::Path) -> Vec<u16> {
    let Ok(dir) = std::fs::read_dir(acp.join("log")) else { return vec![] };
    let mut logs: Vec<(std::time::SystemTime, std::path::PathBuf, u32)> = dir
        .flatten()
        .filter_map(|e| {
            let pid = pid_of(&e.file_name().to_string_lossy())?;
            Some((e.metadata().ok()?.modified().ok()?, e.path(), pid))
        })
        .collect();
    logs.sort_by(|a, b| b.0.cmp(&a.0));
    logs.into_iter()
        .take(AGENT_LOGS)
        .filter(|(_, _, pid)| running(*pid))
        .filter_map(|(_, path, _)| http_port_in(&std::fs::read_to_string(path).ok()?))
        .collect()
}

/// The quota a running agent's language server reads, when one answers.
async fn read_from_agent(id: &str) -> Option<Vec<ServerProviderUsageWindow>> {
    let ports = agent_ports(&super::profile(id).acp);
    let http = client()?;
    for port in ports {
        let requested = domovoi_contracts::time_fmt::now_millis();
        let r = http
            .post(format!("http://127.0.0.1:{port}/exa.language_server_pb.LanguageServerService/RetrieveUserQuotaSummary"))
            .header("Connect-Protocol-Version", "1")
            .json(&json!({}));
        if let Some(answer) = json_of(r).await {
            return Some(summary_windows(&answer["response"], requested));
        }
    }
    None
}

fn limits_of(mut windows: Vec<ServerProviderUsageWindow>, account: Option<String>) -> ServerProviderUsageLimits {
    windows.sort_by(|a, b| kind_rank(&a.kind).cmp(&kind_rank(&b.kind)).then_with(|| a.id.cmp(&b.id)));
    ServerProviderUsageLimits { checked_at: checked_at(), windows: windows.into(), credential_fingerprint: account, reset_credits: None, external_usage: None, unavailable: None }
}

/// Nothing to read the limits from now: no agent running, no sign-in saved in the profile.
const NO_AGENT: &str = "Antigravity's limits show once one of its sessions has run.";

/// One read of instance `id`'s limits: `None` when nothing can read them
/// now (the last reading stays).
async fn read(id: &str) -> Option<ServerProviderUsageLimits> {
    let config = super::config(id);
    let from_agent = read_from_agent(id).await;
    if let Some(windows) = from_agent.clone().filter(|w| !w.is_empty()) {
        return Some(limits_of(windows, None));
    }
    if !config.auth_method.uses_browser() {
        // An API key or Vertex: no subscription quota.
        return Some(unavailable(ServerProviderUsageUnavailableReason::Unsupported, None));
    }
    let token_path = super::profile(id).acp.join("acp_token.json");
    let Some(token) = std::fs::read_to_string(&token_path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
        crate::util::locked(&TOKENS).remove(id);
        // The sign-in is in the keychain, where only the agent reads it.
        return match from_agent {
            Some(_) => Some(unavailable(ServerProviderUsageUnavailableReason::ProbeFailed, Some("Antigravity did not return usable quota information."))),
            None => None,
        };
    };
    let field = |k: &str| token[k].as_str().filter(|v| !v.is_empty()).map(str::to_owned);
    let (Some(client_id), Some(secret), Some(refresh)) = (field("client_id"), field("client_secret"), field("refresh_token")) else {
        return Some(unavailable(ServerProviderUsageUnavailableReason::ProbeFailed, Some("Could not read Antigravity limits. Refresh provider status to retry.")));
    };
    let project = field("project_id");
    let fingerprint = crate::util::sha256_hex(format!("{client_id}\0{refresh}\0{}", project.clone().unwrap_or_default()).as_bytes());
    let Some(http) = client() else { return Some(unavailable(ServerProviderUsageUnavailableReason::ProbeFailed, None)) };
    let base = test_base();
    let url = |real: &str, path: &str| base.as_ref().map_or_else(|| real.to_owned(), |b| format!("{b}{path}"));
    let now = domovoi_contracts::time_fmt::now_millis();
    let cached = crate::util::locked(&TOKENS).get(id).filter(|c| c.fingerprint == fingerprint && c.expires_at_ms > now + 60_000).map(|c| (c.access_token.clone(), c.account.clone()));
    let (access, account) = match cached {
        Some(c) => c,
        None => {
            let body = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("client_id", &client_id)
                .append_pair("client_secret", &secret)
                .append_pair("refresh_token", &refresh)
                .append_pair("grant_type", "refresh_token")
                .finish();
            let refreshed = json_of(http.post(url("https://oauth2.googleapis.com/token", "/token")).header("content-type", "application/x-www-form-urlencoded").body(body)).await;
            let (Some(access), Some(expires_in)) = (refreshed.as_ref().and_then(|r| r["access_token"].as_str().map(str::to_owned)), refreshed.as_ref().and_then(|r| r["expires_in"].as_f64()).filter(|e| *e > 0.0)) else {
                return Some(unavailable(ServerProviderUsageUnavailableReason::ProbeFailed, Some("Could not read Antigravity limits. Refresh provider status to retry.")));
            };
            // The Google account id matches across machines; refresh tokens do not.
            let info = json_of(http.get(url("https://www.googleapis.com/oauth2/v2/userinfo", "/userinfo")).bearer_auth(&access)).await;
            let account = info.and_then(|i| i["id"].as_str().filter(|id| !id.is_empty()).map(|g| crate::util::sha256_hex(format!("google-account\0{g}").as_bytes()))).unwrap_or_else(|| fingerprint.clone());
            crate::util::locked(&TOKENS).insert(id.to_owned(), Cached { fingerprint, account: account.clone(), access_token: access.clone(), expires_at_ms: now + (expires_in * 1000.0) as i64 });
            (access, account)
        }
    };
    // Consumer tiers are metered on the daily host and business ones on prod; each answers what it knows.
    let endpoints: &[&str] = if config.auth_method == super::AuthMethod::OauthBusiness { &[PROD, DAILY] } else { &[DAILY, PROD] };
    let body = project.map_or_else(|| json!({}), |p| json!({ "project": p }));
    let fetch = |operation: &'static str| {
        let (http, access, body, base) = (http.clone(), access.clone(), body.clone(), base.clone());
        async move {
            let mut answers = vec![];
            let hosts: Vec<String> = match &base {
                Some(b) => vec![b.clone()],
                None => endpoints.iter().map(|e| (*e).to_owned()).collect(),
            };
            for host in hosts {
                let requested = domovoi_contracts::time_fmt::now_millis();
                let r = http.post(format!("{host}/v1internal:{operation}")).bearer_auth(&access).header("User-Agent", user_agent()).json(&body);
                if let Some(v) = json_of(r).await {
                    answers.push((v, requested));
                }
            }
            answers
        }
    };
    // The first host's reading of a window wins.
    fn add(windows: &mut Vec<ServerProviderUsageWindow>, found: Vec<ServerProviderUsageWindow>) {
        for w in found {
            if !windows.iter().any(|o| o.id == w.id) {
                windows.push(w);
            }
        }
    }
    let mut windows: Vec<ServerProviderUsageWindow> = vec![];
    for (answer, at) in fetch("retrieveUserQuotaSummary").await {
        add(&mut windows, summary_windows(&answer, at));
    }
    if windows.is_empty() {
        for (answer, _) in fetch("fetchAvailableModels").await {
            add(&mut windows, model_windows(&answer));
        }
    }
    if windows.is_empty() {
        return Some(unavailable(ServerProviderUsageUnavailableReason::ProbeFailed, Some("Antigravity did not return usable quota information.")));
    }
    Some(limits_of(windows, Some(account)))
}

/// Reads instance `id`'s limits in the background (one read at a time);
/// the provider list shows them when they arrive.
pub fn refresh(id: &str) {
    if !crate::util::locked(&READING).insert(id.to_owned()) {
        return;
    }
    let id = id.to_owned();
    tokio::spawn(async move {
        let read = tokio::time::timeout(READ_TIMEOUT, read(&id))
            .await
            .unwrap_or_else(|_| Some(unavailable(ServerProviderUsageUnavailableReason::ProbeFailed, Some("Could not read Antigravity limits. Refresh provider status to retry."))));
        // Nothing to read from now: the last reading stays, or this says when one comes.
        let limits = match read {
            Some(limits) => Some(limits),
            None if crate::usage_limits::get(&id).is_none_or(|l| l.windows.is_empty()) => Some(unavailable(ServerProviderUsageUnavailableReason::ProbeFailed, Some(NO_AGENT))),
            None => None,
        };
        // An account signed out meanwhile keeps no limits of the old one.
        if let Some(limits) = limits
            && super::account(&id).auth.as_deref() != Some("unauthenticated")
        {
            crate::usage_limits::set(&id, limits);
        }
        crate::util::locked(&READING).remove(&id);
    });
}

/// Forgets the instance's token (signed out: another account may sign in).
pub fn forget(id: &str) {
    crate::util::locked(&TOKENS).remove(id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_log_names_its_process_and_language_server_port() {
        // As agy 1.5.14 writes it.
        let log = "I1122 00:16:22.999676      42 server.go:1290] Language server will attempt to listen on host localhost\n\
            I1122 00:16:23.000625      42 server.go:1305] Language server listening on random port at 63363 for HTTPS (gRPC)\n\
            I1122 00:16:23.000935      42 server.go:1313] Language server listening on random port at 63364 for HTTP\n";
        assert_eq!(http_port_in(log), Some(63364), "the plain HTTP port, not the gRPC one");
        assert_eq!(http_port_in("no server here"), None);
        assert_eq!(pid_of("acp-server-20261122_001622-9052.log"), Some(9052));
        assert_eq!(pid_of("cli-20261122.log"), None);
    }

    #[test]
    fn an_agents_quota_answer_reads_like_cloud_codes() {
        // The language server's `RetrieveUserQuotaSummary`: Cloud Code's answer under `response`.
        let answer = serde_json::json!({ "response": { "groups": [
            { "displayName": "Gemini Models", "buckets": [
                { "bucketId": "gemini-weekly", "window": "weekly", "remainingFraction": 0.96127754, "resetTime": "2026-11-28T15:14:33Z", "description": "Used some." },
                { "bucketId": "gemini-5h", "window": "5h", "remainingFraction": 0.87736547, "resetTime": "2026-11-22T03:41:45Z", "description": "Used some." } ] },
            { "displayName": "Claude and GPT models", "buckets": [
                { "bucketId": "3p-5h", "window": "5h", "remainingFraction": 1, "resetTime": "2026-11-22T04:05:00Z" } ] } ] } });
        let w = summary_windows(&answer["response"], 0);
        assert_eq!(w.iter().map(|w| (w.label.as_str(), w.used_percent.as_f64().map(f64::round))).collect::<Vec<_>>(),
            [("Gemini \u{b7} Weekly", Some(4.0)), ("Gemini \u{b7} 5-hour", Some(12.0)), ("Claude & GPT (shared) \u{b7} 5-hour", Some(0.0))]);
    }

    #[test]
    fn quota_buckets_become_gemini_and_shared_windows() {
        let at = "2026-11-21T10:00:00Z".parse::<jiff::Timestamp>().unwrap().as_millisecond();
        let summary = json!({ "groups": [{ "displayName": "Gemini", "buckets": [
            { "bucketId": "gemini-5h", "window": "5h", "remainingFraction": 0.25, "resetTime": "2026-11-21T12:00:00Z" },
            { "bucketId": "gemini-weekly", "window": "weekly", "remainingFraction": 1.0, "resetTime": "2026-11-28T10:00:00Z" },
            { "bucketId": "3p-5h", "window": "5h", "remainingFraction": 0.5, "resetTime": "2026-11-21T11:00:00Z" },
            { "bucketId": "off", "disabled": true, "remainingFraction": 0.1 },
        ] }] });
        let w = summary_windows(&summary, at);
        let shown: Vec<(&str, &str, f64, bool)> = w.iter().map(|w| (w.id.as_str(), w.label.as_str(), w.used_percent.as_f64().unwrap(), w.resets_at.is_some())).collect();
        assert_eq!(shown, [("gemini-5h", "Gemini \u{b7} 5-hour", 75.0, true), ("gemini-weekly", "Gemini \u{b7} Weekly", 0.0, false), ("3p-5h", "Claude & GPT (shared) \u{b7} 5-hour", 50.0, true)]);
        assert_eq!(w[0].window_duration_mins, Some(300));
        let models = json!({ "models": { "gemini-3.8-flash": { "quotaInfo": { "remainingFraction": 0.9 } }, "claude-x": { "modelProvider": "MODEL_PROVIDER_ANTHROPIC", "weeklyQuotaInfo": { "remainingFraction": 0.4 } } } });
        let m = model_windows(&models);
        assert_eq!(m.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(), ["claude-weekly", "gemini-quota"]);
        assert!(user_agent().starts_with("antigravity/hub/2.24.0 (aidev_client; os_type="));
    }
}
