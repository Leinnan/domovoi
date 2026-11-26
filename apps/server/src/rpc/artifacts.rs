//! Artifact RPCs (`artifacts.list`, `artifacts.read`): Domovoi's own methods
//! serving durable, versioned thread artifacts stored by MCP tools or captured
//! Claude Code `Artifact` calls.

use crate::artifacts::{ArtifactDetail, ArtifactSummary, list_for_thread, read_version};
use crate::state::ConnCtx;
use domovoi_rpc::Router;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListInput {
    thread_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListResult {
    artifacts: Vec<ArtifactSummary>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadInput {
    artifact_id: String,
    #[serde(default)]
    version: Option<u32>,
}

#[derive(Serialize)]
struct RpcError {
    message: String,
}

fn fail(message: String) -> RpcError {
    RpcError { message }
}

async fn list(ctx: Arc<ConnCtx>, input: ListInput) -> Result<ListResult, RpcError> {
    let artifacts = list_for_thread(&ctx.state.store, &input.thread_id).map_err(fail)?;
    Ok(ListResult { artifacts })
}

async fn read(ctx: Arc<ConnCtx>, input: ReadInput) -> Result<ArtifactDetail, RpcError> {
    match read_version(&ctx.state.store, &input.artifact_id, input.version).map_err(fail)? {
        Some(detail) => Ok(detail),
        None => Err(fail(format!("Artifact `{}` not found.", input.artifact_id))),
    }
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.unary("artifacts.list", list).unary("artifacts.read", read)
}
