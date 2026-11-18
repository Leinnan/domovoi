//! WebSocket RPC handlers (t3code `WsRpcGroup` subset).

mod access;
mod admin_failure;
mod connect;
mod device;
mod agent_sessions;
mod fs;
mod git_ops;
mod orchestration;
pub(crate) mod projects;
mod local_servers;
mod preview;
mod provider;
mod scheduled_tasks;
mod server;
mod server_admin;
pub(crate) mod shell;
mod telemetry;
mod source_control;
mod terminal;
mod vcs;

use crate::state::ConnCtx;
use effect_rpc::Router;
use futures::stream::{BoxStream, StreamExt};

/// Registers every implemented method.
pub fn router() -> Router<ConnCtx> {
    let r = Router::new().guard(|ctx: &ConnCtx, meta, payload| {
        crate::permissions::check(&meta.tag, payload, &ctx.session.permissions).map_err(|refused| effect_rpc::Failure::fail(&refused))
    });
    let r = server::register(r);
    let r = shell::register(r);
    let r = fs::register(r);
    let r = terminal::register(r);
    let r = orchestration::register(r);
    let r = projects::register(r);
    let r = git_ops::register(r);
    let r = preview::register(r);
    let r = local_servers::register(r);
    let r = device::register(r);
    let r = access::register(r);
    let r = telemetry::register(r);
    let r = connect::register(r);
    let r = server_admin::register(r);
    let r = scheduled_tasks::register(r);
    let r = provider::register(r);
    let r = agent_sessions::register(r);
    let r = source_control::register(r);
    vcs::register(r)
}

/// The failure of a call that cannot fail, and the element of a list that is
/// always empty: a type with no value.
#[derive(Debug, Clone, Copy)]
pub enum Never {}

impl serde::Serialize for Never {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        match *self {}
    }
}

/// Emits `items`, then stays open until the client interrupts (subscriptions
/// that have no further updates yet).
pub fn then_pending<T: Send + 'static, E: Send + 'static>(items: Vec<T>) -> BoxStream<'static, Result<T, E>> {
    futures::stream::iter(items.into_iter().map(Ok)).chain(futures::stream::pending()).boxed()
}

pub use git_ops::available_editors;
pub use shell::snapshot_off_runtime as shell_snapshot;
