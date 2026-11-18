//! Features that reach beyond this machine: the T3 Connect relay client
//! (`cloud.*`), linked environments (`peerLinks.*`), guest sharing
//! (`guestShare.create`), and the thread cards that answer agent requests for
//! them or for secrets and MCP Apps (`peerLinks.answerRequest`,
//! `secrets.answerRequest`, `mcpApps.*`).
//!
//! What is real:
//! - `cloud.getRelayClientStatus` resolves cloudflared like t3code's
//!   `relayClient.resolve`: `T3CODE_CLOUDFLARED_PATH`, a managed copy under the
//!   Domovoi home (`relay-client/<version>/<platform>-<arch>/`), then `PATH`, each
//!   accepted only when `cloudflared version` reports the minimum version;
//! - the settings gates (`guestSharingEnabled`, `linkedEnvironmentsEnabled`)
//!   and every lookup a card answer makes before acting (unknown thread or
//!   item, already answered, stopped run, not an MCP App).
//!
//! Limits (Domovoi contacts no remote service and installs nothing):
//! - `cloud.installRelayClient` never downloads: with no usable client it fails
//!   with `managed_by_policy` and says how to provide one;
//! - no peer links are stored, so the list is empty, linking is `disabled`
//!   and unlinking removes nothing; guest shares need T3 Connect
//!   (`connect_unavailable`);
//! - Domovoi has no secret store, no peer links and no MCP App bridge to its
//!   providers, so a card that passes every check still fails: secrets with
//!   `store_failed` (or `record_failed` for a decline), MCP Apps with
//!   `provider-unsupported`. Domovoi's providers never create secret or link cards.

#![expect(clippy::disallowed_types, reason = "the `type` of an answer to a card, whatever its shape")]

use super::Never;
use super::admin_failure::{Cause, Failure, GuestShareReason, McpAppReason, PeerLinkReason, PeerLinkRequestReason, RelayInstallReason, SecretRequestReason};
use crate::state::{AppState, ConnCtx};
use effect_rpc::Router;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use domovoi_model::typed::{LinkRequestStatus, Run, SecretRequestStatus, TurnItem};

// ---- relay client ----

const CLOUDFLARED_VERSION: &str = "2026.10.4";
const CLOUDFLARED_MIN_VERSION: &str = "2025.6.1";
const CLOUDFLARED_PATH_ENV: &str = "T3CODE_CLOUDFLARED_PATH";
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// `YYYY.M.P` from `cloudflared version` output.
pub fn parse_cloudflared_version(output: &str) -> Option<(u32, u32, u32)> {
    output.split(|c: char| !(c.is_ascii_digit() || c == '.')).find_map(|word| {
        let mut it = word.split('.');
        let (y, m, p) = (it.next()?, it.next()?, it.next()?);
        (y.len() == 4 && (1..=2).contains(&m.len()) && !p.is_empty())
            .then(|| Some((y.parse().ok()?, m.parse().ok()?, p.parse().ok()?)))
            .flatten()
    })
}

fn version_tuple(v: &str) -> (u32, u32, u32) {
    parse_cloudflared_version(v).unwrap_or((0, 0, 0))
}

fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

fn node_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    }
}

fn executable_name() -> &'static str {
    if cfg!(windows) { "cloudflared.exe" } else { "cloudflared" }
}

/// Where a usable relay client was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RelayClientSource {
    /// Named by `T3CODE_CLOUDFLARED_PATH`.
    Override,
    /// The copy under the Domovoi home.
    Managed,
    Path,
}

/// Whether this machine has a relay client the server can run (`RelayClientStatus`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum RelayClientStatus {
    #[serde(rename_all = "camelCase")]
    Available { executable_path: String, source: RelayClientSource, version: String },
    /// None found, or none new enough; `version` is the one t3code would install.
    Missing { version: &'static str },
    /// No relay client exists for this platform.
    Unsupported { platform: &'static str, arch: &'static str, version: &'static str },
}

/// A step of installing the relay client: Domovoi only ever checks.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum InstallStage {
    Checking,
}

/// What `cloud.installRelayClient` streams (`RelayClientInstallProgressEvent`).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum InstallEvent {
    Progress { stage: InstallStage },
    Complete { status: RelayClientStatus },
}

async fn compatible(path: &Path, source: RelayClientSource) -> Option<RelayClientStatus> {
    if !path.is_file() {
        return None;
    }
    let probe = crate::process::command(path).arg("version").stdin(std::process::Stdio::null()).kill_on_drop(true).output();
    let out = tokio::time::timeout(VERSION_PROBE_TIMEOUT, probe).await.ok()?.ok()?;
    let (y, m, p) = parse_cloudflared_version(&String::from_utf8_lossy(&out.stdout))?;
    if (y, m, p) < version_tuple(CLOUDFLARED_MIN_VERSION) {
        return None;
    }
    Some(RelayClientStatus::Available { executable_path: path.to_string_lossy().into_owned(), source, version: format!("{y}.{m}.{p}") })
}

fn on_path() -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(executable_name())).find(|p| p.is_file())
}

async fn relay_client_status(state: &AppState) -> RelayClientStatus {
    if let Some(path) = std::env::var_os(CLOUDFLARED_PATH_ENV).filter(|p| !p.is_empty()) {
        return compatible(Path::new(&path), RelayClientSource::Override).await.unwrap_or_else(missing);
    }
    let platform_dir = format!("{}-{}", node_platform(), node_arch());
    let managed = state.paths.home.join("relay-client").join(CLOUDFLARED_VERSION).join(&platform_dir).join(executable_name());
    if let Some(s) = compatible(&managed, RelayClientSource::Managed).await {
        return s;
    }
    if let Some(s) = match on_path() {
        Some(p) => compatible(&p, RelayClientSource::Path).await,
        None => None,
    } {
        return s;
    }
    let supported = matches!(node_platform(), "darwin" | "linux" | "win32") && matches!(node_arch(), "x64" | "arm64");
    if supported {
        missing()
    } else {
        RelayClientStatus::Unsupported { platform: node_platform(), arch: node_arch(), version: CLOUDFLARED_VERSION }
    }
}

fn missing() -> RelayClientStatus {
    RelayClientStatus::Missing { version: CLOUDFLARED_VERSION }
}

fn install_stream(ctx: Arc<ConnCtx>) -> BoxStream<'static, Result<InstallEvent, Failure>> {
    async_stream::stream! {
        yield Ok(InstallEvent::Progress { stage: InstallStage::Checking });
        let reason = match relay_client_status(&ctx.state).await {
            status @ RelayClientStatus::Available { .. } => {
                yield Ok(InstallEvent::Complete { status });
                return;
            }
            RelayClientStatus::Unsupported { .. } => RelayInstallReason::UnsupportedPlatform,
            RelayClientStatus::Missing { .. } => RelayInstallReason::ManagedByPolicy,
        };
        yield Err(Failure::RelayClientInstall {
            reason,
            message: format!(
                "This Domovoi server does not download the relay client. Install cloudflared {CLOUDFLARED_MIN_VERSION} or newer on PATH, or set {CLOUDFLARED_PATH_ENV} to it."
            ),
        });
    }
    .boxed()
}

// ---- peer links and guest shares ----

const LINKED_ENVIRONMENTS_OFF: &str = "Linked environments is off on this machine. Turn it on in Settings → Labs.";
const LINKED_ENVIRONMENTS_UNAVAILABLE: &str = "Linked environments are not available on this Domovoi server: it does not contact other environments.";

/// `peerLinks.list`: Domovoi stores no links.
#[derive(Serialize)]
struct PeerLinks {
    links: [Never; 0],
}

/// `peerLinks.unlink` (`PeerLinkRemoveResult`): there was nothing to remove.
#[derive(Serialize)]
struct Unlinked {
    removed: bool,
}

async fn link(state: &AppState) -> Failure {
    let message = if state.settings.read().await.linked_environments_enabled == Some(true) { LINKED_ENVIRONMENTS_UNAVAILABLE } else { LINKED_ENVIRONMENTS_OFF };
    Failure::PeerLink { reason: PeerLinkReason::Disabled, message }
}

async fn guest_share(state: &AppState) -> Failure {
    let reason = if state.settings.read().await.guest_sharing_enabled == Some(true) { GuestShareReason::ConnectUnavailable } else { GuestShareReason::Disabled };
    Failure::GuestShareCreate { reason }
}

// ---- thread cards ----

const TERMINAL_RUN_STATUSES: &[&str] = &["completed", "failed", "cancelled", "interrupted", "rolled_back"];

/// The answer to a link request card. What it says is not read: Domovoi links nothing either way.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LinkAnswer {
    thread_id: String,
    turn_item_id: String,
    #[serde(rename = "answer")]
    _answer: IgnoredAny,
}

/// The answer to a secret request card: of what it says, only whether it declines is read.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SecretAnswer {
    thread_id: String,
    turn_item_id: String,
    #[serde(rename = "answer", deserialize_with = "declines")]
    declined: bool,
}

/// Whether a card's answer, whatever its shape, is of type `decline`.
fn declines<'de, D: serde::Deserializer<'de>>(answer: D) -> Result<bool, D::Error> {
    Ok(serde_json::Value::deserialize(answer)?["type"] == "decline")
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

/// `SecretRequests.answer` up to the point where t3code stores the value, or
/// records that the user declined (`declined`).
pub fn answer_secret(threads: Result<Option<ThreadItems>, String>, turn_item_id: &str, declined: bool) -> Failure {
    let (items, runs) = match threads {
        Ok(Some(t)) => t,
        Ok(None) => return secret_error(SecretRequestReason::NotFound, None),
        Err(e) => return secret_error(SecretRequestReason::LoadFailed, Some(&e)),
    };
    let card = items.iter().find_map(|item| match &**item {
        TurnItem::SecretRequest(card) if card.id.as_str() == turn_item_id => Some(card),
        _ => None,
    });
    let Some(card) = card else {
        return secret_error(SecretRequestReason::NotFound, None);
    };
    let (Some(run_id), Some(_)) = (&card.run_id, &card.node_id) else {
        return secret_error(SecretRequestReason::NotFound, None);
    };
    if card.secret_status != SecretRequestStatus::Pending {
        return secret_error(SecretRequestReason::AlreadyAnswered, None);
    }
    let run = runs.iter().find(|run| run.id == *run_id);
    if run.is_none_or(|run| TERMINAL_RUN_STATUSES.contains(&run.status.as_str())) {
        return secret_error(SecretRequestReason::AgentStopped, None);
    }
    if declined {
        secret_error(SecretRequestReason::RecordFailed, Some("This Domovoi server cannot record secret request answers."))
    } else {
        secret_error(SecretRequestReason::StoreFailed, Some("This Domovoi server has no secret store."))
    }
}

fn peer_request_error(reason: PeerLinkRequestReason, cause: Option<&str>) -> Failure {
    Failure::PeerLinkRequest { reason, cause: cause.map(Cause::of) }
}

/// `PeerLinkRequests.answer` up to the point where t3code links or records the answer.
pub fn answer_peer_link(threads: Result<Option<ThreadItems>, String>, turn_item_id: &str) -> Failure {
    let items = match threads {
        Ok(Some((items, _))) => items,
        Ok(None) => return peer_request_error(PeerLinkRequestReason::NotFound, None),
        Err(e) => return peer_request_error(PeerLinkRequestReason::LoadFailed, Some(&e)),
    };
    let card = items.iter().find_map(|item| match &**item {
        TurnItem::LinkRequest(card) if card.id.as_str() == turn_item_id => Some(card),
        _ => None,
    });
    let Some(card) = card else {
        return peer_request_error(PeerLinkRequestReason::NotFound, None);
    };
    if card.run_id.is_none() || card.node_id.is_none() {
        return peer_request_error(PeerLinkRequestReason::NotFound, None);
    }
    if card.link_status != LinkRequestStatus::Pending {
        return peer_request_error(PeerLinkRequestReason::AlreadyAnswered, None);
    }
    peer_request_error(PeerLinkRequestReason::RecordFailed, Some(LINKED_ENVIRONMENTS_UNAVAILABLE))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpAppInput {
    thread_id: String,
    item_id: String,
}

fn mcp_error(thread_id: &str, reason: McpAppReason) -> Failure {
    Failure::McpAppRequest { thread_id: thread_id.to_owned(), reason }
}

/// `mcpAppFromToolItem`: a dynamic tool item whose output names the app it
/// renders, with server and tool matching the item's own tool name.
pub fn is_mcp_app_item(item: &TurnItem) -> bool {
    let TurnItem::DynamicTool(call) = item else { return false };
    if call.provider_thread_id.is_none() {
        return false;
    }
    // What the tool answered is the tool's to shape (the item's `output` is
    // `Schema.Unknown`): an app names itself in it.
    let Some(app) = call.output.as_ref().and_then(|output| output.get("t3McpApp")) else { return false };
    let (Some(server), Some(tool), Some(name)) = (app["server"].as_str(), app["tool"].as_str(), call.tool_name.as_deref()) else {
        return false;
    };
    name == format!("{server}.{tool}") || name == format!("mcp__{server}__{tool}")
}

fn mcp_app(state: &AppState, input: McpAppInput) -> Failure {
    let item = state.store.record::<TurnItem>(&input.thread_id, &input.item_id).ok().flatten();
    if item.as_ref().is_some_and(is_mcp_app_item) {
        mcp_error(&input.thread_id, McpAppReason::ProviderUnsupported)
    } else {
        mcp_error(&input.thread_id, McpAppReason::NotAnApp)
    }
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.unary("cloud.getRelayClientStatus", |ctx: Arc<ConnCtx>, _: IgnoredAny| async move {
        Ok::<_, Never>(relay_client_status(&ctx.state).await)
    })
    .stream("cloud.installRelayClient", |ctx, _: IgnoredAny| install_stream(ctx))
    .unary("peerLinks.list", |_, _: IgnoredAny| async { Ok::<_, Never>(PeerLinks { links: [] }) })
    .unary("peerLinks.link", |ctx: Arc<ConnCtx>, _: IgnoredAny| async move { Err::<Never, _>(link(&ctx.state).await) })
    .unary("peerLinks.unlink", |_, _: IgnoredAny| async { Ok::<_, Never>(Unlinked { removed: false }) })
    .unary("peerLinks.answerRequest", |ctx: Arc<ConnCtx>, input: LinkAnswer| async move {
        Err::<Never, _>(answer_peer_link(thread_items(&ctx.state, &input.thread_id), &input.turn_item_id))
    })
    .unary("guestShare.create", |ctx: Arc<ConnCtx>, _: IgnoredAny| async move { Err::<Never, _>(guest_share(&ctx.state).await) })
    .unary("secrets.answerRequest", |ctx: Arc<ConnCtx>, input: SecretAnswer| async move {
        Err::<Never, _>(answer_secret(thread_items(&ctx.state, &input.thread_id), &input.turn_item_id, input.declined))
    })
    .unary("mcpApps.callTool", |ctx: Arc<ConnCtx>, input: McpAppInput| async move { Err::<Never, _>(mcp_app(&ctx.state, input)) })
    .unary("mcpApps.toolInfo", |ctx: Arc<ConnCtx>, input: McpAppInput| async move { Err::<Never, _>(mcp_app(&ctx.state, input)) })
    .unary("mcpApps.readResource", |ctx: Arc<ConnCtx>, input: McpAppInput| async move { Err::<Never, _>(mcp_app(&ctx.state, input)) })
    .unary("mcpApps.updateModelContext", |ctx: Arc<ConnCtx>, input: McpAppInput| async move {
        Err::<Never, _>(mcp_app(&ctx.state, input))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn cloudflared_versions() {
        assert_eq!(parse_cloudflared_version("cloudflared version 2025.11.1 (built 2025-11-05)"), Some((2025, 11, 1)));
        assert_eq!(parse_cloudflared_version("nothing here 1.2.3"), None);
        assert!(version_tuple("2025.6.1") < version_tuple("2025.10.0"));
    }

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
        let reason = |failure: Failure| json!(failure)["reason"].clone();
        assert_eq!(json!(answer_secret(Err("gone".into()), "i1", false)), json!({ "_tag": "SecretRequestError", "reason": "load_failed", "cause": { "message": "gone" } }));
        assert_eq!(json!(answer_secret(Ok(None), "i1", false)), json!({ "_tag": "SecretRequestError", "reason": "not_found" }));
        assert_eq!(reason(answer_secret(items(card.clone(), "running"), "i2", false)), "not_found");
        assert_eq!(reason(answer_secret(items(card.clone(), "completed"), "i1", false)), "agent_stopped");
        assert_eq!(
            json!(answer_secret(items(card.clone(), "running"), "i1", false)),
            json!({ "_tag": "SecretRequestError", "reason": "store_failed", "cause": { "message": "This Domovoi server has no secret store." } })
        );
        assert_eq!(
            json!(answer_secret(items(card.clone(), "running"), "i1", true)),
            json!({ "_tag": "SecretRequestError", "reason": "record_failed", "cause": { "message": "This Domovoi server cannot record secret request answers." } })
        );
        let mut saved = card;
        saved["secretStatus"] = json!("saved");
        assert_eq!(reason(answer_secret(items(saved, "running"), "i1", false)), "already_answered");
    }

    #[test]
    fn link_answers_check_the_card_first() {
        let card = |status: &str| item(json!({ "type": "link_request", "reason": "To deploy there", "linkStatus": status }));
        assert_eq!(json!(answer_peer_link(Err("gone".into()), "i1")), json!({ "_tag": "PeerLinkRequestError", "reason": "load_failed", "cause": { "message": "gone" } }));
        assert_eq!(json!(answer_peer_link(Ok(None), "i1")), json!({ "_tag": "PeerLinkRequestError", "reason": "not_found" }));
        assert_eq!(json!(answer_peer_link(items(card("pending"), "running"), "i2")), json!({ "_tag": "PeerLinkRequestError", "reason": "not_found" }));
        assert_eq!(json!(answer_peer_link(items(card("linked"), "running"), "i1")), json!({ "_tag": "PeerLinkRequestError", "reason": "already_answered" }));
        assert_eq!(
            json!(answer_peer_link(items(card("pending"), "running"), "i1")),
            json!({ "_tag": "PeerLinkRequestError", "reason": "record_failed", "cause": { "message": LINKED_ENVIRONMENTS_UNAVAILABLE } })
        );
    }

    #[test]
    fn only_an_answer_of_type_decline_declines() {
        let declined = |answer: Value| serde_json::from_value::<SecretAnswer>(json!({ "threadId": "t", "turnItemId": "i", "answer": answer })).unwrap().declined;
        assert!(declined(json!({ "type": "decline" })));
        for other in [json!({ "type": "save", "secret": "x" }), json!({ "type": 5 }), json!({}), json!("decline"), json!(null), json!(["decline"])] {
            assert!(!declined(other.clone()), "{other}");
        }
        assert!(serde_json::from_value::<SecretAnswer>(json!({ "threadId": "t", "turnItemId": "i" })).is_err(), "an answer is required");
        assert!(serde_json::from_value::<LinkAnswer>(json!({ "threadId": "t", "turnItemId": "i" })).is_err(), "an answer is required");
        assert!(serde_json::from_value::<LinkAnswer>(json!({ "threadId": "t", "turnItemId": "i", "answer": null })).is_ok());
    }

    #[test]
    fn relay_client_answers_are_written_as_the_client_reads_them() {
        let available = RelayClientStatus::Available { executable_path: "/bin/cloudflared".into(), source: RelayClientSource::Managed, version: "2026.1.2".into() };
        let written = json!({ "status": "available", "executablePath": "/bin/cloudflared", "source": "managed", "version": "2026.1.2" });
        assert_eq!(json!(available), written);
        assert_eq!(json!([RelayClientSource::Override, RelayClientSource::Path]), json!(["override", "path"]));
        assert_eq!(json!(missing()), json!({ "status": "missing", "version": CLOUDFLARED_VERSION }));
        assert_eq!(
            json!(RelayClientStatus::Unsupported { platform: "sunos", arch: "sparc", version: CLOUDFLARED_VERSION }),
            json!({ "status": "unsupported", "platform": "sunos", "arch": "sparc", "version": CLOUDFLARED_VERSION })
        );
        assert_eq!(json!(InstallEvent::Progress { stage: InstallStage::Checking }), json!({ "type": "progress", "stage": "checking" }));
        assert_eq!(json!(InstallEvent::Complete { status: available }), json!({ "type": "complete", "status": written }));
        assert_eq!(json!(PeerLinks { links: [] }), json!({ "links": [] }));
        assert_eq!(json!(Unlinked { removed: false }), json!({ "removed": false }));
    }

    #[test]
    fn mcp_app_items_name_their_own_tool() {
        let call = |tool_name: &str| -> TurnItem {
            serde_json::from_value(item(json!({ "type": "dynamic_tool", "toolName": tool_name, "output": { "t3McpApp": { "server": "charts", "tool": "show" } } }))).unwrap()
        };
        assert!(is_mcp_app_item(&call("mcp__charts__show")));
        assert!(is_mcp_app_item(&call("charts.show")));
        assert!(!is_mcp_app_item(&call("mcp__other__show")));
    }
}
