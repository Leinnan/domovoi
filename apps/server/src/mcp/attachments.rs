//! Attachment tools (t3code `attachment` toolkit): an agent stages a file
//! or image through a signed upload URL (`crate::attachments`), then sends
//! it as a message to a thread of this project.
//!
//! A pending upload belongs to the thread that prepared it until a message
//! carries it; only that thread may discard it or send it. An attachment that
//! is not pending must already be one of the target thread's messages'.
//!
//! Limits: the owners of pending uploads are kept in memory (pruned after an
//! upload URL's day-and-ten-minutes life), so after a restart a pending upload
//! can no longer be sent or discarded through these tools; no document text is
//! extracted for the agent that receives a file.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the MCP wire: tool schemas, tool arguments and tool results")]

use super::{Call, object, s, tool};
use domovoi_model::typed::{ChatAttachment, ConversationMessage, Run};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// How long the tools remember who prepared an upload: a day after its URL's ten minutes.
const OWNER_TTL_MS: i64 = 24 * 60 * 60 * 1000 + 10 * 60 * 1000;
const MAX_ATTACHMENTS: usize = 8;
const MAX_MESSAGE_CHARS: usize = 120_000;

/// Pending upload id → (thread that prepared it, when). Pruned by age on every prepare.
fn owners() -> &'static Mutex<HashMap<String, (String, i64)>> {
    static O: OnceLock<Mutex<HashMap<String, (String, i64)>>> = OnceLock::new();
    O.get_or_init(Mutex::default)
}

pub fn tools() -> Vec<Value> {
    vec![
        tool("t3_attachment_prepare_upload", "Get an upload URL. POST the file's exact bytes to `uploadUrl` (else this server's `relativeUrl`), then send it with `t3_thread_send_attachments` and the returned `attachmentId`.",
            json!({ "upload": object("{\"type\":\"image\"|\"file\",\"name\":\"shot.png\",\"mimeType\":\"image/png\",\"sizeBytes\":1234}.") }), &["upload"], false),
        tool("t3_attachment_discard", "Discard a pending upload (not a sent attachment).", json!({ "attachmentId": s("From `t3_attachment_prepare_upload`.") }), &["attachmentId"], false),
        tool("t3_thread_send_attachments", "Send uploaded attachments as a new message to a thread of this project (default: this one); it starts the thread or queues behind its work. No retry key. Success does not mean the provider read them or finished.",
            json!({
                "threadId": s("Default: this thread."),
                "message": s("Text sent with them."),
                "attachments": { "type": "array", "minItems": 1, "maxItems": MAX_ATTACHMENTS, "items": object("{\"type\":\"image\"|\"file\",\"id\":\"<attachmentId>\",\"name\":\"...\",\"mimeType\":\"...\",\"sizeBytes\":1234}"), "description": "1 to 8 attachments." },
            }), &["attachments"], false),
    ]
}

pub async fn call(c: &Call<'_>, name: &str) -> Option<Result<Value, String>> {
    Some(match name {
        "t3_attachment_prepare_upload" => prepare(c),
        "t3_attachment_discard" => discard(c),
        "t3_thread_send_attachments" => send(c).await,
        _ => return None,
    })
}

fn now() -> i64 {
    crate::time_fmt::now_millis()
}

fn prepare(c: &Call<'_>) -> Result<Value, String> {
    let request: crate::attachments::UploadRequest = serde_json::from_value(c.args["upload"].clone()).map_err(|e| format!("invalid_request: upload: {e}"))?;
    let url = crate::attachments::issue_upload_url(&c.state.paths.userdata, request).map_err(|e| format!("unavailable: {e}"))?;
    {
        let mut owners = crate::util::locked(owners());
        let at = now();
        owners.retain(|_, (_, issued)| at - *issued <= OWNER_TTL_MS);
        owners.insert(url.attachment_id.clone(), (c.caller.thread_id.clone(), at));
    }
    let mut answer = serde_json::to_value(&url).map_err(|e| e.to_string())?;
    if let Some(origin) = super::ENDPOINT.get().and_then(|e| e.strip_suffix("/mcp")) {
        answer["uploadUrl"] = json!(format!("{origin}{}", url.relative_url));
    }
    Ok(answer)
}

fn prepared_by_caller(c: &Call<'_>, attachment_id: &str) -> bool {
    crate::util::locked(owners()).get(attachment_id).is_some_and(|(owner, _)| *owner == c.caller.thread_id)
}

fn discard(c: &Call<'_>) -> Result<Value, String> {
    let id = c.required("attachmentId")?;
    if !prepared_by_caller(c, &id) {
        return Err("invalid_request: Only the caller that prepared a pending upload can discard it.".into());
    }
    crate::attachments::delete_pending(&c.state.paths.userdata, &id);
    crate::util::locked(owners()).remove(&id);
    Ok(json!({}))
}

fn attachment_id(a: &ChatAttachment) -> Option<&str> {
    match a {
        ChatAttachment::Image(i) => Some(&i.id),
        ChatAttachment::File(f) => Some(&f.id),
        ChatAttachment::Other(_) => None,
    }
}

async fn send(c: &Call<'_>) -> Result<Value, String> {
    let (id, thread) = c.thread()?;
    if thread.archived_at.is_some() {
        return Err("invalid_request: Unarchive the target thread before sending attachments.".into());
    }
    let sent = c.args["attachments"].as_array().cloned().unwrap_or_default();
    if sent.is_empty() || sent.len() > MAX_ATTACHMENTS {
        return Err(format!("invalid_request: send 1 to {MAX_ATTACHMENTS} attachments."));
    }
    let text = c.args["message"].as_str().unwrap_or_default();
    if text.chars().count() > MAX_MESSAGE_CHARS {
        return Err(format!("invalid_request: message is longer than {MAX_MESSAGE_CHARS} characters."));
    }
    let messages = c.store().records::<ConversationMessage>(&id).map_err(|e| e.to_string())?;
    let mut attachments = vec![];
    for raw in sent {
        let mut raw = raw;
        // Screenshot provenance stays in the app; it is no part of what a tool sends.
        if raw["type"] == "image" {
            if let Some(fields) = raw.as_object_mut() {
                fields.remove("source");
            }
        }
        let attachment: ChatAttachment = serde_json::from_value(raw).map_err(|e| format!("invalid_request: attachment: {e}"))?;
        let Some(aid) = attachment_id(&attachment).map(str::to_owned) else { return Err("invalid_request: attachments are images or files.".into()) };
        let owned = messages.iter().flat_map(|m| m.attachments.iter()).find(|held| attachment_id(held) == Some(aid.as_str()));
        let attachment = match owned {
            // Already the thread's: sent as the thread holds it.
            Some(held) => held.clone(),
            None if aid.starts_with("pending-") && prepared_by_caller(c, &aid) => {
                if crate::attachments::stored_file(&c.state.paths.userdata, &aid).is_none() {
                    return Err(format!("invalid_request: attachment {aid} was not uploaded yet: POST its bytes to its upload URL first."));
                }
                attachment
            }
            None => return Err("invalid_request: Attachments must be pending uploads you prepared or already belong to the target thread.".into()),
        };
        attachments.push(attachment);
    }
    let command_id = c.command_id("send-attachments");
    let message_id = format!("message:{command_id}");
    let mut command = json!({ "type": "message.dispatch", "commandId": command_id, "threadId": id, "messageId": message_id, "text": text,
        "attachments": attachments, "createdBy": "agent", "creationSource": "mcp", "dispatchMode": { "type": "queue_after_active" } });
    if id != c.caller.thread_id {
        command["senderThreadId"] = json!(c.caller.thread_id);
    }
    c.dispatch(command).await?;
    {
        let mut owners = crate::util::locked(owners());
        for a in &attachments {
            if let Some(aid) = attachment_id(a) {
                owners.remove(aid);
            }
        }
    }
    let stored = c.store().records::<ConversationMessage>(&id).map_err(|e| e.to_string())?;
    let run_id = stored.iter().find(|m| m.id.as_str() == message_id).and_then(|m| m.run_id.clone());
    let runs = c.store().records::<Run>(&id).map_err(|e| e.to_string())?;
    let status = run_id.as_ref().and_then(|r| runs.iter().find(|x| x.id == *r)).map(|r| r.status.clone());
    Ok(json!({ "threadId": id, "messageId": message_id, "runId": run_id, "status": status }))
}
