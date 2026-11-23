//! Images inside a tool call's result (an MCP tool's `{type: "image", data,
//! mimeType}` parts), served by a signed URL (`tool-output-image`): the
//! clients load the preview independently of the tool row.

use crate::{attachments, store::Store, util::sha256_hex};
use base64::Engine;
use domovoi_model::typed::{AssetCreateUrlResult, TurnItem};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const ROUTE_PREFIX: &str = "/api/assets/tool-image";
const KIND: &str = "tool-output-image-v1";
const TTL_MS: i64 = 7 * 24 * 60 * 60_000;
/// The largest image served (decoded).
const MAX_BYTES: usize = 20 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Claims {
    kind: String,
    thread_id: String,
    item_id: String,
    index: usize,
    digest: String,
    expires_at: i64,
}

/// The `index`th image in a tool call's result: its type and bytes.
#[expect(clippy::disallowed_types, reason = "a tool's result is whatever shape the tool defines (MCP `CallToolResult` here)")]
fn image_of(item: &TurnItem, index: usize) -> Option<(&'static str, Vec<u8>)> {
    let output: &serde_json::Value = match item {
        TurnItem::DynamicTool(tool) => tool.output.as_ref()?,
        TurnItem::Other(unknown) if unknown.item_type() == "mcp_tool_call" => unknown.record().get("output")?,
        _ => return None,
    };
    let part = output.get("content")?.as_array()?.iter().filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("image")).nth(index)?;
    let mime = match part.get("mimeType").and_then(|m| m.as_str())? {
        "image/png" => "image/png",
        "image/jpeg" | "image/jpg" => "image/jpeg",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        _ => return None,
    };
    let data = part.get("data")?.as_str()?.trim();
    if data.len() / 4 * 3 > MAX_BYTES {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(data).ok()?;
    (bytes.len() <= MAX_BYTES).then_some((mime, bytes))
}

fn load(store: &Store, thread_id: &str, item_id: &str, index: usize) -> Option<(&'static str, Vec<u8>)> {
    let item = store.record::<TurnItem>(thread_id, item_id).ok()??;
    image_of(&item, index)
}

/// A URL for the `index`th image in item `item_id`'s result, or why there is none.
pub fn issue(userdata: &Path, store: &Store, thread_id: &str, item_id: &str, index: usize) -> Result<AssetCreateUrlResult, attachments::Failure> {
    let err = |message: &str| attachments::Failure::AssetUrl { message: message.to_owned() };
    let (_, bytes) = load(store, thread_id, item_id, index).ok_or_else(|| err("Tool output image was not found."))?;
    let expires_at = domovoi_contracts::now_millis() + TTL_MS;
    let claims = Claims { kind: KIND.into(), thread_id: thread_id.into(), item_id: item_id.into(), index, digest: sha256_hex(&bytes), expires_at };
    let token = attachments::sign_asset_claims(userdata, &claims).map_err(|_| err("Could not sign the URL."))?;
    Ok(AssetCreateUrlResult { relative_url: format!("{ROUTE_PREFIX}/{token}"), expires_at: expires_at.into(), source_path: None, image_dimensions: None, file_revision: None })
}

/// The image behind a URL `issue` handed out, while it is valid and unchanged.
pub fn read(userdata: &Path, store: &Store, token: &str) -> Option<(&'static str, Vec<u8>)> {
    let claims: Claims = attachments::verify_asset_claims(userdata, token)?;
    if claims.kind != KIND || claims.expires_at <= domovoi_contracts::now_millis() {
        return None;
    }
    let (mime, bytes) = load(store, &claims.thread_id, &claims.item_id, claims.index)?;
    (sha256_hex(&bytes) == claims.digest).then_some((mime, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_nth_image_part_is_decoded() {
        let item: TurnItem = serde_json::from_value(json!({
            "type": "dynamic_tool", "id": "i", "threadId": "t", "runId": null, "nodeId": null, "providerThreadId": null, "providerTurnId": null,
            "nativeItemRef": null, "parentItemId": null, "ordinal": 1, "status": "completed", "title": null, "startedAt": null,
            "completedAt": null, "updatedAt": "2026-01-01T00:00:00.000Z", "toolName": "mcp__cua__js", "input": {},
            "output": { "content": [{ "type": "text", "text": "x" }, { "type": "image", "mimeType": "image/png", "data": "aGk=" }, { "type": "image", "mimeType": "image/svg+xml", "data": "aGk=" }] },
        }))
        .expect("a tool item");
        assert_eq!(image_of(&item, 0), Some(("image/png", b"hi".to_vec())));
        assert_eq!(image_of(&item, 1), None, "only raster types are served");
        assert_eq!(image_of(&item, 2), None);
    }
}
