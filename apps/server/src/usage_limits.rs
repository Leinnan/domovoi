//! Provider accounts' usage limits (t3code `claudeUsageLimits`,
//! `codexUsageLimits`), as the providers report them while they run: Claude's
//! `rate_limit_event` and Codex's `account/rateLimits/updated`. Each update
//! replaces the windows it names; the provider list shows the latest.
//!
//! A turn a limit stopped fails as `usage_limit` with the time the limit
//! resets, which the thread's limit recovery (`usage_recovery`) waits for.

#![expect(clippy::disallowed_types, reason = "the providers' rate limit messages, read as each sends them")]

use serde_json::Value;
use std::collections::HashMap;
use std::sync::RwLock;
use domovoi_model::typed::{ForwardList, ServerProviderUsageLimits, ServerProviderUsageWindow, ServerProviderUsageWindowKind, Timestamp};

static LIMITS: RwLock<Option<HashMap<String, ServerProviderUsageLimits>>> = RwLock::new(None);

const SESSION_MINS: u64 = 5 * 60;
const WEEK_MINS: u64 = 7 * 24 * 60;
const MONTH_MINS: u64 = 30 * 24 * 60;

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
        let checked_at = Timestamp::from(domovoi_contracts::iso_now());
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
        let changed = list != before;
        entry.windows = list.into();
        changed
    };
    if changed {
        crate::providers::CLAUDE_COMMANDS_CHANGED.notify_one();
    }
}

fn iso_from_epoch_seconds(v: &Value) -> Option<Timestamp> {
    let secs = v.as_f64().filter(|s| s.is_finite() && *s > 0.0)?;
    Some(Timestamp::from(domovoi_contracts::time_fmt::iso_from_millis((secs * 1000.0) as i64)))
}

fn percent(v: f64) -> serde_json::Number {
    serde_json::Number::from_f64(v.clamp(0.0, 100.0)).unwrap_or_else(|| serde_json::Number::from(0))
}

fn window(id: &str, kind: ServerProviderUsageWindowKind, label: &str, used: f64, resets_at: Option<Timestamp>, mins: u64) -> ServerProviderUsageWindow {
    ServerProviderUsageWindow { id: id.to_owned(), kind, label: label.to_owned(), model_ids: None, provider_ids: None, used_percent: percent(used), resets_at, window_duration_mins: Some(mins) }
}

/// The window a Claude `rate_limit_info` reports (utilization 0-1), for its
/// five-hour and weekly limits (t3code `claudeRateLimitEventToUpdate`).
pub fn claude_window(info: &Value) -> Option<ServerProviderUsageWindow> {
    let used = info["utilization"].as_f64()? * 100.0;
    let resets_at = iso_from_epoch_seconds(&info["resetsAt"]);
    match info["rateLimitType"].as_str()? {
        "five_hour" => Some(window("five_hour", ServerProviderUsageWindowKind::Session, "Session", used, resets_at, SESSION_MINS)),
        "seven_day" => Some(window("seven_day", ServerProviderUsageWindowKind::Weekly, "Weekly", used, resets_at, WEEK_MINS)),
        _ => None,
    }
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

/// The windows of a Codex rate limit snapshot (t3code `codexRateLimitsToWindows`):
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
        .max_by(|a, b| a.as_str().cmp(b.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_events_become_windows_and_say_when_blocked() {
        // As Claude Code 2.1.319 sends one.
        let info = json!({ "status": "allowed_warning", "resetsAt": 1795186800, "rateLimitType": "seven_day", "utilization": 0.95, "overageStatus": "rejected", "isUsingOverage": false });
        let w = claude_window(&info).unwrap();
        assert_eq!((w.id.as_str(), w.label.as_str(), w.used_percent.as_f64()), ("seven_day", "Weekly", Some(95.0)));
        assert_eq!(w.resets_at.unwrap().as_str(), "2026-11-20T15:00:00.000Z");
        assert!(!claude_blocked(&info));
        assert!(claude_blocked(&json!({ "status": "rejected", "overageStatus": "rejected" })));
        assert!(!claude_blocked(&json!({ "status": "rejected", "overageStatus": "allowed" })), "overage carries on");
        assert!(claude_window(&json!({ "rateLimitType": "seven_day_opus", "utilization": 0.5 })).is_none());
    }

    #[test]
    fn codex_snapshots_become_windows() {
        // As codex-cli 0.170 sends one.
        let snapshot = json!({ "limitId": "codex", "primary": { "usedPercent": 100, "windowDurationMins": 300, "resetsAt": 1794601906 },
            "secondary": { "usedPercent": 4, "windowDurationMins": 10080, "resetsAt": 1794993523 }, "planType": "plus", "individualLimit": null });
        let w = codex_windows(&snapshot);
        assert_eq!(w.iter().map(|w| (w.id.as_str(), w.label.as_str())).collect::<Vec<_>>(), [("primary", "Session"), ("secondary", "Weekly")]);
        assert!(codex_windows(&json!({ "limitId": "gpt-6-sol", "primary": { "usedPercent": 1 } })).is_empty(), "model limits are not the account's");
        merge("codex-test", w);
        assert_eq!(exhausted_reset_at("codex-test"), codex_windows(&snapshot)[0].resets_at);
        assert!(codex_usage_error(&json!("usageLimitExceeded")));
        assert!(codex_usage_error(&json!({ "responseTooManyFailedAttempts": { "httpStatusCode": 429 } })));
        assert!(!codex_usage_error(&json!({ "httpConnectionFailed": { "httpStatusCode": 502 } })));
    }
}
