//! Multi-instance and cross-provider usage-limit failover coordinator.
//!
//! Tracks per-instance availability and `limited_until` cooldowns fed both by
//! live quota windows (`usage_limits`) and runtime rate-limit failures, then
//! selects ordered failover candidates across configured `failoverPools`
//! (same driver first, then fallback drivers when allowed).

use domovoi_model::typed::{
    FailoverStatusResult, InstanceAvailability, ModelSelection, ProviderInstanceId, ServerProvider, ServerProviderUsageWindow, ServerSettings,
};
use std::collections::{HashMap, HashSet};
use std::sync::{OnceLock, RwLock};

/// Default backoff when a provider reports a usage limit without a reset time (60 minutes).
const DEFAULT_LIMIT_BACKOFF_MS: i64 = 60 * 60 * 1000;

#[derive(Clone, Debug, Default)]
struct InstanceState {
    limited_until_ms: Option<i64>,
    last_error: Option<String>,
    active_jobs: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailoverCandidate {
    pub instance_id: String,
    pub driver: String,
    pub model: String,
}

impl FailoverCandidate {
    pub fn to_model_selection(&self) -> ModelSelection {
        ModelSelection {
            instance_id: ProviderInstanceId::from(self.instance_id.as_str()),
            model: self.model.clone(),
            options: None,
        }
    }
}

#[derive(Default)]
pub struct FailoverCoordinator {
    states: RwLock<HashMap<String, InstanceState>>,
}

pub fn global() -> &'static FailoverCoordinator {
    static COORD: OnceLock<FailoverCoordinator> = OnceLock::new();
    COORD.get_or_init(FailoverCoordinator::default)
}

/// RAII guard that tracks active job counts per provider instance.
pub struct ActiveJobGuard<'a> {
    coord: &'a FailoverCoordinator,
    instance_id: String,
}

impl Drop for ActiveJobGuard<'_> {
    fn drop(&mut self) {
        let mut map = self.coord.states.write().unwrap_or_else(|e| e.into_inner());
        if let Some(st) = map.get_mut(&self.instance_id) {
            st.active_jobs = st.active_jobs.saturating_sub(1);
        }
    }
}

impl FailoverCoordinator {
    pub fn acquire_job(&self, instance_id: &str) -> ActiveJobGuard<'_> {
        let mut map = self.states.write().unwrap_or_else(|e| e.into_inner());
        map.entry(instance_id.to_owned()).or_default().active_jobs += 1;
        ActiveJobGuard { coord: self, instance_id: instance_id.to_owned() }
    }

    /// Marks `instance_id` rate-limited until `reset_at_iso` (or `now + 60m` if absent/past).
    pub fn mark_limited(&self, instance_id: &str, reset_at_iso: Option<&str>, detail: impl Into<String>) {
        let now = crate::time_fmt::now_millis();
        let until = reset_at_iso
            .and_then(crate::time_fmt::millis_from_iso)
            .filter(|ms| *ms > now)
            .unwrap_or(now + DEFAULT_LIMIT_BACKOFF_MS);
        let mut map = self.states.write().unwrap_or_else(|e| e.into_inner());
        let st = map.entry(instance_id.to_owned()).or_default();
        st.limited_until_ms = Some(st.limited_until_ms.map_or(until, |prev| prev.max(until)));
        st.last_error = Some(detail.into());
    }

    /// Clears recorded limit state for `instance_id` (e.g. after sign-out or reset).
    pub fn clear(&self, instance_id: &str) {
        let mut map = self.states.write().unwrap_or_else(|e| e.into_inner());
        if let Some(st) = map.get_mut(instance_id) {
            st.limited_until_ms = None;
            st.last_error = None;
        }
    }

    /// Syncs proactive cooldown state from live `usage_limits` windows.
    pub fn sync_from_usage_limits(&self, instance_id: &str, windows: &[ServerProviderUsageWindow]) {
        let now = crate::time_fmt::now_millis();
        let exhausted_until = windows
            .iter()
            .filter(|w| w.used_percent.as_f64().is_some_and(|p| p >= 99.5))
            .filter_map(|w| w.resets_at.as_ref().and_then(|t| crate::time_fmt::millis_from_iso(t.as_str())))
            .filter(|ms| *ms > now)
            .max();
        if let Some(until) = exhausted_until {
            let mut map = self.states.write().unwrap_or_else(|e| e.into_inner());
            let st = map.entry(instance_id.to_owned()).or_default();
            st.limited_until_ms = Some(st.limited_until_ms.map_or(until, |prev| prev.max(until)));
            if st.last_error.is_none() {
                st.last_error = Some("Provider usage window is at 100%.".into());
            }
        }
    }

    /// Removes expired cooldowns where `limited_until_ms <= now`.
    pub fn sweep_expired(&self) {
        let now = crate::time_fmt::now_millis();
        let mut map = self.states.write().unwrap_or_else(|e| e.into_inner());
        for st in map.values_mut() {
            if st.limited_until_ms.is_some_and(|ms| ms <= now) {
                st.limited_until_ms = None;
                st.last_error = None;
            }
        }
    }

    pub fn is_limited(&self, instance_id: &str) -> bool {
        let now = crate::time_fmt::now_millis();
        let map = self.states.read().unwrap_or_else(|e| e.into_inner());
        map.get(instance_id).and_then(|s| s.limited_until_ms).is_some_and(|ms| ms > now)
    }

    /// Orders healthy failover candidates for `(primary_instance, primary_model)`.
    pub fn candidates(
        &self,
        primary_instance: &str,
        primary_model: &str,
        settings: &ServerSettings,
        providers: &[ServerProvider],
    ) -> Vec<FailoverCandidate> {
        self.sweep_expired();
        let pools = settings.failover_pools.as_deref().unwrap_or(&[]);
        let matching_pool = pools
            .iter()
            .find(|pool| pool.instance_ids.iter().any(|id| id.as_str() == primary_instance))
            .or_else(|| pools.first());

        let (ordered_ids, same_driver_first, allow_cross_provider): (Vec<String>, bool, bool) = match matching_pool {
            Some(pool) => (
                pool.instance_ids.iter().map(|id| id.as_str().to_owned()).collect(),
                pool.same_driver_first,
                pool.allow_cross_provider,
            ),
            None => {
                // Implicit pool: primary first, then other enabled ready instances.
                let mut ids = vec![primary_instance.to_owned()];
                for p in providers {
                    let id = p.instance_id.as_str();
                    if id != primary_instance && p.enabled && p.installed {
                        ids.push(id.to_owned());
                    }
                }
                (ids, true, false)
            }
        };

        let driver_of = |id: &str| -> String {
            providers
                .iter()
                .find(|p| p.instance_id.as_str() == id)
                .map(|p| p.driver.as_str().to_owned())
                .unwrap_or_else(|| crate::orchestration::commands::driver_for_instance(id))
        };
        let default_model_of = |id: &str, drv: &str| -> Option<String> {
            if let Some(p) = providers.iter().find(|p| p.instance_id.as_str() == id)
                && let Some(m) = p.models.iter().find(|m| !m.slug.is_empty())
            {
                return Some(m.slug.clone());
            }
            match drv {
                "claudeAgent" => Some("claude-sonnet-4-6".into()),
                "codex" => Some("gpt-5.3-codex".into()),
                _ => None,
            }
        };
        let is_usable = |id: &str| -> bool {
            if self.is_limited(id) {
                return false;
            }
            // If we have a provider snapshot, ensure the instance is enabled and installed.
            if let Some(p) = providers.iter().find(|p| p.instance_id.as_str() == id) {
                return p.enabled && p.installed;
            }
            true
        };

        let primary_driver = driver_of(primary_instance);
        let mut seen = HashSet::new();
        let mut same_driver = Vec::new();
        let mut other_driver = Vec::new();

        // Always consider primary first if usable.
        let mut sequence = Vec::with_capacity(ordered_ids.len() + 1);
        sequence.push(primary_instance.to_owned());
        sequence.extend(ordered_ids);

        for id in sequence {
            if !seen.insert(id.clone()) || !is_usable(&id) {
                continue;
            }
            let drv = driver_of(&id);
            let same = drv == primary_driver;
            if !same && !allow_cross_provider {
                continue;
            }
            let model = if same && !primary_model.is_empty() {
                primary_model.to_owned()
            } else if let Some(m) = default_model_of(&id, &drv) {
                m
            } else {
                continue;
            };
            let cand = FailoverCandidate { instance_id: id, driver: drv, model };
            if same {
                same_driver.push(cand);
            } else {
                other_driver.push(cand);
            }
        }

        if same_driver_first {
            same_driver.extend(other_driver);
            same_driver
        } else {
            // Preserve pool order strictly (primary was inserted first).
            let mut merged = same_driver;
            merged.extend(other_driver);
            merged
        }
    }

    /// Builds live failover status for `providers.failoverStatus`.
    pub fn status(&self, settings: &ServerSettings, providers: &[ServerProvider]) -> FailoverStatusResult {
        self.sweep_expired();
        let now = crate::time_fmt::now_millis();
        let map = self.states.read().unwrap_or_else(|e| e.into_inner());
        let instances = providers
            .iter()
            .filter(|p| p.enabled)
            .map(|p| {
                let id = p.instance_id.as_str();
                let st = map.get(id);
                let limited_until = st.and_then(|s| s.limited_until_ms).filter(|ms| *ms > now).map(crate::time_fmt::iso_from_millis);
                InstanceAvailability {
                    instance_id: id.to_owned(),
                    driver: p.driver.as_str().to_owned(),
                    display_name: p.display_name.clone().unwrap_or_else(|| id.to_owned()),
                    available: p.installed && limited_until.is_none(),
                    limited_until,
                    last_error: st.and_then(|s| s.last_error.clone()),
                    active_jobs: st.map_or(0, |s| s.active_jobs),
                }
            })
            .collect();
        FailoverStatusResult {
            pools: settings.failover_pools.clone().unwrap_or_default(),
            instances,
        }
    }
}

/// Returns `Some(optional_reset_iso)` when an error message indicates a usage/rate limit.
pub fn classify_usage_limit_error(detail: &str) -> Option<Option<String>> {
    let lower = detail.to_ascii_lowercase();
    let is_limit = lower.contains("usage limit")
        || lower.contains("rate limit")
        || lower.contains("rate_limit")
        || lower.contains("too many requests")
        || lower.contains("http 429")
        || lower.contains("status 429")
        || lower.contains("hit your limit")
        || lower.contains("quota exceeded")
        || lower.contains("out of extra usage");
    if !is_limit {
        return None;
    }
    // Extract an ISO-8601 timestamp if present in the message.
    let reset = detail
        .split_whitespace()
        .map(|tok| tok.trim_matches(|c: char| matches!(c, '.' | ',' | ';' | ')' | '(' | '"' | '\'')))
        .find(|tok| tok.contains('T') && tok.ends_with('Z') && crate::time_fmt::millis_from_iso(tok).is_some())
        .map(str::to_owned);
    Some(reset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use domovoi_model::typed::{FailoverPoolConfig, ServerProviderUsageWindowKind, Timestamp};

    fn provider(id: &str, driver: &str, model: &str) -> ServerProvider {
        serde_json::from_value(serde_json::json!({
            "instanceId": id,
            "driver": driver,
            "displayName": id,
            "enabled": true,
            "installed": true,
            "status": "ready",
            "auth": { "status": "authenticated" },
            "checkedAt": "2026-01-01T00:00:00.000Z",
            "models": [{ "slug": model, "name": model, "isCustom": false, "isDefault": true }],
            "slashCommands": [],
            "skills": [],
        }))
        .unwrap()
    }

    #[test]
    fn prefers_same_driver_then_cross_provider_and_recovers_after_reset() {
        let coord = FailoverCoordinator::default();
        let providers = vec![
            provider("claude-1", "claudeAgent", "claude-opus-4-6"),
            provider("claude-2", "claudeAgent", "claude-opus-4-6"),
            provider("codex-1", "codex", "gpt-5.3-codex"),
        ];
        let settings = ServerSettings {
            failover_pools: Some(vec![FailoverPoolConfig {
                id: "default".into(),
                name: "Default".into(),
                instance_ids: vec![
                    ProviderInstanceId::from("claude-1"),
                    ProviderInstanceId::from("codex-1"),
                    ProviderInstanceId::from("claude-2"),
                ],
                same_driver_first: true,
                allow_cross_provider: true,
            }]),
            ..ServerSettings::default()
        };

        let cands = coord.candidates("claude-1", "claude-opus-4-6", &settings, &providers);
        let ids: Vec<&str> = cands.iter().map(|c| c.instance_id.as_str()).collect();
        assert_eq!(ids, vec!["claude-1", "claude-2", "codex-1"]);

        // Mark claude-1 limited -> claude-2 takes over first, then codex-1.
        let future = crate::time_fmt::iso_from_millis(crate::time_fmt::now_millis() + 60_000);
        coord.mark_limited("claude-1", Some(&future), "hit usage limit");
        let cands = coord.candidates("claude-1", "claude-opus-4-6", &settings, &providers);
        let ids: Vec<&str> = cands.iter().map(|c| c.instance_id.as_str()).collect();
        assert_eq!(ids, vec!["claude-2", "codex-1"]);

        // Proactive sync from 100% usage window marks claude-2 limited too -> falls over to codex-1.
        coord.sync_from_usage_limits(
            "claude-2",
            &[ServerProviderUsageWindow {
                id: "session".into(),
                kind: ServerProviderUsageWindowKind::Session,
                label: "Session".into(),
                model_ids: None,
                provider_ids: None,
                used_percent: serde_json::Number::from(100),
                resets_at: Some(Timestamp::from(future)),
                window_duration_mins: Some(300),
            }],
        );
        let cands = coord.candidates("claude-1", "claude-opus-4-6", &settings, &providers);
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].instance_id, "codex-1");
        assert_eq!(cands[0].model, "gpt-5.3-codex");

        // Clearing restores claude-1.
        coord.clear("claude-1");
        assert!(!coord.is_limited("claude-1"));
    }

    #[test]
    fn classifies_rate_limit_messages_and_extracts_reset_iso() {
        assert_eq!(classify_usage_limit_error("Syntax error in JSON"), None);
        assert_eq!(
            classify_usage_limit_error("You hit your limit; resets at 2026-11-27T00:00:00.000Z."),
            Some(Some("2026-11-27T00:00:00.000Z".into()))
        );
        assert_eq!(classify_usage_limit_error("HTTP 429 Too Many Requests"), Some(None));
    }
}
