//! WebSocket RPC handlers (t3code `WsRpcGroup` subset).

mod access;
pub(crate) mod admin_failure;
mod artifacts;
mod connect;
mod agent_sessions;
mod fs;
mod git_ops;
pub(crate) mod issues;
pub(crate) mod orchestration;
pub(crate) mod projects;
mod local_servers;
pub(crate) mod preview;
mod provider;
mod scheduled_tasks;
mod server;
mod server_admin;
pub(crate) mod shell;
mod telemetry;
pub(crate) mod source_control;
mod terminal;
mod vcs;

use crate::state::ConnCtx;
use domovoi_rpc::Router;
use futures::stream::{BoxStream, StreamExt};

/// Registers every implemented method.
pub fn router() -> Router<ConnCtx> {
    let r = Router::new().guard(|ctx: &ConnCtx, meta, payload| {
        crate::permissions::check(&meta.tag, payload, &ctx.session.permissions).map_err(|refused| domovoi_rpc::Failure::fail(&refused))
    });
    // Reading the diagnostics would otherwise trace itself (t3code's `RPC_METHODS_WITH_TRACING_DISABLED`).
    let r = r.untraced(&["server.getTraceDiagnostics", "server.getProcessDiagnostics", "server.getProcessResourceHistory", "server.signalProcess"]);
    let r = server::register(r);
    let r = shell::register(r);
    let r = fs::register(r);
    let r = terminal::register(r);
    let r = orchestration::register(r);
    let r = projects::register(r);
    let r = git_ops::register(r);
    let r = preview::register(r);
    let r = local_servers::register(r);
    let r = access::register(r);
    let r = telemetry::register(r);
    let r = connect::register(r);
    let r = server_admin::register(r);
    let r = scheduled_tasks::register(r);
    let r = provider::register(r);
    let r = agent_sessions::register(r);
    let r = source_control::register(r);
    let r = artifacts::register(r);
    let r = issues::register(r);
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

pub use shell::snapshot_off_runtime as shell_snapshot;

#[cfg(test)]
mod tests {
    use domovoi_api::{Caller, Kind, METHODS};
    use std::collections::BTreeSet;

    /// The methods the client core calls, read from where it builds its
    /// requests (`apps/domovoi-core/src/requests.rs`): the literal given to
    /// `built(`, `bare(` or `method:`, and a method named by a constant.
    fn called_by_the_client_core() -> BTreeSet<String> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../domovoi-core/src/requests.rs");
        let source = std::fs::read_to_string(path).expect("the client core's requests");
        let source = source.split("#[cfg(test)]").next().unwrap_or_default();
        let mut methods = BTreeSet::new();
        for opening in ["built(\"", "bare(\"", "method: \"", ": &str = \""] {
            for after in source.split(opening).skip(1) {
                if let Some((method, _)) = after.split_once('"') {
                    methods.insert(method.to_owned());
                }
            }
        }
        assert!(methods.len() > 50, "the client core's requests were not read: {methods:?}");
        methods
    }

    /// The router answers exactly the methods `domovoi-api` defines, each the way it says.
    #[test]
    fn the_registered_methods_are_the_defined_ones() {
        let router = super::router();
        let defined: BTreeSet<&str> = METHODS.iter().map(|method| method.name).collect();
        let registered: BTreeSet<&str> = router.tags().collect();
        assert_eq!(registered.difference(&defined).collect::<Vec<_>>(), Vec::<&&str>::new(), "registered, not defined");
        assert_eq!(defined.difference(&registered).collect::<Vec<_>>(), Vec::<&&str>::new(), "defined, not registered");
        let other_kind: Vec<&str> = METHODS.iter().filter(|method| router.is_stream(method.name) != (method.kind == Kind::Stream)).map(|method| method.name).collect();
        assert!(other_kind.is_empty(), "defined as one kind of call and registered as the other: {other_kind:?}");
    }

    /// The methods said to be the client core's are the ones it builds
    /// requests for: one it calls is there, and one it stopped calling says
    /// which feature keeps it, or goes.
    #[test]
    fn the_methods_of_the_client_core_are_the_ones_it_calls() {
        let called = called_by_the_client_core();
        let said: BTreeSet<String> = METHODS.iter().filter(|method| method.caller == Caller::Client).map(|method| method.name.to_owned()).collect();
        assert_eq!(called.difference(&said).collect::<Vec<_>>(), Vec::<&String>::new(), "called by the client core, not defined as its");
        assert_eq!(said.difference(&called).collect::<Vec<_>>(), Vec::<&String>::new(), "defined as the client core's, not called by it");
    }
}
