//! `agentSessions.scan` / `agentSessions.import` over [`crate::agent_sessions`].
//! Both read files, so they run on the blocking pool.

use crate::agent_sessions::{self as sessions, Failure, ScanEnv};
use crate::state::ConnCtx;
use domovoi_rpc::Router;
use serde::Deserialize;
use serde::de::IgnoredAny;
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportInput {
    project_id: String,
    expected_workspace_root: Option<String>,
}

async fn env(ctx: &ConnCtx) -> ScanEnv {
    let settings = ctx.state.settings.read().await.clone();
    ScanEnv::new(&settings, &ctx.state.paths.home)
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.unary("agentSessions.scan", |ctx: Arc<ConnCtx>, _input: IgnoredAny| async move {
        let env = env(&ctx).await;
        let store = ctx.state.store.clone();
        // A scan that did not end (it panicked) is told as one that could not read.
        tokio::task::spawn_blocking(move || sessions::scan(&env, &store)).await.map_err(Failure::read_projects)?
    })
    .unary("agentSessions.import", |ctx: Arc<ConnCtx>, input: ImportInput| async move {
        let env = env(&ctx).await;
        let store = ctx.state.store.clone();
        tokio::task::spawn_blocking(move || {
            sessions::import_recent(&env, &store, &input.project_id, input.expected_workspace_root.as_deref())
        })
        .await
        .map_err(Failure::read_projects)?
    })
}
