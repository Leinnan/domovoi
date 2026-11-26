//! Threads a usage limit stopped.
//!
//! A thread whose latest run failed on a usage limit can be told what to do
//! when the limit resets (`thread.metadata.update` `limitRecovery`): resume
//! (a "Continue where you left off." message once the reset passes) and/or
//! snooze until then. With `autoResumeLimitedThreads` or
//! `snoozeLimitedThreads` on, the server arms that by itself for each
//! limited thread (not for a reset more than a day past). The worker looks
//! every 30 seconds.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "commands sent as their wire JSON, and turn items read as the projection writes them")]

use crate::state::SharedState;
use crate::store::Store;
use serde_json::{Value, json};
use std::time::Duration;
use domovoi_model::typed::{Run, RunStatus, Timestamp, TurnItem};

const LOOK_EVERY: Duration = Duration::from_secs(30);
/// A reset longer ago than this is not acted on by itself.
const STALE_MS: i64 = 24 * 60 * 60 * 1000;
pub const CONTINUE: &str = "Continue where you left off.";

/// When the limit that failed `run` resets, if a usage limit failed it.
pub fn usage_limit_reset(store: &Store, thread_id: &str, run: &Run) -> Option<Timestamp> {
    if run.status != RunStatus::Failed {
        return None;
    }
    let items = store.records::<TurnItem>(thread_id).ok()?;
    items
        .iter()
        .filter_map(|i| serde_json::to_value(&**i).ok())
        .filter(|i| i["type"] == "error" && i["runId"] == json!(run.id) && i["failure"]["class"] == "usage_limit")
        .filter_map(|i| i["failure"]["resetAt"].as_str().map(|r| Timestamp::from(r.to_owned())))
        .last()
}

fn millis(t: &str) -> Option<i64> {
    t.parse::<jiff::Timestamp>().ok().map(|t| t.as_millisecond())
}

pub fn start(state: SharedState) {
    tokio::spawn(async move {
        loop {
            look(&state).await;
            // `DOMOVOI_USAGE_RECOVERY_EVERY_MS` looks more often (for tests).
            let every = std::env::var("DOMOVOI_USAGE_RECOVERY_EVERY_MS").ok().and_then(|v| v.parse().ok()).map_or(LOOK_EVERY, Duration::from_millis);
            tokio::time::sleep(every).await;
        }
    });
}

async fn dispatch(state: &SharedState, command: Value) {
    if let Err(e) = crate::rpc::shell::dispatch_command(&state.orchestrator, command).await {
        tracing::info!("usage limit recovery: {e}");
    }
}

/// One pass over the threads: arm recovery where the settings say to, and
/// resume the threads whose reset has passed.
pub async fn look(state: &SharedState) {
    let (auto_resume, auto_snooze) = {
        let s = state.settings.read().await;
        (s.auto_resume_limited_threads == Some(true), s.snooze_limited_threads == Some(true))
    };
    let now = crate::time_fmt::now_millis();
    let Ok(threads) = state.store.app_threads() else { return };
    for t in threads.into_iter().filter(|t| t.deleted_at.is_none() && t.archived_at.is_none()) {
        let tid = t.id.as_str().to_owned();
        let Ok(runs) = state.store.records::<Run>(&tid) else { continue };
        let Some(latest) = runs.iter().max_by_key(|r| r.ordinal) else { continue };
        let Some(reset) = usage_limit_reset(&state.store, &tid, latest) else { continue };
        let reset_ms = millis(reset.as_str()).unwrap_or(now);
        let recovery = t.limit_recovery.clone().flatten();
        match recovery {
            None if (auto_resume || auto_snooze) && now - reset_ms < STALE_MS => {
                dispatch(state, json!({ "type": "thread.metadata.update", "commandId": format!("usage-limit-arm:{}", latest.id), "threadId": tid,
                    "limitRecovery": { "runId": latest.id, "resetAt": reset, "autoResume": auto_resume, "snooze": auto_snooze } }))
                .await;
            }
            Some(r) if r.run_id == latest.id && r.auto_resume && millis(r.reset_at.as_str()).is_some_and(|at| at <= now) => {
                let snoozed = t.snoozed_until.clone().flatten().and_then(|s| millis(s.as_str())).is_some_and(|until| until > now);
                if snoozed {
                    continue;
                }
                let command_id = format!("usage-limit-continue:{}", latest.id);
                let mut send = json!({ "type": "message.dispatch", "commandId": command_id, "threadId": tid, "messageId": format!("message:{command_id}"),
                    "text": CONTINUE, "attachments": [], "createdBy": "system", "creationSource": "server", "dispatchMode": { "type": "queue_after_active" },
                    "usageLimitContinuationOfRunId": latest.id });
                if let Some(request) = &r.request_id {
                    send["usageLimitRecoveryRequestId"] = json!(request);
                }
                dispatch(state, send).await;
                dispatch(state, json!({ "type": "thread.metadata.update", "commandId": format!("usage-limit-done:{}", latest.id), "threadId": tid, "limitRecovery": null })).await;
            }
            _ => {}
        }
    }
}
