//! Preview tabs, after t3code's `PreviewManager` (`preview/Manager.ts`), plus
//! local dev-server discovery (`rpc/local_servers.rs`).
//!
//! The manager is ported in full: the server owns each tab's snapshot (URL,
//! navigation status, viewport, colour scheme, zoom) and publishes `opened`,
//! `navigated`, `resized`, `failed` and `closed` events with a revision. A
//! desktop tab's own browser renders the page and reports status back; a tab
//! opened with `runtime: "server"` is driven by the server's headless browser
//! (`crate::preview::browser`, t3code's `ServerBrowser`), which mirrors these
//! events and reports the tab's status itself (`serverControlled`).
//! `preview.reportProfiles` and `preview.clearProfile` go to that browser.
//!
//! Limits:
//! - The manager lives for the process (like t3code's layer), so its state is a
//!   process-wide instance rather than an `AppState` field.
//! - No `registerBeforeClose`: a closed server tab's page is closed after the
//!   `closed` event, not before it.

use super::Never;
use super::admin_failure::{Failure, InvalidUrlReason, UrlCause};
use crate::state::ConnCtx;
use domovoi_rpc::Router;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use serde_json::Number;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

// ---- URL normalization (`normalizePreviewUrl` in shared/preview.ts) ----

const KNOWN_NON_WEB_SCHEMES: &[&str] = &[
    "about:", "blob:", "chrome:", "data:", "file:", "ftp:", "javascript:", "mailto:", "sms:", "ssh:", "tel:",
    "view-source:", "ws:", "wss:",
];

#[derive(Debug, PartialEq)]
pub struct InvalidUrl {
    pub input_length: usize,
    pub reason: InvalidUrlReason,
    pub protocol: Option<String>,
}

impl From<InvalidUrl> for Failure {
    fn from(invalid: InvalidUrl) -> Self {
        let InvalidUrl { input_length, reason, protocol } = invalid;
        let named = protocol.as_ref().map(|p| format!(": {p}")).unwrap_or_default();
        let message = format!(
            "Invalid preview URL ({}{named}; input length {input_length}). Use an http(s) URL or a schemeless host such as localhost:5173.",
            reason.as_str()
        );
        Failure::PreviewInvalidUrl { input_length, reason, protocol, cause: UrlCause { name: "PreviewUrlNormalizationError", message } }
    }
}

/// `^([A-Za-z][A-Za-z\d+.-]*):`, lower-cased with its colon.
fn explicit_protocol(s: &str) -> Option<String> {
    let mut chars = s.char_indices();
    let (_, first) = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    for (i, c) in chars {
        if c == ':' {
            return Some(format!("{}:", s[..i].to_ascii_lowercase()));
        }
        if !(c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')) {
            return None;
        }
    }
    None
}

/// `^[a-z][a-z\d+.-]*://` (case-insensitive).
fn has_scheme_with_authority(s: &str) -> bool {
    explicit_protocol(s).is_some_and(|p| s[p.len()..].starts_with("//"))
}

/// `^(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1?\])(?::|/|$)` (case-insensitive).
fn has_loopback_prefix(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    ["localhost", "127.0.0.1", "0.0.0.0", "[::1]", "[::]"].iter().any(|host| {
        lower.strip_prefix(host).is_some_and(|rest| rest.is_empty() || rest.starts_with(':') || rest.starts_with('/'))
    })
}

pub fn normalize_preview_url(raw: &str) -> Result<String, InvalidUrl> {
    let input_length = raw.encode_utf16().count();
    let fail = |reason, protocol| InvalidUrl { input_length, reason, protocol };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(fail(InvalidUrlReason::Empty, None));
    }
    if let Some(p) = explicit_protocol(trimmed).filter(|p| KNOWN_NON_WEB_SCHEMES.contains(&p.as_str())) {
        return Err(fail(InvalidUrlReason::UnsupportedProtocol, Some(p)));
    }
    let candidate = if has_scheme_with_authority(trimmed) {
        trimmed.to_owned()
    } else {
        format!("{}://{trimmed}", if has_loopback_prefix(trimmed) { "http" } else { "https" })
    };
    let parsed = url::Url::parse(&candidate).map_err(|_| fail(InvalidUrlReason::Parse, explicit_protocol(&candidate)))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(fail(InvalidUrlReason::UnsupportedProtocol, Some(format!("{}:", parsed.scheme()))));
    }
    Ok(parsed.to_string())
}

// ---- the manager ----

/// How large a tab's page is drawn (`PreviewViewportSetting`). Sizes are kept
/// as the numbers the client sent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "_tag", rename_all = "lowercase")]
pub enum Viewport {
    /// As large as the pane showing it.
    Fill,
    Freeform { width: Number, height: Number },
    /// A device's size, by the preset's name.
    #[serde(rename_all = "camelCase")]
    Preset { width: Number, height: Number, preset_id: String },
}

/// A download a failed load turned out to be (`PreviewFileDownload`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDownload {
    id: String,
    file_name: String,
}

/// What a tab's page is doing (`PreviewNavStatus`), as the client's browser reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum NavStatus {
    Idle,
    Loading { url: String, title: String },
    Success { url: String, title: String },
    LoadFailed {
        url: String,
        title: String,
        /// The browser's error code, as sent.
        code: Number,
        description: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        download: Option<FileDownload>,
    },
}

/// Defines a closed set of strings of the preview contract that the server
/// keeps and sends back without reading: a value outside the set is kept as
/// it was sent.
macro_rules! kept_string {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $literal:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $literal)] $variant,)+
            /// A value this build does not know.
            #[serde(untagged)]
            Other(String),
        }
    };
}

kept_string! {
    /// Light or dark, as a tab's page is told (`PreviewAppearancePreference`).
    ColorScheme { System = "system", Light = "light", Dark = "dark" }
}

kept_string! {
    /// Whose browser renders a tab (`PreviewRuntime`).
    Runtime { Desktop = "desktop", Server = "server" }
}

kept_string! {
    /// What a client is asked to clear of a tab's page.
    Clear { Cookies = "cookies", Cache = "cache" }
}

/// A preview tab as the server holds it (`PreviewSessionSnapshot`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub thread_id: String,
    pub tab_id: String,
    pub nav_status: NavStatus,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub viewport: Viewport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color_scheme: Option<ColorScheme>,
    /// One of the zoom steps, as the number the client sent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zoom_factor: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<Runtime>,
    /// The server runs this tab in its headless browser, so clients stream it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headless: Option<bool>,
    /// The agent session that opened this server tab.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automation_owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reveal: Option<bool>,
    /// An agent asked clients to show the tab (`requestReveal`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reveal_request: Option<RevealRequest>,
    pub updated_at: String,
}

/// `revealRequest`: a new id each time an agent asks to show a tab; `force`
/// when it asked explicitly.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RevealRequest {
    pub id: String,
    pub force: bool,
}

/// What the client's browser is asked to do besides taking a tab's new settings.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReloadRequest {
    /// Only ever `true`: a reload that was not asked for is left out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hard_reload: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clear: Option<Clear>,
}

/// What happened to a tab (the member of `PreviewEvent`, named by `type`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Change {
    Opened { snapshot: Snapshot },
    Navigated { snapshot: Snapshot },
    /// The viewport, colour scheme or zoom changed.
    Resized {
        snapshot: Snapshot,
        #[serde(skip_serializing_if = "Option::is_none")]
        request: Option<ReloadRequest>,
    },
    /// The page did not load.
    Failed {
        url: String,
        title: String,
        code: Number,
        description: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        download: Option<FileDownload>,
    },
    Closed,
}

/// A change to a tab as subscribers are told (`PreviewEvent`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub thread_id: String,
    pub tab_id: String,
    created_at: String,
    /// Which server process counts the revisions.
    server_epoch: String,
    revision: u64,
    #[serde(flatten)]
    pub change: Change,
}

/// The tabs of a thread (`PreviewListResult`), least recently changed first.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TabList {
    pub sessions: Vec<Snapshot>,
    server_epoch: String,
    revision: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenInput {
    pub thread_id: String,
    pub url: Option<String>,
    pub viewport: Option<Viewport>,
    pub profile_id: Option<String>,
    pub runtime: Option<Runtime>,
    pub color_scheme: Option<ColorScheme>,
    pub zoom_factor: Option<Number>,
    pub reveal: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResizeInput {
    pub thread_id: String,
    pub tab_id: String,
    pub viewport: Viewport,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdjustInput {
    pub thread_id: String,
    pub tab_id: String,
    pub color_scheme: Option<ColorScheme>,
    pub zoom_factor: Option<Number>,
    pub hard_reload: Option<bool>,
    pub clear: Option<Clear>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseInput {
    pub thread_id: String,
    pub tab_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportStatusInput {
    pub thread_id: String,
    pub tab_id: String,
    pub nav_status: NavStatus,
    pub can_go_back: bool,
    pub can_go_forward: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadOnly {
    thread_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClearProfileInput {
    profile_id: String,
}

#[derive(Default)]
struct ManagerState {
    /// Keyed by `(threadId, tabId)`.
    sessions: BTreeMap<(String, String), Snapshot>,
    revision: u64,
}

pub struct PreviewManager {
    state: Mutex<ManagerState>,
    events: broadcast::Sender<Event>,
    tab_sequence: AtomicU64,
}

impl Default for PreviewManager {
    fn default() -> Self {
        PreviewManager { state: Mutex::default(), events: broadcast::channel(256).0, tab_sequence: AtomicU64::new(0) }
    }
}

impl PreviewManager {
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// `newPreviewTabId`: `tab_` plus a base-36 sequence number.
    fn new_tab_id(&self) -> String {
        let n = self.tab_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let mut digits = vec![];
        let mut v = n;
        while v > 0 {
            digits.push(std::char::from_digit((v % 36) as u32, 36).expect("a digit below 36"));
            v /= 36;
        }
        format!("tab_{}", digits.iter().rev().collect::<String>())
    }

    /// Tells subscribers what happened to the tab at `created_at`, as the state's `revision`.
    fn publish(&self, epoch: &str, revision: u64, (thread_id, tab_id): (&str, &str), created_at: String, change: Change) {
        let event = Event { thread_id: thread_id.to_owned(), tab_id: tab_id.to_owned(), created_at, server_epoch: epoch.to_owned(), revision, change };
        let _ = self.events.send(event);
    }

    /// `open`; `automation_owner` names the agent session opening a server tab.
    pub fn open(&self, epoch: &str, input: OpenInput, automation_owner: Option<String>) -> Result<Snapshot, Failure> {
        // Persisted client surfaces must not bind to a different tab after a restart.
        let tab_id = match input.runtime {
            Some(Runtime::Server) => format!("{}_{epoch}", self.new_tab_id()),
            _ => self.new_tab_id(),
        };
        let nav_status = match &input.url {
            Some(url) => NavStatus::Loading { url: normalize_preview_url(url)?, title: String::new() },
            None => NavStatus::Idle,
        };
        let updated_at = crate::time_fmt::iso_now();
        let snapshot = Snapshot {
            thread_id: input.thread_id.clone(),
            tab_id: tab_id.clone(),
            nav_status,
            can_go_back: false,
            can_go_forward: false,
            viewport: input.viewport.unwrap_or(Viewport::Fill),
            color_scheme: input.color_scheme,
            zoom_factor: input.zoom_factor,
            profile_id: input.profile_id,
            automation_owner: automation_owner.filter(|_| input.runtime == Some(Runtime::Server)),
            runtime: input.runtime,
            headless: None,
            reveal: input.reveal,
            reveal_request: None,
            updated_at: updated_at.clone(),
        };
        let mut state = crate::util::locked(&self.state);
        state.revision += 1;
        state.sessions.insert((input.thread_id.clone(), tab_id.clone()), snapshot.clone());
        self.publish(epoch, state.revision, (&input.thread_id, &tab_id), updated_at, Change::Opened { snapshot: snapshot.clone() });
        Ok(snapshot)
    }

    /// `mutateExistingSession`: applies `f` to the tab's snapshot (with the
    /// time of the change, which it also publishes its event at), all under
    /// the lock so events keep state-transition order.
    fn mutate(
        &self,
        epoch: &str,
        thread_id: &str,
        tab_id: &str,
        f: impl FnOnce(&mut Snapshot, &str) -> Result<Change, Failure>,
    ) -> Result<Snapshot, Failure> {
        let mut state = crate::util::locked(&self.state);
        let key = (thread_id.to_owned(), tab_id.to_owned());
        let Some(current) = state.sessions.get(&key) else {
            return Err(Failure::no_session(thread_id, tab_id));
        };
        let mut next = current.clone();
        let at = crate::time_fmt::iso_now();
        let change = f(&mut next, &at)?;
        state.revision += 1;
        state.sessions.insert(key, next.clone());
        self.publish(epoch, state.revision, (thread_id, tab_id), at, change);
        Ok(next)
    }

    /// `reportStatus`; only the server's own browser (`server_controlled`) reports a server tab.
    pub fn report_status(&self, epoch: &str, input: ReportStatusInput, server_controlled: bool) -> Result<(), Failure> {
        let ReportStatusInput { thread_id, tab_id, nav_status, can_go_back, can_go_forward } = input;
        self.mutate(epoch, &thread_id, &tab_id, |s, at| {
            if s.runtime == Some(Runtime::Server) && !server_controlled {
                return Err(Failure::PreviewControlRequired { tab_id: tab_id.clone() });
            }
            s.nav_status = nav_status.clone();
            s.can_go_back = can_go_back;
            s.can_go_forward = can_go_forward;
            s.updated_at = at.to_owned();
            Ok(match nav_status {
                NavStatus::LoadFailed { url, title, code, description, download } => Change::Failed { url, title, code, description, download },
                _ => Change::Navigated { snapshot: s.clone() },
            })
        })
        .map(|_| ())
    }

    /// `markHeadless`: the server runs the tab headless, until it closes.
    #[cfg_attr(not(feature = "preview-browser"), expect(dead_code, reason = "only the browser marks tabs"))]
    pub fn mark_headless(&self, epoch: &str, thread_id: &str, tab_id: &str) -> Result<(), Failure> {
        if self.snapshot(thread_id, tab_id).is_some_and(|s| s.headless == Some(true)) {
            return Ok(());
        }
        self.mutate(epoch, thread_id, tab_id, |s, at| {
            s.headless = Some(true);
            s.updated_at = at.to_owned();
            Ok(Change::Resized { snapshot: s.clone(), request: None })
        })
        .map(|_| ())
    }

    /// `requestReveal`: asks clients to show the tab.
    #[cfg_attr(not(feature = "preview-browser"), expect(dead_code, reason = "only the browser's agents ask"))]
    pub fn request_reveal(&self, epoch: &str, thread_id: &str, tab_id: &str, force: bool) -> Result<(), Failure> {
        self.mutate(epoch, thread_id, tab_id, |s, at| {
            s.reveal = Some(true);
            s.reveal_request = Some(RevealRequest { id: crate::util::new_uuid(), force });
            s.updated_at = at.to_owned();
            Ok(Change::Navigated { snapshot: s.clone() })
        })
        .map(|_| ())
    }

    /// The tab as the manager holds it.
    pub fn snapshot(&self, thread_id: &str, tab_id: &str) -> Option<Snapshot> {
        crate::util::locked(&self.state).sessions.get(&(thread_id.to_owned(), tab_id.to_owned())).cloned()
    }

    pub fn resize(&self, epoch: &str, input: ResizeInput) -> Result<Snapshot, Failure> {
        self.mutate(epoch, &input.thread_id, &input.tab_id, |s, at| {
            s.viewport = input.viewport.clone();
            s.updated_at = at.to_owned();
            Ok(Change::Resized { snapshot: s.clone(), request: None })
        })
    }

    pub fn adjust(&self, epoch: &str, input: AdjustInput) -> Result<Snapshot, Failure> {
        self.mutate(epoch, &input.thread_id, &input.tab_id, |s, at| {
            if let Some(c) = &input.color_scheme {
                s.color_scheme = Some(c.clone());
            }
            if let Some(z) = &input.zoom_factor {
                s.zoom_factor = Some(z.clone());
            }
            s.updated_at = at.to_owned();
            let hard_reload = input.hard_reload == Some(true);
            let request = (hard_reload || input.clear.is_some()).then(|| ReloadRequest { hard_reload: hard_reload.then_some(true), clear: input.clear.clone() });
            Ok(Change::Resized { snapshot: s.clone(), request })
        })
    }

    pub fn close(&self, epoch: &str, input: CloseInput) {
        let created_at = crate::time_fmt::iso_now();
        let mut state = crate::util::locked(&self.state);
        let targets: Vec<(String, String)> = state
            .sessions
            .keys()
            .filter(|(thread, tab)| *thread == input.thread_id && input.tab_id.as_ref().is_none_or(|t| t == tab))
            .cloned()
            .collect();
        for key in targets {
            state.sessions.remove(&key);
            state.revision += 1;
            self.publish(epoch, state.revision, (&key.0, &key.1), created_at.clone(), Change::Closed);
        }
    }

    pub fn list(&self, epoch: &str, thread_id: &str) -> TabList {
        let state = crate::util::locked(&self.state);
        let mut sessions: Vec<Snapshot> = state.sessions.iter().filter(|((thread, _), _)| thread == thread_id).map(|(_, s)| s.clone()).collect();
        sessions.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
        TabList { sessions, server_epoch: epoch.to_owned(), revision: state.revision }
    }
}

static MANAGER: LazyLock<PreviewManager> = LazyLock::new(PreviewManager::default);

/// The process's preview manager.
pub fn manager() -> &'static PreviewManager {
    &MANAGER
}

fn events() -> BoxStream<'static, Result<Event, Never>> {
    let mut rx = MANAGER.subscribe();
    async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(event) => yield Ok(event),
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    }
    .boxed()
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.unary("preview.open", |ctx: Arc<ConnCtx>, input: OpenInput| async move {
        // The server's browser must be listening before the tab is announced.
        crate::preview::host::start(&ctx.state);
        MANAGER.open(&ctx.state.server_epoch, input, None)
    })
        .unary("preview.resize", |ctx: Arc<ConnCtx>, input: ResizeInput| async move {
            MANAGER.resize(&ctx.state.server_epoch, input)
        })
        .unary("preview.adjust", |ctx: Arc<ConnCtx>, input: AdjustInput| async move {
            MANAGER.adjust(&ctx.state.server_epoch, input)
        })
        .unary("preview.close", |ctx: Arc<ConnCtx>, input: CloseInput| async move {
            MANAGER.close(&ctx.state.server_epoch, input);
            Ok::<_, Never>(())
        })
        .unary("preview.list", |ctx: Arc<ConnCtx>, input: ThreadOnly| async move {
            Ok::<_, Never>(MANAGER.list(&ctx.state.server_epoch, &input.thread_id))
        })
        .unary("preview.reportStatus", |ctx: Arc<ConnCtx>, input: ReportStatusInput| async move {
            MANAGER.report_status(&ctx.state.server_epoch, input, false)
        })
        .unary("preview.reportProfiles", |ctx: Arc<ConnCtx>, input: crate::preview::host::ProfilesReport| async move {
            crate::preview::host::report_profiles(&ctx.state, input);
            Ok::<_, Never>(())
        })
        .unary("preview.clearProfile", |ctx: Arc<ConnCtx>, input: ClearProfileInput| async move {
            crate::preview::host::clear_profile(&ctx.state, &input.profile_id)
                .await
                .map_err(|cause| Failure::PreviewClearProfile { profile_id: input.profile_id, cause: super::admin_failure::Cause::of(cause) })
        })
        .stream("subscribePreviewEvents", |_, _: IgnoredAny| events())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn normalizes_values() {
        assert_eq!(normalize_preview_url("localhost:5173").unwrap(), "http://localhost:5173/");
        assert_eq!(normalize_preview_url("  127.0.0.1:3000/app ").unwrap(), "http://127.0.0.1:3000/app");
        assert_eq!(normalize_preview_url("[::1]:8080").unwrap(), "http://[::1]:8080/");
        assert_eq!(normalize_preview_url("example.com").unwrap(), "https://example.com/");
        assert_eq!(normalize_preview_url("HTTP://Example.com/a b").unwrap(), "http://example.com/a%20b");
        let e = normalize_preview_url("   ").unwrap_err();
        assert_eq!((e.reason, e.input_length), (InvalidUrlReason::Empty, 3));
        let e = normalize_preview_url("file:///etc/passwd").unwrap_err();
        assert_eq!((e.reason, e.protocol.as_deref()), (InvalidUrlReason::UnsupportedProtocol, Some("file:")));
        let e = normalize_preview_url("custom://x").unwrap_err();
        assert_eq!((e.reason, e.protocol.as_deref()), (InvalidUrlReason::UnsupportedProtocol, Some("custom:")));
        let e = normalize_preview_url("http://[bad").unwrap_err();
        assert_eq!((e.reason, e.protocol.as_deref()), (InvalidUrlReason::Parse, Some("http:")));
        assert_eq!(
            json!(Failure::from(e)),
            json!({ "_tag": "PreviewInvalidUrlError", "inputLength": 11, "reason": "parse", "protocol": "http:", "cause": { "name": "PreviewUrlNormalizationError",
                "message": "Invalid preview URL (parse: http:; input length 11). Use an http(s) URL or a schemeless host such as localhost:5173." } })
        );
    }

    #[test]
    fn tab_ids_are_base36_sequences() {
        let m = PreviewManager::default();
        m.tab_sequence.store(35, Ordering::Relaxed);
        assert_eq!(m.new_tab_id(), "tab_10");
        assert_eq!(m.new_tab_id(), "tab_11");
    }

    /// An input as a client sends it.
    fn sent<T: serde::de::DeserializeOwned>(payload: Value) -> T {
        serde_json::from_value(payload).unwrap()
    }

    fn open(m: &PreviewManager, runtime: Option<&str>) -> Snapshot {
        m.open("epoch", sent(json!({ "threadId": "t", "url": "localhost:3000", "runtime": runtime })), Some("agent".into())).unwrap()
    }

    /// The event with the time it happened at left out.
    fn timeless(event: Event) -> Value {
        let mut event = json!(event);
        for at in ["/createdAt", "/snapshot/updatedAt"] {
            if let Some(at) = event.pointer_mut(at) {
                *at = json!("<at>");
            }
        }
        event
    }

    #[test]
    fn lifecycle_publishes_ordered_events() {
        let m = PreviewManager::default();
        let mut rx = m.subscribe();
        let s = open(&m, None);
        assert_eq!(s.tab_id, "tab_1");
        assert_eq!(json!(s.nav_status), json!({ "_tag": "Loading", "url": "http://localhost:3000/", "title": "" }));
        assert_eq!(json!(s.viewport), json!({ "_tag": "fill" }));
        let resized = m.resize("epoch", sent(json!({ "threadId": "t", "tabId": "tab_1", "viewport": { "_tag": "freeform", "width": 300, "height": 200 } }))).unwrap();
        assert_eq!(json!(resized.viewport), json!({ "_tag": "freeform", "width": 300, "height": 200 }));
        let adjusted = m.adjust("epoch", sent(json!({ "threadId": "t", "tabId": "tab_1", "colorScheme": "dark", "zoomFactor": 1, "hardReload": true }))).unwrap();
        assert_eq!(adjusted.color_scheme, Some(ColorScheme::Dark));
        let failed = json!({ "_tag": "LoadFailed", "url": "http://localhost:3000/", "title": "", "code": -102, "description": "refused" });
        m.report_status("epoch", sent(json!({ "threadId": "t", "tabId": "tab_1", "navStatus": failed, "canGoBack": true, "canGoForward": false })), false).unwrap();
        let listed = m.list("epoch", "t");
        // The tab as the client is sent it: the sizes and the zoom step are the numbers it gave.
        let snapshot = json!({ "threadId": "t", "tabId": "tab_1", "navStatus": failed, "canGoBack": true, "canGoForward": false,
            "viewport": { "_tag": "freeform", "width": 300, "height": 200 }, "colorScheme": "dark", "zoomFactor": 1, "updatedAt": listed.sessions[0].updated_at });
        assert_eq!(json!(listed), json!({ "sessions": [snapshot], "serverEpoch": "epoch", "revision": 4 }));
        m.close("epoch", CloseInput { thread_id: "t".into(), tab_id: None });
        assert_eq!(json!(m.list("epoch", "t")), json!({ "sessions": [], "serverEpoch": "epoch", "revision": 5 }));

        let seen: Vec<Value> = (0..5).map(|_| timeless(rx.try_recv().unwrap())).collect();
        let event = |kind: &str, revision: u64| json!({ "type": kind, "threadId": "t", "tabId": "tab_1", "createdAt": "<at>", "serverEpoch": "epoch", "revision": revision });
        let with = |mut event: Value, more: Value| {
            event.as_object_mut().unwrap().extend(more.as_object().unwrap().clone());
            event
        };
        let loading = json!({ "threadId": "t", "tabId": "tab_1", "navStatus": { "_tag": "Loading", "url": "http://localhost:3000/", "title": "" },
            "canGoBack": false, "canGoForward": false, "viewport": { "_tag": "fill" }, "updatedAt": "<at>" });
        assert_eq!(seen[0], with(event("opened", 1), json!({ "snapshot": loading })));
        let sized = with(loading, json!({ "viewport": { "_tag": "freeform", "width": 300, "height": 200 } }));
        assert_eq!(seen[1], with(event("resized", 2), json!({ "snapshot": sized })));
        let dark = with(sized, json!({ "colorScheme": "dark", "zoomFactor": 1 }));
        assert_eq!(seen[2], with(event("resized", 3), json!({ "snapshot": dark, "request": { "hardReload": true } })));
        assert_eq!(seen[3], with(event("failed", 4), json!({ "url": "http://localhost:3000/", "title": "", "code": -102, "description": "refused" })));
        assert_eq!(seen[4], event("closed", 5));
    }

    #[test]
    fn server_tabs_need_server_control_and_unknown_tabs_fail() {
        let m = PreviewManager::default();
        let s = open(&m, Some("server"));
        assert_eq!(s.tab_id, "tab_1_epoch");
        let report = |tab: &str| sent(json!({ "threadId": "t", "tabId": tab, "navStatus": { "_tag": "Idle" }, "canGoBack": false, "canGoForward": false }));
        assert!(matches!(m.report_status("epoch", report("tab_1_epoch"), false), Err(Failure::PreviewControlRequired { .. })));
        // The server's browser reports it; an agent's tab names its owner, and is headless once marked.
        m.report_status("epoch", report("tab_1_epoch"), true).unwrap();
        assert_eq!(s.automation_owner.as_deref(), Some("agent"));
        m.mark_headless("epoch", "t", "tab_1_epoch").unwrap();
        m.mark_headless("epoch", "t", "tab_1_epoch").unwrap();
        m.request_reveal("epoch", "t", "tab_1_epoch", true).unwrap();
        let shown = m.snapshot("t", "tab_1_epoch").unwrap();
        assert_eq!((shown.headless, shown.reveal, shown.reveal_request.map(|r| r.force)), (Some(true), Some(true), Some(true)));
        assert_eq!(m.list("epoch", "t").revision, 4, "marking twice publishes once");
        let input = ResizeInput { thread_id: "t".into(), tab_id: "nope".into(), viewport: Viewport::Fill };
        assert!(matches!(m.resize("epoch", input), Err(Failure::PreviewSessionLookup { .. })));
    }

    #[test]
    fn what_a_newer_client_names_is_kept_as_sent() {
        let m = PreviewManager::default();
        let s = m.open("epoch", sent(json!({ "threadId": "t", "runtime": "cloud", "viewport": null })), Some("agent".into())).unwrap();
        assert_eq!(s.automation_owner, None, "only a server tab has an owner");
        assert_eq!(json!(s.runtime), json!("cloud"));
        assert_eq!((&s.tab_id[..], &s.viewport), ("tab_1", &Viewport::Fill));
        let adjusted = m.adjust("epoch", sent(json!({ "threadId": "t", "tabId": "tab_1", "colorScheme": "sepia", "zoomFactor": 1.25, "clear": "storage" }))).unwrap();
        assert_eq!(json!(adjusted.color_scheme), json!("sepia"));
        assert_eq!(json!(adjusted.zoom_factor), json!(1.25));
    }
}
