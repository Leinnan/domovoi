//! Linked environment tools (t3code `mcp/toolkits/orchestrator`:
//! `t3_environment_links` and `t3_environment_unlink`, after
//! `PeerForwarding.links`): an agent lists the environments this server is
//! linked to over iroh (`crate::peer_links`), with whether each answers now,
//! and forgets one. `t3_thread_list` with `environmentId` lists the threads
//! of a linked environment, read through the link (`PeerForwarding.call`).
//!
//! Limits: no `t3_environment_link` (asking the user for a link in a card),
//! and no other tool acts in a linked environment yet (`environmentId` on the
//! other thread tools, `delegate_task` targets). The tools are listed while Linked
//! environments is off too, and answer that it is off.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the MCP wire: tool schemas, tool arguments and tool results")]

use super::{Call, s, tool};
use serde_json::{Value, json};

/// The next step for an agent whose machine is not linked (`NOT_LISTED`).
const NOT_LISTED: &str = "Not listed: not linked yet. Ask the user to link it in Settings, with a pairing link from that machine (domovoi-server pair --remote there).";
/// What an agent hears while the beta is off here (`linkedEnvironmentsOff`).
const LINKS_OFF: &str =
    "Linked environments is off on this machine, so no tool can act on another machine. Leave out environmentId to work here; the user can turn it on in Settings → Labs.";

pub fn tools() -> Vec<Value> {
    vec![
        tool(
            "t3_environment_links",
            "List linked environments (the user's other machines) and whether each answers now. Expired: the user links it again with a new pairing code from that machine.",
            json!({}),
            &[],
            true,
        ),
        tool(
            "t3_environment_unlink",
            "Forget a linked environment. Its session there is signed out when it answers; otherwise that machine's Connections list it until revoked there.",
            json!({ "environmentId": s("The linked environment to forget, from t3_environment_links.") }),
            &["environmentId"],
            false,
        ),
    ]
}

pub async fn call(c: &Call<'_>, name: &str) -> Option<Result<Value, String>> {
    Some(match name {
        "t3_environment_links" => links(c).await,
        "t3_environment_unlink" => unlink(c).await,
        _ => return None,
    })
}

/// A tool call that names a linked environment (`environmentId` other than
/// this one's), run there through the link; `None` for a call made here.
pub async fn forwarded(c: &Call<'_>, name: &str) -> Option<Result<Value, String>> {
    let target = c.str("environmentId").filter(|id| *id != c.state.environment.environment_id)?;
    Some(match name {
        "t3_thread_list" => list_threads_there(c, &target).await,
        _ => return None,
    })
}

/// `t3_thread_list` in a linked environment: its threads (every project
/// there) from its shell snapshot, newest first, read with the link's token.
async fn list_threads_there(c: &Call<'_>, environment_id: &str) -> Result<Value, String> {
    if !crate::peer_links::enabled(c.state).await {
        return Err(LINKS_OFF.into());
    }
    let links = crate::peer_links::links(c.state).map_err(|e| e.to_string())?;
    let snapshot: domovoi_model::typed::ShellHttpSnapshot = links.fetch(c.state, environment_id, "/api/orchestration/shell").await.map_err(|e| e.to_string())?;
    let title = c.str("titleContains").map(|t| t.to_lowercase());
    let statuses: Option<Vec<String>> = c.args["statuses"].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect());
    let mut threads: Vec<_> = snapshot
        .threads
        .into_iter()
        .filter(|t| title.as_ref().is_none_or(|q| t.title.to_lowercase().contains(q)))
        .filter(|t| c.args["settled"].as_bool().is_none_or(|want| t.settled_at.is_some() == want))
        .collect();
    threads.sort_by(|a, b| b.updated_at.as_str().cmp(a.updated_at.as_str()));
    let rows: Vec<Value> = threads
        .into_iter()
        .map(|t| json!({ "threadId": t.id, "projectId": t.project_id, "title": t.title, "status": t.status, "updatedAt": t.updated_at,
            "archived": t.archived_at.is_some(), "model": t.model_selection.model, "providerInstanceId": t.provider_instance_id }))
        .filter(|row| statuses.as_ref().is_none_or(|s| row["status"].as_str().is_some_and(|st| s.iter().any(|w| w == st))))
        .collect();
    let from = c.args["cursor"].as_u64().unwrap_or(0) as usize;
    let page: Vec<Value> = rows.iter().skip(from).take(c.limit(20, 100)).cloned().collect();
    let next = (from + page.len() < rows.len()).then_some(from + page.len());
    Ok(json!({ "environmentId": environment_id, "threads": page, "nextCursor": next }))
}

/// `OrchestratorMcpEnvironmentLinksResult`.
async fn links(c: &Call<'_>) -> Result<Value, String> {
    if !crate::peer_links::enabled(c.state).await {
        return Err(LINKS_OFF.into());
    }
    let links = crate::peer_links::links(c.state).map_err(|e| e.to_string())?;
    let listed = links.list(c.state).await.map_err(|e| e.to_string())?;
    let links: Vec<Value> = listed
        .into_iter()
        .map(|summary| {
            json!({
                "environmentId": summary.link.environment_id, "label": summary.link.label, "status": summary.status,
                "access": summary.link.access, "expiresAt": summary.link.expires_at, "lastError": summary.link.last_error,
            })
        })
        .collect();
    Ok(json!({ "environmentId": c.state.environment.environment_id, "links": links, "notListed": NOT_LISTED }))
}

/// `OrchestratorMcpEnvironmentUnlinkResult`. Every agent here reaches the linked machine through the link, so forgetting it is an environment setting.
async fn unlink(c: &Call<'_>) -> Result<Value, String> {
    let environment_id = c.required("environmentId")?;
    let links = crate::peer_links::links(c.state).map_err(|e| e.to_string())?;
    let removed = links.unlink(c.state, &environment_id).await.map_err(|e| e.to_string())?;
    Ok(json!({ "environmentId": environment_id, "removed": removed }))
}
