//! A remote server's tunnel is started again when the old one may be stuck:
//! on return from a suspension, and after an attempt through it could not
//! reach the server. (A bridge left on sockets iOS took away fails every
//! dial until the app is restarted.)

use crux_core::App;
use crux_http::HttpError;
use crux_http::protocol::{HttpRequest, HttpResult};
use domovoi_core::model::{Connection, Environment};
use domovoi_core::socket::SocketEvent;
use domovoi_core::tunnel::{TunnelOperation, TunnelOutput};
use domovoi_core::{Effect, Event, Model, DomovoiApp};

fn run(model: &mut Model, event: Event) -> Vec<Effect> {
    let mut cmd = DomovoiApp.update(event, model);
    cmd.effects().collect()
}

fn tunnel_ops(effects: &[Effect]) -> Vec<TunnelOperation> {
    effects.iter().filter_map(|e| if let Effect::Tunnel(r) = e { Some(r.operation.clone()) } else { None }).collect()
}

fn is_start(op: &TunnelOperation) -> bool {
    matches!(op, TunnelOperation::Start { .. })
}

/// A device signed in to a remote server, not connected yet.
fn signed_in() -> Model {
    let mut model = Model::default();
    model.surface = "mobile".into();
    model.environments = vec![Environment { id: "mac".into(), label: "Mac".into(), ticket: "endpointabc".into(), ..Default::default() }];
    model.active_env = Some("mac".into());
    model.token = Some("tok".into());
    model.connection = Connection::Reconnecting;
    model
}

/// Starts the tunnel on `port` and returns the command holding the ticket
/// request it leads to, with that request.
fn tunnel_up(model: &mut Model, port: u16) -> (crux_core::Command<Effect, Event>, crux_core::Request<HttpRequest>) {
    let tunnel_gen = model.tunnel_gen;
    let mut cmd = DomovoiApp.update(Event::Tunnel { tunnel_gen, out: TunnelOutput::Started { port } }, model);
    let ticket = cmd
        .effects()
        .find_map(|e| match e {
            Effect::Http(r) if r.operation.url.ends_with("/api/auth/websocket-ticket/rpc") => Some(r),
            _ => None,
        })
        .expect("ticket requested through the tunnel");
    assert!(ticket.operation.url.starts_with(&format!("http://127.0.0.1:{port}/")));
    (cmd, ticket)
}

#[test]
fn an_attempt_that_cannot_reach_the_server_starts_a_new_tunnel() {
    let mut model = signed_in();
    assert!(tunnel_ops(&run(&mut model, Event::Reconnect)).iter().any(is_start));
    let (mut cmd, mut ticket) = tunnel_up(&mut model, 4242);
    assert_eq!(model.tunnel.port, Some(4242));

    // The bridge cannot reach the server: the loopback request fails.
    ticket.resolve(HttpResult::Err(HttpError::Io("The network connection was lost.".into()))).unwrap();
    let mut effects = vec![];
    for event in cmd.events().collect::<Vec<_>>() {
        effects.extend(run(&mut model, event));
    }
    assert_eq!(tunnel_ops(&effects), vec![TunnelOperation::Stop], "the stuck bridge is stopped");
    assert_eq!(model.tunnel.port, None);
    assert_eq!(model.connection, Connection::Reconnecting);
    assert!(model.retry_in > 0, "backs off as before");

    // The retry starts a new bridge instead of reusing the old port.
    let retry = model.retry_gen;
    let effects = run(&mut model, Event::RetryTimer { retry_gen: retry });
    assert!(tunnel_ops(&effects).iter().any(is_start), "{:?}", tunnel_ops(&effects));
}

#[test]
fn returning_from_a_suspension_starts_a_new_tunnel() {
    let mut model = signed_in();
    let _ = run(&mut model, Event::Reconnect);
    let (mut cmd, mut ticket) = tunnel_up(&mut model, 4242);
    ticket.resolve(HttpResult::Ok(crux_http::protocol::HttpResponse::status(200).json(serde_json::json!({ "ticket": "t", "expiresAt": "2026-11-13T00:00:00.000Z", "protocol": domovoi_model::PROTOCOL_VERSION })).build())).unwrap();
    for event in cmd.events().collect::<Vec<_>>() {
        let _ = run(&mut model, event);
    }
    let attempt = model.attempt;
    let _ = run(&mut model, Event::Socket { attempt, event: SocketEvent::Opened });
    assert_eq!(model.connection, Connection::Connected);

    let _ = run(&mut model, Event::Suspend);
    let effects = run(&mut model, Event::Resume);
    let ops = tunnel_ops(&effects);
    assert_eq!(ops.first(), Some(&TunnelOperation::Stop), "the old bridge goes first: {ops:?}");
    assert!(ops.iter().any(is_start), "then a new one starts: {ops:?}");
    assert!(!effects.iter().any(|e| matches!(e, Effect::Http(_))), "nothing is sent through the old port");

    // A late status from the old bridge changes nothing.
    let old = model.tunnel_gen - 1;
    assert!(run(&mut model, Event::Tunnel { tunnel_gen: old, out: TunnelOutput::Started { port: 4242 } }).is_empty());
    let (_, ticket) = tunnel_up(&mut model, 5151);
    assert!(ticket.operation.url.starts_with("http://127.0.0.1:5151/"));
}
