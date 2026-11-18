//! A measurement of everything the view shows besides the open thread's
//! rows: the whole view the core builds, and every frame it sends, at each
//! step of a client walking through its screens, must be what was recorded
//! earlier.
//!
//! The walks are driven the way a shell drives the core: events in, effects
//! answered. What the server says comes from the recorded TypeScript-server
//! sessions (`fixtures/golden`), from the sample payloads next to the typed
//! records (`crates/domovoi-model/tests/samples`), and, when `DOMOVOI_THREADS` names an
//! export of a server's own threads, from the shells of those threads.
//! Nothing here reaches into the model's server state, so the same file
//! measures a core that keeps that state as JSON and one that keeps records.
//!
//! A recording is one JSON line per step: its id, a hash of the view, a hash
//! of each of the view's parts (so a difference names the part), and a hash
//! of the frames sent since the step before. Ids the core makes up (command
//! ids, a new thread's id) are named by their order before hashing.

use crux_core::App;
use crux_http::protocol::{HttpResponse, HttpResult};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use domovoi_core::rpc::Route;
use domovoi_core::socket::{SocketEvent, SocketOperation};
use domovoi_core::store::{StoreOperation, StoreOutput};
use domovoi_core::view::ViewModel;
use domovoi_core::{Effect, Event, Model, DomovoiApp};
use domovoi_model::typed::{RecordKind, Thread, ThreadStreamItem};

mod common;
use common::{canonical, uuids};

/// FNV-1a: the same on every machine and toolchain.
fn hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3))
}

/// One step of a walk, as recorded.
struct Step {
    id: String,
    /// The view's parts, each as JSON.
    parts: Vec<(String, String)>,
    /// The frames sent and notifications posted since the step before.
    sent: Vec<String>,
    /// The offline copies saved since the step before.
    saved: Vec<String>,
    /// The commands and launches sent since the step before that the server
    /// would not read as they were written (`common::read_by_the_server`).
    refused: Vec<String>,
}

impl Step {
    fn digest(&self) -> Value {
        let parts: Map<String, Value> = self.parts.iter().map(|(name, json)| (name.clone(), json!(format!("{:016x}", hash(json))))).collect();
        let whole: String = self.parts.iter().map(|(_, json)| format!("{:016x}", hash(json))).collect();
        json!({
            "id": self.id,
            "view": format!("{:016x}", hash(&whole)),
            "parts": parts,
            "sent": self.sent.len(),
            "frames": format!("{:016x}", hash(&self.sent.join("\n"))),
            "saved": self.saved.iter().map(|copy| format!("{:016x}", hash(copy))).collect::<Vec<_>>(),
        })
    }
}

/// A client being walked through its screens.
struct Walk<'a> {
    name: String,
    model: Model,
    store: HashMap<String, String>,
    /// What the server answers over HTTP, by path.
    http: HashMap<String, (u16, Value)>,
    sent: Vec<Value>,
    saved: Vec<String>,
    refused: Vec<String>,
    /// UUIDs the server (the data fed in) named: any other is the core's own.
    given: HashSet<String>,
    made: Vec<String>,
    steps: usize,
    /// Answers since the last step that found no open request to answer.
    unasked: usize,
    sink: &'a mut dyn FnMut(Step),
}

impl<'a> Walk<'a> {
    fn new(name: &str, sink: &'a mut dyn FnMut(Step)) -> Self {
        let mut http = HashMap::new();
        http.insert("/oauth/token".to_owned(), (200, sample("auth.access-token")));
        http.insert("/api/auth/websocket-ticket/rpc".to_owned(), (200, sample("auth.ws-ticket")));
        Self { name: name.to_owned(), model: Model::default(), store: HashMap::new(), http, sent: vec![], saved: vec![], refused: vec![], given: HashSet::new(), made: vec![], steps: 0, unasked: 0, sink }
    }

    /// A client connected to a server, at `NOW`.
    fn connected(name: &str, sink: &'a mut dyn FnMut(Step)) -> Self {
        let mut walk = Self::new(name, sink);
        walk.ev(Event::Tick { now_ms: NOW });
        walk.ev(Event::Connect { server_url: "http://127.0.0.1:1".into(), pairing_code: "CODE".into() });
        walk.socket(SocketEvent::Opened);
        walk
    }

    fn note(&mut self, text: &str) {
        for at in uuids(text) {
            self.given.insert(text[at..at + 36].to_owned());
        }
    }

    /// Runs `event`, answering what it asks of the store and of HTTP, and
    /// whatever those answers lead to.
    fn ev(&mut self, event: Event) {
        let mut queue = VecDeque::from([event]);
        while let Some(event) = queue.pop_front() {
            let mut command = DomovoiApp.update(event, &mut self.model);
            loop {
                let effects: Vec<Effect> = command.effects().collect();
                let answered = effects.iter().any(|effect| matches!(effect, Effect::Store(_) | Effect::Http(_)));
                for effect in effects {
                    match effect {
                        Effect::Store(mut request) => {
                            let value = match &request.operation {
                                StoreOperation::Load { key } => self.store.get(key).cloned(),
                                StoreOperation::Save { key, value } => {
                                    if key.starts_with("cache:") {
                                        self.saved.push(value.clone());
                                    }
                                    self.store.insert(key.clone(), value.clone());
                                    None
                                }
                                StoreOperation::Delete { key } => {
                                    self.store.remove(key);
                                    None
                                }
                            };
                            request.resolve(StoreOutput::Value { value }).expect("the store answers");
                        }
                        Effect::Http(mut request) => {
                            let url = &request.operation.url;
                            let path = url.splitn(4, '/').nth(3).map(|path| format!("/{path}")).unwrap_or_default();
                            let (status, body) = self.http.get(&path).cloned().unwrap_or((404, json!({})));
                            // A JSON body as the JSON it is, so that only what it says is compared.
                            let asked = serde_json::from_slice::<Value>(&request.operation.body).unwrap_or_else(|_| json!(String::from_utf8_lossy(&request.operation.body)));
                            self.sent.push(json!({ "http": request.operation.method, "path": path, "body": asked }));
                            request.resolve(HttpResult::Ok(HttpResponse::status(status).json(body).build())).expect("HTTP answers");
                        }
                        Effect::Socket(request) => match &request.operation {
                            SocketOperation::Send { text } => {
                                let frame: Value = serde_json::from_str(text).unwrap_or(Value::Null);
                                self.refused.extend(common::read_by_the_server(&frame).err());
                                self.sent.push(frame);
                            }
                            SocketOperation::Open { .. } => self.sent.push(json!("socket opened")),
                            other => self.sent.push(json!(format!("{other:?}"))),
                        },
                        Effect::Notify(request) => self.sent.push(json!(format!("{:?}", request.operation))),
                        Effect::Render(_) | Effect::Tunnel(_) => {}
                    }
                }
                queue.extend(command.events());
                if !answered {
                    break;
                }
            }
        }
    }

    fn socket(&mut self, event: SocketEvent) {
        let attempt = self.model.attempt;
        self.ev(Event::Socket { attempt, event });
    }

    fn frame(&mut self, frame: Value) {
        let text = frame.to_string();
        self.note(&text);
        self.socket(SocketEvent::Message { text });
    }

    /// The open request whose route is `wanted`, or the first that `wanted` picks.
    fn request(&self, wanted: impl Fn(&Route) -> bool) -> Option<String> {
        let mut routes: Vec<Route> = self.model.rpc.routes().into_iter().filter(|route| wanted(route)).collect();
        routes.sort_by_key(|route| format!("{route:?}"));
        routes.first().and_then(|route| self.model.rpc.id_of(route))
    }

    /// The server sends `values` on the stream `wanted` picks. `false` if the
    /// client has no such stream open (and nothing is sent).
    fn chunk(&mut self, wanted: impl Fn(&Route) -> bool, values: &[Value]) -> bool {
        let Some(id) = self.request(wanted) else {
            self.unasked += 1;
            return false;
        };
        self.frame(json!({ "_tag": "Chunk", "requestId": id, "values": values }));
        true
    }

    /// The server answers the call `wanted` picks with `value`.
    fn exit(&mut self, wanted: impl Fn(&Route) -> bool, value: Value) -> bool {
        let Some(id) = self.request(wanted) else {
            self.unasked += 1;
            return false;
        };
        self.frame(json!({ "_tag": "Exit", "requestId": id, "exit": { "_tag": "Success", "value": value } }));
        true
    }

    /// The server refuses the call `wanted` picks.
    fn fail(&mut self, wanted: impl Fn(&Route) -> bool, message: &str) -> bool {
        let Some(id) = self.request(wanted) else {
            self.unasked += 1;
            return false;
        };
        self.frame(json!({ "_tag": "Exit", "requestId": id, "exit": { "_tag": "Failure", "cause": [{ "_tag": "Fail", "error": { "_tag": "Refused", "message": message } }] } }));
        true
    }

    /// A stream or call the client did not open itself is opened for it.
    fn open(&mut self, route: Route) {
        if !self.model.rpc.has(&route) {
            let _ = self.model.rpc.request("opened-by-the-walk", &json!({}), route);
        }
    }

    fn view(&self) -> ViewModel {
        DomovoiApp.view(&self.model)
    }

    /// Names the ids the core made up by their order, in `text`.
    fn named(&mut self, text: &str) -> String {
        let found = uuids(text);
        if found.is_empty() {
            return text.to_owned();
        }
        let mut out = String::with_capacity(text.len());
        let mut from = 0;
        for at in found {
            let id = &text[at..at + 36];
            out.push_str(&text[from..at]);
            from = at + 36;
            if self.given.contains(id) {
                out.push_str(id);
                continue;
            }
            let n = match self.made.iter().position(|made| made == id) {
                Some(n) => n,
                None => {
                    self.made.push(id.to_owned());
                    self.made.len() - 1
                }
            };
            out.push_str(&format!("made-{n}"));
        }
        out.push_str(&text[from..]);
        out
    }

    /// Records where the walk stands.
    fn step(&mut self, label: &str) {
        self.steps += 1;
        let id = format!("{}#{:04}:{label}", self.name, self.steps);
        if std::mem::take(&mut self.unasked) > 0 && std::env::var("DOMOVOI_WALK_UNASKED").is_ok() {
            println!("    an answer no request was open for, before {id}");
        }
        // Frames first, in an order that does not depend on the ids in them
        // (the client's open requests are kept in a hash map).
        let mut sent: Vec<(String, String)> = std::mem::take(&mut self.sent)
            .into_iter()
            .map(|mut frame| {
                if let Some(fields) = frame.as_object_mut() {
                    fields.remove("id");
                    fields.remove("requestId");
                }
                let text = canonical(&frame);
                let mut blank = text.clone();
                for at in uuids(&text) {
                    blank.replace_range(at..at + 36, "------------------------------------");
                }
                (blank, text)
            })
            .collect();
        sent.sort();
        let sent: Vec<String> = sent.into_iter().map(|(_, text)| self.named(&text)).collect();
        let Ok(Value::Object(view)) = serde_json::to_value(self.view()) else { panic!("a view is an object") };
        let parts: Vec<(String, String)> = view.into_iter().map(|(name, part)| (name, part.to_string())).map(|(name, part)| (name, self.named(&part))).collect();
        let saved: Vec<String> = std::mem::take(&mut self.saved)
            .into_iter()
            .map(|copy| serde_json::from_str::<Value>(&copy).map(|mut copy| {
                // When it was saved is not what was saved.
                if let Some(fields) = copy.as_object_mut() {
                    fields.remove("savedAt");
                }
                canonical(&copy)
            }).unwrap_or(copy))
            .map(|copy| self.named(&copy))
            .collect();
        (self.sink)(Step { id, parts, sent, saved, refused: std::mem::take(&mut self.refused) });
    }

    /// An event, then a step named after it.
    fn go(&mut self, label: &str, event: Event) {
        self.ev(event);
        self.step(label);
    }
}

/// When the walks happen: after everything in the recordings and the export.
const NOW: i64 = 1_795_000_000_000;

fn root() -> String {
    format!("{}/../..", env!("CARGO_MANIFEST_DIR"))
}

/// A sample payload of the typed records' tests.
fn sample(name: &str) -> Value {
    let path = format!("{}/crates/domovoi-model/tests/samples/{name}.json", root());
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|why| panic!("{path}: {why}"))).unwrap_or_else(|why| panic!("{path}: {why}"))
}

/// A sample that is a stream: its items.
fn samples(name: &str) -> Vec<Value> {
    match sample(name) {
        Value::Array(items) => items,
        one => vec![one],
    }
}

/// One frame of a recorded session with the request it belongs to.
struct Recorded {
    /// The request as the web client sent it.
    request: Value,
    /// `Request`, `Chunk` or `Exit`.
    kind: String,
    values: Vec<Value>,
    /// A successful exit's value.
    value: Option<Value>,
}

fn recorded(name: &str) -> Vec<Recorded> {
    let path = format!("{}/fixtures/golden/{name}/wire.ndjson", root());
    let text = std::fs::read_to_string(path).expect("the recorded session");
    let mut requests: BTreeMap<(i64, String), Value> = BTreeMap::new();
    let mut out = vec![];
    for record in text.lines().map(|line| serde_json::from_str::<Value>(line).expect("a JSON line")) {
        let frame = &record["frame"];
        if record["kind"] != "ws" || !frame.is_object() {
            continue;
        }
        let connection = record["conn"].as_i64().unwrap_or(0);
        if frame["_tag"] == "Request" {
            requests.insert((connection, frame["id"].to_string()), frame.clone());
            out.push(Recorded { request: frame.clone(), kind: "Request".into(), values: vec![], value: None });
            continue;
        }
        let Some(request) = requests.get(&(connection, frame["requestId"].to_string())) else { continue };
        match frame["_tag"].as_str() {
            Some("Chunk") => out.push(Recorded { request: request.clone(), kind: "Chunk".into(), values: frame["values"].as_array().cloned().unwrap_or_default(), value: None }),
            Some("Exit") if frame["exit"]["_tag"] == "Success" => out.push(Recorded { request: request.clone(), kind: "Exit".into(), values: vec![], value: Some(frame["exit"]["value"].clone()) }),
            _ => {}
        }
    }
    out
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

/// Replays a recorded session into a connected client: every stream the core
/// keeps, every thread the web client opened (opened here as it was there),
/// and the answers to the calls the core makes too. A step per frame.
fn replay(walk: &mut Walk, name: &str) {
    let mut skipped: BTreeMap<String, usize> = BTreeMap::new();
    for frame in recorded(name) {
        let tag = text(&frame.request["tag"]);
        let payload = &frame.request["payload"];
        if frame.kind == "Request" {
            // What the web client opened, this client opens.
            match tag.as_str() {
                "orchestration.subscribeThread" => walk.go(&format!("open {tag}"), Event::SelectThread { thread_id: text(&payload["threadId"]) }),
                "filesystem.browse" => walk.go(&format!("ask {tag}"), Event::BrowseFolders { path: text(&payload["partialPath"]) }),
                "projects.readFile" => {
                    walk.ev(Event::ShowPanel { tab: "files".into() });
                    walk.go(&format!("ask {tag}"), Event::OpenFile { path: text(&payload["relativePath"]) });
                }
                _ => {}
            }
            continue;
        }
        let route: Option<Route> = match tag.as_str() {
            "subscribeServerConfig" => Some(Route::Config),
            "subscribeServerLifecycle" => Some(Route::Lifecycle),
            "orchestration.subscribeShell" => Some(Route::Shell),
            "subscribeTerminalMetadata" => Some(Route::TerminalMetadata),
            "orchestration.subscribeThread" => Some(Route::Thread(text(&payload["threadId"]))),
            "subscribeVcsStatus" => Some(Route::VcsStatus(text(&payload["cwd"]))),
            "terminal.attach" => Some(Route::Terminal { thread_id: text(&payload["threadId"]), terminal_id: text(&payload["terminalId"]) }),
            "server.updateSettings" => Some(Route::Settings),
            "vcs.listRefs" => Some(Route::ListRefs(text(&payload["cwd"]))),
            "assets.createUrl" => Some(Route::AssetUrl(text(&payload["resource"]["attachmentId"]))),
            "projects.readFile" => Some(Route::ReadFile(text(&payload["relativePath"]))),
            "projects.mutate" => Some(Route::ProjectCreated),
            _ => None,
        };
        let kind = frame.values.first().map(|value| value["type"].as_str().or(value["kind"].as_str()).or(value["_tag"].as_str()).unwrap_or("item").to_owned()).unwrap_or_default();
        let done = match (&route, frame.kind.as_str()) {
            (Some(route), "Chunk") => walk.chunk(|open| open == route, &frame.values),
            (None, "Exit") if tag == "filesystem.browse" => walk.exit(|open| matches!(open, Route::Browse(_)), frame.value.clone().unwrap_or_default()),
            (Some(route), "Exit") => {
                // A call the web client made and this one did not: made now.
                if matches!(route, Route::Settings | Route::ListRefs(_) | Route::ProjectCreated) {
                    walk.open(route.clone());
                }
                walk.exit(|open| open == route, frame.value.clone().unwrap_or_default())
            }
            _ => false,
        };
        if done {
            walk.step(&format!("{} {tag} {kind}", frame.kind));
        } else {
            *skipped.entry(format!("{} {tag}", frame.kind)).or_default() += 1;
        }
    }
    // What of the recording this client has no use for, so a walk that
    // stops replaying something shows.
    println!("{name}: of the recording, not replayed: {skipped:?}");
    walk.sent.push(json!({ "not replayed": skipped }));
    walk.step("replayed");
}

/// The ids of the view's projects, and of the threads listed (under them,
/// or in the status sections).
fn listed(view: &ViewModel) -> (Vec<String>, Vec<String>) {
    let projects = view.sidebar.projects.iter().map(|project| project.id.clone()).collect();
    let by_project = view.sidebar.projects.iter().flat_map(|project| project.threads.iter().chain(&project.earlier));
    let by_status = view.sidebar.sections.iter().flat_map(|section| section.rows.iter());
    let threads = by_project.chain(by_status).map(|thread| thread.id.clone()).collect();
    (projects, threads)
}

/// The thread list and the palette: status sections, filters, searches,
/// every project.
fn walk_lists(walk: &mut Walk) {
    // Grouped by status: the collapsed sections opened, Running closed and
    // opened again, all of each shown.
    for section in ["hibernating", "archived", "running", "running"] {
        walk.go("toggle a section", Event::ToggleSection { section: section.into() });
    }
    for section in ["inbox", "hibernating", "archived"] {
        walk.go("all of a section", Event::ShowAllInSection { section: section.into() });
    }
    walk.go("grouped by project", Event::SetSidebarGrouping { grouping: "project".into() });
    let view = walk.view();
    let (projects, threads) = listed(&view);
    walk.go("unread only", Event::SetUnreadOnly { on: true });
    walk.go("all again", Event::SetUnreadOnly { on: false });
    for project in projects.iter().take(4) {
        walk.go("show all of a project", Event::ShowAllThreads { project_id: project.clone() });
        walk.go("collapse a project", Event::ToggleProject { project_id: project.clone() });
    }
    for project in projects.iter().take(2) {
        walk.go("expand a project", Event::ToggleProject { project_id: project.clone() });
    }
    // Threads waiting on the user, answered from their live activities.
    let waiting: Vec<String> = view.sidebar.projects.iter().flat_map(|project| project.threads.iter().chain(&project.earlier)).filter(|thread| thread.status == "approval").map(|thread| thread.id.clone()).take(4).collect();
    for thread in waiting {
        walk.go("answered from a live activity", Event::RespondToThread { environment_id: String::new(), thread_id: thread, decision: "accept".into() });
    }
    walk.go("search, one letter", Event::SetSearch { query: "a".into() });
    walk.go("search", Event::SetSearch { query: "the".into() });
    // The server's matches: in the threads listed, by their own projects.
    let project_of: HashMap<&str, &str> = view.sidebar.projects.iter().flat_map(|project| project.threads.iter().chain(&project.earlier)).map(|thread| (thread.id.as_str(), thread.project_id.as_str())).collect();
    let mut found = sample("orchestration.searchThreads");
    let template = found["matches"][0].clone();
    let matches: Vec<Value> = threads
        .iter()
        .step_by(3)
        .take(6)
        .enumerate()
        .map(|(n, thread)| {
            let mut hit = template.clone();
            hit["threadId"] = json!(thread);
            hit["projectId"] = json!(project_of.get(thread.as_str()).copied().unwrap_or_default());
            hit["snippet"] = json!(format!("…match {n} in the thread…"));
            hit["source"] = json!(if n % 2 == 0 { "user" } else { "assistant" });
            hit
        })
        .collect();
    found["matches"] = json!(matches);
    walk.exit(|route| matches!(route, Route::SearchThreads(_)), found);
    walk.step("search results");
    walk.go("search cleared", Event::SetSearch { query: String::new() });

    walk.go("palette", Event::OpenPalette);
    let title_word = view.sidebar.projects.iter().flat_map(|project| project.threads.first()).filter_map(|thread| thread.title.split_whitespace().next().map(str::to_owned)).next().unwrap_or_else(|| "a".into());
    let project_word = view.sidebar.projects.first().map(|project| project.title.chars().take(3).collect::<String>()).unwrap_or_default();
    for query in ["new", "thread", "e", title_word.as_str(), project_word.as_str(), "zzzz", ""] {
        walk.go("palette query", Event::SetPaletteQuery { query: query.into() });
    }
    walk.go("palette down", Event::MovePalette { delta: 3 });
    walk.go("palette up", Event::MovePalette { delta: -1 });
    walk.go("palette closed", Event::ClosePalette);
    walk.go("grouped by status", Event::SetSidebarGrouping { grouping: "status".into() });
}

/// The settings the server last sent, as the recording has them.
fn recorded_settings() -> Value {
    recorded("ts-session1").into_iter().filter(|frame| frame.kind == "Chunk" && frame.request["tag"] == "subscribeServerConfig").flat_map(|frame| frame.values).filter(|value| value["type"] == "snapshot").map(|value| value["config"]["settings"].clone()).last().expect("a recorded config snapshot")
}

/// The composer of a new thread: the model picker, favorites, hidden
/// models, efforts, modes, slash commands and mentions.
fn walk_composer(walk: &mut Walk) {
    let project = walk.model.projects.first().map(|project| project.id.as_str().to_owned());
    walk.go("new thread", Event::NewThread { project_id: project });
    for query in ["son", "gpt", "5", "open claude", "zzzz", ""] {
        walk.go("model query", Event::SetModelQuery { query: query.into() });
    }
    walk.go("hidden models shown", Event::SetShowHiddenModels { show: true });
    let view = walk.view();
    let models: Vec<(String, String)> = view.composer.model_groups.iter().flat_map(|group| group.models.iter().take(2)).map(|row| (row.option.instance_id.clone(), row.option.model.clone())).collect();
    let settings = recorded_settings();
    if let Some((instance, slug)) = models.first().cloned() {
        // Shown at once, taken back when the server refuses.
        walk.go("favorite, unconfirmed", Event::SetModelFavorite { instance_id: instance.clone(), model: slug.clone(), favorite: true });
        walk.fail(|route| *route == Route::Settings, "no");
        walk.step("favorite refused");
        walk.go("favorite again", Event::SetModelFavorite { instance_id: instance.clone(), model: slug.clone(), favorite: true });
        let mut confirmed = settings.clone();
        confirmed["favorites"] = json!([{ "provider": instance, "model": slug }]);
        walk.exit(|route| *route == Route::Settings, confirmed.clone());
        walk.step("favorite confirmed");
        if let Some((other, hidden)) = models.get(1).cloned() {
            walk.go("hidden, unconfirmed", Event::SetModelHidden { instance_id: other.clone(), model: hidden.clone(), hidden: true });
            confirmed["providerModelPreferences"] = json!({ other.clone(): { "hiddenModels": [hidden], "modelOrder": [] } });
            walk.exit(|route| *route == Route::Settings, confirmed.clone());
            walk.step("hidden confirmed");
            walk.go("hidden models not shown", Event::SetShowHiddenModels { show: false });
            walk.go("favorite hidden, unconfirmed", Event::SetModelHidden { instance_id: instance.clone(), model: slug.clone(), hidden: true });
            walk.fail(|route| *route == Route::Settings, "no");
            walk.step("favorite hidden refused");
        }
        let reordered = view.composer.model_groups.iter().find(|group| group.models.len() >= 3).and_then(|group| group.models.last()).map(|row| (row.option.instance_id.clone(), row.option.model.clone()));
        if let Some((third, ordered)) = reordered {
            confirmed["providerModelPreferences"][third.as_str()] = json!({ "hiddenModels": [], "modelOrder": [ordered] });
            walk.chunk(|route| *route == Route::Config, &[json!({ "version": 1, "type": "settingsUpdated", "payload": { "settings": confirmed } })]);
            walk.step("a custom model order");
            for query in ["son", "5", ""] {
                walk.go("model query, with favorites", Event::SetModelQuery { query: query.into() });
            }
        }
    }
    for (instance, slug) in models.iter().take(8) {
        walk.go("model picked", Event::SelectModel { instance_id: instance.clone(), model: slug.clone() });
        let efforts: Vec<String> = walk.view().composer.efforts.iter().map(|effort| effort.value.clone()).collect();
        for effort in efforts.into_iter().take(2) {
            walk.go("effort picked", Event::SelectEffort { value: effort });
        }
    }
    for mode in ["approval-required", "auto-accept-edits", "full-access"] {
        walk.go("permissions", Event::SetRuntimeMode { mode: mode.into() });
    }
    walk.go("plan mode", Event::SetInteractionMode { mode: "plan".into() });
    walk.go("default mode", Event::SetInteractionMode { mode: "default".into() });
    walk.go("in a worktree", Event::SetWorkspaceMode { mode: "worktree".into() });
    walk.go("base branch", Event::SetBaseBranch { branch: "main".into() });
    walk.go("in the checkout", Event::SetWorkspaceMode { mode: "local".into() });
    for draft in ["/", "/re", "/zzzz", "look at @sr"] {
        walk.go("draft", Event::SetDraft { text: draft.into() });
    }
    if walk.exit(|route| matches!(route, Route::Mention(_)), sample("projects.searchEntries")) {
        walk.step("mention results");
        walk.go("mention down", Event::MoveMention { delta: 1 });
        walk.go("mention picked", Event::PickMention { path: String::new() });
    }
    walk.go("a scheduled task", Event::CreateScheduled { name: "Nightly".into(), prompt: "Run the tests".into(), project_id: walk.view().sidebar.selected_project_id.unwrap_or_default(), interval_minutes: 90 });
    walk.go("draft cleared", Event::SetDraft { text: String::new() });
}

/// The settings window: every section, every setting switched, keybindings,
/// remote access.
fn walk_settings(walk: &mut Walk) {
    walk.go("settings", Event::ShowRoute { route: "settings".into() });
    walk.go("providers", Event::LoadSettings { section: "providers".into() });
    walk.go("providers refreshed", Event::RefreshProviders);
    walk.ev(Event::LoadSettings { section: "scheduled".into() });
    for item in samples("scheduledTasks.subscribe") {
        walk.chunk(|route| *route == Route::ScheduledTasks, &[item]);
        walk.step("scheduled tasks");
    }
    walk.ev(Event::LoadSettings { section: "storage".into() });
    for item in samples("server.getDatabaseStorage") {
        walk.chunk(|route| *route == Route::DatabaseStorage, &[item]);
        walk.step("database storage");
    }
    for item in samples("server.getStorageCleanupReport") {
        walk.chunk(|route| *route == Route::CleanupReport, &[item]);
        walk.step("cleanup report");
    }
    walk.go("cleanup started", Event::RunStorageCleanup);
    walk.exit(|route| *route == Route::Command("Storage cleanup".into()), Value::Null);
    walk.step("cleanup finished");
    walk.ev(Event::LoadSettings { section: "diagnostics".into() });
    walk.exit(|route| *route == Route::Diagnostics, sample("server.getProcessDiagnostics"));
    walk.exit(|route| *route == Route::HostResources, sample("server.getHostResources"));
    walk.step("diagnostics");

    // Access: who is signed in, and the server's remote access.
    walk.http.insert("/api/domovoi/remote".into(), (200, sample("api.domovoi.remote")));
    walk.http.insert("/api/domovoi/remote/pairing".into(), (200, sample("api.domovoi.remote.pairing")));
    walk.http.insert("/api/domovoi/remote/configure".into(), (200, sample("api.domovoi.remote.off")));
    walk.go("access", Event::LoadSettings { section: "access".into() });
    let mut access = samples("subscribeAuthAccess").into_iter();
    if let Some(snapshot) = access.next() {
        walk.chunk(|route| *route == Route::AuthAccess, &[snapshot]);
        walk.step("access snapshot");
    }
    walk.go("a pairing link", Event::CreatePairingLink);
    for item in access {
        walk.chunk(|route| *route == Route::AuthAccess, &[item]);
        walk.step("access changed");
    }
    walk.go("a second pairing link", Event::CreatePairingLink);
    // A link is of no use once it has expired.
    walk.go("the link expired", Event::Tick { now_ms: NOW + 40 * 86_400_000 });
    walk.ev(Event::Tick { now_ms: NOW });
    walk.go("remote access off", Event::SetRemoteAccess { enabled: false });
    walk.http.insert("/api/domovoi/remote".into(), (403, sample("api.domovoi.remote.rejected")));
    walk.go("remote access refused", Event::LoadRemoteAccess);
    walk.http.remove("/api/domovoi/remote");
    walk.go("no remote access on this server", Event::LoadRemoteAccess);
    walk.http.insert("/api/domovoi/remote".into(), (200, sample("api.domovoi.remote")));
    walk.go("remote access back", Event::LoadRemoteAccess);

    // Every setting the page shows, switched.
    let mut settings = recorded_settings();
    let fields: Vec<(String, String, bool, String, Vec<String>)> = walk.view().settings.groups.iter().flat_map(|group| group.fields.iter()).map(|field| (field.key.clone(), field.kind.clone(), field.bool_value, field.text_value.clone(), field.choices.iter().map(|choice| choice.value.clone()).collect())).collect();
    for (key, kind, on, shown, choices) in fields {
        match kind.as_str() {
            "bool" => {
                walk.go(&format!("{key} switched"), Event::SetSettingBool { key: key.clone(), value: !on });
                settings[key.as_str()] = json!(!on);
                walk.exit(|route| *route == Route::Settings, settings.clone());
                walk.step(&format!("{key} confirmed"));
            }
            "choice" => {
                let other = choices.iter().find(|choice| **choice != shown).cloned().unwrap_or_default();
                walk.go(&format!("{key} chosen"), Event::SetSettingText { key: key.clone(), value: other });
                walk.fail(|route| *route == Route::Settings, "no");
                walk.step(&format!("{key} refused"));
            }
            _ => {
                let typed = if shown.parse::<i64>().is_ok() { "12" } else { "somewhere" };
                walk.go(&format!("{key} typed"), Event::SetSettingText { key: key.clone(), value: typed.into() });
                settings[key.as_str()] = if shown.parse::<i64>().is_ok() { json!(12) } else { json!(typed) };
                walk.exit(|route| *route == Route::Settings, settings.clone());
                walk.step(&format!("{key} confirmed"));
            }
        }
    }
    walk.go("a number setting given words", Event::SetSettingText { key: "sidebarAutoSettleAfterDays".into(), value: "soon".into() });
    walk.fail(|route| *route == Route::Settings, "no");
    walk.step("words refused");

    // Keybindings: the server's list, one removed.
    for item in samples("subscribeServerConfig").into_iter().filter(|item| item["type"] == "keybindingsUpdated") {
        walk.chunk(|route| *route == Route::Config, &[item]);
        walk.step("keybindings");
    }
    if let Some(binding) = walk.view().settings.keybindings.first().cloned() {
        walk.go("keybinding removed", Event::RemoveKeybinding { command: binding.command, shortcut: binding.shortcut, condition: binding.condition });
        walk.exit(|route| *route == Route::Keybindings, sample("server.removeKeybinding"));
        walk.step("keybindings after");
    }
    for item in samples("subscribeServerConfig").into_iter().filter(|item| item["type"] == "providerStatuses") {
        walk.chunk(|route| *route == Route::Config, &[item]);
        walk.step("provider statuses");
    }
    walk.go("archived", Event::LoadSettings { section: "archived".into() });
}

/// Pull requests, usage, the import picker and the folder browser.
fn walk_pages(walk: &mut Walk) {
    let project = walk.model.projects.first().map(|project| (project.id.as_str().to_owned(), project.workspace_root.clone())).unwrap_or_default();
    walk.go("pull requests", Event::ShowRoute { route: "pull-requests".into() });
    let mut list = sample("pullRequests.list");
    for (n, entry) in list["entries"].as_array_mut().into_iter().flatten().enumerate() {
        // The first two are this server's project's; the last one's project is gone.
        if n < 2 {
            entry["projectId"] = json!(project.0);
        }
    }
    walk.exit(|route| *route == Route::PullRequests, list.clone());
    walk.step("pull requests listed");
    let keys: Vec<String> = walk.view().pull_requests.items.iter().map(|row| row.key.clone()).collect();
    for key in &keys {
        walk.go("pull request opened", Event::SelectPullRequest { key: key.clone() });
        walk.exit(|route| matches!(route, Route::PullRequestDetail(_)), sample("pullRequests.summary"));
        walk.step("pull request summary");
        walk.exit(|route| matches!(route, Route::PullRequestChecks(_)), sample("pullRequests.checks"));
        walk.step("pull request checks");
    }
    if let Some(key) = keys.first() {
        walk.go("merge", Event::PullRequestAction { key: key.clone(), action: "merge".into(), merge_method: "squash".into() });
        walk.exit(|route| matches!(route, Route::PullRequestAction { .. }), sample("pullRequests.runAction"));
        walk.step("merged");
        walk.exit(|route| *route == Route::PullRequests, list.clone());
        walk.exit(|route| matches!(route, Route::PullRequestDetail(_)), sample("pullRequests.summary"));
        walk.exit(|route| matches!(route, Route::PullRequestChecks(_)), Value::Null);
        walk.step("listed again");
        walk.go("review", Event::ReviewPullRequest { key: key.clone(), verdict: "approve".into(), body: " fine ".into() });
        walk.fail(|route| matches!(route, Route::PullRequestAction { .. }), "not allowed");
        walk.step("review refused");
        walk.go("comment", Event::CommentOnPullRequest { key: key.clone(), body: "note".into() });
        walk.exit(|route| matches!(route, Route::PullRequestAction { .. }), Value::Null);
        walk.exit(|route| *route == Route::PullRequests, list.clone());
        walk.exit(|route| matches!(route, Route::PullRequestDetail(_)), sample("pullRequests.summary"));
        walk.exit(|route| matches!(route, Route::PullRequestChecks(_)), sample("pullRequests.checks"));
        walk.step("commented");
    }
    for key in keys.iter().rev() {
        walk.go("pull request thread", Event::StartPullRequestThread { key: key.clone() });
        if walk.exit(|route| matches!(route, Route::PullRequestThread(_)), sample("git.preparePullRequestThread")) {
            walk.step("pull request checked out");
        }
    }
    let mut none = list.clone();
    none["entries"] = json!([]);
    walk.ev(Event::LoadPullRequests);
    walk.exit(|route| *route == Route::PullRequests, none);
    walk.step("no pull requests");
    walk.ev(Event::LoadPullRequests);
    walk.fail(|route| *route == Route::PullRequests, "gh is not signed in");
    walk.step("pull requests failed");

    walk.go("usage", Event::ShowRoute { route: "usage".into() });
    walk.exit(|route| *route == Route::Usage, sample("server.getUsageSummary"));
    walk.step("usage shown");
    for (days, name) in [(30, "server.getUsageSummary.unpriced"), (1, "server.getUsageSummary.hourly")] {
        walk.ev(Event::LoadUsage { days });
        walk.exit(|route| *route == Route::Usage, sample(name));
        walk.step(name);
    }

    walk.go("looking for sessions", Event::ScanAgentSessions);
    let mut scan = sample("agentSessions.scan");
    if let Some(first) = scan["candidates"].as_array_mut().and_then(|candidates| candidates.first_mut()) {
        // Already a project of this server.
        first["projectId"] = json!(project.0);
        first["path"] = json!(project.1);
    }
    walk.exit(|route| *route == Route::AgentScan, scan.clone());
    walk.step("sessions found");
    let paths: Vec<String> = walk.view().agent_sessions.candidates.iter().map(|candidate| candidate.path.clone()).collect();
    walk.go("importing", Event::ImportAgentSessions { paths });
    for _ in 0..16 {
        let created = walk.exit(|route| matches!(route, Route::ImportProject { .. }), sample("projects.createNew"));
        let imported = walk.exit(|route| matches!(route, Route::ImportSessions(_)), sample("agentSessions.import"));
        if !created && !imported {
            break;
        }
        walk.step("import step");
    }
    walk.exit(|route| *route == Route::AgentScan, scan);
    walk.step("imported");
    walk.ev(Event::ScanAgentSessions);
    walk.fail(|route| *route == Route::AgentScan, "cannot read ~/.claude");
    walk.step("scan failed");

    walk.go("browsing", Event::BrowseFolders { path: project.1.clone() });
    let parent = project.1.trim_end_matches('/').to_owned();
    walk.exit(|route| matches!(route, Route::Browse(_)), json!({ "parentPath": parent, "entries": [{ "name": "src", "fullPath": format!("{parent}/src") }, { "name": "docs", "fullPath": format!("{parent}/docs") }] }));
    walk.step("a project's folder");
    walk.go("browsing home", Event::BrowseFolders { path: "~".into() });
    walk.fail(|route| matches!(route, Route::Browse(_)), "no such folder");
    walk.step("browse failed");
    walk.go("a project by its path", Event::AddProject { path: "/work/new".into() });
    walk.go("a project by its name", Event::CreateProject { name: " new ".into() });
    walk.go("a project renamed", Event::RenameProject { project_id: project.0.clone(), title: " Renamed ".into() });
}

/// A thread opened from the list, its panel (diff, files, git, terminals),
/// its find bar, and the intents that read its row in the list.
fn walk_thread(walk: &mut Walk, thread: &str, snapshot: Option<Value>) {
    walk.go("thread opened", Event::SelectThread { thread_id: thread.to_owned() });
    if let Some(snapshot) = snapshot {
        walk.chunk(|route| matches!(route, Route::Thread(_)), &[snapshot]);
        walk.step("thread shown");
    }
    walk.go("panel", Event::ShowPanel { tab: "diff".into() });
    if walk.exit(|route| matches!(route, Route::Diff(_)), sample("review.getDiffPreview")) {
        walk.step("diff");
    }
    walk.go("branch diff", Event::SelectDiffSource { source: "branch".into() });
    walk.exit(|route| matches!(route, Route::Diff(_)), sample("review.getDiffPreview"));
    walk.step("branch diff shown");
    // The thread's own diff, where it has checkpoints to diff.
    walk.go("thread diff", Event::SelectDiffSource { source: "thread".into() });
    let of_thread = walk.request(|route| *route == Route::Diff("thread".into())).is_some();
    walk.exit(|route| matches!(route, Route::Diff(_)), sample(if of_thread { "orchestration.getFullThreadDiff" } else { "review.getDiffPreview" }));
    walk.step("thread diff shown");
    walk.ev(Event::SelectDiffSource { source: "working".into() });
    walk.exit(|route| matches!(route, Route::Diff(_)), json!({ "cwd": "/work", "generatedAt": "2026-11-17T00:00:00.000Z", "sources": [] }));
    walk.step("no changes");

    walk.go("files", Event::ShowPanel { tab: "files".into() });
    if walk.exit(|route| *route == Route::ListEntries(String::new()), sample("projects.listEntries")) {
        walk.step("files listed");
        let folder = walk.view().panel.files.entries.iter().find(|entry| entry.directory).map(|entry| entry.path.clone());
        if let Some(folder) = folder {
            walk.go("folder opened", Event::ToggleDirectory { path: folder.clone() });
            let mut inside = sample("projects.listEntries");
            for entry in inside["entries"].as_array_mut().into_iter().flatten() {
                entry["path"] = json!(format!("{folder}/{}", text(&entry["path"])));
            }
            walk.exit(|route| matches!(route, Route::ListEntries(_)), inside);
            walk.step("folder listed");
        }
        walk.go("file query", Event::SetFileQuery { query: "ma".into() });
        walk.exit(|route| matches!(route, Route::FileSearch(_)), sample("projects.searchEntries"));
        walk.step("files found");
        let file = walk.view().panel.files.results.iter().find(|entry| !entry.directory).map(|entry| entry.path.clone()).unwrap_or_else(|| "README.md".into());
        walk.go("file opened", Event::OpenFile { path: file.clone() });
        walk.exit(|route| matches!(route, Route::ReadFile(_)), json!({ "relativePath": file, "contents": "fn main() {}\n", "byteLength": 13, "truncated": false, "exists": true }));
        walk.step("file shown");
    }

    walk.go("git", Event::ShowPanel { tab: "git".into() });
    for item in samples("subscribeVcsStatus.domovoi-server") {
        if walk.chunk(|route| matches!(route, Route::VcsStatus(_)), &[item]) {
            walk.step("git status");
        }
    }
    if walk.exit(|route| matches!(route, Route::ListRefs(_)), sample("vcs.listRefs.domovoi-server")) {
        walk.step("branches");
    }
    walk.go("worktree from the branch", Event::SetWorkspaceMode { mode: "worktree".into() });
    for (name, message) in [("git.runStackedAction", "Fix it"), ("git.runStackedAction.failed", "")] {
        walk.go("git action", Event::GitAction { action: "commit_push".into(), message: message.into() });
        for item in samples(name) {
            if walk.chunk(|route| matches!(route, Route::GitAction(_)), &[item]) {
                walk.step("git progress");
            }
        }
        if let Some(id) = walk.request(|route| matches!(route, Route::GitAction(_))) {
            walk.model.rpc.forget(&id);
        }
    }
    walk.go("pull", Event::GitPull);
    walk.exit(|route| *route == Route::Command("Pull".into()), sample("vcs.pull"));
    walk.step("pulled");
    walk.go("git refreshed", Event::RefreshGit);

    walk.go("terminal", Event::ShowPanel { tab: "terminal".into() });
    for item in samples("terminal.attach") {
        if walk.chunk(|route| matches!(route, Route::Terminal { .. }), &[item]) {
            walk.step("terminal output");
        }
    }
    for mut item in samples("subscribeTerminalMetadata") {
        // This thread's terminals.
        for at in ["terminal", "terminals"] {
            match item.get_mut(at) {
                Some(Value::Array(list)) => list.iter_mut().for_each(|summary| summary["threadId"] = json!(thread)),
                Some(Value::Object(summary)) => {
                    summary.insert("threadId".into(), json!(thread));
                }
                _ => {}
            }
        }
        if item.get("threadId").is_some() {
            item["threadId"] = json!(thread);
        }
        walk.chunk(|route| *route == Route::TerminalMetadata, &[item]);
        walk.step("terminals");
    }
    walk.go("panel closed", Event::TogglePanel);

    walk.go("find", Event::FindInThread { query: "a".into() });
    let rows: Vec<String> = walk.view().thread.map(|page| page.items.iter().map(|row| row.id.clone()).collect()).unwrap_or_default();
    let mut found = sample("orchestration.searchThread");
    if !rows.is_empty() {
        let hit = found["navigation"][0].clone();
        let navigation: Vec<Value> = rows.iter().step_by(2).take(5).enumerate().map(|(n, row)| {
            let mut entry = hit.clone();
            entry["entryId"] = json!(row);
            entry["startIndex"] = json!(n * 2);
            entry["count"] = json!(2);
            entry
        }).collect();
        found["totalMatches"] = json!(navigation.len() * 2);
        found["activeIndex"] = json!(0);
        found["match"]["entryId"] = navigation[0]["entryId"].clone();
        found["navigation"] = json!(navigation);
    }
    if walk.exit(|route| matches!(route, Route::FindInThread(_)), found.clone()) {
        walk.step("found");
        walk.go("next match", Event::FindNext { forward: true });
        walk.exit(|route| matches!(route, Route::FindInThread(_)), found);
        walk.step("next match shown");
    }
    walk.go("find closed", Event::CloseFind);

    // What reads the thread's row in the list on its way to the server.
    walk.go("message typed", Event::SetDraft { text: "And then?".into() });
    walk.go("message sent", Event::Send);
    walk.exit(|route| matches!(route, Route::Send(_)), json!({ "sequence": 1 }));
    walk.step("message taken");
    walk.go("steer", Event::SetDraft { text: "Wait".into() });
    walk.go("steered", Event::Steer);
    walk.go("fork", Event::ForkThread { thread_id: thread.to_owned() });
    walk.go("merge back", Event::MergeBack);
    walk.go("snooze", Event::SnoozeThread { thread_id: thread.to_owned(), until_ms: NOW + 3_600_000 });
    walk.go("undo", Event::ToastAction);
    walk.go("answer from a live activity", Event::RespondToThread { environment_id: String::new(), thread_id: thread.to_owned(), decision: "accept".into() });
    walk.go("palette", Event::OpenPalette);
    walk.go("pin from the palette", Event::RunPaletteItem { id: "action:pin".into() });
    walk.go("archive", Event::ArchiveThread { thread_id: thread.to_owned(), archived: true });
    walk.go("toast gone", Event::DismissToast);
    walk.go("toast gone", Event::DismissToast);
}

/// Leaving this server for another and coming back, and a launch without
/// the server from what was saved.
fn walk_servers(walk: &mut Walk, thread: Option<&str>, snapshot: Option<Value>) {
    if let Some(thread) = thread {
        walk.go("thread opened", Event::SelectThread { thread_id: thread.to_owned() });
    }
    if let Some(snapshot) = snapshot {
        walk.chunk(|route| matches!(route, Route::Thread(_)), &[snapshot]);
        walk.step("thread shown");
    }
    // Long enough since the last save for the next tick to save.
    walk.go("a tick that saves", Event::Tick { now_ms: NOW + 600_000 });
    walk.go("to the background", Event::Suspend);
    let environment = walk.model.active_env.clone().unwrap_or_default();
    let copy = walk.store.get(&format!("cache:shell:{environment}")).cloned();
    let drafts = walk.store.get(&format!("drafts:{environment}")).cloned();
    if let (Ok(folder), Some(copy)) = (std::env::var("DOMOVOI_OFFLINE_COPIES_OUT"), &copy) {
        std::fs::create_dir_all(&folder).expect("the folder for offline copies");
        std::fs::write(format!("{folder}/{}.json", walk.name.replace(':', "-")), copy).expect("an offline copy written");
    }
    // The copy another build saved at this point of the same walk, if one is
    // given: what this build shows of it is measured instead.
    let copy = match std::env::var("DOMOVOI_OFFLINE_COPIES_IN") {
        Ok(folder) => Some(std::fs::read_to_string(format!("{folder}/{}.json", walk.name.replace(':', "-"))).expect("the other build's offline copy")),
        Err(_) => copy,
    };
    walk.go("back", Event::Resume);
    walk.socket(SocketEvent::Opened);
    walk.step("connected again");
    walk.go("another server", Event::Connect { server_url: "http://127.0.0.1:2".into(), pairing_code: "CODE".into() });
    walk.socket(SocketEvent::Opened);
    walk.step("connected to it");
    walk.go("the first server again", Event::SelectEnvironment { id: environment.clone() });

    // A launch with no server: the saved copy, and the thread that was open.
    let given = walk.given.clone();
    let mut fresh = Walk::new(&format!("{}:offline", walk.name), &mut *walk.sink);
    fresh.given = given;
    fresh.model.active_env = Some(environment.clone());
    fresh.ev(Event::Tick { now_ms: NOW + 700_000 });
    fresh.go("drafts back", Event::DraftsLoaded { environment_id: environment.clone(), value: drafts });
    fresh.go("the saved copy", Event::CacheLoaded { environment_id: environment.clone(), value: copy.clone() });
    fresh.go("unread only, offline", Event::SetUnreadOnly { on: true });
    fresh.ev(Event::SetUnreadOnly { on: false });
    fresh.go("palette, offline", Event::OpenPalette);
    fresh.go("another thread, offline", Event::NewThread { project_id: None });
    // A copy that is not one, and another server's.
    let name = format!("{}:offline-broken", walk.name);
    let mut broken = Walk::new(&name, &mut *fresh.sink);
    broken.model.active_env = Some(environment.clone());
    broken.go("a copy cut short", Event::CacheLoaded { environment_id: environment.clone(), value: copy.as_ref().map(|copy| copy[..copy.len() / 2].to_owned()) });
    broken.go("a copy of another version", Event::CacheLoaded { environment_id: environment.clone(), value: Some(json!({ "version": 99, "projects": [], "threads": [] }).to_string()) });
    broken.go("another server's copy", Event::CacheLoaded { environment_id: "elsewhere".into(), value: copy });
}

/// The whole of a client's screens on what it has now.
fn walk_everything(walk: &mut Walk, snapshot_of: &dyn Fn(&str) -> Option<Value>) {
    walk_lists(walk);
    walk_composer(walk);
    walk_settings(walk);
    walk_pages(walk);
    // Threads whose records are at hand, so their pages have rows.
    let (_, threads) = listed(&walk.view());
    let mut known = threads.iter().filter(|thread| snapshot_of(thread).is_some());
    let first = known.next().or(threads.first()).cloned();
    let second = known.next().or(threads.get(1)).cloned();
    if let Some(thread) = &first {
        walk_thread(walk, thread, snapshot_of(thread));
    }
    walk_servers(walk, second.as_deref(), second.as_deref().and_then(snapshot_of));
}

// ---- a server's own threads ----

fn stored<'a>(row: &'a Value, kind: &'a str) -> impl Iterator<Item = &'a Value> {
    row["entities"].as_array().into_iter().flatten().filter(move |stored| stored["kind"] == kind).map(|stored| &stored["record"])
}

/// The snapshot a server sends of the thread in `row` of the export.
fn snapshot_of(row: &Value) -> Value {
    let mut projection: Map<String, Value> = RecordKind::ALL.iter().map(|kind| (kind.collection().to_owned(), stored(row, kind.collection()).cloned().collect())).collect();
    projection.insert("thread".into(), row["thread"].clone());
    projection.insert("visibleTurnItems".into(), json!([]));
    projection.insert("updatedAt".into(), row["thread"]["updatedAt"].clone());
    json!({ "kind": "snapshot", "snapshotSequence": 1, "projection": projection })
}

/// The shell of the thread in `row`, as its records give it.
fn shell_of(row: &Value) -> Option<Value> {
    match ThreadStreamItem::from_value(snapshot_of(row)) {
        Ok(ThreadStreamItem::Snapshot(snapshot)) => serde_json::to_value(Thread::from_snapshot(*snapshot).shell()).ok(),
        _ => None,
    }
}

/// States the export has few or none of, given to some of its shells so
/// every branch of the list is walked: pinned, snoozed, settled, waiting on
/// the user, working, unread, forked, a subagent's, imported.
fn varied(mut shells: Vec<Value>) -> Vec<Value> {
    let ids: Vec<Value> = shells.iter().map(|shell| shell["id"].clone()).collect();
    let at = |ms: i64| domovoi_core::app::iso_from_millis(ms);
    for (n, shell) in shells.iter_mut().enumerate() {
        if n % 11 == 3 {
            shell["pinnedAt"] = json!(at(NOW - 1000));
        }
        if n % 13 == 5 {
            shell["snoozedUntil"] = json!(at(NOW + 3_600_000 + n as i64));
            shell["snoozedAt"] = json!(at(NOW - 60_000));
        }
        if n % 7 == 2 {
            shell["settledAt"] = json!(at(NOW - 86_400_000));
            shell["settledOverride"] = json!("settled");
        }
        if n % 23 == 7 {
            shell["pendingRuntimeRequest"] = json!({ "id": format!("request-{n}"), "kind": if n % 2 == 0 { "user_input" } else { "command" }, "createdAt": at(NOW - 5000) });
        }
        if n % 29 == 8 {
            shell["activeRunId"] = json!(format!("run-{n}"));
            shell["activityRunStatus"] = json!("running");
            shell["status"] = json!("running");
        }
        if n % 31 == 9 {
            shell["status"] = json!("completed");
            shell["lastVisitedAt"] = Value::Null;
        }
        if n % 17 == 4 && n > 0 {
            shell["lineage"]["parentThreadId"] = ids[n - 1].clone();
            shell["lineage"]["relationshipToParent"] = json!("fork");
        }
        if n % 19 == 6 && n > 0 {
            shell["lineage"]["parentThreadId"] = ids[n - 1].clone();
            shell["lineage"]["relationshipToParent"] = json!("subagent");
        }
        if n % 41 == 10 {
            shell["id"] = json!(format!("import:{}:{}", if n % 2 == 0 { "codex-1" } else { "claudeAgent" }, text(&shell["id"])));
        }
    }
    shells
}

/// A project as a server lists it.
fn project(id: &Value, n: usize) -> Value {
    json!({
        "id": id, "title": format!("Project {n}"), "workspaceRoot": if n == 5 { "/work/scratch".to_owned() } else { format!("/work/project-{n}") },
        "defaultModelSelection": null, "scripts": [], "pinnedAt": if n % 9 == 1 { json!("2026-11-01T00:00:00.000Z") } else { Value::Null },
        "createdAt": "2026-10-01T00:00:00.000Z", "updatedAt": "2026-10-01T00:00:00.000Z",
    })
}

/// The export's threads as rows, the shells of its threads, and a project
/// for each project they name.
fn exported(path: &str) -> (Vec<Value>, Vec<Value>, Vec<Value>) {
    let export = std::fs::read_to_string(path).expect("the exported threads");
    let rows: Vec<Value> = export.lines().filter(|line| !line.trim().is_empty()).map(|line| serde_json::from_str(line).expect("a JSON line")).collect();
    let shells = varied(rows.iter().filter_map(shell_of).collect());
    let mut projects: Vec<Value> = vec![];
    for shell in &shells {
        if !projects.iter().any(|project| project["id"] == shell["projectId"]) {
            projects.push(project(&shell["projectId"], projects.len()));
        }
    }
    (rows, shells, projects)
}

/// A client of a server that has the exported threads: booted from the
/// recorded config, listing every thread under every project.
fn walk_export(path: &str, sink: &mut dyn FnMut(Step)) {
    let (rows, shells, projects) = exported(path);
    let mut walk = Walk::connected("export", sink);
    for frame in recorded("ts-session1") {
        if frame.kind != "Chunk" {
            continue;
        }
        let route = match text(&frame.request["tag"]).as_str() {
            "subscribeServerConfig" => Route::Config,
            "subscribeServerLifecycle" => Route::Lifecycle,
            _ => continue,
        };
        walk.chunk(|open| *open == route, &frame.values);
    }
    walk.step("booted");
    walk.chunk(|route| *route == Route::Shell, &[json!({ "kind": "snapshot", "snapshot": { "schemaVersion": 2, "snapshotSequence": 10, "threads": shells, "archivedThreads": [], "projects": projects } })]);
    walk.step("every thread listed");
    let archived: Vec<Value> = shells.iter().step_by(40).map(|shell| {
        let mut shell = shell.clone();
        shell["archivedAt"] = json!("2026-11-10T00:00:00.000Z");
        shell
    }).collect();
    walk.exit(|route| *route == Route::Archived, json!({ "schemaVersion": 2, "snapshotSequence": 10, "projects": projects, "threads": archived }));
    walk.step("archived listed");
    walk.go("archived page", Event::LoadSettings { section: "archived".into() });

    // The list kept current: threads change, move to the archive and back, go.
    let mut sequence = 10;
    let mut next = || {
        sequence += 1;
        sequence
    };
    for (n, shell) in shells.iter().enumerate().filter(|(n, _)| n % 60 == 1) {
        let mut changed = shell.clone();
        changed["title"] = json!(format!("{} (renamed)", text(&shell["title"])));
        changed["status"] = json!(["completed", "failed", "running", "interrupted"][n % 4]);
        changed["pendingRuntimeRequest"] = if n % 120 == 1 { json!({ "id": format!("request-{n}"), "kind": "command", "createdAt": "2026-11-17T00:00:00.000Z" }) } else { Value::Null };
        changed["updatedAt"] = json!(domovoi_core::app::iso_from_millis(NOW - 1000 + n as i64));
        walk.chunk(|route| *route == Route::Shell, &[json!({ "kind": "thread.updated", "sequence": next(), "location": "active", "thread": changed })]);
        walk.step("a thread changed");
        if n % 180 == 1 {
            let mut archived = changed.clone();
            archived["archivedAt"] = json!("2026-11-17T00:00:00.000Z");
            walk.chunk(|route| *route == Route::Shell, &[json!({ "kind": "thread.updated", "sequence": next(), "location": "archive", "thread": archived })]);
            walk.step("a thread archived");
            walk.chunk(|route| *route == Route::Shell, &[json!({ "kind": "thread.updated", "sequence": next(), "location": "active", "thread": changed })]);
            walk.step("a thread unarchived");
        }
        if n % 240 == 1 {
            walk.chunk(|route| *route == Route::Shell, &[json!({ "kind": "thread.removed", "sequence": next(), "location": "active", "threadId": shell["id"] })]);
            walk.step("a thread removed");
        }
    }
    if let Some(first) = projects.first() {
        let mut renamed = first.clone();
        renamed["title"] = json!("Renamed project");
        walk.chunk(|route| *route == Route::Shell, &[json!({ "kind": "project.updated", "sequence": next(), "project": renamed })]);
        walk.step("a project renamed");
        walk.chunk(|route| *route == Route::Shell, &[json!({ "kind": "project.updated", "sequence": next(), "project": project(&json!("3b0e4f0a-0000-4000-8000-000000000001"), 99) })]);
        walk.step("a project added");
        walk.chunk(|route| *route == Route::Shell, &[json!({ "kind": "project.removed", "sequence": next(), "projectId": "3b0e4f0a-0000-4000-8000-000000000001" })]);
        walk.step("a project removed");
        let identified: Vec<Value> = projects.iter().take(3).map(|project| {
            let mut project = project.clone();
            project["title"] = json!(format!("{} (identified)", text(&project["title"])));
            project
        }).collect();
        walk.chunk(|route| *route == Route::Shell, &[json!({ "kind": "snapshot", "resolvedRepositoryIdentityRoots": ["/work/project-0"], "snapshot": { "schemaVersion": 2, "snapshotSequence": next(), "threads": [], "archivedThreads": [], "projects": identified } })]);
        walk.step("project identities");
        walk.chunk(|route| *route == Route::Shell, &[json!({ "kind": "synchronized" })]);
        walk.step("synchronized");
    }

    // Threads opened from the list: the page's head, the composer and the
    // panel read the thread's row.
    for row in rows.iter().step_by(12) {
        let thread = text(&row["threadId"]);
        walk.ev(Event::SelectThread { thread_id: thread.clone() });
        walk.step("a thread selected");
        if walk.chunk(|route| *route == Route::Thread(thread.clone()), &[snapshot_of(row)]) {
            walk.step("a thread shown");
        }
    }
    walk_everything(&mut walk, &|thread| rows.iter().find(|row| row["threadId"] == thread).map(snapshot_of));
}

/// Every step of the measurement, in order.
fn steps(sink: &mut dyn FnMut(Step)) {
    for name in ["ts-session1", "ts-session2-boot"] {
        let mut walk = Walk::connected(&format!("fixture:{name}"), sink);
        walk.step("connected");
        replay(&mut walk, name);
        // The snapshots the recorded server sent of its threads.
        let snapshots: Vec<(String, Value)> = recorded(name)
            .into_iter()
            .filter(|frame| frame.kind == "Chunk" && frame.request["tag"] == "orchestration.subscribeThread")
            .flat_map(|frame| frame.values.into_iter().filter(|value| value["kind"] == "snapshot").map(move |value| (text(&frame.request["payload"]["threadId"]), value)).collect::<Vec<_>>())
            .collect();
        walk_everything(&mut walk, &|thread| snapshots.iter().rev().find(|(of, _)| of == thread).map(|(_, snapshot)| snapshot.clone()));
    }
    if let Ok(path) = std::env::var("DOMOVOI_THREADS") {
        walk_export(&path, sink);
    }
}

/// The views the core builds and the frames it sends are the ones recorded.
/// `DOMOVOI_WALK_DIGESTS` names the recording; `DOMOVOI_THREADS` an export of a
/// server's threads, one line per thread (see `stored_threads_agree` in
/// `domovoi-model` for the query that writes it), without which only the
/// recorded sessions are walked. `DOMOVOI_WALK_DIGESTS_OUT` names a file to write
/// what is built now to, as the next recording; `DOMOVOI_WALK_DUMP` a folder to
/// write the views and frames of the steps that differ to;
/// `DOMOVOI_WALK_FRAMES_OUT` a file to write every step's frames to, one line per
/// step, to compare what two builds send frame by frame;
/// `DOMOVOI_OFFLINE_COPIES_OUT` a folder for the offline copies the walks save,
/// and `DOMOVOI_OFFLINE_COPIES_IN` a folder of copies saved that way by another
/// build, to launch from in place of this build's own.
///
/// ```sh
/// DOMOVOI_THREADS=threads.ndjson DOMOVOI_WALK_DIGESTS=walk.ndjson \
///   cargo test -p domovoi_core --test view_digests -- --ignored --nocapture
/// ```
#[test]
#[ignore = "measurement against a recording; needs DOMOVOI_WALK_DIGESTS (or DOMOVOI_WALK_DIGESTS_OUT to make one)"]
fn views_match_recorded_digests() {
    use std::io::Write as _;
    let mut recorded: BTreeMap<String, Value> = std::env::var("DOMOVOI_WALK_DIGESTS")
        .ok()
        .map(|path| std::fs::read_to_string(path).expect("the recording"))
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Value>(line).expect("a JSON line"))
        .map(|step| (text(&step["id"]), step))
        .collect();
    let comparing = !recorded.is_empty();
    let mut out = std::env::var("DOMOVOI_WALK_DIGESTS_OUT").ok().map(|path| std::io::BufWriter::new(std::fs::File::create(path).expect("the file to write")));
    let dump = std::env::var("DOMOVOI_WALK_DUMP").ok();
    let mut frames_out = std::env::var("DOMOVOI_WALK_FRAMES_OUT").ok().map(|path| std::io::BufWriter::new(std::fs::File::create(path).expect("the file for frames")));
    let mut refused: Vec<String> = vec![];
    let dump_all: Option<String> = std::env::var("DOMOVOI_WALK_DUMP_IDS").ok();
    let (mut same, mut views, mut frames, mut copies) = (BTreeMap::<String, usize>::new(), 0usize, 0usize, 0usize);
    let mut different: Vec<String> = vec![];
    let mut sink = |step: Step| {
        let now = step.digest();
        if let Some(out) = &mut out {
            writeln!(out, "{now}").expect("written");
        }
        if let Some(out) = &mut frames_out {
            let sent: Vec<Value> = step.sent.iter().map(|frame| serde_json::from_str(frame).unwrap_or_else(|_| json!(frame))).collect();
            writeln!(out, "{}", json!({ "id": step.id, "sent": sent })).expect("written");
        }
        refused.extend(step.refused.iter().map(|why| format!("{}: {why}", step.id)));
        let walk = step.id.split('#').next().unwrap_or_default().to_owned();
        let was = recorded.remove(&step.id);
        let differs = comparing && was.as_ref() != Some(&now);
        if let Some(folder) = &dump
            && (differs || dump_all.as_ref().is_some_and(|ids| ids.split(',').any(|id| step.id.contains(id))))
        {
            std::fs::create_dir_all(folder).expect("the dump folder");
            let name: String = step.id.chars().map(|c| if c.is_alphanumeric() || matches!(c, '#' | '-' | '.') { c } else { '_' }).collect();
            let parts: Map<String, Value> = step.parts.iter().map(|(name, part)| (name.clone(), serde_json::from_str(part).unwrap_or(Value::Null))).collect();
            let whole = json!({ "id": step.id, "view": parts, "sent": step.sent, "saved": step.saved });
            std::fs::write(format!("{folder}/{name}.json"), serde_json::to_string_pretty(&whole).expect("JSON")).expect("a dump written");
        }
        match was {
            Some(was) if was == now => {
                *same.entry(walk).or_default() += 1;
                views += 1;
                frames += step.sent.len();
                copies += step.saved.len();
            }
            Some(was) => {
                let mut what: Vec<String> = now["parts"].as_object().into_iter().flatten().filter(|(name, part)| was["parts"][name.as_str()] != **part).map(|(name, _)| format!("view.{name}")).collect();
                if was["frames"] != now["frames"] {
                    what.push(format!("frames sent ({} recorded, {} now)", was["sent"], now["sent"]));
                }
                if was["saved"] != now["saved"] {
                    what.push("the offline copy saved".into());
                }
                different.push(format!("{}: {}", step.id, what.join(", ")));
            }
            None if comparing => different.push(format!("{}: not in the recording", step.id)),
            None => {
                *same.entry(walk).or_default() += 1;
                views += 1;
            }
        }
    };
    steps(&mut sink);
    println!("{} commands or launches the server would not read as sent", refused.len());
    for why in refused.iter().take(40) {
        println!("    {why}");
    }
    for (walk, count) in &same {
        println!("{count} steps of {walk} {}", if comparing { "match the recording" } else { "recorded" });
    }
    if !comparing {
        println!("{views} steps written; nothing compared (DOMOVOI_WALK_DIGESTS names no recording)");
        assert!(refused.is_empty(), "{} commands or launches the server would not read as sent", refused.len());
        return;
    }
    // Steps of the recording this run did not reach (the export's, without the export).
    let export_given = std::env::var("DOMOVOI_THREADS").is_ok();
    let unbuilt: Vec<&String> = recorded.keys().filter(|id| export_given || !id.starts_with("export")).collect();
    println!("{views} views compared and equal, with {frames} frames sent and {copies} offline copies saved");
    println!("{} steps differ from the recording, {} recorded steps were not reached", different.len(), unbuilt.len());
    for step in different.iter().take(40) {
        println!("    {step}");
    }
    for id in unbuilt.iter().take(12) {
        println!("    not reached: {id}");
    }
    assert!(different.is_empty() && unbuilt.is_empty(), "{} steps differ from the recording, {} were not reached", different.len(), unbuilt.len());
    assert!(refused.is_empty(), "{} commands or launches the server would not read as sent", refused.len());
}

/// Every command and every launch the walks send is one domovoi-server reads as
/// it was written: it decodes as a `Command` of a known type (or as a
/// `ThreadLaunchInput`) and encodes back to what was sent. The server refuses
/// a command that does not decode, so this is what keeps the core and the
/// server's edge agreed. Walks the recorded sessions (and the export too
/// when `DOMOVOI_THREADS` names one).
#[test]
fn what_the_walks_send_is_read_by_the_server() {
    let (mut refused, mut commands) = (Vec::<String>::new(), 0usize);
    let mut sink = |step: Step| {
        commands += step.sent.iter().filter(|frame| frame.contains("\"orchestration.dispatchCommand\"") || frame.contains("\"orchestration.launchThread\"")).count();
        refused.extend(step.refused.iter().map(|why| format!("{}: {why}", step.id)));
    };
    steps(&mut sink);
    println!("{commands} commands and launches sent by the walks, {} not read as sent", refused.len());
    assert!(commands > 0, "the walks send commands");
    assert!(refused.is_empty(), "the server would not read these as sent:\n{}", refused.join("\n"));
}

/// What the view of a server's whole thread list costs, with no thread open:
/// a client booted from the recorded config, listing the exported threads.
/// A measurement to read, not an assertion; run it optimized.
///
/// ```sh
/// DOMOVOI_THREADS=threads.ndjson cargo test --release -p domovoi_core --test view_digests list_view_cost -- --ignored --nocapture
/// ```
#[test]
#[ignore = "a timing to read; needs DOMOVOI_THREADS"]
fn list_view_cost() {
    let path = std::env::var("DOMOVOI_THREADS").expect("DOMOVOI_THREADS names the exported threads");
    let (_, shells, projects) = exported(&path);
    let mut sink = |_: Step| {};
    let mut walk = Walk::connected("cost", &mut sink);
    for frame in recorded("ts-session1").into_iter().filter(|frame| frame.kind == "Chunk" && frame.request["tag"] == "subscribeServerConfig") {
        walk.chunk(|open| *open == Route::Config, &frame.values);
    }
    let snapshot = json!({ "kind": "snapshot", "snapshot": { "schemaVersion": 2, "snapshotSequence": 10, "threads": shells, "archivedThreads": [], "projects": projects } });
    // Reading the list as it arrives, then showing it.
    let frame = json!({ "_tag": "Chunk", "requestId": walk.request(|route| *route == Route::Shell).expect("the list's stream"), "values": [snapshot] }).to_string();
    let mut best = (f64::MAX, f64::MAX, f64::MAX);
    for _ in 0..7 {
        let started = std::time::Instant::now();
        for _ in 0..20 {
            walk.socket(SocketEvent::Message { text: frame.clone() });
        }
        best.0 = best.0.min(started.elapsed().as_secs_f64() * 1000.0 / 20.0);
        let started = std::time::Instant::now();
        let mut rows = 0;
        for _ in 0..200 {
            rows = listed(&walk.view()).1.len();
        }
        best.1 = best.1.min(started.elapsed().as_secs_f64() * 1000.0 / 200.0);
        walk.ev(Event::SetUnreadOnly { on: true });
        let started = std::time::Instant::now();
        for _ in 0..200 {
            let _ = walk.view();
        }
        best.2 = best.2.min(started.elapsed().as_secs_f64() * 1000.0 / 200.0);
        walk.ev(Event::SetUnreadOnly { on: false });
        assert!(rows > 0, "threads are listed");
    }
    println!("{} threads in {} projects: {:.3} ms to take in the list's snapshot, {:.3} ms per view, {:.3} ms per view of unread threads only (best of 7)", shells.len(), projects.len(), best.0, best.1, best.2);
}
