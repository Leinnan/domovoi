//! Mermaid diagrams, drawn by the server: a shell names a completed Mermaid
//! block with a key of its own (`LoadDiagram`), the core asks the server for
//! a URL of it as a PNG (`assets.createUrl`, resource `mermaid-diagram`), and
//! the view lists each diagram's URL by key. When the server cannot draw one
//! it fails the call, and the shell shows the source.

use crate::{
    Model,
    model::Connection,
    requests,
    rpc::Route,
    update::{Cmd, call, uuid},
    view::DiagramView,
};
use crux_core::{Command, render::render};
use domovoi_model::typed::AssetCreateUrlResult;
use std::collections::VecDeque;

/// Diagrams kept; the oldest asked for go first.
const LIMIT: usize = 64;
/// How long a URL is used when the server gave no expiry.
const DEFAULT_TTL_MS: i64 = 60_000;

#[derive(Default)]
pub struct State {
    entries: VecDeque<Entry>,
}

struct Entry {
    key: String,
    /// The server it was asked of (`Model::active_env`).
    server: Option<String>,
    /// The call waiting for its answer (empty once answered).
    request: String,
    url: Option<String>,
    /// When `url` stops working; `None` while the call is out.
    expires_at: Option<i64>,
    /// Asked for while there was no connection.
    offline: bool,
}

impl State {
    fn get(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.key == key)
    }
}

/// Asks the server to draw `source`, unless a URL for `key` is on its way or still valid.
pub fn load(model: &mut Model, key: String, source: String, dark: bool) -> Cmd {
    let server = model.active_env.clone();
    if model.diagrams.get(&key).is_some_and(|e| {
        e.server == server && !e.offline && e.expires_at.is_none_or(|at| at > model.now_ms)
    }) {
        return Command::done();
    }
    model.diagrams.entries.retain(|e| e.key != key);
    while model.diagrams.entries.len() >= LIMIT {
        model.diagrams.entries.pop_front();
    }
    let connected = model.connection == Connection::Connected;
    let request = if connected { uuid() } else { String::new() };
    model.diagrams.entries.push_back(Entry {
        key: key.clone(),
        server,
        request: request.clone(),
        url: None,
        expires_at: (!connected).then_some(model.now_ms),
        offline: !connected,
    });
    if !connected {
        return render();
    }
    call(model, requests::mermaid_diagram(&source, dark), Route::Diagram { key, request }).and(render())
}

/// The server's answer (`None`: it failed or could not be read).
pub fn reply(model: &mut Model, key: &str, request: &str, result: Option<AssetCreateUrlResult>) {
    let now = model.now_ms;
    let Some(entry) = model.diagrams.entries.iter_mut().find(|e| e.key == key && e.request == request) else {
        return;
    };
    entry.request.clear();
    entry.expires_at = Some(
        result.as_ref().and_then(|asset| asset.expires_at.as_i64()).unwrap_or(now + DEFAULT_TTL_MS),
    );
    entry.url = result
        .map(|asset| asset.relative_url)
        .filter(|url| url.starts_with("/api/assets/diagram/"));
}

/// Calls that died with the socket count as asked for offline: they are
/// asked again once it is back.
pub fn disconnected(model: &mut Model) {
    let now = model.now_ms;
    for entry in &mut model.diagrams.entries {
        if entry.expires_at.is_none() {
            entry.request.clear();
            entry.expires_at = Some(now);
            entry.offline = true;
        }
    }
}

pub fn view(model: &Model) -> Vec<DiagramView> {
    let connected = model.connection == Connection::Connected;
    model
        .diagrams
        .entries
        .iter()
        .map(|e| {
            let current = e.server == model.active_env;
            let stale = !current || e.offline || e.expires_at.is_some_and(|at| at <= model.shown_ms);
            DiagramView {
                key: e.key.clone(),
                url: e
                    .url
                    .as_ref()
                    .filter(|_| current)
                    .map(|url| format!("{}{url}", model.server_url)),
                pending: e.expires_at.is_none(),
                failed: current && !e.offline && e.expires_at.is_some() && e.url.is_none(),
                offline: e.offline && !connected,
                load_key: (connected && stale).then(|| {
                    format!("{}\0{}", model.attempt, model.active_env.as_deref().unwrap_or_default())
                }),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected() -> Model {
        let mut model = Model::default();
        model.connection = Connection::Connected;
        model.server_url = "http://127.0.0.1:1".into();
        model.now_ms = 1_000;
        model.shown_ms = 1_000;
        model
    }

    fn request(model: &Model, key: &str) -> String {
        model.diagrams.get(key).expect("asked for").request.clone()
    }

    #[test]
    fn a_drawn_diagram_has_its_url_until_it_expires() {
        let mut model = connected();
        let _ = load(&mut model, "k".into(), "flowchart LR\nA-->B".into(), true);
        let shown = view(&model);
        assert!(shown[0].pending && shown[0].url.is_none() && shown[0].load_key.is_none());
        let asked = request(&model, "k");
        reply(&mut model, "k", &asked, Some(AssetCreateUrlResult {
            relative_url: "/api/assets/diagram/token".into(),
            expires_at: 61_000.into(),
            source_path: None,
            image_dimensions: None,
            file_revision: None,
        }));
        let shown = view(&model);
        assert_eq!(shown[0].url.as_deref(), Some("http://127.0.0.1:1/api/assets/diagram/token"));
        assert!(!shown[0].failed && shown[0].load_key.is_none());
        model.shown_ms = 61_000;
        assert!(view(&model)[0].load_key.is_some(), "expired: ask again");
    }

    #[test]
    fn a_failed_call_shows_as_failed_and_offline_ones_wait_for_the_socket() {
        let mut model = connected();
        let _ = load(&mut model, "k".into(), "nonsense".into(), false);
        let asked = request(&model, "k");
        reply(&mut model, "k", &asked, None);
        assert!(view(&model)[0].failed);

        let mut model = Model::default();
        let _ = load(&mut model, "k".into(), "flowchart LR\nA-->B".into(), false);
        let shown = view(&model);
        assert!(shown[0].offline && !shown[0].failed && shown[0].load_key.is_none());
        model.connection = Connection::Connected;
        assert!(view(&model)[0].load_key.is_some(), "connected: ask again");
    }

    #[test]
    fn the_request_names_the_diagram_and_its_theme() {
        let request = requests::mermaid_diagram("pie\n\"a\": 1", true);
        assert_eq!(request.method(), "assets.createUrl");
        let payload = serde_json::to_value(request.payload()).expect("payload");
        assert_eq!(payload["resource"]["_tag"], "mermaid-diagram");
        assert_eq!(payload["resource"]["theme"], "dark");
    }
}
