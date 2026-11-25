//! The server's headless browser (t3code `ServerBrowser.ts`): it runs every
//! preview tab opened with `runtime: "server"` in a Chrome page it drives
//! over the DevTools protocol, reports the page's status to the manager
//! (`serverControlled`), streams it to viewers (`stream.rs`) and runs the
//! agents' preview tools.
//!
//! As t3code's:
//! - It mirrors the manager's events: a server tab a client opened starts at
//!   once (and is marked `headless`); a closed tab's page closes; a resize or
//!   adjustment applies the viewport, colour scheme, zoom, cookie or cache
//!   clearing and hard reload. A page that closes itself closes its tab.
//! - Pages are drawn at device scale 2 (zoom multiplies it); a `fill` tab
//!   takes the latest viewer's size, else 1280x800.
//! - Control (t3code `SessionControl`): a tab an agent opened is that agent's;
//!   one a client opened is unclaimed. A viewer with `preview:operate` takes
//!   control of an unclaimed tab on attach, and with `takeControl` of any tab
//!   nobody else controls; while a human controls a tab agents may only read
//!   it (`status`, `snapshot`, `waitFor`). Agent actions and controller input
//!   run one at a time per tab; a handoff makes waiting actions fail with
//!   `interrupted` and invalidates snapshot refs.
//! - Limits per agent session (8 tabs) and per server (32); agent tabs nobody
//!   watches close after 30 idle minutes; a profile's browser closes 5
//!   minutes after its last tab.
//! - Console (200), network (200) and action timeline (50) buffers per tab.
//!
//! Limits: no popups adopted as tabs (a page's `window.open` target stays
//! invisible); no file chooser interception (`upload` sets an
//! `<input type=file>` by locator only); no downloads (they are refused); no
//! recording; no clipboard copy to viewers; no desktop-rendered tabs
//! (`DesktopBrowserChannel`); a crashed page is reported, not reopened; title
//! changes without a navigation are seen at the next status read; profile
//! reports are the last client's, not a union per connected client.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the DevTools protocol and the agent tools' inputs and results are JSON")]

use super::cdp::{Cdp, CdpError, Event, Session};
use super::chrome::{self, LaunchError};
use super::host::{Ack, AttachError, AttachInput, BrowserProfile, HtmlShot, OpError, Output, ProfilesReport, Request};
use super::page::{self, LocatorError, Target};
use crate::rpc::preview::{Change, ColorScheme, NavStatus, ReportStatusInput, ResizeInput, Runtime, Snapshot, Viewport, manager, normalize_preview_url};
use crate::state::SharedState;
use crate::util::locked;
use base64::Engine;
use futures::FutureExt;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, mpsc, watch};
use tokio_util::sync::CancellationToken;

const RENDER_SCALE: f64 = 2.0;
const UNATTACHED_FILL: (u32, u32) = (1280, 800);
const AGENT_TAB_LIMIT: usize = 8;
const SERVER_TAB_LIMIT: usize = 32;
const AGENT_TAB_IDLE: Duration = Duration::from_secs(30 * 60);
const BROWSER_IDLE: Duration = Duration::from_secs(5 * 60);
const SWEEP_EVERY: Duration = Duration::from_secs(60);
const DIAGNOSTIC_LIMIT: usize = 200;
const ACTION_LIMIT: usize = 50;
const NAVIGATION_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_TIMEOUT_MS: u64 = 15_000;
const MAX_EVALUATION_BYTES: usize = 64_000;
const MAX_VISIBLE_TEXT: usize = 20_000;
const MAX_ELEMENTS: usize = 300;
/// The widest screenshot an agent is sent; a saved one keeps the full size.
const MAX_SCREENSHOT_WIDTH: u32 = 1280;
const SCREENCAST_ACK_PACE: Duration = Duration::from_millis(100);
const VIEWER_OUTPUT: usize = 64;
const WAIT_POLL: Duration = Duration::from_millis(100);
const TITLE_LIMIT: usize = 512;
/// How long `preview_open` with `open: true` waits for a client to show the tab.
const REVEAL_VIEWER_WAIT: Duration = Duration::from_secs(1);
const HTML_PAGE_URL: &str = "http://t3-page.localhost/page.html";
const HTML_FAVICON_URL: &str = "http://t3-page.localhost/favicon.ico";
const HTML_VIEWPORT_HEIGHT: u64 = 800;
const HTML_MAX_CAPTURE_HEIGHT: u64 = 4_000;
const HTML_TIMEOUT: Duration = Duration::from_secs(20);
const HTML_MAX_CONSOLE: usize = 20;
const HTML_MAX_CONSOLE_CHARS: usize = 500;

const EDITABLE_AT_POINT: &str = r#"((x, y) => {
  let element = document.elementFromPoint(x, y);
  while (element && element.shadowRoot) {
    const inner = element.shadowRoot.elementFromPoint(x, y);
    if (!inner || inner === element) break;
    element = inner;
  }
  if (element && element.tagName === "LABEL" && element.control) element = element.control;
  if (!element) return false;
  if (element.tagName === "IFRAME" || element.tagName === "FRAME") return true;
  if (element.isContentEditable) return true;
  if (element.tagName === "TEXTAREA") return !element.disabled && !element.readOnly;
  if (element.tagName !== "INPUT") return false;
  const nonText = ["button", "checkbox", "color", "file", "hidden", "image", "radio", "range", "reset", "submit"];
  return !nonText.includes(element.type) && !element.disabled && !element.readOnly;
})"#;

/// Operations agents may run on a tab they cannot act on.
const READ_OPERATIONS: [&str; 3] = ["status", "snapshot", "waitFor"];

/// `PREVIEW_VIEWPORT_PRESETS`: id, width, height (CSS pixels, portrait as Chromium lists them).
const PRESETS: [(&str, u32, u32); 17] = [
    ("iphone-se", 375, 667),
    ("iphone-xr", 414, 896),
    ("iphone-12-pro", 390, 844),
    ("iphone-14-pro-max", 430, 932),
    ("pixel-7", 412, 915),
    ("samsung-galaxy-s8-plus", 360, 740),
    ("samsung-galaxy-s20-ultra", 412, 915),
    ("ipad-mini", 768, 1024),
    ("ipad-air", 820, 1180),
    ("ipad-pro", 1024, 1366),
    ("surface-pro-7", 912, 1368),
    ("surface-duo", 540, 720),
    ("galaxy-z-fold-5", 344, 882),
    ("asus-zenbook-fold", 853, 1280),
    ("samsung-galaxy-a51-71", 412, 914),
    ("nest-hub", 1024, 600),
    ("nest-hub-max", 1280, 800),
];

type Key = (String, String);

/// A browser process for one profile.
struct Chrome {
    profile: String,
    cdp: Arc<Cdp>,
    /// Since when it has had no tab.
    idle_since: Mutex<Option<Instant>>,
    _process: chrome::Process,
}

#[derive(Clone)]
struct LoadError {
    url: String,
    code: i64,
    description: String,
}

#[derive(Clone)]
struct Dialog {
    kind: String,
    message: String,
    default_value: String,
}

struct TabState {
    url: String,
    title: String,
    loading: bool,
    load_error: Option<LoadError>,
    main_frame: String,
    main_request: Option<String>,
    /// URL of the document request in flight.
    pending_url: Option<String>,
    loads: u64,
    dom_loads: u64,
    console: VecDeque<Value>,
    network: VecDeque<(String, Value)>,
    actions: VecDeque<Value>,
    dialog: Option<Dialog>,
    setting: Viewport,
    color_scheme: String,
    zoom: f64,
    /// The page's CSS size.
    layout: (u32, u32),
    agent: Option<String>,
    controller: Option<String>,
    generation: u64,
    /// CSS paths of the latest snapshot's elements, by ref number.
    refs: Vec<String>,
    used_at: Instant,
    last_report: Option<String>,
}

struct Tab {
    thread_id: String,
    tab_id: String,
    target_id: String,
    context_id: Option<String>,
    session: Session,
    chrome: Arc<Chrome>,
    profile_id: Option<String>,
    st: Mutex<TabState>,
    changed: watch::Sender<u64>,
    dialog_opened: Notify,
    viewer_attached: Notify,
    /// Agent actions and controller input, one at a time.
    queue: tokio::sync::Mutex<()>,
    viewers: Mutex<Vec<Arc<ViewerLink>>>,
    closed: CancellationToken,
    created: Instant,
}

impl Tab {
    fn key(&self) -> Key {
        (self.thread_id.clone(), self.tab_id.clone())
    }
    fn st(&self) -> std::sync::MutexGuard<'_, TabState> {
        locked(&self.st)
    }
    fn bump(&self) {
        self.changed.send_modify(|n| *n += 1);
    }
    fn visible(&self) -> bool {
        !locked(&self.viewers).is_empty()
    }
    async fn call(&self, method: &str, params: Value) -> Result<Value, OpError> {
        self.session.call(method, params).await.map_err(cdp_failed)
    }
}

fn cdp_failed(e: CdpError) -> OpError {
    match e {
        CdpError::Timeout(m) => OpError::timeout(format!("The browser did not answer {m} in time.")),
        CdpError::Closed => OpError::control("closed"),
        other => OpError::execution(other.to_string()),
    }
}

struct ViewerLink {
    id: String,
    can_operate: bool,
    out: mpsc::Sender<Output>,
    /// `(width, height, order)` the viewer last asked for.
    requested: Mutex<Option<(u32, u32, u64)>>,
    pressed_keys: Mutex<HashMap<String, (String, String)>>,
    pressed_buttons: Mutex<HashMap<String, (f64, f64)>>,
    session: Session,
}

impl ViewerLink {
    fn text(&self, value: Value) {
        let _ = self.out.try_send(Output::Text(value.to_string()));
    }
}

/// A viewer of one tab (t3code `ServerBrowserViewer`).
pub struct Viewer {
    pub output: mpsc::Receiver<Output>,
    link: Arc<ViewerLink>,
    tab: Arc<Tab>,
    browser: Arc<ServerBrowser>,
    frames: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
struct Reported {
    profiles: Vec<BrowserProfile>,
    default_profile_id: String,
}

pub struct ServerBrowser {
    state: SharedState,
    chromes: tokio::sync::Mutex<HashMap<String, Arc<Chrome>>>,
    tabs: Mutex<HashMap<Key, Arc<Tab>>>,
    /// Tabs being set up; one at a time.
    opening: tokio::sync::Mutex<()>,
    profiles: Mutex<Option<Reported>>,
    sequence: AtomicU64,
}

static SERVICE: OnceLock<Arc<ServerBrowser>> = OnceLock::new();

/// The browser service, started on first use: it mirrors the manager's events from then on.
pub fn service(state: &SharedState) -> Arc<ServerBrowser> {
    SERVICE
        .get_or_init(|| {
            let saved = std::fs::read_to_string(profiles_path(state)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| {
                let profiles: Vec<BrowserProfile> = serde_json::from_value(v["profiles"].clone()).ok()?;
                Some(resolve_profiles(profiles, v["defaultProfileId"].as_str()?))
            });
            let browser = Arc::new(ServerBrowser {
                state: state.clone(),
                chromes: Default::default(),
                tabs: Default::default(),
                opening: Default::default(),
                profiles: Mutex::new(saved),
                sequence: AtomicU64::new(0),
            });
            // Subscribed before any tab opens, so its `opened` event is seen.
            let mut events = manager().subscribe();
            let mirror = browser.clone();
            tokio::spawn(async move {
                loop {
                    match events.recv().await {
                        Ok(event) => mirror.clone().mirror(event).await,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
            let sweeper = browser.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        () = sweeper.state.shutdown.cancelled() => break,
                        () = tokio::time::sleep(SWEEP_EVERY) => sweeper.sweep().await,
                    }
                }
                sweeper.shutdown().await;
            });
            browser
        })
        .clone()
}

fn profiles_path(state: &SharedState) -> std::path::PathBuf {
    state.paths.userdata.join("server-browser").join("reported-profiles.json")
}

const BUILT_IN_PROFILES: [(&str, &str, &str); 2] = [("default", "Default", "persistent"), ("incognito", "Incognito", "incognito")];

/// `resolveBrowserProfiles`: the built-in ones first, then the reported ones (as persistent).
fn resolve_profiles(reported: Vec<BrowserProfile>, default_id: &str) -> Reported {
    let mut profiles: Vec<BrowserProfile> = BUILT_IN_PROFILES.iter().map(|(id, name, kind)| BrowserProfile { id: (*id).into(), name: (*name).into(), kind: (*kind).into() }).collect();
    for p in reported {
        if profiles.iter().any(|q| q.id == p.id) {
            continue;
        }
        profiles.push(BrowserProfile { kind: "persistent".into(), ..p });
    }
    let default_profile_id = if profiles.iter().any(|p| p.id == default_id) { default_id.to_owned() } else { "default".into() };
    Reported { profiles, default_profile_id }
}

fn number(v: &serde_json::Number) -> f64 {
    v.as_f64().unwrap_or(0.0)
}

/// A setting's fixed CSS size, if it has one.
fn fixed_size(setting: &Viewport) -> Option<(u32, u32)> {
    match setting {
        Viewport::Fill => None,
        Viewport::Freeform { width, height } | Viewport::Preset { width, height, .. } => Some((number(width).round() as u32, number(height).round() as u32)),
    }
}

fn color_value(scheme: &Option<ColorScheme>) -> String {
    match scheme {
        Some(ColorScheme::Light) => "light".into(),
        Some(ColorScheme::Dark) => "dark".into(),
        _ => "system".into(),
    }
}

fn now() -> String {
    domovoi_contracts::iso_now()
}

fn push_bounded<T>(list: &mut VecDeque<T>, item: T, limit: usize) {
    list.push_back(item);
    while list.len() > limit {
        list.pop_front();
    }
}

/// A remote object as console text.
fn remote_text(v: &Value) -> String {
    match &v["value"] {
        Value::String(s) => s.clone(),
        Value::Null => v["description"].as_str().map(str::to_owned).unwrap_or_else(|| v["type"].as_str().unwrap_or_default().to_owned()),
        other => other.to_string(),
    }
}

impl ServerBrowser {
    fn epoch(&self) -> &str {
        &self.state.server_epoch
    }

    // ---- browsers and tabs ----

    async fn chrome_for(self: &Arc<Self>, profile: &str) -> Result<Arc<Chrome>, LaunchError> {
        let mut chromes = self.chromes.lock().await;
        if let Some(c) = chromes.get(profile).filter(|c| !c.cdp.closed.is_cancelled()) {
            return Ok(c.clone());
        }
        let exe = chrome::executable(&self.state.paths.home)?;
        let dir = chrome::profile_dir(&self.state.paths.userdata, profile);
        let process = chrome::launch(&exe, &dir, false).await?;
        let cdp = Cdp::connect(process.port, &process.path).await.map_err(|e| LaunchError::Failed(e.to_string()))?;
        let mut browser_events = cdp.subscribe("");
        let _ = cdp.call(None, "Target.setDiscoverTargets", json!({ "discover": true })).await;
        let _ = cdp.call(None, "Browser.setDownloadBehavior", json!({ "behavior": "deny" })).await;
        let c = Arc::new(Chrome { profile: profile.to_owned(), cdp: cdp.clone(), idle_since: Mutex::new(Some(Instant::now())), _process: process });
        chromes.insert(profile.to_owned(), c.clone());
        let me = self.clone();
        let watched = c.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = watched.cdp.closed.cancelled() => break,
                    event = browser_events.recv() => match event {
                        // A page that closed itself closes its tab.
                        Some(Event { method, params }) if method == "Target.targetDestroyed" => {
                            let target = params["targetId"].as_str().unwrap_or_default().to_owned();
                            let tab = locked(&me.tabs).values().find(|t| t.target_id == target).cloned();
                            if let Some(tab) = tab {
                                me.drop_tab(&tab, true).await;
                            }
                        }
                        Some(_) => {}
                        None => break,
                    },
                }
            }
            // The browser went away: its tabs close with it.
            let gone: Vec<Arc<Tab>> = locked(&me.tabs).values().filter(|t| Arc::ptr_eq(&t.chrome, &watched)).cloned().collect();
            for tab in gone {
                me.drop_tab(&tab, true).await;
            }
            let mut chromes = me.chromes.lock().await;
            if chromes.get(&watched.profile).is_some_and(|c| Arc::ptr_eq(c, &watched)) {
                chromes.remove(&watched.profile);
            }
        });
        Ok(c)
    }

    fn reported(&self) -> Reported {
        locked(&self.profiles).clone().unwrap_or_else(|| resolve_profiles(vec![], "default"))
    }

    pub fn report_profiles(&self, report: ProfilesReport) {
        let resolved = resolve_profiles(report.profiles, &report.default_profile_id);
        let saved = json!({
            "profiles": resolved.profiles.iter().filter(|p| !BUILT_IN_PROFILES.iter().any(|(id, ..)| *id == p.id)).collect::<Vec<_>>(),
            "defaultProfileId": resolved.default_profile_id,
        });
        *locked(&self.profiles) = Some(resolved);
        let path = profiles_path(&self.state);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, saved.to_string()).and_then(|()| std::fs::rename(&tmp, &path)).is_err() {
            tracing::warn!("could not save the reported browser profiles");
        }
    }

    pub async fn clear_profile(self: &Arc<Self>, profile_id: &str) -> Result<(), String> {
        if profile_id == "incognito" {
            return Ok(());
        }
        let chrome = self.chromes.lock().await.remove(profile_id);
        if let Some(chrome) = chrome {
            let tabs: Vec<Arc<Tab>> = locked(&self.tabs).values().filter(|t| Arc::ptr_eq(&t.chrome, &chrome)).cloned().collect();
            for tab in tabs {
                self.drop_tab(&tab, true).await;
            }
            let _ = chrome.cdp.call(None, "Browser.close", json!({})).await;
            drop(chrome);
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        match std::fs::remove_dir_all(chrome::profile_dir(&self.state.paths.userdata, profile_id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        }
    }

    /// The tab, starting it when the manager lists it as a server tab.
    async fn find_tab(self: &Arc<Self>, thread_id: &str, tab_id: &str) -> Result<Option<Arc<Tab>>, LaunchError> {
        if let Some(tab) = locked(&self.tabs).get(&(thread_id.to_owned(), tab_id.to_owned())).cloned() {
            return Ok(Some(tab));
        }
        match manager().snapshot(thread_id, tab_id).filter(|s| s.runtime == Some(Runtime::Server)) {
            Some(snapshot) => self.ensure_tab(&snapshot).await.map(Some),
            None => Ok(None),
        }
    }

    async fn ensure_tab(self: &Arc<Self>, snapshot: &Snapshot) -> Result<Arc<Tab>, LaunchError> {
        let key = (snapshot.thread_id.clone(), snapshot.tab_id.clone());
        let tab = {
            let _opening = self.opening.lock().await;
            if let Some(tab) = locked(&self.tabs).get(&key).cloned() {
                return Ok(tab);
            }
            if manager().snapshot(&key.0, &key.1).is_none() {
                return Err(LaunchError::Failed("the tab closed while it started".into()));
            }
            let tab = self.create_tab(snapshot).await?;
            locked(&self.tabs).insert(key.clone(), tab.clone());
            *locked(&tab.chrome.idle_since) = None;
            tab
        };
        let _ = manager().mark_headless(self.epoch(), &key.0, &key.1);
        // Outside the lock: a slow first page holds up no other tab.
        if let NavStatus::Loading { url, .. } = &snapshot.nav_status {
            let _ = self.start_navigation(&tab, url).await;
        }
        Ok(tab)
    }

    async fn create_tab(self: &Arc<Self>, snapshot: &Snapshot) -> Result<Arc<Tab>, LaunchError> {
        let failed = |e: CdpError| LaunchError::Failed(e.to_string());
        let profile = snapshot.profile_id.clone().unwrap_or_else(|| self.reported().default_profile_id);
        let incognito = profile == "incognito";
        let chrome = self.chrome_for(if incognito { "default" } else { &profile }).await?;
        let context_id = if incognito {
            let made = chrome.cdp.call(None, "Target.createBrowserContext", json!({ "disposeOnDetach": false })).await.map_err(failed)?;
            made["browserContextId"].as_str().map(str::to_owned)
        } else {
            None
        };
        let mut params = json!({ "url": "about:blank" });
        if let Some(ctx) = &context_id {
            params["browserContextId"] = json!(ctx);
        }
        let target = chrome.cdp.call(None, "Target.createTarget", params).await.map_err(failed)?;
        let target_id = target["targetId"].as_str().unwrap_or_default().to_owned();
        let (session, events) = chrome.cdp.attach(&target_id).await.map_err(failed)?;
        for method in ["Page.enable", "Runtime.enable", "Network.enable", "Log.enable"] {
            session.call(method, json!({})).await.map_err(failed)?;
        }
        let _ = session.call("Emulation.setFocusEmulationEnabled", json!({ "enabled": true })).await;
        let tree = session.call("Page.getFrameTree", json!({})).await.map_err(failed)?;
        let zoom = snapshot.zoom_factor.as_ref().map(number).filter(|z| *z > 0.0).unwrap_or(1.0);
        let st = TabState {
            url: "about:blank".into(),
            title: String::new(),
            loading: false,
            load_error: None,
            main_frame: tree["frameTree"]["frame"]["id"].as_str().unwrap_or_default().to_owned(),
            main_request: None,
            pending_url: None,
            loads: 0,
            dom_loads: 0,
            console: VecDeque::new(),
            network: VecDeque::new(),
            actions: VecDeque::new(),
            dialog: None,
            setting: snapshot.viewport.clone(),
            color_scheme: "system".into(),
            zoom,
            layout: UNATTACHED_FILL,
            agent: snapshot.automation_owner.clone(),
            controller: None,
            generation: 0,
            refs: vec![],
            used_at: Instant::now(),
            last_report: None,
        };
        let tab = Arc::new(Tab {
            thread_id: snapshot.thread_id.clone(),
            tab_id: snapshot.tab_id.clone(),
            target_id,
            context_id,
            session,
            chrome,
            profile_id: snapshot.profile_id.clone(),
            st: Mutex::new(st),
            changed: watch::channel(0).0,
            dialog_opened: Notify::new(),
            viewer_attached: Notify::new(),
            queue: tokio::sync::Mutex::new(()),
            viewers: Mutex::new(vec![]),
            closed: CancellationToken::new(),
            created: Instant::now(),
        });
        self.apply_size(&tab).await;
        self.apply_color(&tab, &color_value(&snapshot.color_scheme)).await;
        tokio::spawn(self.clone().run_events(tab.clone(), events));
        Ok(tab)
    }

    /// Closes the tab's page; `close_session` also closes it in the manager.
    async fn drop_tab(&self, tab: &Arc<Tab>, close_session: bool) {
        let removed = {
            let mut tabs = locked(&self.tabs);
            match tabs.get(&tab.key()) {
                Some(t) if Arc::ptr_eq(t, tab) => tabs.remove(&tab.key()).is_some(),
                _ => false,
            }
        };
        tab.closed.cancel();
        for viewer in locked(&tab.viewers).drain(..) {
            let _ = viewer.out.try_send(Output::Gone);
        }
        if !removed {
            return;
        }
        let _ = tab.chrome.cdp.call(None, "Target.closeTarget", json!({ "targetId": tab.target_id })).await;
        if let Some(ctx) = &tab.context_id {
            let _ = tab.chrome.cdp.call(None, "Target.disposeBrowserContext", json!({ "browserContextId": ctx })).await;
        }
        if !locked(&self.tabs).values().any(|t| Arc::ptr_eq(&t.chrome, &tab.chrome)) {
            *locked(&tab.chrome.idle_since) = Some(Instant::now());
        }
        if close_session {
            manager().close(self.epoch(), crate::rpc::preview::CloseInput { thread_id: tab.thread_id.clone(), tab_id: Some(tab.tab_id.clone()) });
        }
    }

    /// Closes idle agent tabs and idle browsers.
    async fn sweep(self: &Arc<Self>) {
        let idle: Vec<Arc<Tab>> = locked(&self.tabs).values().filter(|t| t.st().agent.is_some() && !t.visible() && t.st().used_at.elapsed() > AGENT_TAB_IDLE).cloned().collect();
        for tab in idle {
            self.drop_tab(&tab, true).await;
        }
        let idle: Vec<Arc<Chrome>> = {
            let mut chromes = self.chromes.lock().await;
            let idle = chromes.values().filter(|c| locked(&c.idle_since).is_some_and(|since| since.elapsed() >= BROWSER_IDLE)).cloned().collect();
            chromes.retain(|_, c| locked(&c.idle_since).is_none_or(|since| since.elapsed() < BROWSER_IDLE));
            idle
        };
        // Its watcher holds it until the browser is gone.
        for chrome in idle {
            let _ = chrome.cdp.call(None, "Browser.close", json!({})).await;
        }
    }

    async fn shutdown(&self) {
        let tabs: Vec<Arc<Tab>> = locked(&self.tabs).drain().map(|(_, t)| t).collect();
        for tab in tabs {
            tab.closed.cancel();
        }
        let chromes: Vec<Arc<Chrome>> = self.chromes.lock().await.drain().map(|(_, c)| c).collect();
        for chrome in chromes {
            let _ = chrome.cdp.call(None, "Browser.close", json!({})).await;
        }
    }

    /// Follows the manager (t3code `mirrorManagerEvent`).
    async fn mirror(self: Arc<Self>, event: crate::rpc::preview::Event) {
        match event.change {
            Change::Opened { snapshot } if snapshot.runtime == Some(Runtime::Server) && snapshot.automation_owner.is_none() => {
                tokio::spawn(async move {
                    if let Err(e) = self.ensure_tab(&snapshot).await {
                        tracing::warn!("server preview tab failed to start: {e}");
                    }
                });
            }
            Change::Closed => {
                let tab = locked(&self.tabs).get(&(event.thread_id.clone(), event.tab_id.clone())).cloned();
                if let Some(tab) = tab {
                    self.drop_tab(&tab, false).await;
                }
            }
            Change::Resized { snapshot, request } => {
                let Some(tab) = locked(&self.tabs).get(&(event.thread_id.clone(), event.tab_id.clone())).cloned() else { return };
                // Applied behind the tab's running action, without holding up other tabs' events.
                tokio::spawn(async move { self.apply_resized(&tab, &snapshot, request).await });
            }
            _ => {}
        }
    }

    /// A resize or adjustment the manager announced: viewport, zoom, colour
    /// scheme, then any clearing and hard reload.
    async fn apply_resized(&self, tab: &Tab, snapshot: &Snapshot, request: Option<crate::rpc::preview::ReloadRequest>) {
        let _queue = tab.queue.lock().await;
        let zoom = snapshot.zoom_factor.as_ref().map(number).filter(|z| *z > 0.0).unwrap_or(1.0);
        let changed = {
            let mut st = tab.st();
            let changed = st.setting != snapshot.viewport || (st.zoom - zoom).abs() > f64::EPSILON;
            st.setting = snapshot.viewport.clone();
            st.zoom = zoom;
            changed
        };
        if changed {
            self.apply_size(tab).await;
        }
        self.apply_color(tab, &color_value(&snapshot.color_scheme)).await;
        let Some(request) = request else { return };
        match request.clear {
            Some(crate::rpc::preview::Clear::Cookies) => {
                let _ = tab.session.call("Network.clearBrowserCookies", json!({})).await;
            }
            Some(crate::rpc::preview::Clear::Cache) => {
                let _ = tab.session.call("Network.clearBrowserCache", json!({})).await;
            }
            _ => {}
        }
        if request.hard_reload == Some(true) {
            let _ = tab.session.call("Page.reload", json!({ "ignoreCache": true })).await;
        }
    }

    // ---- page state ----

    async fn run_events(self: Arc<Self>, tab: Arc<Tab>, mut events: mpsc::Receiver<Event>) {
        loop {
            let event = tokio::select! {
                () = tab.closed.cancelled() => break,
                event = events.recv() => match event { Some(e) => e, None => break },
            };
            self.on_event(&tab, event).await;
            tab.bump();
        }
        tab.session.cdp.unsubscribe(&tab.session.id);
    }

    async fn on_event(&self, tab: &Arc<Tab>, Event { method, params }: Event) {
        let p = &params;
        match method.as_str() {
            "Network.requestWillBeSent" => {
                let id = p["requestId"].as_str().unwrap_or_default().to_owned();
                let url = p["request"]["url"].as_str().unwrap_or_default().to_owned();
                let entry = json!({ "url": page::without_data_payload(&url), "method": p["request"]["method"], "status": null, "failed": false, "timestamp": now() });
                let main_document = {
                    let mut st = tab.st();
                    push_bounded(&mut st.network, (id.clone(), entry), DIAGNOSTIC_LIMIT);
                    let main = p["type"] == "Document" && p["frameId"].as_str() == Some(st.main_frame.as_str());
                    if main {
                        st.main_request = Some(id);
                        st.pending_url = Some(url.clone());
                        st.loading = true;
                        st.load_error = None;
                        st.refs.clear();
                    }
                    main
                };
                if main_document {
                    self.report(tab, NavStatus::Loading { url: clip_url(&url), title: String::new() }).await;
                }
            }
            "Network.responseReceived" => {
                let id = p["requestId"].as_str().unwrap_or_default();
                let mut st = tab.st();
                if let Some((_, entry)) = st.network.iter_mut().rev().find(|(r, _)| r == id) {
                    entry["status"] = p["response"]["status"].clone();
                }
            }
            "Network.loadingFailed" => {
                let id = p["requestId"].as_str().unwrap_or_default();
                let error_text = p["errorText"].as_str().unwrap_or_default().to_owned();
                let failed_main = {
                    let mut st = tab.st();
                    if let Some((_, entry)) = st.network.iter_mut().rev().find(|(r, _)| r == id) {
                        entry["failed"] = json!(true);
                        entry["errorText"] = json!(error_text);
                    }
                    if st.main_request.as_deref() == Some(id) && p["canceled"] != true {
                        let (description, code) = page::net_error(&error_text);
                        let url = clip_url(&st.pending_url.clone().unwrap_or_else(|| st.url.clone()));
                        st.load_error = Some(LoadError { url: url.clone(), code, description: description.clone() });
                        st.loading = false;
                        Some((url, code, description))
                    } else {
                        if st.main_request.as_deref() == Some(id) {
                            st.loading = false;
                        }
                        None
                    }
                };
                if let Some((url, code, description)) = failed_main {
                    self.report(tab, NavStatus::LoadFailed { url, title: String::new(), code: code.into(), description, download: None }).await;
                }
            }
            "Page.frameNavigated" => {
                let frame = &p["frame"];
                let restored = {
                    let mut st = tab.st();
                    let main = frame["parentId"].is_null() && frame["id"].as_str() == Some(st.main_frame.as_str());
                    if main {
                        st.url = frame["unreachableUrl"].as_str().or(frame["url"].as_str()).unwrap_or_default().to_owned();
                        st.refs.clear();
                    }
                    main && p["type"] == "BackForwardCacheRestore"
                };
                // A cached page comes back without loading.
                if restored {
                    tab.st().load_error = None;
                    self.report_loaded(tab).await;
                }
            }
            "Page.navigatedWithinDocument" => {
                let same = p["frameId"].as_str() == Some(tab.st().main_frame.as_str());
                if same {
                    tab.st().url = p["url"].as_str().unwrap_or_default().to_owned();
                    self.report_loaded(tab).await;
                }
            }
            "Page.domContentEventFired" => {
                tab.st().dom_loads += 1;
            }
            "Page.loadEventFired" => {
                {
                    let mut st = tab.st();
                    st.loads += 1;
                    st.loading = false;
                }
                self.report_loaded(tab).await;
            }
            "Page.frameStoppedLoading" => {
                let mut st = tab.st();
                if p["frameId"].as_str() == Some(st.main_frame.as_str()) {
                    st.loading = false;
                }
            }
            "Page.javascriptDialogOpening" => {
                tab.st().dialog = Some(Dialog {
                    kind: p["type"].as_str().unwrap_or("alert").to_owned(),
                    message: p["message"].as_str().unwrap_or_default().chars().take(4000).collect(),
                    default_value: p["defaultPrompt"].as_str().unwrap_or_default().chars().take(4000).collect(),
                });
                tab.dialog_opened.notify_waiters();
                self.broadcast_control(tab);
            }
            "Page.javascriptDialogClosed" => {
                {
                    let mut st = tab.st();
                    st.dialog = None;
                    st.refs.clear();
                }
                self.broadcast_control(tab);
            }
            "Runtime.consoleAPICalled" => {
                let level = match p["type"].as_str().unwrap_or("log") {
                    "warning" => "warning",
                    "error" | "assert" => "error",
                    "info" => "info",
                    "debug" => "debug",
                    _ => "log",
                };
                let text = p["args"].as_array().map(|a| a.iter().map(remote_text).collect::<Vec<_>>().join(" ")).unwrap_or_default();
                push_bounded(&mut tab.st().console, json!({ "level": level, "text": text, "timestamp": now() }), DIAGNOSTIC_LIMIT);
            }
            "Runtime.exceptionThrown" => {
                let d = &p["exceptionDetails"];
                let text = d["exception"]["description"].as_str().or(d["text"].as_str()).unwrap_or_default().to_owned();
                push_bounded(&mut tab.st().console, json!({ "level": "error", "text": text, "timestamp": now(), "source": "pageerror" }), DIAGNOSTIC_LIMIT);
            }
            "Log.entryAdded" => {
                let e = &p["entry"];
                let level = match e["level"].as_str().unwrap_or("info") {
                    "verbose" => "debug",
                    other => other,
                }
                .to_owned();
                let text = page::without_data_payload(e["text"].as_str().unwrap_or_default());
                push_bounded(&mut tab.st().console, json!({ "level": level, "text": text, "timestamp": now(), "source": e["source"] }), DIAGNOSTIC_LIMIT);
            }
            "Inspector.targetCrashed" => {
                let url = {
                    let mut st = tab.st();
                    let url = clip_url(&st.url);
                    st.load_error = Some(LoadError { url: url.clone(), code: -2, description: "Page crashed".into() });
                    st.loading = false;
                    url
                };
                self.report(tab, NavStatus::LoadFailed { url, title: String::new(), code: (-2).into(), description: "Page crashed".into(), download: None }).await;
            }
            _ => {}
        }
    }

    /// The page's history: whether it can go back and forward, and the current entry's title.
    async fn history(&self, tab: &Tab) -> (bool, bool, Option<String>) {
        match tab.session.call("Page.getNavigationHistory", json!({})).await {
            Ok(h) => {
                let index = h["currentIndex"].as_i64().unwrap_or(0);
                let count = h["entries"].as_array().map_or(0, Vec::len) as i64;
                let title = h["entries"][index as usize]["title"].as_str().map(|t| t.chars().take(TITLE_LIMIT).collect());
                (index > 0, index < count - 1, title)
            }
            Err(_) => (false, false, None),
        }
    }

    /// `reportLoaded`: a loaded page's URL and title, unless it failed.
    async fn report_loaded(&self, tab: &Arc<Tab>) {
        let url = {
            let st = tab.st();
            if st.load_error.is_some() || st.url == "about:blank" || st.url.starts_with("chrome-error://") {
                return;
            }
            st.url.clone()
        };
        let (_, _, title) = self.history(tab).await;
        let title = title.unwrap_or_default();
        tab.st().title = title.clone();
        self.report(tab, NavStatus::Success { url: clip_url(&url), title }).await;
    }

    /// Reports the tab's status to the manager (t3code `report`), once per change.
    async fn report(&self, tab: &Tab, nav_status: NavStatus) {
        let (can_go_back, can_go_forward, _) = self.history(tab).await;
        let input = ReportStatusInput { thread_id: tab.thread_id.clone(), tab_id: tab.tab_id.clone(), nav_status, can_go_back, can_go_forward };
        let key = json!([input.nav_status, can_go_back, can_go_forward]).to_string();
        {
            let mut st = tab.st();
            if st.last_report.as_deref() == Some(key.as_str()) {
                return;
            }
            st.last_report = Some(key);
        }
        let _ = manager().report_status(self.epoch(), input, true);
    }

    async fn start_navigation(&self, tab: &Arc<Tab>, url: &str) -> Result<(), OpError> {
        let answer = tab.call("Page.navigate", json!({ "url": url })).await?;
        if let Some(error) = answer["errorText"].as_str().filter(|e| !e.is_empty()) {
            let (description, code) = page::net_error(error);
            let shown = clip_url(url);
            {
                let mut st = tab.st();
                st.load_error = Some(LoadError { url: shown.clone(), code, description: description.clone() });
                st.loading = false;
            }
            tab.bump();
            self.report(tab, NavStatus::LoadFailed { url: shown, title: String::new(), code: code.into(), description: description.clone(), download: None }).await;
            return Err(OpError::execution(format!("Navigation to {} failed: {description}", diagnostic_url(url))));
        }
        Ok(())
    }

    /// Waits until `done` holds of the tab's state, or `limit` passes (`false`).
    async fn wait_state(&self, tab: &Tab, limit: Duration, done: impl Fn(&TabState) -> bool) -> bool {
        let mut changes = tab.changed.subscribe();
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            if done(&tab.st()) {
                return true;
            }
            tokio::select! {
                changed = changes.changed() => if changed.is_err() { return false },
                () = tokio::time::sleep_until(deadline) => return done(&tab.st()),
                () = tab.closed.cancelled() => return false,
            }
        }
    }

    /// Navigates and waits for `readiness` (a page whose load stalls is reported loading, not failed).
    async fn navigate(&self, tab: &Arc<Tab>, url: &str, readiness: &str, timeout: Duration) -> Result<(), OpError> {
        let (loads, dom) = {
            let st = tab.st();
            (st.loads, st.dom_loads)
        };
        self.start_navigation(tab, url).await?;
        if readiness != "none" {
            let dom_only = readiness == "domContentLoaded";
            self.wait_state(tab, timeout, |st| st.load_error.is_some() || if dom_only { st.dom_loads > dom } else { st.loads > loads }).await;
        }
        self.assert_loaded(tab)
    }

    /// `assertNavigationSucceeded`.
    fn assert_loaded(&self, tab: &Tab) -> Result<(), OpError> {
        match tab.st().load_error.clone() {
            Some(e) => Err(OpError::execution(format!("Navigation to {} failed: {}", diagnostic_url(&e.url), e.description))),
            None => Ok(()),
        }
    }

    // ---- rendering ----

    /// The page's CSS size: the setting's, else the newest viewer's, else 1280x800.
    fn css_size(&self, tab: &Tab) -> (u32, u32) {
        if let Some(size) = fixed_size(&tab.st().setting) {
            return size;
        }
        locked(&tab.viewers).iter().filter_map(|v| *locked(&v.requested)).max_by_key(|r| r.2).map(|(w, h, _)| (w, h)).unwrap_or(UNATTACHED_FILL)
    }

    async fn apply_size(&self, tab: &Tab) {
        let (w, h) = self.css_size(tab);
        let zoom = tab.st().zoom;
        let layout = (((w as f64) / zoom).round().max(1.0) as u32, ((h as f64) / zoom).round().max(1.0) as u32);
        let _ = tab
            .session
            .call("Emulation.setDeviceMetricsOverride", json!({ "width": layout.0, "height": layout.1, "deviceScaleFactor": RENDER_SCALE * zoom, "mobile": false }))
            .await;
        tab.st().layout = layout;
        for v in locked(&tab.viewers).iter() {
            v.text(json!({ "type": "viewport", "width": layout.0, "height": layout.1 }));
        }
    }

    async fn apply_color(&self, tab: &Tab, scheme: &str) {
        if tab.st().color_scheme == scheme {
            return;
        }
        tab.st().color_scheme = scheme.to_owned();
        let value = if scheme == "system" { "" } else { scheme };
        let _ = tab.session.call("Emulation.setEmulatedMedia", json!({ "features": [{ "name": "prefers-color-scheme", "value": value }] })).await;
    }

    fn dialog_status(st: &TabState) -> Value {
        match &st.dialog {
            Some(d) => json!({ "type": d.kind, "message": d.message, "defaultValue": d.default_value }),
            None => Value::Null,
        }
    }

    fn broadcast_control(&self, tab: &Tab) {
        let (controller, agent, generation, dialog) = {
            let st = tab.st();
            (st.controller.clone(), st.agent.is_some(), st.generation, Self::dialog_status(&st))
        };
        for v in locked(&tab.viewers).iter() {
            let who = match &controller {
                Some(c) if *c == v.id => "you",
                Some(_) => "another-viewer",
                None if agent => "agent",
                None => "unclaimed",
            };
            v.text(json!({ "type": "control", "canOperate": v.can_operate, "controller": who, "generation": generation, "dialog": dialog }));
        }
    }

    /// A change of controller: waiting actions find a new generation and stop.
    fn set_controller(&self, tab: &Tab, controller: Option<String>) {
        {
            let mut st = tab.st();
            st.controller = controller;
            st.generation += 1;
            st.refs.clear();
        }
        self.broadcast_control(tab);
    }

    // ---- agent operations ----

    fn tab_owner(st: &TabState) -> &'static str {
        if st.controller.is_some() {
            "human"
        } else if st.agent.is_some() {
            "agent"
        } else {
            "unclaimed"
        }
    }

    fn thread_tabs(&self, thread_id: &str) -> Vec<Arc<Tab>> {
        let mut tabs: Vec<Arc<Tab>> = locked(&self.tabs).values().filter(|t| t.thread_id == thread_id).cloned().collect();
        tabs.sort_by_key(|t| t.created);
        tabs
    }

    fn owned_tabs(&self, thread_id: &str, agent: &str) -> Vec<Arc<Tab>> {
        self.thread_tabs(thread_id).into_iter().filter(|t| t.st().agent.as_deref() == Some(agent)).collect()
    }

    /// `latestThreadTab`: the agent's newest tab, else the thread's most visible, most recently used one.
    fn latest_tab(&self, thread_id: &str, agent: &str) -> Option<Arc<Tab>> {
        if let Some(t) = self.owned_tabs(thread_id, agent).pop() {
            return Some(t);
        }
        let mut tabs = self.thread_tabs(thread_id);
        tabs.sort_by_key(|t| (t.visible(), t.st().used_at));
        tabs.pop()
    }

    fn profile_status(&self) -> Value {
        let reported = locked(&self.profiles).clone();
        let profiles = reported.as_ref().map(|r| r.profiles.clone()).unwrap_or_else(|| self.reported().profiles);
        let mut out = json!({
            "profiles": profiles.iter().map(|p| if p.kind == "incognito" { json!({ "id": p.id, "name": p.name, "incognito": true }) } else { json!({ "id": p.id, "name": p.name }) }).collect::<Vec<_>>(),
        });
        if let Some(r) = reported {
            out["defaultProfileId"] = json!(r.default_profile_id);
        }
        out
    }

    /// `statusWithTitle`.
    async fn status(&self, tab: Option<&Arc<Tab>>, agent: &str) -> Value {
        let Some(tab) = tab else {
            let mut s = json!({ "available": false, "visible": false, "tabId": null, "url": null, "title": null, "loading": false });
            merge(&mut s, self.profile_status());
            return s;
        };
        let (_, _, title) = if tab.st().dialog.is_none() { self.history(tab).await } else { (false, false, None) };
        if let Some(title) = &title {
            let changed = tab.st().title != *title;
            if changed {
                self.report_loaded(tab).await;
            }
        }
        let tabs: Vec<Value> = self
            .thread_tabs(&tab.thread_id)
            .iter()
            .map(|c| {
                let st = c.st();
                let mut entry = json!({ "tabId": c.tab_id, "url": status_url(&st), "owner": Self::tab_owner(&st), "ownedByCaller": st.agent.as_deref() == Some(agent), "visible": c.visible() });
                if let Some(p) = &c.profile_id {
                    entry["profileId"] = json!(p);
                }
                entry
            })
            .collect();
        let st = tab.st();
        let url = status_url(&st);
        let mut s = json!({
            "available": true,
            "visible": tab.visible(),
            "tabId": tab.tab_id,
            "url": url,
            "title": if url.is_null() || st.dialog.is_some() { Value::Null } else { title.filter(|t| !t.is_empty()).map_or(Value::Null, Value::String) },
            "loading": st.loading,
            "control": { "owner": Self::tab_owner(&st), "ownedByCaller": st.agent.as_deref() == Some(agent), "generation": st.generation },
            "dialog": Self::dialog_status(&st),
            "fileChooser": null,
            "viewportSetting": st.setting,
            "viewport": { "width": st.layout.0, "height": st.layout.1 },
            "tabs": tabs,
            "downloads": [],
        });
        if let Some(e) = &st.load_error {
            s["loadError"] = json!({ "url": e.url, "code": e.code, "description": e.description });
        }
        drop(st);
        merge(&mut s, self.profile_status());
        s
    }

    fn resolve_open_profile(&self, requested: Option<&str>) -> Result<Option<String>, OpError> {
        let reported = locked(&self.profiles).clone();
        let Some(requested) = requested else { return Ok(reported.map(|r| r.default_profile_id)) };
        let profiles = reported.map(|r| r.profiles).unwrap_or_else(|| self.reported().profiles);
        let wanted = requested.to_lowercase();
        match profiles.iter().find(|p| p.id == requested).or_else(|| profiles.iter().find(|p| p.name.to_lowercase() == wanted)) {
            Some(p) => Ok(Some(p.id.clone())),
            None => Err(OpError::new(
                "PreviewAutomationUnknownProfileError",
                format!(
                    "No browser profile is named \"{requested}\". Use one of: {}.",
                    profiles.iter().map(|p| format!("{} (id {})", p.name, p.id)).collect::<Vec<_>>().join(", ")
                ),
            )),
        }
    }

    /// Runs one agent operation (t3code `runOperation`), within its timeout.
    pub async fn invoke(self: &Arc<Self>, request: Request) -> Result<Value, OpError> {
        let deadline = tokio::time::Instant::now() + request.timeout;
        let ms = request.timeout.as_millis();
        let result = match tokio::time::timeout_at(deadline, self.run_operation(&request, deadline)).await {
            Ok(r) => r,
            Err(_) => Err(OpError::timeout(format!("Preview automation {} timed out after {ms}ms.", request.operation))),
        };
        for tab in self.thread_tabs(&request.thread_id) {
            if tab.st().agent.as_deref() == Some(request.agent.as_str()) || request.tab_id.as_deref() == Some(tab.tab_id.as_str()) {
                tab.st().used_at = Instant::now();
            }
        }
        result
    }

    async fn run_operation(self: &Arc<Self>, req: &Request, deadline: tokio::time::Instant) -> Result<Value, OpError> {
        let explicit = req.tab_id.is_some();
        match req.operation {
            "status" => {
                let tab = match &req.tab_id {
                    Some(id) => locked(&self.tabs).get(&(req.thread_id.clone(), id.clone())).cloned(),
                    None => self.latest_tab(&req.thread_id, &req.agent),
                };
                return Ok(self.status(tab.as_ref(), &req.agent).await);
            }
            "open" => return self.open(req, deadline).await,
            _ => {}
        }
        if !explicit && self.owned_tabs(&req.thread_id, &req.agent).len() > 1 {
            return Err(OpError::control("tabRequired"));
        }
        let tab = match &req.tab_id {
            None => self.latest_tab(&req.thread_id, &req.agent),
            Some(id) => self.find_tab(&req.thread_id, id).await.map_err(|e| OpError::unavailable(&e))?,
        };
        let Some(tab) = tab else {
            return Err(OpError::new("PreviewAutomationTabNotFoundError", "No server preview tab is open for this thread. Call preview_open first."));
        };
        let read = READ_OPERATIONS.contains(&req.operation);
        let (agent_may_act, controller) = {
            let st = tab.st();
            (st.agent.is_none() || st.agent.as_deref() == Some(req.agent.as_str()), st.controller.is_some())
        };
        if !read && !agent_may_act {
            return Err(OpError::control("agentMismatch"));
        }
        match req.operation {
            "close" => {
                if tab.st().agent.is_none() {
                    return Err(OpError::control("unclaimedTab"));
                }
                if controller {
                    return Err(OpError::control("humanControl"));
                }
                manager().close(self.epoch(), crate::rpc::preview::CloseInput { thread_id: tab.thread_id.clone(), tab_id: Some(tab.tab_id.clone()) });
                self.drop_tab(&tab, false).await;
                return Ok(json!({}));
            }
            "dialog" => {
                if controller {
                    return Err(OpError::control("humanControl"));
                }
                self.resolve_dialog(&tab, req.input["accept"] == true, req.input["promptText"].as_str()).await?;
                return Ok(self.status(Some(&tab), &req.agent).await);
            }
            _ => {}
        }
        if read && (!agent_may_act || controller) {
            // Observing does not wait behind the human's input.
            if tab.st().dialog.is_some() {
                return Err(OpError::control("dialogPending"));
            }
            return self.execute(&tab, req, deadline).await;
        }
        if controller {
            return Err(OpError::control("humanControl"));
        }
        let generation = tab.st().generation;
        let _queue = tab.queue.lock().await;
        let refused = {
            let st = tab.st();
            if tab.closed.is_cancelled() {
                Some("closed")
            } else if st.generation != generation || st.controller.is_some() {
                Some("interrupted")
            } else if st.dialog.is_some() {
                Some("dialogPending")
            } else {
                None
            }
        };
        if let Some(reason) = refused {
            return Err(OpError::control(reason));
        }
        self.execute(&tab, req, deadline).await
    }

    async fn open(self: &Arc<Self>, req: &Request, deadline: tokio::time::Instant) -> Result<Value, OpError> {
        let input = &req.input;
        let url = match input["url"].as_str() {
            Some(raw) => Some(normalize_preview_url(raw).map_err(|e| OpError::new("PreviewAutomationExecutionError", crate::rpc::admin_failure::Failure::from(e).to_string()))?),
            None => None,
        };
        let reuse = input["reuseExistingTab"] != false;
        let explicit = req.tab_id.is_some();
        let owned = self.owned_tabs(&req.thread_id, &req.agent);
        if reuse && !explicit && owned.len() > 1 {
            return Err(OpError::new("PreviewAutomationControlInterruptedError", "Multiple tabs are open. Pass tabId or reuseExistingTab=false."));
        }
        let found = match (&req.tab_id, reuse) {
            (Some(id), true) => self.find_tab(&req.thread_id, id).await.map_err(|e| OpError::unavailable(&e))?,
            (None, true) => owned.last().cloned(),
            _ => None,
        };
        if reuse && explicit && found.is_none() {
            let desktop = req.tab_id.as_ref().and_then(|id| manager().snapshot(&req.thread_id, id)).is_some_and(|s| s.runtime != Some(Runtime::Server));
            return Err(if desktop {
                OpError::execution("This tab belongs to the desktop runtime. This environment's server browser cannot control desktop tabs. Omit tabId to open a server tab instead.")
            } else {
                OpError::new("PreviewAutomationTabNotFoundError", "The requested server preview tab does not exist.")
            });
        }
        let nav_timeout = NAVIGATION_TIMEOUT.min(deadline.saturating_duration_since(tokio::time::Instant::now()));
        let tab = match found {
            Some(tab) => {
                let refused = {
                    let st = tab.st();
                    // Another session's tab is read with an explicit tabId, never acted on.
                    if st.agent.is_some() && st.agent.as_deref() != Some(req.agent.as_str()) {
                        Some("agentMismatch")
                    } else if st.controller.is_some() {
                        Some("humanControl")
                    } else if st.dialog.is_some() {
                        Some("dialogPending")
                    } else {
                        None
                    }
                };
                if let Some(reason) = refused {
                    return Err(OpError::control(reason));
                }
                if let Some(url) = &url {
                    let _queue = tab.queue.lock().await;
                    self.navigate(&tab, url, "load", nav_timeout).await?;
                }
                tab
            }
            None => {
                let profile_id = self.resolve_open_profile(input["profileId"].as_str())?;
                let tabs = locked(&self.tabs).len();
                if tabs >= SERVER_TAB_LIMIT || owned.len() >= AGENT_TAB_LIMIT {
                    return Err(OpError::new(
                        "PreviewAutomationControlInterruptedError",
                        format!("Too many server browser tabs are open ({AGENT_TAB_LIMIT} per agent session, {SERVER_TAB_LIMIT} per server). Close one with t3_preview_close or reuse one from preview_status tabs."),
                    ));
                }
                // The browser must be running before the tab is announced, so a missing one fails the call, not a tab.
                self.chrome_for(if profile_id.as_deref() == Some("incognito") { "default" } else { profile_id.as_deref().unwrap_or("default") }).await.map_err(|e| OpError::unavailable(&e))?;
                let open = crate::rpc::preview::OpenInput {
                    thread_id: req.thread_id.clone(),
                    url: None,
                    viewport: None,
                    profile_id,
                    runtime: Some(Runtime::Server),
                    color_scheme: None,
                    zoom_factor: None,
                    reveal: Some(false),
                };
                let snapshot = manager().open(self.epoch(), open, Some(req.agent.clone())).map_err(|e| OpError::execution(e.to_string()))?;
                let tab = match self.ensure_tab(&snapshot).await {
                    Ok(tab) => tab,
                    Err(e) => {
                        manager().close(self.epoch(), crate::rpc::preview::CloseInput { thread_id: snapshot.thread_id, tab_id: Some(snapshot.tab_id) });
                        return Err(OpError::unavailable(&e));
                    }
                };
                if let Some(url) = &url {
                    // A failed open leaves no tab behind to block later reuse.
                    if let Err(e) = self.navigate(&tab, url, "load", nav_timeout).await {
                        manager().close(self.epoch(), crate::rpc::preview::CloseInput { thread_id: tab.thread_id.clone(), tab_id: Some(tab.tab_id.clone()) });
                        self.drop_tab(&tab, false).await;
                        return Err(e);
                    }
                }
                tab
            }
        };
        let reveal = input["open"].as_bool().or(input["show"].as_bool());
        if reveal != Some(false) {
            let _ = manager().request_reveal(self.epoch(), &tab.thread_id, &tab.tab_id, reveal == Some(true));
            if reveal == Some(true) && !tab.visible() {
                let attached = tab.viewer_attached.notified();
                let _ = tokio::time::timeout(REVEAL_VIEWER_WAIT, attached).await;
            }
        }
        Ok(self.status(Some(&tab), &req.agent).await)
    }

    async fn resolve_dialog(&self, tab: &Arc<Tab>, accept: bool, prompt: Option<&str>) -> Result<(), OpError> {
        if tab.st().dialog.is_none() {
            return Err(OpError::execution("No dialog is pending."));
        }
        let mut params = json!({ "accept": accept });
        if let Some(text) = prompt {
            params["promptText"] = json!(text);
        }
        tab.call("Page.handleJavaScriptDialog", params).await?;
        {
            let mut st = tab.st();
            st.dialog = None;
            st.refs.clear();
        }
        self.broadcast_control(tab);
        Ok(())
    }

    fn record_start(&self, tab: &Tab, action: &str) -> String {
        let id = crate::util::new_uuid();
        push_bounded(&mut tab.st().actions, json!({ "id": id, "action": action, "status": "running", "startedAt": now() }), ACTION_LIMIT);
        id
    }

    fn record_end(&self, tab: &Tab, id: &str, outcome: &Result<Value, OpError>) {
        let mut st = tab.st();
        if let Some(event) = st.actions.iter_mut().find(|e| e["id"] == id) {
            event["status"] = json!(if outcome.is_ok() { "succeeded" } else { "failed" });
            event["completedAt"] = json!(now());
            if let Err(e) = outcome {
                event["error"] = json!(e.message.lines().next().unwrap_or_default());
            }
        }
    }

    async fn execute(self: &Arc<Self>, tab: &Arc<Tab>, req: &Request, deadline: tokio::time::Instant) -> Result<Value, OpError> {
        let input = &req.input;
        let recorded = ["navigate", "click", "hover", "select", "drag", "upload", "type", "press", "scroll"].contains(&req.operation);
        let id = recorded.then(|| self.record_start(tab, req.operation));
        let outcome = self.execute_unrecorded(tab, req, input, deadline).await;
        if let Some(id) = id {
            self.record_end(tab, &id, &outcome);
        }
        outcome
    }

    async fn execute_unrecorded(self: &Arc<Self>, tab: &Arc<Tab>, req: &Request, input: &Value, deadline: tokio::time::Instant) -> Result<Value, OpError> {
        let remaining = || deadline.saturating_duration_since(tokio::time::Instant::now());
        let timeout = |input: &Value| input["timeoutMs"].as_u64().map(Duration::from_millis).unwrap_or(Duration::from_millis(DEFAULT_TIMEOUT_MS)).min(remaining());
        match req.operation {
            "navigate" => {
                let url = navigation_url(input)?;
                let readiness = input["readiness"].as_str().unwrap_or("load");
                self.navigate(tab, &url, readiness, timeout(input)).await?;
                Ok(self.status(Some(tab), &req.agent).await)
            }
            "resize" => {
                let setting = resize_setting(input)?;
                manager()
                    .resize(self.epoch(), ResizeInput { thread_id: tab.thread_id.clone(), tab_id: tab.tab_id.clone(), viewport: setting.clone() })
                    .map_err(|e| OpError::execution(e.to_string()))?;
                tab.st().setting = setting.clone();
                self.apply_size(tab).await;
                let layout = tab.st().layout;
                Ok(json!({ "tabId": tab.tab_id, "setting": setting, "viewport": { "width": layout.0, "height": layout.1 } }))
            }
            "setColorScheme" => {
                let scheme = input["colorScheme"].as_str().unwrap_or("system").to_owned();
                let parsed: ColorScheme = serde_json::from_value(json!(scheme)).unwrap_or(ColorScheme::System);
                manager()
                    .adjust(
                        self.epoch(),
                        crate::rpc::preview::AdjustInput { thread_id: tab.thread_id.clone(), tab_id: tab.tab_id.clone(), color_scheme: Some(parsed), zoom_factor: None, hard_reload: None, clear: None },
                    )
                    .map_err(|e| OpError::execution(e.to_string()))?;
                self.apply_color(tab, &scheme).await;
                Ok(json!({ "tabId": tab.tab_id, "colorScheme": scheme }))
            }
            "snapshot" => self.snapshot(tab, input["fullScreenshot"] == true, input["pixelsRequired"] != false).await,
            "click" => {
                let point = self.pointer_target(tab, input, timeout(input)).await?;
                let button = input["button"].as_str().unwrap_or("left").to_owned();
                let count = input["clickCount"].as_u64().unwrap_or(1).clamp(1, 3);
                self.click(tab, &tab.session, point, &button, count).await?;
                Ok(Value::Null)
            }
            "hover" => {
                let point = self.pointer_target(tab, input, timeout(input)).await?;
                self.mouse(&tab.session, "mouseMoved", point, "none", 0, 0).await?;
                Ok(Value::Null)
            }
            "select" => {
                let target = self.target_of(tab, input, "select")?.ok_or_else(|| OpError::execution("Provide exactly one of selector or locator."))?;
                let done = self.wait_element(tab, &target, page::SELECT, &json!({ "values": input["values"] }), timeout(input), "select").await?;
                if done["notSelect"] == true {
                    return Err(OpError::execution("The target is not a native <select>. For a custom dropdown, click it open, then click the option."));
                }
                Ok(json!({ "selected": done["selected"] }))
            }
            "drag" => {
                let source = page::parse_locator(input["source"].as_str().unwrap_or_default(), &tab.st().refs).map_err(|e| locator_error(e, "drag", "locator"))?;
                let target = page::parse_locator(input["target"].as_str().unwrap_or_default(), &tab.st().refs).map_err(|e| locator_error(e, "drag", "locator"))?;
                let limit = timeout(input);
                let from = point_of(&self.wait_element(tab, &source, page::POINT, &Value::Null, limit, "drag").await?);
                let to = point_of(&self.wait_element(tab, &target, page::POINT, &Value::Null, limit, "drag").await?);
                self.mouse(&tab.session, "mouseMoved", from, "none", 0, 0).await?;
                self.mouse(&tab.session, "mousePressed", from, "left", 1, 1).await?;
                for step in 1..=5 {
                    let f = step as f64 / 5.0;
                    self.mouse(&tab.session, "mouseMoved", (from.0 + (to.0 - from.0) * f, from.1 + (to.1 - from.1) * f), "left", 0, 1).await?;
                }
                self.mouse(&tab.session, "mouseReleased", to, "left", 1, 0).await?;
                Ok(Value::Null)
            }
            "upload" => self.upload(tab, input, timeout(input)).await,
            "type" => {
                let target = self.target_of(tab, input, "type")?;
                let text = input["text"].as_str().unwrap_or_default();
                let clear = input["clear"] == true;
                let focused = match &target {
                    Some(target) => self.wait_element(tab, target, page::FOCUS_TEXT, &json!({ "clear": clear }), timeout(input), "type").await?,
                    None => self.eval(tab, &page::element_script(page::FOCUS_TEXT, &Value::Null, &json!({ "clear": clear }))).await?,
                };
                if focused["editable"] != true {
                    return Err(OpError::new("PreviewAutomationTargetNotEditableError", "The target is not an enabled text field, so no text was typed."));
                }
                if text.is_empty() {
                    if clear {
                        self.key(&tab.session, "Delete", 0).await?;
                    }
                } else {
                    tab.call("Input.insertText", json!({ "text": text })).await?;
                }
                Ok(Value::Null)
            }
            "press" => {
                let raw = input["key"].as_str().unwrap_or_default();
                let mut modifiers: Vec<String> = input["modifiers"].as_array().map(|m| m.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
                let key = match raw.rsplit_once('+') {
                    Some((mods, key)) if !key.is_empty() && raw.len() > 1 => {
                        modifiers.extend(mods.split('+').map(str::to_owned));
                        key
                    }
                    _ => raw,
                };
                self.key(&tab.session, key, page::modifier_bits(&modifiers)).await?;
                Ok(Value::Null)
            }
            "scroll" => {
                let target = self.target_of(tab, input, "scroll")?;
                let arg = json!({ "dx": input["deltaX"].as_f64().unwrap_or(0.0), "dy": input["deltaY"].as_f64().unwrap_or(0.0) });
                match &target {
                    Some(target) => {
                        self.wait_element(tab, target, page::SCROLL, &arg, timeout(input), "scroll").await?;
                    }
                    None => {
                        self.eval(tab, &page::element_script(page::SCROLL, &Value::Null, &arg)).await?;
                    }
                }
                Ok(Value::Null)
            }
            "evaluate" => self.evaluate(tab, input, remaining()).await,
            "waitFor" => self.wait_for(tab, input, timeout(input)).await,
            "recordingStart" | "recordingStop" => Err(OpError::new("PreviewAutomationUnsupportedClientError", "This server's browser does not record. Take snapshots instead.")),
            other => Err(OpError::new("PreviewAutomationUnsupportedClientError", format!("Preview automation client server-browser does not support {other}."))),
        }
    }

    /// The target a pointer action's `locator`, `selector` or `x`/`y` names, scrolled into view.
    async fn pointer_target(&self, tab: &Arc<Tab>, input: &Value, limit: Duration) -> Result<(f64, f64), OpError> {
        if let (Some(x), Some(y)) = (input["x"].as_f64(), input["y"].as_f64()) {
            return Ok((x, y));
        }
        let target = self.target_of(tab, input, "click")?.ok_or_else(|| OpError::execution("Provide exactly one target."))?;
        Ok(point_of(&self.wait_element(tab, &target, page::POINT, &Value::Null, limit, "click").await?))
    }

    fn target_of(&self, tab: &Tab, input: &Value, operation: &str) -> Result<Option<Target>, OpError> {
        if let Some(locator) = input["locator"].as_str() {
            return page::parse_locator(locator, &tab.st().refs).map(Some).map_err(|e| locator_error(e, operation, "locator"));
        }
        Ok(input["selector"].as_str().map(|css| Target::Css(css.to_owned())))
    }

    /// Runs an element script until it finds its element (Playwright's auto-wait), up to `limit`.
    async fn wait_element(&self, tab: &Tab, target: &Target, body: &str, arg: &Value, limit: Duration, operation: &str) -> Result<Value, OpError> {
        let deadline = tokio::time::Instant::now() + limit;
        let script = page::element_script(body, &target.spec(), arg);
        loop {
            let answer = self.eval(tab, &script).await?;
            if answer["invalid"] == true {
                let kind = if matches!(target, Target::Css(_)) { "selector" } else { "locator" };
                return Err(locator_error(LocatorError::Invalid, operation, kind));
            }
            if answer["found"] != false {
                return Ok(answer);
            }
            if tokio::time::Instant::now() + WAIT_POLL > deadline {
                return Err(OpError::timeout(format!(
                    "No element matched the target within {}ms. Take a fresh snapshot and use its locator.",
                    limit.as_millis()
                )));
            }
            tokio::time::sleep(WAIT_POLL).await;
        }
    }

    /// Evaluates `expression` in the page, by value.
    async fn eval(&self, tab: &Tab, expression: &str) -> Result<Value, OpError> {
        let r = tab.call("Runtime.evaluate", json!({ "expression": expression, "returnByValue": true, "awaitPromise": true })).await?;
        if let Some(e) = r.get("exceptionDetails") {
            return Err(OpError::execution(e["exception"]["description"].as_str().or(e["text"].as_str()).unwrap_or("The page script failed.").to_owned()));
        }
        Ok(r["result"]["value"].clone())
    }

    async fn mouse(&self, session: &Session, kind: &str, (x, y): (f64, f64), button: &str, clicks: u64, buttons: u64) -> Result<(), OpError> {
        session
            .call("Input.dispatchMouseEvent", json!({ "type": kind, "x": x, "y": y, "button": button, "buttons": buttons, "clickCount": clicks }))
            .await
            .map(|_| ())
            .map_err(cdp_failed)
    }

    /// A click; it returns once a dialog the click opened shows, not when the dialog closes.
    async fn click(&self, tab: &Tab, session: &Session, point: (f64, f64), button: &str, count: u64) -> Result<(), OpError> {
        let bit = match button {
            "right" => 2,
            "middle" => 4,
            _ => 1,
        };
        let dialog = tab.dialog_opened.notified();
        tokio::pin!(dialog);
        let clicks = async {
            self.mouse(session, "mouseMoved", point, "none", 0, 0).await?;
            for n in 1..=count {
                self.mouse(session, "mousePressed", point, button, n, bit).await?;
                self.mouse(session, "mouseReleased", point, button, n, 0).await?;
            }
            Ok::<_, OpError>(())
        };
        tokio::select! {
            r = clicks => r,
            () = &mut dialog => Ok(()),
        }
    }

    async fn key(&self, session: &Session, key: &str, modifiers: i64) -> Result<(), OpError> {
        let (key, code, vk, text) = page::key_event(key);
        let mut down = json!({ "type": if text.is_some() { "keyDown" } else { "rawKeyDown" }, "key": key, "code": code, "windowsVirtualKeyCode": vk, "modifiers": modifiers });
        if let Some(text) = text.filter(|_| modifiers & 0b0111 == 0) {
            down["text"] = json!(text);
            down["unmodifiedText"] = json!(text);
        }
        session.call("Input.dispatchKeyEvent", down).await.map_err(cdp_failed)?;
        session
            .call("Input.dispatchKeyEvent", json!({ "type": "keyUp", "key": key, "code": code, "windowsVirtualKeyCode": vk, "modifiers": modifiers }))
            .await
            .map_err(cdp_failed)?;
        Ok(())
    }

    async fn upload(&self, tab: &Tab, input: &Value, limit: Duration) -> Result<Value, OpError> {
        let Some(target) = self.target_of(tab, input, "upload")? else {
            return Err(OpError::execution("No file picker is open on this page. Pass a locator for an <input type=file>."));
        };
        let paths: Vec<String> = input["paths"].as_array().map(|p| p.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
        for path in &paths {
            let p = std::path::Path::new(path);
            if !p.is_absolute() || !p.is_file() {
                return Err(OpError::execution(format!("{path} is not an absolute path to a file.")));
            }
        }
        self.wait_element(tab, &target, page::EXISTS, &Value::Null, limit, "upload").await?;
        let r = tab.call("Runtime.evaluate", json!({ "expression": page::element_script(page::ELEMENT, &target.spec(), &Value::Null), "returnByValue": false })).await?;
        let Some(object) = r["result"]["objectId"].as_str().filter(|_| r["result"]["subtype"] == "node") else {
            return Err(OpError::execution("The target is not an <input type=file>."));
        };
        tab.call("DOM.setFileInputFiles", json!({ "files": paths, "objectId": object })).await?;
        Ok(json!({}))
    }

    /// t3code `evaluate`: by value, top-level `await` allowed, stopped at the timeout, at most 64 KB.
    async fn evaluate(&self, tab: &Tab, input: &Value, limit: Duration) -> Result<Value, OpError> {
        let expression = input["expression"].as_str().unwrap_or_default();
        let params = json!({ "expression": expression, "awaitPromise": input["awaitPromise"] != false, "returnByValue": true });
        let run = async {
            let mut r = tab.call("Runtime.evaluate", params.clone()).await?;
            let e = &r["exceptionDetails"];
            let unparsed = e["exception"]["className"] == "SyntaxError" && !e["scriptId"].is_null() && e["stackTrace"].is_null();
            if unparsed && expression.contains("await") {
                let mut repl = params.clone();
                repl["replMode"] = json!(true);
                r = tab.call("Runtime.evaluate", repl).await?;
            }
            Ok::<_, OpError>(r)
        };
        let r = match tokio::time::timeout(limit, run).await {
            Ok(r) => r?,
            Err(_) => {
                let _ = tab.session.call("Runtime.terminateExecution", json!({})).await;
                return Err(OpError::timeout(format!("Evaluation did not finish within {}ms and was stopped.", limit.as_millis())));
            }
        };
        if let Some(e) = r.get("exceptionDetails") {
            return Err(OpError::execution(e["exception"]["description"].as_str().or(e["text"].as_str()).unwrap_or("The expression failed.").to_owned()));
        }
        let value = match r["result"].get("value") {
            Some(v) => v.clone(),
            None => r["result"]["description"].clone(),
        };
        let bytes = value.to_string().len();
        if bytes > MAX_EVALUATION_BYTES {
            return Err(OpError::new("PreviewAutomationResultTooLargeError", format!("Evaluation result is {bytes} bytes; the limit is {MAX_EVALUATION_BYTES}.")));
        }
        Ok(value)
    }

    async fn wait_for(&self, tab: &Tab, input: &Value, limit: Duration) -> Result<Value, OpError> {
        let target = self.target_of(tab, input, "waitFor")?;
        let text = input["text"].as_str();
        let url = input["urlIncludes"].as_str();
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            let mut all = true;
            if let Some(target) = &target {
                let found = self.eval(tab, &page::element_script(page::EXISTS, &target.spec(), &Value::Null)).await.unwrap_or(Value::Null);
                if found["invalid"] == true {
                    return Err(locator_error(LocatorError::Invalid, "waitFor", "locator"));
                }
                all &= found["found"] == true;
            }
            if let Some(text) = text.filter(|_| all) {
                let expression = format!("((document.body && document.body.innerText) || \"\").includes({})", json!(text));
                all &= self.eval(tab, &expression).await.unwrap_or(Value::Null) == true;
            }
            if let Some(url) = url {
                all &= tab.st().url.contains(url);
            }
            if all {
                return Ok(json!({}));
            }
            if tokio::time::Instant::now() + WAIT_POLL > deadline {
                return Err(OpError::timeout(format!("Waited {}ms without every condition holding.", limit.as_millis())));
            }
            tokio::time::sleep(WAIT_POLL).await;
        }
    }

    /// `snapshot` / `snapshotMetadata`: page state, elements (whose refs replace the last ones),
    /// diagnostics, and a PNG of the viewport (`full`: also the uncapped one).
    async fn snapshot(&self, tab: &Tab, full: bool, pixels: bool) -> Result<Value, OpError> {
        let page_state = self.eval(tab, &page::snapshot_script(MAX_VISIBLE_TEXT, MAX_ELEMENTS)).await?;
        let elements = page_state["interactiveElements"].as_array().cloned().unwrap_or_default();
        let refs: Vec<String> = elements.iter().map(|e| e["selector"].as_str().unwrap_or_default().to_owned()).collect();
        let tree = page::tree(&elements);
        let (url, title, loading, load_error, console, network, actions, layout, zoom) = {
            let mut st = tab.st();
            st.refs = refs;
            (
                status_url(&st),
                st.title.clone(),
                st.loading || page_state["loading"] == true,
                st.load_error.clone(),
                st.console.iter().cloned().collect::<Vec<_>>(),
                st.network.iter().map(|(_, e)| e.clone()).collect::<Vec<_>>(),
                st.actions.iter().cloned().collect::<Vec<_>>(),
                st.layout,
                st.zoom,
            )
        };
        let page_title = page_state["title"].as_str().map(|t| t.chars().take(TITLE_LIMIT).collect::<String>()).unwrap_or(title);
        let mut out = json!({
            "url": url.as_str().map(str::to_owned).or_else(|| page_state["url"].as_str().map(page::without_data_payload)).unwrap_or_default(),
            "title": page_title,
            "loading": loading,
            "visibleText": page_state["visibleText"],
            "interactiveElements": elements,
            "accessibilityTree": tree,
            "consoleEntries": console,
            "networkEntries": network,
            "actionTimeline": actions,
        });
        if let Some(e) = load_error {
            out["loadError"] = json!({ "url": e.url, "code": e.code, "description": e.description });
            out["url"] = json!(e.url);
        }
        let scale = RENDER_SCALE * zoom;
        let device = ((layout.0 as f64 * scale).round() as u32, (layout.1 as f64 * scale).round() as u32);
        if !pixels {
            let (w, h) = capped(device);
            out["screenshot"] = json!({ "width": w, "height": h });
            return Ok(out);
        }
        let shot = tab.call("Page.captureScreenshot", json!({ "format": "png" })).await?;
        let data = shot["data"].as_str().unwrap_or_default().to_owned();
        let bytes = base64::engine::general_purpose::STANDARD.decode(&data).map_err(|e| OpError::execution(e.to_string()))?;
        let size = imagesize::blob_size(&bytes).map(|s| (s.width as u32, s.height as u32)).unwrap_or(device);
        let (agent_png, agent_size) = if size.0 > MAX_SCREENSHOT_WIDTH { shrink(&bytes).unwrap_or((data.clone(), size)) } else { (data.clone(), size) };
        out["screenshot"] = json!({ "mimeType": "image/png", "data": agent_png, "width": agent_size.0, "height": agent_size.1 });
        if full && agent_size != size {
            out["fullScreenshot"] = json!({ "data": data, "width": size.0, "height": size.1 });
        }
        Ok(out)
    }

    // ---- viewers ----

    /// t3code `attachViewer`: a screencast of the tab and its input.
    pub async fn attach(self: &Arc<Self>, input: AttachInput) -> Result<Viewer, AttachError> {
        let tab = match self.find_tab(&input.thread_id, &input.tab_id).await {
            Ok(Some(tab)) => tab,
            Ok(None) => return Err(AttachError::Gone),
            Err(e) => return Err(AttachError::Launch(e.to_string())),
        };
        let (session, mut frames) = tab.chrome.cdp.attach(&tab.target_id).await.map_err(|e| AttachError::Launch(e.to_string()))?;
        let (out, output) = mpsc::channel(VIEWER_OUTPUT);
        let id = format!("viewer-{}", self.sequence.fetch_add(1, Ordering::Relaxed) + 1);
        let link = Arc::new(ViewerLink {
            id: id.clone(),
            can_operate: input.can_operate,
            out,
            requested: Mutex::new(None),
            pressed_keys: Mutex::default(),
            pressed_buttons: Mutex::default(),
            session: session.clone(),
        });
        locked(&tab.viewers).push(link.clone());
        tab.viewer_attached.notify_waiters();
        let take = input.can_operate && {
            let st = tab.st();
            st.agent.is_none() && st.controller.is_none()
        };
        if take {
            self.set_controller(&tab, Some(id));
        } else {
            self.broadcast_control(&tab);
        }
        let layout = tab.st().layout;
        link.text(json!({ "type": "viewport", "width": layout.0, "height": layout.1 }));
        let quality = input.quality.clamp(1, 100);
        let _ = session
            .call("Page.startScreencast", json!({ "format": "jpeg", "quality": quality, "maxWidth": input.max_width.max(1), "maxHeight": input.max_height.max(1), "everyNthFrame": 1 }))
            .await;
        let forward = link.clone();
        let closed = tab.closed.clone();
        let frames = tokio::spawn(async move {
            let mut got_frame = false;
            let still = tokio::time::sleep(Duration::from_millis(300));
            tokio::pin!(still);
            loop {
                tokio::select! {
                    () = closed.cancelled() => break,
                    () = &mut still, if !got_frame => {
                        got_frame = true;
                        // An idle page does not repaint for a new screencast: start from a still.
                        if let Ok(shot) = forward.session.call("Page.captureScreenshot", json!({ "format": "jpeg", "quality": quality })).await
                            && let Ok(data) = base64::engine::general_purpose::STANDARD.decode(shot["data"].as_str().unwrap_or_default())
                        {
                            let _ = forward.out.try_send(Output::Frame { data, ack: None });
                        }
                    }
                    event = frames.recv() => {
                        let Some(Event { method, params }) = event else { break };
                        if method != "Page.screencastFrame" {
                            continue;
                        }
                        got_frame = true;
                        let frame_session = params["sessionId"].as_i64().unwrap_or_default();
                        let session = forward.session.clone();
                        let arrived = tokio::time::Instant::now();
                        let ack = Ack(async move {
                            tokio::time::sleep_until(arrived + SCREENCAST_ACK_PACE).await;
                            let _ = session.call("Page.screencastFrameAck", json!({ "sessionId": frame_session })).await;
                        }.boxed());
                        let Ok(data) = base64::engine::general_purpose::STANDARD.decode(params["data"].as_str().unwrap_or_default()) else { continue };
                        if let Err(mpsc::error::TrySendError::Full(Output::Frame { ack: Some(ack), .. })) = forward.out.try_send(Output::Frame { data, ack: Some(ack) }) {
                            // Dropped frames must still release the browser.
                            tokio::spawn(ack.send());
                        }
                    }
                }
            }
        });
        Ok(Viewer { output, link, tab, browser: self.clone(), frames })
    }

    async fn release_input(&self, link: &ViewerLink) {
        let keys: Vec<(String, String)> = locked(&link.pressed_keys).drain().map(|(_, k)| k).collect();
        for (key, code) in keys {
            let _ = link.session.call("Input.dispatchKeyEvent", json!({ "type": "keyUp", "key": key, "code": code })).await;
        }
        let buttons: Vec<(String, (f64, f64))> = locked(&link.pressed_buttons).drain().collect();
        for (button, (x, y)) in buttons {
            let _ = link.session.call("Input.dispatchMouseEvent", json!({ "type": "mouseReleased", "button": button, "x": x, "y": y, "buttons": 0, "clickCount": 1 })).await;
        }
        *locked(&link.requested) = None;
    }

    /// One message of a viewer (t3code's viewer `input` and `dispatchViewerInput`).
    async fn viewer_input(self: &Arc<Self>, tab: &Arc<Tab>, link: &Arc<ViewerLink>, message: Value) {
        if !link.can_operate || !message.is_object() {
            return;
        }
        let me = Some(link.id.clone());
        match message["type"].as_str().unwrap_or_default() {
            "takeControl" => {
                let other = tab.st().controller.clone().filter(|c| *c != link.id);
                if other.is_none() && tab.st().controller != me {
                    // Reserved at once, so actions waiting their turn find a new generation;
                    // the running one drains before this viewer's input runs.
                    self.set_controller(tab, me);
                    drop(tab.queue.lock().await);
                } else {
                    self.broadcast_control(tab);
                }
                return;
            }
            "releaseControl" => {
                if tab.st().controller == me {
                    self.set_controller(tab, None);
                    let _queue = tab.queue.lock().await;
                    self.release_input(link).await;
                } else {
                    self.broadcast_control(tab);
                }
                return;
            }
            "dialog" => {
                if tab.st().controller == me && message["accept"].is_boolean() {
                    let _ = self.resolve_dialog(tab, message["accept"] == true, message["promptText"].as_str()).await;
                }
                return;
            }
            _ => {}
        }
        if tab.st().controller != me {
            self.broadcast_control(tab);
            return;
        }
        let _queue = tab.queue.lock().await;
        if tab.st().controller != me {
            return;
        }
        if let Err(e) = self.dispatch_viewer_input(tab, link, &message).await {
            tracing::debug!("server preview input failed: {}", e.message);
        }
    }

    async fn dispatch_viewer_input(self: &Arc<Self>, tab: &Arc<Tab>, link: &Arc<ViewerLink>, m: &Value) -> Result<(), OpError> {
        let num = |k: &str| m[k].as_f64().unwrap_or(0.0);
        let modifiers = m["modifiers"].as_i64().unwrap_or(0);
        let session = &link.session;
        match m["type"].as_str().unwrap_or_default() {
            "mouse" => {
                let kind = match m["action"].as_str() {
                    Some("down") => "mousePressed",
                    Some("up") => "mouseReleased",
                    _ => "mouseMoved",
                };
                let button = m["button"].as_str().filter(|b| ["none", "left", "middle", "right"].contains(b)).unwrap_or("none").to_owned();
                let clicks = m["clickCount"].as_i64().unwrap_or(if kind == "mouseMoved" { 0 } else { 1 });
                let (x, y) = (num("x"), num("y"));
                let event = json!({ "type": kind, "x": x, "y": y, "button": button, "buttons": m["buttons"].as_i64().unwrap_or(0), "clickCount": clicks, "modifiers": modifiers });
                // A click that opens a dialog would hold the queue until the dialog closes.
                let dialog = tab.dialog_opened.notified();
                tokio::pin!(dialog);
                tokio::select! {
                    r = session.call("Input.dispatchMouseEvent", event) => { r.map_err(cdp_failed)?; }
                    () = &mut dialog => {}
                }
                if button != "none" {
                    match kind {
                        "mousePressed" => {
                            locked(&link.pressed_buttons).insert(button, (x, y));
                        }
                        "mouseReleased" => {
                            locked(&link.pressed_buttons).remove(&button);
                        }
                        _ => {}
                    }
                }
            }
            "wheel" => {
                session
                    .call("Input.dispatchMouseEvent", json!({ "type": "mouseWheel", "x": num("x"), "y": num("y"), "deltaX": num("deltaX"), "deltaY": num("deltaY"), "modifiers": modifiers }))
                    .await
                    .map_err(cdp_failed)?;
            }
            "key" => {
                let key = m["key"].as_str().unwrap_or_default().to_owned();
                let code = m["code"].as_str().unwrap_or_default().to_owned();
                let slot = if code.is_empty() { key.clone() } else { code.clone() };
                if m["action"] == "up" {
                    session.call("Input.dispatchKeyEvent", json!({ "type": "keyUp", "key": key, "code": code, "modifiers": modifiers })).await.map_err(cdp_failed)?;
                    locked(&link.pressed_keys).remove(&slot);
                    return Ok(());
                }
                let text = m["text"].as_str().filter(|t| !t.is_empty());
                let vk = m["keyCode"].as_i64().unwrap_or_else(|| if key.chars().count() == 1 { key.to_uppercase().chars().next().map_or(0, |c| c as i64) } else { 0 });
                let mut event = json!({ "type": if text.is_some() { "keyDown" } else { "rawKeyDown" }, "key": key, "code": code, "modifiers": modifiers, "windowsVirtualKeyCode": vk });
                if let Some(text) = text {
                    event["text"] = json!(text);
                    event["unmodifiedText"] = json!(text);
                }
                session.call("Input.dispatchKeyEvent", event).await.map_err(cdp_failed)?;
                locked(&link.pressed_keys).insert(slot, (key, code));
            }
            "text" => {
                if let Some(text) = m["text"].as_str().filter(|t| !t.is_empty()) {
                    let text: String = text.chars().take(10_000).collect();
                    session.call("Input.insertText", json!({ "text": text })).await.map_err(cdp_failed)?;
                }
            }
            "resize" => {
                let w = num("width").round().min(3840.0);
                let h = num("height").round().min(2160.0);
                if w < 100.0 || h < 100.0 {
                    return Ok(());
                }
                let order = self.sequence.fetch_add(1, Ordering::Relaxed);
                *locked(&link.requested) = Some((w as u32, h as u32, order));
                if matches!(tab.st().setting, Viewport::Fill) {
                    self.apply_size(tab).await;
                }
            }
            "viewport" => {
                let setting: Viewport = serde_json::from_value(m["setting"].clone()).map_err(|e| OpError::execution(e.to_string()))?;
                manager()
                    .resize(self.epoch(), ResizeInput { thread_id: tab.thread_id.clone(), tab_id: tab.tab_id.clone(), viewport: setting.clone() })
                    .map_err(|e| OpError::execution(e.to_string()))?;
                tab.st().setting = setting;
                self.apply_size(tab).await;
            }
            "navigate" => {
                if let Some(url) = m["url"].as_str() {
                    let url = normalize_preview_url(url).map_err(|_| OpError::execution("invalid URL"))?;
                    let _ = self.start_navigation(tab, &url).await;
                }
            }
            "history" => {
                let h = tab.call("Page.getNavigationHistory", json!({})).await?;
                let index = h["currentIndex"].as_i64().unwrap_or(0) + if num("delta") < 0.0 { -1 } else { 1 };
                if let Some(entry) = h["entries"].as_array().and_then(|e| usize::try_from(index).ok().and_then(|i| e.get(i))) {
                    tab.call("Page.navigateToHistoryEntry", json!({ "entryId": entry["id"] })).await?;
                }
            }
            "stopLoading" => {
                tab.call("Page.stopLoading", json!({})).await?;
                tab.st().loading = false;
                self.report_loaded(tab).await;
            }
            "reload" => {
                let crashed = tab.st().load_error.as_ref().filter(|e| e.description == "Page crashed").map(|e| e.url.clone());
                match crashed {
                    Some(url) => {
                        let _ = self.start_navigation(tab, &url).await;
                    }
                    None => {
                        tab.call("Page.reload", json!({ "ignoreCache": m["ignoreCache"] == true })).await?;
                    }
                }
            }
            "probe" => {
                let (x, y) = (num("x"), num("y"));
                let editable = self.eval(tab, &format!("{EDITABLE_AT_POINT}({x}, {y})")).await.unwrap_or(Value::Null) == true;
                link.text(json!({ "type": "probe", "x": x, "y": y, "editable": editable }));
            }
            _ => {}
        }
        Ok(())
    }

    async fn detach(&self, tab: &Arc<Tab>, link: &Arc<ViewerLink>) {
        if tab.st().controller.as_deref() == Some(link.id.as_str()) {
            self.set_controller(tab, None);
            self.release_input(link).await;
        }
        locked(&tab.viewers).retain(|v| !Arc::ptr_eq(v, link));
        let _ = link.session.call("Page.stopScreencast", json!({})).await;
        link.session.detach().await;
        self.broadcast_control(tab);
        if matches!(tab.st().setting, Viewport::Fill) && !tab.closed.is_cancelled() {
            self.apply_size(tab).await;
        }
    }

    // ---- html_preview ----

    /// t3code `captureHtmlScreenshot`: the page served at a private origin in a fresh
    /// context, measured, and captured up to 4,000 px tall.
    pub async fn html_preview(self: &Arc<Self>, html: String, width: u32, dark: bool) -> Result<HtmlShot, String> {
        let chrome = self.chrome_for("default").await.map_err(|e| e.to_string())?;
        let cdp = chrome.cdp.clone();
        let ctx = cdp.call(None, "Target.createBrowserContext", json!({ "disposeOnDetach": false })).await.map_err(|e| e.to_string())?;
        let ctx = ctx["browserContextId"].as_str().unwrap_or_default().to_owned();
        let shot = tokio::time::timeout(HTML_TIMEOUT, capture_html(&cdp, &ctx, html, width, dark)).await;
        let _ = cdp.call(None, "Target.disposeBrowserContext", json!({ "browserContextId": ctx })).await;
        match shot {
            Ok(r) => r,
            Err(_) => Err("the page did not finish loading within 20 seconds".into()),
        }
    }
}

async fn capture_html(cdp: &Arc<Cdp>, ctx: &str, html: String, width: u32, dark: bool) -> Result<HtmlShot, String> {
    let e = |e: CdpError| e.to_string();
    let target = cdp.call(None, "Target.createTarget", json!({ "url": "about:blank", "browserContextId": ctx })).await.map_err(e)?;
    let (session, mut events) = cdp.attach(target["targetId"].as_str().unwrap_or_default()).await.map_err(e)?;
    for method in ["Page.enable", "Runtime.enable", "Log.enable"] {
        session.call(method, json!({})).await.map_err(e)?;
    }
    session.call("Fetch.enable", json!({ "patterns": [{ "urlPattern": "*" }] })).await.map_err(e)?;
    session.call("Emulation.setDeviceMetricsOverride", json!({ "width": width, "height": HTML_VIEWPORT_HEIGHT, "deviceScaleFactor": 1, "mobile": false })).await.map_err(e)?;
    session
        .call("Emulation.setEmulatedMedia", json!({ "features": [{ "name": "prefers-color-scheme", "value": if dark { "dark" } else { "light" } }] }))
        .await
        .map_err(e)?;
    let main_frame = session.call("Page.getFrameTree", json!({})).await.map_err(e)?["frameTree"]["frame"]["id"].as_str().unwrap_or_default().to_owned();
    let body = base64::engine::general_purpose::STANDARD.encode(html.as_bytes());
    let console: Arc<Mutex<(Vec<(String, String)>, usize)>> = Arc::default();
    let (loaded_tx, loaded_rx) = tokio::sync::oneshot::channel::<()>();
    let pump = {
        let (session, console) = (session.clone(), console.clone());
        tokio::spawn(async move {
            let mut loaded = Some(loaded_tx);
            while let Some(Event { method, params: p }) = events.recv().await {
                let entry = match method.as_str() {
                    "Fetch.requestPaused" => {
                        let id = p["requestId"].clone();
                        let url = p["request"]["url"].as_str().unwrap_or_default().split('#').next().unwrap_or_default().to_owned();
                        let answer = if url == HTML_PAGE_URL {
                            session
                                .call("Fetch.fulfillRequest", json!({ "requestId": id, "responseCode": 200, "responseHeaders": [{ "name": "Content-Type", "value": "text/html; charset=utf-8" }], "body": body }))
                                .await
                        } else if url == HTML_FAVICON_URL {
                            // The browser asks for one; the page has none.
                            session.call("Fetch.fulfillRequest", json!({ "requestId": id, "responseCode": 204, "responseHeaders": [] })).await
                        } else if p["resourceType"] == "Document" && p["frameId"].as_str() == Some(main_frame.as_str()) {
                            // The main frame stays on the page.
                            session.call("Fetch.failRequest", json!({ "requestId": id, "errorReason": "AccessDenied" })).await
                        } else {
                            session.call("Fetch.continueRequest", json!({ "requestId": id })).await
                        };
                        let _ = answer;
                        None
                    }
                    "Page.loadEventFired" => {
                        if let Some(tx) = loaded.take() {
                            let _ = tx.send(());
                        }
                        None
                    }
                    "Runtime.consoleAPICalled" => {
                        let level = match p["type"].as_str().unwrap_or_default() {
                            "log" | "debug" | "dir" | "dirxml" | "table" | "trace" => Some("log"),
                            "info" => Some("info"),
                            "warning" => Some("warning"),
                            "error" | "assert" => Some("error"),
                            _ => None,
                        };
                        level.map(|l| (l.to_owned(), p["args"].as_array().map(|a| a.iter().map(remote_text).collect::<Vec<_>>().join(" ")).unwrap_or_default()))
                    }
                    "Runtime.exceptionThrown" => {
                        let d = &p["exceptionDetails"];
                        Some(("error".to_owned(), d["exception"]["description"].as_str().or(d["text"].as_str()).unwrap_or_default().to_owned()))
                    }
                    "Log.entryAdded" => {
                        let entry = &p["entry"];
                        let level = entry["level"].as_str().unwrap_or_default();
                        (level == "error" || level == "warning").then(|| {
                            let text = entry["text"].as_str().unwrap_or_default();
                            (level.to_owned(), match entry["url"].as_str() { Some(url) => format!("{text} {url}"), None => text.to_owned() })
                        })
                    }
                    _ => None,
                };
                if let Some((level, text)) = entry {
                    let mut c = locked(&console);
                    if c.0.len() >= HTML_MAX_CONSOLE {
                        c.1 += 1;
                    } else {
                        let text = text.replace(HTML_PAGE_URL, "page.html");
                        let clipped = if text.chars().count() > HTML_MAX_CONSOLE_CHARS { format!("{}…", text.chars().take(HTML_MAX_CONSOLE_CHARS).collect::<String>()) } else { text };
                        c.0.push((level, clipped));
                    }
                }
            }
        })
    };
    let nav = session.call("Page.navigate", json!({ "url": HTML_PAGE_URL })).await.map_err(e)?;
    if nav["errorText"].as_str().is_some_and(|t| !t.is_empty()) {
        pump.abort();
        return Err("the page could not be opened".into());
    }
    let _ = loaded_rx.await;
    let settle = "document.fonts.ready.then(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve(true)))))";
    let _ = session.call("Runtime.evaluate", json!({ "expression": settle, "awaitPromise": true })).await;
    let measure = "(() => { const root = document.documentElement; return root.scrollHeight > root.clientHeight ? root.scrollHeight : root.getBoundingClientRect().height; })()";
    let measured = session.call("Runtime.evaluate", json!({ "expression": measure, "returnByValue": true })).await.map_err(e)?;
    let content_height = measured["result"]["value"].as_f64().unwrap_or(0.0).ceil().max(0.0) as u64;
    let captured_height = content_height.clamp(1, HTML_MAX_CAPTURE_HEIGHT);
    let shot = session
        .call("Page.captureScreenshot", json!({ "format": "png", "captureBeyondViewport": true, "clip": { "x": 0, "y": 0, "width": width, "height": captured_height, "scale": 1 } }))
        .await
        .map_err(e)?;
    pump.abort();
    let _ = cdp.call(None, "Target.closeTarget", json!({ "targetId": target["targetId"] })).await;
    let (mut messages, omitted) = locked(&console).clone();
    if omitted > 0 {
        messages.push(("warning".into(), format!("{omitted} more console messages were omitted.")));
    }
    Ok(HtmlShot { png: shot["data"].as_str().unwrap_or_default().to_owned(), content_height, captured_height, console: messages })
}

impl Viewer {
    /// A message the viewer sent (anything but an `ack`).
    pub async fn input(&self, message: Value) {
        self.browser.viewer_input(&self.tab, &self.link, message).await;
    }

    /// What the viewer is sent (taken once; later calls get a closed queue).
    pub fn take_output(&mut self) -> mpsc::Receiver<Output> {
        std::mem::replace(&mut self.output, mpsc::channel(1).1)
    }

    /// Detaches: control is released and the screencast stops.
    pub async fn close(&self) {
        self.frames.abort();
        self.browser.detach(&self.tab, &self.link).await;
    }
}

fn merge(into: &mut Value, more: Value) {
    if let (Some(into), Value::Object(more)) = (into.as_object_mut(), more) {
        into.extend(more);
    }
}

fn clip_url(url: &str) -> String {
    page::without_data_payload(url).chars().take(2048).collect()
}

/// A URL for an error message: no credentials, query or fragment.
fn diagnostic_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut u) => {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.set_query(None);
            u.set_fragment(None);
            page::without_data_payload(u.as_str())
        }
        Err(_) => "the requested page".into(),
    }
}

/// The URL a status names: the failed one, nothing for a blank page.
fn status_url(st: &TabState) -> Value {
    if let Some(e) = &st.load_error {
        return json!(e.url);
    }
    if st.url == "about:blank" || st.url.is_empty() { Value::Null } else { json!(clip_url(&st.url)) }
}

fn point_of(answer: &Value) -> (f64, f64) {
    (answer["x"].as_f64().unwrap_or(0.0), answer["y"].as_f64().unwrap_or(0.0))
}

fn capped((w, h): (u32, u32)) -> (u32, u32) {
    if w <= MAX_SCREENSHOT_WIDTH {
        (w, h)
    } else {
        (MAX_SCREENSHOT_WIDTH, ((h as f64) * MAX_SCREENSHOT_WIDTH as f64 / w as f64).round() as u32)
    }
}

/// The PNG scaled to `MAX_SCREENSHOT_WIDTH`, as base64.
fn shrink(png: &[u8]) -> Option<(String, (u32, u32))> {
    let image = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?;
    let (w, h) = capped((image.width(), image.height()));
    let small = image.resize_exact(w, h, image::imageops::FilterType::Triangle);
    let mut out = std::io::Cursor::new(Vec::new());
    small.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some((base64::engine::general_purpose::STANDARD.encode(out.into_inner()), (w, h)))
}

fn locator_error(e: LocatorError, operation: &str, kind: &str) -> OpError {
    match e {
        LocatorError::StaleRef => OpError::new(
            "PreviewAutomationInvalidSelectorError",
            "This element ref is stale. Navigation, dialogs, and new snapshots replace refs; take a fresh snapshot and use its refs.",
        ),
        LocatorError::Invalid => {
            let guidance = if kind == "locator" {
                "Use aria-ref=<ref> from the latest snapshot or a role/text selector; put CSS in selector."
            } else {
                "selector takes CSS; use locator for aria-ref=<ref>, role, or text targets."
            };
            OpError::new("PreviewAutomationInvalidSelectorError", format!("Preview automation {operation} received an invalid {kind}. {guidance}"))
        }
    }
}

/// `resolveNavigationUrl`: a URL, or a dev-server port of this machine.
fn navigation_url(input: &Value) -> Result<String, OpError> {
    let invalid = |e: crate::rpc::preview::InvalidUrl| OpError::execution(crate::rpc::admin_failure::Failure::from(e).to_string());
    if let Some(url) = input["url"].as_str() {
        return normalize_preview_url(url).map_err(invalid);
    }
    let target = &input["target"];
    match target["kind"].as_str() {
        Some("url") => normalize_preview_url(target["url"].as_str().unwrap_or_default()).map_err(invalid),
        Some("environment-port") => {
            let port = target["port"].as_u64().filter(|p| (1..65_536).contains(p)).ok_or_else(|| OpError::execution("Provide a port from 1 to 65535."))?;
            let path = target["path"].as_str().unwrap_or("");
            let path = if path.is_empty() || path.starts_with('/') { path.to_owned() } else { format!("/{path}") };
            Ok(format!("{}://localhost:{port}{path}", target["protocol"].as_str().unwrap_or("http")))
        }
        _ => Err(OpError::execution("Provide exactly one of url or target.")),
    }
}

/// `resolvePreviewViewport`.
fn resize_setting(input: &Value) -> Result<Viewport, OpError> {
    let n = |v: u32| serde_json::Number::from(v);
    match input["mode"].as_str() {
        Some("fill") => Ok(Viewport::Fill),
        Some("preset") => {
            let id = input["preset"].as_str().unwrap_or_default();
            let (_, w, h) = PRESETS.iter().find(|(p, ..)| *p == id).ok_or_else(|| OpError::execution(format!("Unknown preview viewport preset: {id}")))?;
            let landscape = input["orientation"] == "landscape";
            let portrait = input["orientation"] == "portrait";
            let native_portrait = h >= w;
            let swap = (landscape && native_portrait) || (portrait && !native_portrait);
            let (w, h) = if swap { (*h, *w) } else { (*w, *h) };
            Ok(Viewport::Preset { width: n(w), height: n(h), preset_id: id.to_owned() })
        }
        Some("freeform") => {
            let w = input["width"].as_u64().and_then(|v| u32::try_from(v).ok());
            let h = input["height"].as_u64().and_then(|v| u32::try_from(v).ok());
            match (w, h) {
                (Some(w), Some(h)) if (240..=3840).contains(&w) && (240..=3840).contains(&h) && u64::from(w) * u64::from(h) <= 3840 * 2160 => Ok(Viewport::Freeform { width: n(w), height: n(h) }),
                _ => Err(OpError::execution("Freeform mode requires width and height from 240 to 3840, at most 3840x2160 pixels in area.")),
            }
        }
        _ => Err(OpError::execution("mode is one of fill, freeform or preset.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewports_resolve_like_t3code() {
        assert_eq!(resize_setting(&json!({ "mode": "fill" })).unwrap(), Viewport::Fill);
        let preset = resize_setting(&json!({ "mode": "preset", "preset": "iphone-se", "orientation": "landscape" })).unwrap();
        assert_eq!(json!(preset), json!({ "_tag": "preset", "width": 667, "height": 375, "presetId": "iphone-se" }));
        let free = resize_setting(&json!({ "mode": "freeform", "width": 800, "height": 600 })).unwrap();
        assert_eq!(fixed_size(&free), Some((800, 600)));
        assert!(resize_setting(&json!({ "mode": "freeform", "width": 100, "height": 600 })).is_err());
        assert!(resize_setting(&json!({ "mode": "preset", "preset": "nope" })).is_err());
        assert_eq!(capped((2560, 1600)), (1280, 800));
    }

    #[test]
    fn dev_server_targets_are_this_machine() {
        assert_eq!(navigation_url(&json!({ "target": { "kind": "environment-port", "port": 5173, "path": "settings?tab=1" } })).unwrap(), "http://localhost:5173/settings?tab=1");
        assert_eq!(navigation_url(&json!({ "url": "localhost:3000" })).unwrap(), "http://localhost:3000/");
        assert!(navigation_url(&json!({ "url": "file:///etc/passwd" })).is_err());
        assert_eq!(diagnostic_url("http://u:p@a.test/x?q=1#f"), "http://a.test/x");
    }

    #[test]
    fn reported_profiles_keep_the_built_in_ones_first() {
        let r = resolve_profiles(vec![BrowserProfile { id: "work".into(), name: "Work".into(), kind: "incognito".into() }, BrowserProfile { id: "default".into(), name: "Mine".into(), kind: "persistent".into() }], "work");
        assert_eq!(r.profiles.iter().map(|p| (p.id.as_str(), p.kind.as_str())).collect::<Vec<_>>(), [("default", "persistent"), ("incognito", "incognito"), ("work", "persistent")]);
        assert_eq!(r.default_profile_id, "work");
        assert_eq!(resolve_profiles(vec![], "gone").default_profile_id, "default");
    }
}
