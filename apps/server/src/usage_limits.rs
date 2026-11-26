//! Provider accounts' usage limits (t3code `claudeUsageLimits`,
//! `codexUsageLimits`): read when a provider starts and every few minutes
//! (Claude's `get_usage`, Codex's `account/rateLimits/read`, Antigravity's
//! quota service), and as the providers report them while they run (Claude's
//! `rate_limit_event`, Codex's `account/rateLimits/updated`). Each update
//! replaces the windows it names; the provider list shows the latest. Kept in
//! a file, so a restarted server shows them (with when they were checked)
//! before a provider reports again.
//!
//! A turn a limit stopped fails as `usage_limit` with the time the limit
//! resets, which the thread's limit recovery (`usage_recovery`) waits for.

#![expect(clippy::disallowed_types, reason = "the providers' rate limit messages, read as each sends them")]

use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};
use domovoi_model::typed::{ForwardList, ServerProviderUsageLimits, ServerProviderUsageWindow, ServerProviderUsageWindowKind, Timestamp};

static LIMITS: RwLock<Option<HashMap<String, ServerProviderUsageLimits>>> = RwLock::new(None);
static FILE: OnceLock<PathBuf> = OnceLock::new();

const SESSION_MINS: u64 = 5 * 60;
const WEEK_MINS: u64 = 7 * 24 * 60;
const MONTH_MINS: u64 = 30 * 24 * 60;

/// Loads the limits remembered in `dir` and remembers new ones there.
pub fn init(dir: &std::path::Path) {
    let file = dir.join("provider-usage-limits.json");
    if let Some(saved) = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str::<HashMap<String, ServerProviderUsageLimits>>(&t).ok()) {
        let mut all = LIMITS.write().unwrap_or_else(|e| e.into_inner());
        let all = all.get_or_insert_with(HashMap::new);
        for (instance, limits) in saved {
            all.entry(instance).or_insert(limits);
        }
    }
    let _ = FILE.set(file);
}

/// Writes the limits to the file and tells clients (something changed).
fn changed() {
    if let Some(file) = FILE.get() {
        let text = LIMITS.read().unwrap_or_else(|e| e.into_inner()).as_ref().map(|all| serde_json::to_string(all).unwrap_or_default()).unwrap_or_else(|| "{}".into());
        let tmp = file.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, file);
        }
    }
    crate::providers::CLAUDE_COMMANDS_CHANGED.notify_one();
}

/// Whether the instance's limits were checked more than `age` ago (or never).
pub fn stale(instance: &str, age: std::time::Duration) -> bool {
    let Some(limits) = get(instance) else { return true };
    crate::time_fmt::millis_from_iso(limits.checked_at.as_str()).is_none_or(|checked| crate::time_fmt::now_millis() - checked > age.as_millis() as i64)
}

/// The instance's limits as last reported.
pub fn get(instance: &str) -> Option<ServerProviderUsageLimits> {
    LIMITS.read().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(instance).cloned()
}

/// Replaces the windows `windows` names; the provider list is sent again when anything changed.
pub fn merge(instance: &str, windows: Vec<ServerProviderUsageWindow>) {
    if windows.is_empty() {
        return;
    }
    let changed = {
        let mut all = LIMITS.write().unwrap_or_else(|e| e.into_inner());
        let all = all.get_or_insert_with(HashMap::new);
        let checked_at = Timestamp::from(crate::time_fmt::iso_now());
        let entry = all.entry(instance.to_owned()).or_insert_with(|| ServerProviderUsageLimits {
            checked_at: checked_at.clone(),
            windows: ForwardList::default(),
            credential_fingerprint: None,
            reset_credits: None,
            external_usage: None,
            unavailable: None,
        });
        let mut list: Vec<ServerProviderUsageWindow> = entry.windows.iter().cloned().collect();
        let before = list.clone();
        for w in windows {
            match list.iter_mut().find(|old| old.id == w.id) {
                Some(old) => *old = w,
                None => list.push(w),
            }
        }
        entry.checked_at = checked_at;
        // Windows reported: they can be read after all.
        let changed = list != before || entry.unavailable.is_some();
        entry.unavailable = None;
        entry.windows = list.into();
        changed
    };
    if changed {
        self::changed();
    }
}

/// Forgets the instance's limits (its account signed out: another may sign in).
pub fn clear(instance: &str) {
    let removed = LIMITS.write().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|all| all.remove(instance)).is_some();
    if removed {
        changed();
    }
}

/// Replaces all of the instance's limits with a read of them (a quota
/// service's answer, not a stream of updates).
pub fn set(instance: &str, limits: ServerProviderUsageLimits) {
    let changed = {
        let mut all = LIMITS.write().unwrap_or_else(|e| e.into_inner());
        let all = all.get_or_insert_with(HashMap::new);
        let same = all.get(instance).is_some_and(|old| old.windows == limits.windows && old.unavailable == limits.unavailable && old.credential_fingerprint == limits.credential_fingerprint);
        all.insert(instance.to_owned(), limits);
        !same
    };
    if changed {
        self::changed();
    }
}

fn iso_from_epoch_seconds(v: &Value) -> Option<Timestamp> {
    let secs = v.as_f64().filter(|s| s.is_finite() && *s > 0.0)?;
    Some(Timestamp::from(crate::time_fmt::iso_from_millis((secs * 1000.0) as i64)))
}

fn percent(v: f64) -> serde_json::Number {
    serde_json::Number::from_f64(v.clamp(0.0, 100.0)).unwrap_or_else(|| serde_json::Number::from(0))
}

fn window(id: &str, kind: ServerProviderUsageWindowKind, label: &str, used: f64, resets_at: Option<Timestamp>, mins: u64) -> ServerProviderUsageWindow {
    ServerProviderUsageWindow { id: id.to_owned(), kind, label: label.to_owned(), model_ids: None, provider_ids: None, used_percent: percent(used), resets_at, window_duration_mins: Some(mins) }
}

fn claude_period(rate_limit_type: &str) -> Option<(ServerProviderUsageWindowKind, &'static str, u64)> {
    match rate_limit_type {
        "five_hour" => Some((ServerProviderUsageWindowKind::Session, "Session", SESSION_MINS)),
        "seven_day" => Some((ServerProviderUsageWindowKind::Weekly, "Weekly", WEEK_MINS)),
        _ => None,
    }
}

/// The windows a Claude `rate_limit_info` reports (utilization 0-1), for its
/// five-hour and weekly limits. Claude
/// Code sends the top-level `utilization` only past a warning threshold; every
/// event names its windows' use in `unifiedWindows`.
pub fn claude_windows(info: &Value) -> Vec<ServerProviderUsageWindow> {
    let mut out: Vec<ServerProviderUsageWindow> = vec![];
    for (id, w) in info["unifiedWindows"].as_object().into_iter().flatten() {
        let (Some((kind, label, mins)), Some(used)) = (claude_period(id), w["utilization"].as_f64()) else { continue };
        out.push(window(id, kind, label, used * 100.0, iso_from_epoch_seconds(&w["resetsAt"]), mins));
    }
    if let (Some(id), Some(used)) = (info["rateLimitType"].as_str(), info["utilization"].as_f64())
        && let Some((kind, label, mins)) = claude_period(id)
        && !out.iter().any(|w| w.id == id)
    {
        out.push(window(id, kind, label, used * 100.0, iso_from_epoch_seconds(&info["resetsAt"]), mins));
    }
    out
}

fn iso_from_text(v: &Value) -> Option<Timestamp> {
    let millis = crate::time_fmt::millis_from_iso(v.as_str()?)?;
    Some(Timestamp::from(crate::time_fmt::iso_from_millis(millis)))
}

fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_owned()
}

/// Percent of `used` in `limit`, when both are amounts.
fn share(used: &Value, limit: &Value) -> Option<f64> {
    let (used, limit) = (used.as_f64()?, limit.as_f64()?);
    (used >= 0.0 && limit > 0.0).then(|| used / limit * 100.0)
}

/// A Claude account's limits as `get_usage` reads them (t3code
/// `claudeUsageResponseToLimits`; percentages already 0-100): five-hour and
/// weekly, each model's own weekly limit, and the monthly spend of extra
/// usage when it is on. An account with no subscription limits (an API key)
/// says so.
pub fn claude_usage_limits(response: &Value) -> ServerProviderUsageLimits {
    use domovoi_model::typed::{ServerProviderUsageUnavailable, ServerProviderUsageUnavailableReason as Reason};
    let checked_at = Timestamp::from(crate::time_fmt::iso_now());
    let limits = &response["rate_limits"];
    let unavailable = |reason| ServerProviderUsageLimits {
        checked_at: checked_at.clone(),
        windows: ForwardList::default(),
        credential_fingerprint: None,
        reset_credits: None,
        external_usage: None,
        unavailable: Some(ServerProviderUsageUnavailable { reason, message: None }),
    };
    if response["rate_limits_available"] != true {
        return unavailable(Reason::Unsupported);
    }
    if !limits.is_object() {
        return unavailable(Reason::ProbeFailed);
    }
    let mut windows: Vec<ServerProviderUsageWindow> = vec![];
    for id in ["five_hour", "seven_day"] {
        let (Some((kind, label, mins)), Some(used)) = (claude_period(id), limits[id]["utilization"].as_f64()) else { continue };
        windows.push(window(id, kind, label, used, iso_from_text(&limits[id]["resets_at"]), mins));
    }
    // A model's own weekly limit: `model_scoped` in some releases, a scoped
    // entry of `limits` in others.
    let scoped = limits["model_scoped"].as_array().into_iter().flatten().filter_map(|e| Some((e["display_name"].as_str()?, e["utilization"].as_f64()?, &e["resets_at"])));
    let listed = limits["limits"].as_array().into_iter().flatten().filter(|e| e["group"] == "weekly" && e["kind"].as_str().is_some_and(|k| k.ends_with("_scoped"))).filter_map(|e| {
        Some((e["scope"]["model"]["display_name"].as_str()?, e["percent"].as_f64()?, &e["resets_at"]))
    });
    for (name, used, resets) in scoped.chain(listed) {
        let id = format!("seven_day_{}", slug(name));
        if name.trim().is_empty() || windows.iter().any(|w| w.id == id) {
            continue;
        }
        windows.push(window(&id, ServerProviderUsageWindowKind::Weekly, &format!("Weekly \u{b7} {}", name.trim()), used, iso_from_text(resets), WEEK_MINS));
    }
    let spend = &limits["spend"];
    let same_money = spend["used"]["currency"] == spend["limit"]["currency"] && spend["used"]["exponent"] == spend["limit"]["exponent"];
    let spent = if spend["enabled"] == true && same_money {
        share(&spend["used"]["amount_minor"], &spend["limit"]["amount_minor"])
    } else if limits["extra_usage"]["is_enabled"] == true {
        share(&limits["extra_usage"]["used_credits"], &limits["extra_usage"]["monthly_limit"]).or_else(|| limits["extra_usage"]["utilization"].as_f64())
    } else {
        None
    };
    if let Some(used) = spent.filter(|p| p.is_finite()) {
        let mut w = window("monthly_spend", ServerProviderUsageWindowKind::Monthly, "Monthly spend", used, None, MONTH_MINS);
        w.window_duration_mins = None;
        windows.push(w);
    }
    ServerProviderUsageLimits { checked_at, windows: windows.into(), credential_fingerprint: None, reset_credits: None, external_usage: None, unavailable: None }
}

/// Whether a Claude `rate_limit_info` says the turn is blocked: rejected,
/// with no overage to carry on in.
pub fn claude_blocked(info: &Value) -> bool {
    let overage = matches!(info["overageStatus"].as_str(), Some("allowed" | "allowed_warning")) || info["isUsingOverage"] == true || info["overageInUse"] == true;
    info["status"] == "rejected" && !overage
}

/// When a blocked Claude limit resets.
pub fn claude_reset_at(info: &Value) -> Option<Timestamp> {
    iso_from_epoch_seconds(&info["resetsAt"])
}

/// The windows of a Codex rate limit snapshot:
/// `primary` and `secondary` positions, and a monthly credit limit.
pub fn codex_windows(snapshot: &Value) -> Vec<ServerProviderUsageWindow> {
    if snapshot["limitId"].as_str().is_some_and(|id| id != "codex") {
        return vec![];
    }
    let monthly_plan = matches!(snapshot["planType"].as_str(), Some("free" | "go"));
    let mut out = vec![];
    for (id, fallback) in [("primary", if monthly_plan { MONTH_MINS } else { SESSION_MINS }), ("secondary", WEEK_MINS)] {
        let w = &snapshot[id];
        let Some(used) = w["usedPercent"].as_f64() else { continue };
        let mins = w["windowDurationMins"].as_u64().unwrap_or(fallback);
        let (kind, label) = if mins >= MONTH_MINS {
            (ServerProviderUsageWindowKind::Monthly, "Monthly")
        } else if mins >= WEEK_MINS {
            (ServerProviderUsageWindowKind::Weekly, "Weekly")
        } else {
            (ServerProviderUsageWindowKind::Session, "Session")
        };
        out.push(window(id, kind, label, used, iso_from_epoch_seconds(&w["resetsAt"]), mins));
    }
    if let Some(remaining) = snapshot["individualLimit"]["remainingPercent"].as_f64() {
        let mut w = window("individual", ServerProviderUsageWindowKind::Monthly, "Monthly credit limit", 100.0 - remaining, iso_from_epoch_seconds(&snapshot["individualLimit"]["resetsAt"]), MONTH_MINS);
        w.window_duration_mins = None;
        out.push(w);
    }
    out
}

/// Whether a Codex error (`codexErrorInfo`) is a usage or rate limit.
pub fn codex_usage_error(info: &Value) -> bool {
    let code = match info {
        Value::String(s) => Some(s.as_str()),
        Value::Object(m) => m.keys().next().map(String::as_str),
        _ => None,
    };
    let exhausted_429 = info["responseTooManyFailedAttempts"]["httpStatusCode"] == 429;
    matches!(code, Some("usageLimitExceeded" | "rateLimitExceeded")) || exhausted_429
}

/// When the instance's used-up windows reset: the latest of them (t3code
/// `codexUsageLimitResetAt`), so resuming then finds every one open again.
pub fn exhausted_reset_at(instance: &str) -> Option<Timestamp> {
    let limits = get(instance)?;
    limits
        .windows
        .iter()
        .filter(|w| w.used_percent.as_f64().is_some_and(|p| p >= 100.0))
        .filter_map(|w| w.resets_at.clone())
        // A window already past its reset is open again.
        .filter(|r| crate::time_fmt::millis_from_iso(r.as_str()).is_some_and(|at| at > crate::time_fmt::now_millis()))
        .max_by(|a, b| a.as_str().cmp(b.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_events_become_windows_and_say_when_blocked() {
        // As Claude Code 2.1.319 sends one past a warning threshold.
        let info = json!({ "status": "allowed_warning", "resetsAt": 1795186800, "rateLimitType": "seven_day", "utilization": 0.95, "overageStatus": "rejected", "isUsingOverage": false,
            "surpassedThreshold": 0.75, "unifiedWindows": { "seven_day": { "utilization": 0.95, "resetsAt": 1795186800, "status": "allowed_warning" } } });
        let w = claude_windows(&info);
        assert_eq!(w.iter().map(|w| (w.id.as_str(), w.label.as_str(), w.used_percent.as_f64())).collect::<Vec<_>>(), [("seven_day", "Weekly", Some(95.0))]);
        assert_eq!(w[0].resets_at.as_ref().unwrap().as_str(), "2026-11-20T15:00:00.000Z");
        assert!(!claude_blocked(&info));
        assert!(claude_blocked(&json!({ "status": "rejected", "overageStatus": "rejected" })));
        assert!(!claude_blocked(&json!({ "status": "rejected", "overageStatus": "allowed" })), "overage carries on");
        assert!(claude_windows(&json!({ "rateLimitType": "seven_day_opus", "utilization": 0.5 })).is_empty());
    }

    #[test]
    fn claude_events_below_a_threshold_still_name_their_windows() {
        // Most events: `allowed`, with the use only in `unifiedWindows`.
        let info = json!({ "status": "allowed", "resetsAt": 1795791600, "rateLimitType": "seven_day", "overageStatus": "rejected", "isUsingOverage": false,
            "unifiedWindows": { "seven_day": { "utilization": 0.02, "resetsAt": 1795791600, "status": "allowed" }, "five_hour": { "utilization": 0.4, "resetsAt": 1795320000, "status": "allowed" } } });
        let mut w = claude_windows(&info);
        w.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(w.iter().map(|w| (w.id.as_str(), w.label.as_str(), w.used_percent.as_f64())).collect::<Vec<_>>(), [("five_hour", "Session", Some(40.0)), ("seven_day", "Weekly", Some(2.0))]);
    }

    #[test]
    fn claude_get_usage_reads_every_limit() {
        // As Claude Code 2.1.319 answers `get_usage` (trimmed).
        let answer = json!({ "subscription_type": "max", "rate_limits_available": true, "rate_limits": {
            "five_hour": { "utilization": 12, "resets_at": "2026-11-22T05:00:00.130108+00:00" },
            "seven_day": { "utilization": 21, "resets_at": "2026-11-27T15:00:00.130108+00:00" },
            "seven_day_opus": null,
            "extra_usage": { "is_enabled": true, "monthly_limit": 6500, "used_credits": 650, "utilization": 0, "currency": "EUR" },
            "limits": [
                { "kind": "weekly_all", "group": "weekly", "percent": 21, "resets_at": "2026-11-27T15:00:00.130108+00:00", "scope": null },
                { "kind": "weekly_scoped", "group": "weekly", "percent": 3, "resets_at": "2026-11-27T15:00:00+00:00", "scope": { "model": { "id": null, "display_name": "Fable" } } },
            ],
            "spend": { "enabled": false, "used": { "amount_minor": 0, "currency": "EUR", "exponent": 2 }, "limit": { "amount_minor": 6500, "currency": "EUR", "exponent": 2 } },
        } });
        let limits = claude_usage_limits(&answer);
        assert!(limits.unavailable.is_none());
        let got: Vec<_> = limits.windows.iter().map(|w| (w.id.as_str(), w.label.as_str(), w.used_percent.as_f64().map(f64::round))).collect();
        assert_eq!(got, [("five_hour", "Session", Some(12.0)), ("seven_day", "Weekly", Some(21.0)), ("seven_day_fable", "Weekly \u{b7} Fable", Some(3.0)), ("monthly_spend", "Monthly spend", Some(10.0))]);
        assert_eq!(limits.windows.iter().next().unwrap().resets_at.as_ref().unwrap().as_str(), "2026-11-22T05:00:00.130Z");
        // An account with no subscription limits (an API key), and a five-hour window not in force.
        let api = claude_usage_limits(&json!({ "rate_limits_available": false }));
        assert!(api.windows.is_empty() && api.unavailable.is_some_and(|u| u.reason == domovoi_model::typed::ServerProviderUsageUnavailableReason::Unsupported));
        let weekly_only = claude_usage_limits(&json!({ "rate_limits_available": true, "rate_limits": { "five_hour": null, "seven_day": { "utilization": 21, "resets_at": null } } }));
        assert_eq!(weekly_only.windows.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(), ["seven_day"]);
    }

    #[test]
    fn codex_snapshots_become_windows() {
        // As codex-cli 0.170 sends one.
        let soon = crate::time_fmt::now_millis() / 1000 + 3600;
        let snapshot = json!({ "limitId": "codex", "primary": { "usedPercent": 100, "windowDurationMins": 300, "resetsAt": soon },
            "secondary": { "usedPercent": 4, "windowDurationMins": 10080, "resetsAt": soon + 86_400 }, "planType": "plus", "individualLimit": null });
        let w = codex_windows(&snapshot);
        assert_eq!(w.iter().map(|w| (w.id.as_str(), w.label.as_str())).collect::<Vec<_>>(), [("primary", "Session"), ("secondary", "Weekly")]);
        assert!(codex_windows(&json!({ "limitId": "gpt-6-sol", "primary": { "usedPercent": 1 } })).is_empty(), "model limits are not the account's");
        merge("codex-test", w);
        assert_eq!(exhausted_reset_at("codex-test"), codex_windows(&snapshot)[0].resets_at);
        // A used-up window already past its reset is open again.
        let past = json!({ "limitId": "codex", "primary": { "usedPercent": 100, "windowDurationMins": 300, "resetsAt": 1794601906 } });
        merge("codex-past", codex_windows(&past));
        assert_eq!(exhausted_reset_at("codex-past"), None);
        assert!(codex_usage_error(&json!("usageLimitExceeded")));
        assert!(codex_usage_error(&json!({ "responseTooManyFailedAttempts": { "httpStatusCode": 429 } })));
        assert!(!codex_usage_error(&json!({ "httpConnectionFailed": { "httpStatusCode": 502 } })));
    }
}
