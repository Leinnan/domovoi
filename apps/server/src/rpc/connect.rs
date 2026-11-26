//! Linked environments (`peerLinks.*`, over iroh: `crate::peer_links`) and
//! the thread card that answers an agent's request for a secret
//! (`secrets.answerRequest`): every lookup the answer makes before acting
//! (unknown thread or item, already answered, stopped run).

#![expect(clippy::disallowed_types, reason = "the `type` of an answer to a card, whatever its shape")]

use super::admin_failure::{Cause, Failure, SecretRequestReason};
use crate::state::{AppState, ConnCtx};
use effect_rpc::Router;
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use domovoi_model::typed::{Run, SecretRequestItem, SecretRequestStatus, TurnItem};

// ---- relay client ----

// ---- peer links and guest shares ----

/// `peerLinks.list`.
#[derive(Serialize)]
struct PeerLinks {
    links: Vec<crate::peer_links::Summary>,
}

/// `peerLinks.unlink` (`PeerLinkRemoveResult`).
#[derive(Serialize)]
struct Unlinked {
    removed: bool,
}

/// `PeerLinkRemoveInput`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnlinkInput {
    environment_id: String,
}

async fn list_links(state: &AppState) -> Result<PeerLinks, Failure> {
    Ok(PeerLinks { links: crate::peer_links::links(state)?.list(state).await? })
}

async fn link(state: &AppState, input: crate::peer_links::CreateInput) -> Result<crate::peer_links::PeerLink, Failure> {
    Ok(crate::peer_links::links(state)?.link(state, input, false).await?.0)
}

async fn unlink(state: &AppState, input: UnlinkInput) -> Result<Unlinked, Failure> {
    Ok(Unlinked { removed: crate::peer_links::links(state)?.unlink(state, &input.environment_id).await? })
}

// ---- thread cards ----

const TERMINAL_RUN_STATUSES: &[&str] = &["completed", "failed", "cancelled", "interrupted", "rolled_back"];

/// The answer to a secret request card. Its shape is read only once the card waits for it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SecretAnswer {
    thread_id: String,
    turn_item_id: String,
    #[serde(deserialize_with = "secret_answer")]
    answer: SecretChoice,
}

/// What the user did with a secret card (`SecretRequestAnswerInput.answer`).
#[derive(Debug, PartialEq)]
enum SecretChoice {
    Save(String),
    Decline,
    /// A secret provider's reference, or an answer of no known shape.
    Unreadable,
}

/// Any shape is taken: an answer of type `decline` declines, one of type
/// `save` saves its trimmed `secret`, anything else cannot be read.
fn secret_answer<'de, D: serde::Deserializer<'de>>(answer: D) -> Result<SecretChoice, D::Error> {
    let answer = serde_json::Value::deserialize(answer)?;
    Ok(match answer["type"].as_str() {
        Some("decline") => SecretChoice::Decline,
        Some("save") => match answer["secret"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
            Some(secret) => SecretChoice::Save(secret.to_owned()),
            None => SecretChoice::Unreadable,
        },
        _ => SecretChoice::Unreadable,
    })
}

/// A thread's turn items and runs.
type ThreadItems = (Vec<Arc<TurnItem>>, Vec<Arc<Run>>);

/// A thread's turn items and runs: an error for an unknown thread, `None` for a
/// deleted one (its cards went with it).
fn thread_items(state: &AppState, thread_id: &str) -> Result<Option<ThreadItems>, String> {
    let store = &state.store;
    let Some(record) = store.app_thread(thread_id).map_err(|e| e.to_string())? else {
        return Err(format!("Thread {thread_id} not found."));
    };
    if record.deleted_at.is_some() {
        return Ok(None);
    }
    Ok(Some((store.records::<TurnItem>(thread_id).map_err(|e| e.to_string())?, store.records::<Run>(thread_id).map_err(|e| e.to_string())?)))
}

fn secret_error(reason: SecretRequestReason, cause: Option<&str>) -> Failure {
    Failure::SecretRequest { reason, cause: cause.map(Cause::of) }
}

/// The card `turn_item_id` of a thread, if it waits for an answer
/// (`SecretRequests.answer`'s checks, in its order).
pub fn waiting_secret_card(threads: Result<Option<ThreadItems>, String>, turn_item_id: &str) -> Result<SecretRequestItem, Failure> {
    let (items, runs) = match threads {
        Ok(Some(t)) => t,
        Ok(None) => return Err(secret_error(SecretRequestReason::NotFound, None)),
        Err(e) => return Err(secret_error(SecretRequestReason::LoadFailed, Some(&e))),
    };
    let card = items.iter().find_map(|item| match &**item {
        TurnItem::SecretRequest(card) if card.id.as_str() == turn_item_id => Some(card),
        _ => None,
    });
    let Some(card) = card else {
        return Err(secret_error(SecretRequestReason::NotFound, None));
    };
    let (Some(run_id), Some(_)) = (&card.run_id, &card.node_id) else {
        return Err(secret_error(SecretRequestReason::NotFound, None));
    };
    if card.secret_status != SecretRequestStatus::Pending {
        return Err(secret_error(SecretRequestReason::AlreadyAnswered, None));
    }
    let run = runs.iter().find(|run| run.id == *run_id);
    if run.is_none_or(|run| TERMINAL_RUN_STATUSES.contains(&run.status.as_str())) {
        return Err(secret_error(SecretRequestReason::AgentStopped, None));
    }
    Ok((**card).clone())
}

/// `secrets.answerRequest` (t3code `SecretRequests.answer`): the value is
/// stored before the card is marked saved, so no card shows saved without
/// it, and is removed again when the card can no longer hand it to the agent.
async fn answer_secret(state: &AppState, input: SecretAnswer) -> Result<(), Failure> {
    use domovoi_model::typed::command::SecretRequestRecord;
    let card = waiting_secret_card(thread_items(state, &input.thread_id), &input.turn_item_id)?;
    let (Some(run_id), Some(node_id)) = (card.run_id.clone(), card.node_id.clone()) else {
        return Err(secret_error(SecretRequestReason::NotFound, None));
    };
    let userdata = &state.paths.userdata;
    let project_id = state.store.app_thread(&input.thread_id).ok().flatten().map(|t| t.project_id.as_str().to_owned()).unwrap_or_default();
    let secret = match input.answer {
        SecretChoice::Save(secret) => Some(secret),
        SecretChoice::Decline => None,
        SecretChoice::Unreadable => return Err(secret_error(SecretRequestReason::ReferenceUnreadable, Some("This server reads no secret providers: enter the value."))),
    };
    if let Some(secret) = &secret {
        crate::secrets::requests::save(userdata, &input.thread_id, card.id.as_str(), &project_id, secret)
            .map_err(|refused| secret_error(SecretRequestReason::StoreFailed, Some(refused.message())))?;
    }
    let status = if secret.is_some() { SecretRequestStatus::Saved } else { SecretRequestStatus::Declined };
    let record = SecretRequestRecord {
        command_id: format!("secret-request:{}:{}", card.id.as_str(), status.as_str()).into(),
        thread_id: input.thread_id.as_str().into(),
        run_id,
        node_id,
        turn_item_id: card.id.clone(),
        label: card.label.clone(),
        reason: card.reason.clone(),
        url: card.url.clone(),
        placeholder: card.placeholder.clone(),
        delivery: card.delivery.clone(),
        secret_status: status,
    };
    if let Err(e) = state.orchestrator.dispatch(record.into()).await {
        // A pending card can be answered again: the failed save goes.
        crate::secrets::requests::forget(userdata, &input.thread_id, card.id.as_str());
        return Err(secret_error(SecretRequestReason::RecordFailed, Some(&e.to_string())));
    }
    if secret.is_none() {
        return Ok(());
    }
    // The wait may have closed the card meanwhile: no one would get its ref.
    let saved = state.store.record::<TurnItem>(&input.thread_id, card.id.as_str()).ok().flatten();
    if !matches!(saved, Some(TurnItem::SecretRequest(card)) if card.secret_status == SecretRequestStatus::Saved) {
        crate::secrets::requests::forget(userdata, &input.thread_id, card.id.as_str());
        return Err(secret_error(SecretRequestReason::AgentStopped, None));
    }
    Ok(())
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.unary("peerLinks.list", |ctx: Arc<ConnCtx>, _: IgnoredAny| async move { list_links(&ctx.state).await })
        .unary("peerLinks.link", |ctx: Arc<ConnCtx>, input: crate::peer_links::CreateInput| async move { link(&ctx.state, input).await })
        .unary("peerLinks.unlink", |ctx: Arc<ConnCtx>, input: UnlinkInput| async move { unlink(&ctx.state, input).await })
        .unary("secrets.answerRequest", |ctx: Arc<ConnCtx>, input: SecretAnswer| async move { answer_secret(&ctx.state, input).await })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};


    /// A turn item of the given type with the fields every item carries.
    fn item(own: Value) -> Value {
        crate::test_records::with(json!({ "id": "i1", "threadId": "t1", "runId": "run-1", "nodeId": "n1", "providerThreadId": "p", "ordinal": 1,
            "status": "running", "updatedAt": crate::test_records::AT }), own)
    }

    fn items(item: Value, run_status: &str) -> Result<Option<ThreadItems>, String> {
        let run = serde_json::from_value(crate::test_records::run("t1", "run-1", run_status)).unwrap();
        Ok(Some((vec![Arc::new(serde_json::from_value(item).unwrap())], vec![Arc::new(run)])))
    }

    #[test]
    fn secret_answers_check_the_card_first() {
        let card = item(json!({ "type": "secret_request", "label": "Token", "reason": "To deploy", "secretStatus": "pending" }));
        let failed = |found: Result<SecretRequestItem, Failure>| json!(found.err().unwrap());
        let reason = |found: Result<SecretRequestItem, Failure>| failed(found)["reason"].clone();
        assert_eq!(failed(waiting_secret_card(Err("gone".into()), "i1")), json!({ "_tag": "SecretRequestError", "reason": "load_failed", "cause": { "message": "gone" } }));
        assert_eq!(failed(waiting_secret_card(Ok(None), "i1")), json!({ "_tag": "SecretRequestError", "reason": "not_found" }));
        assert_eq!(reason(waiting_secret_card(items(card.clone(), "running"), "i2")), "not_found");
        assert_eq!(reason(waiting_secret_card(items(card.clone(), "completed"), "i1")), "agent_stopped");
        assert_eq!(waiting_secret_card(items(card.clone(), "running"), "i1").unwrap().label, "Token");
        let mut saved = card;
        saved["secretStatus"] = json!("saved");
        assert_eq!(reason(waiting_secret_card(items(saved, "running"), "i1")), "already_answered");
    }

    #[test]
    fn secret_answers_of_any_shape_are_taken() {
        let choice = |answer: Value| serde_json::from_value::<SecretAnswer>(json!({ "threadId": "t", "turnItemId": "i", "answer": answer })).unwrap().answer;
        assert_eq!(choice(json!({ "type": "decline" })), SecretChoice::Decline);
        assert_eq!(choice(json!({ "type": "save", "secret": "  s3cret " })), SecretChoice::Save("s3cret".into()));
        for other in [json!({ "type": "save", "secret": " " }), json!({ "type": "reference", "reference": { "provider": "op", "reference": "x" } }), json!({ "type": 5 }), json!({}), json!("decline"), json!(null), json!(["decline"])] {
            assert_eq!(choice(other.clone()), SecretChoice::Unreadable, "{other}");
        }
        assert!(serde_json::from_value::<SecretAnswer>(json!({ "threadId": "t", "turnItemId": "i" })).is_err(), "an answer is required");
    }
}
