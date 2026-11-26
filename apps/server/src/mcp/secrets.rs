//! `request_secret`: the
//! agent asks the user for one secret through a card in its thread, and
//! waits for the answer. The user's value goes to `crate::secrets::requests`
//! (`secrets.answerRequest`); the card (`secret_request.record`) and this
//! tool's result only ever carry its status, a one-use `secretRef`, or the
//! path of an owner-only file (`delivery: "file"`), never the value.
//!
//! The card lives only as long as this call: a timeout, the run ending, a
//! restart of the run, or the call itself going away closes it as cancelled.
//!
//! Limits: a `secretRef` is spent only by `schedule_task` (a webhook's
//! `signature.secretRef`); a pasted `value` is written to a file at once.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the MCP wire: tool schemas, tool arguments and tool results")]

use super::{Call, n, one_of, s, tool};
use crate::orchestration::events::enc;
use crate::state::AppState;
use domovoi_model::typed::command::SecretRequestRecord;
use domovoi_model::typed::{Command, NodeId, Run, RunId, SecretDelivery, SecretRequestStatus, TurnItem, TurnItemId};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MAX_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const POLL: Duration = Duration::from_millis(500);

/// What the agent reads when its card ends without a secret.
fn unsaved_note(status: &str) -> &'static str {
    match status {
        "declined" => "The user declined. Don't ask for it in chat or another way; say what's blocked and go on without it.",
        "cancelled" => "The card closed with the run. Ask again with a new clientRequestId only if the secret is still needed.",
        _ => "No answer in time. Don't ask in chat; ask again with a new clientRequestId only if it is still needed.",
    }
}

pub fn tools() -> Vec<Value> {
    vec![tool(
        "request_secret",
        "Ask the user for one secret (a login, token, PIN, pairing code or sudo password only they can give) on a single-line card in this thread; never combine values, never ask in chat. Returns a one-use `secretRef` (for `schedule_task`'s webhook `signature.secretRef`), or with `delivery: \"file\"` an owner-only file to read only inside a command, e.g. `sudo -S -p '' <cmd> < \"$file\"`. Sign-in or setup page: `url`. A secret the user already pasted in this thread, for a script: `value` (no card).",
        json!({
            "label": s("Card title, such as 'GitHub webhook secret'."),
            "reason": s("What it is for and where to get it. A link goes in `url`."),
            "url": s("Sign-in or setup page for the value (http or https). Shown as a button."),
            "placeholder": s("Hint shown in the empty field."),
            "delivery": one_of(&["secretRef", "file"], "secretRef (default) for one of Domovoi's tools; file for a command."),
            "value": s("Pasted in this thread; no card."),
            "timeoutMs": n("How long to wait for the user (default 600000, 1000 to 3600000)."),
            "clientRequestId": s("Retry key."),
        }),
        &["label", "reason"],
        false,
    )]
}

pub async fn call(c: &Call<'_>, name: &str) -> Option<Result<Value, String>> {
    Some(match name {
        "request_secret" => request(c).await,
        _ => return None,
    })
}

/// `secretRequestPageUrl`: an http(s) URL with a host.
fn page_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else { return false };
    matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") && rest.split(['/', '?', '#']).next().is_some_and(|host| !host.is_empty())
}

/// The fields of the card, as `secret_request.record` writes them.
struct Card {
    thread_id: String,
    run_id: RunId,
    node_id: NodeId,
    turn_item_id: TurnItemId,
    label: String,
    reason: String,
    url: Option<String>,
    placeholder: Option<String>,
    delivery: Option<SecretDelivery>,
    key: String,
}

impl Card {
    fn record(&self, status: SecretRequestStatus) -> Command {
        Command::SecretRequestRecord(Box::new(SecretRequestRecord {
            command_id: format!("mcp-secret-{}:{}:{}", status.as_str(), self.thread_id, self.key).into(),
            thread_id: self.thread_id.as_str().into(),
            run_id: self.run_id.clone(),
            node_id: self.node_id.clone(),
            turn_item_id: self.turn_item_id.clone(),
            label: self.label.clone(),
            reason: self.reason.clone(),
            url: self.url.clone(),
            placeholder: self.placeholder.clone(),
            delivery: self.delivery.clone(),
            secret_status: status,
        }))
    }

    async fn close(&self, state: &AppState) {
        if let Err(e) = state.orchestrator.dispatch(self.record(SecretRequestStatus::Cancelled)).await {
            tracing::warn!(thread_id = %self.thread_id, "could not close a secret request card: {e}");
        }
    }

    /// The card's status as the thread holds it.
    fn status(&self, state: &AppState) -> Option<SecretRequestStatus> {
        match state.store.record::<TurnItem>(&self.thread_id, self.turn_item_id.as_str()).ok().flatten() {
            Some(TurnItem::SecretRequest(card)) => Some(card.secret_status),
            _ => None,
        }
    }
}

/// Closes the card when the call goes away before it ended (the agent's
/// request was dropped): only this call could hand the agent its answer.
struct OpenCard {
    state: Arc<AppState>,
    card: Option<Card>,
}

impl Drop for OpenCard {
    fn drop(&mut self) {
        if let Some(card) = self.card.take() {
            let state = self.state.clone();
            tokio::spawn(async move { card.close(&state).await });
        }
    }
}

fn is_over(run: &Run) -> bool {
    !(run.status == domovoi_model::typed::RunStatus::Queued || crate::orchestration::commands::ACTIVE_RUN.contains(&run.status.as_str()))
}

async fn request(c: &Call<'_>) -> Result<Value, String> {
    let label = c.required("label")?;
    let reason = c.required("reason")?;
    let url = c.str("url");
    if url.as_deref().is_some_and(|u| !page_url(u)) {
        return Err("invalid_request: `url` must be an http(s) URL.".into());
    }
    let file = match c.str("delivery").as_deref() {
        None | Some("secretRef") => false,
        Some("file") => true,
        Some(other) => return Err(format!("invalid_request: unknown delivery {other}.")),
    };
    let thread_id = c.caller.thread_id.clone();
    let runs = c.store().records::<Run>(&thread_id).map_err(|e| e.to_string())?;
    let run = super::active_run(&runs).filter(|run| run.root_node_id.is_some() && run.provider_instance_id == c.caller.thread.provider_instance_id);
    let Some(run) = run.cloned() else {
        return Err("parent_not_active: Asking for a secret requires an active run owned by this MCP provider session.".into());
    };
    let request_key = c.str("clientRequestId").unwrap_or_else(crate::util::new_uuid);
    let userdata = c.state.paths.userdata.clone();
    // A secret the user already pasted needs no card: it goes straight to a file.
    if let Some(value) = c.args["value"].as_str().filter(|v| !v.is_empty()) {
        if c.str("delivery").as_deref() == Some("secretRef") {
            return Err("invalid_request: A pasted `value` is saved as a file. Leave out `delivery: \"secretRef\"`.".into());
        }
        let path = crate::secrets::requests::save_pasted(&userdata, &thread_id, &format!("{}:{request_key}", run.id.as_str()), value).map_err(|r| r.message().to_owned())?;
        return Ok(json!({ "status": "saved", "file": path.to_string_lossy() }));
    }
    let key = format!("{}:{request_key}", run.id.as_str());
    let Some(node_id) = run.root_node_id.clone() else { return Err("parent_not_active: the run has no root.".into()) };
    let card = Card {
        turn_item_id: format!("turn-item:secret-request:{}:{}", enc(&thread_id), enc(&key)).into(),
        thread_id: thread_id.clone(),
        run_id: run.id.clone(),
        node_id: node_id.clone(),
        label,
        reason,
        url,
        placeholder: c.str("placeholder"),
        delivery: file.then_some(SecretDelivery::File),
        key,
    };
    c.orch().dispatch(card.record(SecretRequestStatus::Pending)).await.map_err(|e| format!("orchestration_error: Could not record the secret request: {e}"))?;
    let timeout = c.args["timeoutMs"].as_u64().map_or(DEFAULT_TIMEOUT, Duration::from_millis).clamp(Duration::from_secs(1), MAX_TIMEOUT);
    let deadline = tokio::time::Instant::now() + timeout;
    let mut open = OpenCard { state: c.state.clone(), card: Some(card) };
    let status = loop {
        let Some(card) = open.card.as_ref() else { break SecretRequestStatus::Cancelled };
        if let Some(status) = card.status(c.state).filter(|s| *s != SecretRequestStatus::Pending) {
            break status;
        }
        let runs = c.store().records::<Run>(&thread_id).map_err(|e| e.to_string())?;
        // A steering restart gave the run a new root: the card belonged to the attempt before.
        let current = runs.iter().find(|r| r.id == card.run_id);
        if current.is_none_or(|r| is_over(r) || r.root_node_id.as_ref() != Some(&card.node_id)) {
            card.close(c.state).await;
            break card.status(c.state).unwrap_or(SecretRequestStatus::Cancelled);
        }
        if tokio::time::Instant::now() >= deadline {
            card.close(c.state).await;
            // Answered just as the wait ran out: that answer stands.
            break card.status(c.state).filter(|s| matches!(s, SecretRequestStatus::Saved | SecretRequestStatus::Declined)).unwrap_or(SecretRequestStatus::Other("timed_out".into()));
        }
        tokio::time::sleep(POLL).await;
    };
    let Some(card) = open.card.take() else { return Err("orchestration_error: the secret request was lost.".into()) };
    if status != SecretRequestStatus::Saved {
        let status = status.as_str().to_owned();
        return Ok(json!({ "status": status, "note": unsaved_note(&status) }));
    }
    if file {
        let path = crate::secrets::requests::save_to_file(&userdata, &thread_id, card.turn_item_id.as_str(), &c.caller.project_id).map_err(|r| format!("orchestration_error: {}", r.message()))?;
        return Ok(json!({ "status": "saved", "file": path.to_string_lossy() }));
    }
    match crate::secrets::requests::saved_ref(&userdata, &thread_id, card.turn_item_id.as_str()) {
        Some(secret_ref) => Ok(json!({ "status": "saved", "secretRef": secret_ref })),
        None => Err("orchestration_error: The user saved the secret, but it could not be read. Ask again with a new clientRequestId.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_pages_open_from_a_card() {
        assert!(page_url("https://github.com/settings/tokens"));
        assert!(page_url("http://localhost:8080"));
        assert!(!page_url("javascript:alert(1)"));
        assert!(!page_url("file:///etc/passwd"));
        assert!(!page_url("https:///nohost"));
        assert!(unsaved_note("declined").starts_with("The user declined"));
        assert!(unsaved_note("timed_out").starts_with("No answer in time"));
    }
}
