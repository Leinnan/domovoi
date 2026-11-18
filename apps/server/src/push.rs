//! APNs push straight from the server (Domovoi only; t3code pushes through its
//! relay). When a thread starts needing approval, finishes or fails, every
//! registered device whose app has no socket open (closed or in the
//! background) gets an alert. The rules are `domovoi_model::typed`'s (`shell_status`, `notice`), shared with
//! the client's local notifications.
//!
//! Configure with `--apns-key <AuthKey.p8> --apns-key-id … --apns-team-id …
//! [--apns-topic com.crayen.domovoi]`, or the same fields in `<home>/push.json`. The
//! key is read from its path; it never needs to be copied anywhere.
//! Devices register through `POST /api/domovoi/push/devices` with their session.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "APNs payloads and JWT parts")]

use crate::db::Db;
use crate::state::SharedState;
use crate::util::locked;
use base64::Engine;
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use domovoi_contracts::time_fmt::{iso_from_millis, now_millis};
use domovoi_model::typed::{RuntimeRequestKind, ShellStatus, ThreadShell, TurnItem, UserInputQuestion, notice, shell_status};

const PRODUCTION: &str = "https://api.push.apple.com";
const SANDBOX: &str = "https://api.sandbox.push.apple.com";
/// Apple wants provider tokens refreshed between 20 and 60 minutes. Counted
/// in wall-clock time: a monotonic clock stops while the machine sleeps, and
/// a token signed before a night's sleep is long expired after it.
const JWT_LIFETIME_MS: i64 = 40 * 60 * 1000;
/// Thread events arrive in bursts while a turn streams; statuses are checked
/// this long after the first event of a burst. A request answered within the
/// window (auto-approved, or approved on another device) is not announced.
const DEBOUNCE: Duration = Duration::from_millis(800);
/// How long nothing is sent after Apple says to slow down.
const THROTTLE_PAUSE: Duration = Duration::from_secs(60);
/// Pushes in flight at once.
const SENDS_AT_ONCE: usize = 4;
/// Alert titles are thread titles (user text): kept well inside APNs' 4 KB.
const TITLE_CHARS: usize = 120;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushConfig {
    pub key_path: Option<PathBuf>,
    pub key_id: Option<String>,
    pub team_id: Option<String>,
    pub topic: Option<String>,
    /// Send no thread titles to Apple: alerts are titled with this server's
    /// name and say only what happened.
    pub private_alerts: Option<bool>,
}

impl PushConfig {
    /// `<home>/push.json`, with command-line values taking precedence.
    pub fn load(home: &Path, cli: PushConfig) -> PushConfig {
        let file: PushConfig = std::fs::read(home.join("push.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        PushConfig {
            key_path: cli.key_path.or(file.key_path),
            key_id: cli.key_id.or(file.key_id),
            team_id: cli.team_id.or(file.team_id),
            topic: cli.topic.or(file.topic),
            private_alerts: cli.private_alerts.or(file.private_alerts),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SendError {
    /// The token is no longer valid (the app was removed): forget it.
    Gone(String),
    /// `BadDeviceToken`: no such token on this APNs host. It may belong to
    /// the other one (a sandbox token marked production, or the reverse).
    WrongHost,
    /// The provider token was refused as expired or invalid.
    ProviderToken(String),
    /// Apple asked to slow down.
    Throttled(String),
    Other(String),
}

/// What became of a push.
#[derive(Debug, PartialEq, Eq)]
pub enum Delivery {
    Sent,
    /// Sent through the other APNs host than the device registered for;
    /// its record should say so from now on.
    SentOnOtherHost,
    /// The device is gone: forget it.
    Gone(String),
    Failed(String),
}

/// An APNs provider: token-based auth (ES256 JWT) over HTTP/2.
pub struct Apns {
    client: reqwest::Client,
    key: EcdsaKeyPair,
    rng: ring::rand::SystemRandom,
    key_id: String,
    team_id: String,
    topic: String,
    sandbox_base: String,
    production_base: String,
    pub private_alerts: bool,
    /// The provider token and when it was signed (wall-clock ms).
    jwt: Mutex<Option<(String, i64)>>,
    /// No sends before this (wall-clock ms), after Apple throttled us.
    paused_until: Mutex<i64>,
}

impl Apns {
    pub fn new(config: &PushConfig) -> Result<Option<Apns>, String> {
        let (Some(path), Some(key_id), Some(team_id)) = (&config.key_path, &config.key_id, &config.team_id) else {
            return Ok(None);
        };
        let pem = std::fs::read_to_string(path).map_err(|e| format!("APNs key {}: {e}", path.display()))?;
        let mut apns = Apns::from_pem(&pem, key_id, team_id, config.topic.as_deref().unwrap_or("com.crayen.domovoi"))?;
        apns.private_alerts = config.private_alerts.unwrap_or(false);
        // Tests point the hosts at a mock: both at once, or each by itself.
        let both = std::env::var("DOMOVOI_APNS_URL").ok();
        if let Some(url) = std::env::var("DOMOVOI_APNS_SANDBOX_URL").ok().or_else(|| both.clone()) {
            apns.sandbox_base = url;
        }
        if let Some(url) = std::env::var("DOMOVOI_APNS_PRODUCTION_URL").ok().or(both) {
            apns.production_base = url;
        }
        Ok(Some(apns))
    }

    pub fn from_pem(pem: &str, key_id: &str, team_id: &str, topic: &str) -> Result<Apns, String> {
        let body: String = pem.lines().filter(|l| !l.starts_with("-----")).map(str::trim).collect();
        let der = base64::engine::general_purpose::STANDARD.decode(body).map_err(|e| format!("APNs key is not PEM: {e}"))?;
        let rng = ring::rand::SystemRandom::new();
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &der, &rng).map_err(|e| format!("APNs key is not a P-256 PKCS#8 key: {e}"))?;
        // reqwest's rustls backend comes without a crypto provider (see main).
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .http2_prior_knowledge()
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Apns {
            client,
            key,
            rng,
            key_id: key_id.into(),
            team_id: team_id.into(),
            topic: topic.into(),
            sandbox_base: SANDBOX.into(),
            production_base: PRODUCTION.into(),
            private_alerts: false,
            jwt: Mutex::new(None),
            paused_until: Mutex::new(0),
        })
    }

    /// The provider token, re-signed when it nears Apple's limit.
    fn jwt(&self) -> Result<String, String> {
        self.jwt_at(now_millis())
    }

    fn jwt_at(&self, now: i64) -> Result<String, String> {
        let mut cached = locked(&self.jwt);
        if let Some((token, signed)) = cached.as_ref()
            && (0..JWT_LIFETIME_MS).contains(&(now - signed))
        {
            return Ok(token.clone());
        }
        let b64 = |v: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
        let header = b64(json!({ "alg": "ES256", "kid": self.key_id }).to_string().as_bytes());
        let claims = b64(json!({ "iss": self.team_id, "iat": now / 1000 }).to_string().as_bytes());
        let input = format!("{header}.{claims}");
        let signature = self.key.sign(&self.rng, input.as_bytes()).map_err(|e| format!("signing the APNs token: {e}"))?;
        let token = format!("{input}.{}", b64(signature.as_ref()));
        *cached = Some((token.clone(), now));
        Ok(token)
    }

    /// One request to one APNs host.
    async fn send(&self, device: &str, sandbox: bool, payload: &Value, collapse_id: &str) -> Result<(), SendError> {
        self.post(device, sandbox, payload, "alert", &self.topic, Some(collapse_id)).await
    }

    /// An update to a Live Activity (`liveactivity` pushes go to the app's
    /// topic plus `.push-type.liveactivity`).
    pub async fn send_live(&self, token: &str, sandbox: bool, payload: &Value) -> Result<(), String> {
        if now_millis() < *locked(&self.paused_until) {
            return Err("paused after APNs asked to slow down".into());
        }
        let topic = format!("{}.push-type.liveactivity", self.topic);
        self.post(token, sandbox, payload, "liveactivity", &topic, None).await.map_err(describe)
    }

    async fn post(&self, device: &str, sandbox: bool, payload: &Value, push_type: &str, topic: &str, collapse_id: Option<&str>) -> Result<(), SendError> {
        let base = if sandbox { &self.sandbox_base } else { &self.production_base };
        let jwt = self.jwt().map_err(SendError::Other)?;
        let mut req = self
            .client
            .post(format!("{base}/3/device/{device}"))
            .header("authorization", format!("bearer {jwt}"))
            .header("apns-topic", topic)
            .header("apns-push-type", push_type)
            .header("apns-priority", "10");
        if let Some(id) = collapse_id {
            req = req.header("apns-collapse-id", collapse(id).as_ref());
        }
        let res = req.json(payload).send().await.map_err(|e| SendError::Other(e.to_string()))?;
        let status = res.status().as_u16();
        if status == 200 {
            return Ok(());
        }
        let reason = res.json::<Value>().await.ok().and_then(|b| b["reason"].as_str().map(str::to_owned)).unwrap_or_default();
        Err(match (status, reason.as_str()) {
            (410, _) | (400, "DeviceTokenNotForTopic") => SendError::Gone(reason),
            (400, "BadDeviceToken") => SendError::WrongHost,
            (403, "ExpiredProviderToken" | "InvalidProviderToken") => SendError::ProviderToken(reason),
            (429, _) => SendError::Throttled(reason),
            _ => SendError::Other(format!("APNs answered {status} {reason}")),
        })
    }

    /// Sends one push, recovering from what can be recovered: a refused
    /// provider token is re-signed once, and a token unknown to its host is
    /// tried on the other one before the device is given up.
    pub async fn deliver(&self, device: &str, sandbox: bool, payload: &Value, collapse_id: &str) -> Delivery {
        if now_millis() < *locked(&self.paused_until) {
            return Delivery::Failed("paused after APNs asked to slow down".into());
        }
        let mut first = self.send(device, sandbox, payload, collapse_id).await;
        if let Err(SendError::ProviderToken(reason)) = &first {
            tracing::warn!("APNs refused the provider token ({reason}); signing a new one");
            *locked(&self.jwt) = None;
            first = self.send(device, sandbox, payload, collapse_id).await;
        }
        match first {
            Ok(()) => Delivery::Sent,
            Err(SendError::WrongHost) => match self.send(device, !sandbox, payload, collapse_id).await {
                Ok(()) => Delivery::SentOnOtherHost,
                Err(SendError::WrongHost | SendError::Gone(_)) => Delivery::Gone("BadDeviceToken".into()),
                Err(e) => Delivery::Failed(describe(e)),
            },
            Err(SendError::Gone(reason)) => Delivery::Gone(reason),
            Err(SendError::Throttled(reason)) => {
                *locked(&self.paused_until) = now_millis() + THROTTLE_PAUSE.as_millis() as i64;
                Delivery::Failed(format!("APNs asked to slow down ({reason}); pausing for a minute"))
            }
            Err(e) => Delivery::Failed(describe(e)),
        }
    }
}

fn describe(e: SendError) -> String {
    match e {
        SendError::Gone(r) | SendError::ProviderToken(r) | SendError::Throttled(r) | SendError::Other(r) => r,
        SendError::WrongHost => "BadDeviceToken".into(),
    }
}

/// `apns-collapse-id` is a header of at most 64 bytes. Thread ids come from
/// clients: a long or non-ASCII one (which a header cannot carry) is stood in
/// for by a hash of it.
fn collapse(id: &str) -> std::borrow::Cow<'_, str> {
    if id.len() <= 64 && id.bytes().all(|b| b.is_ascii_graphic()) {
        id.into()
    } else {
        crate::util::sha256_hex(id.as_bytes())[..32].to_owned().into()
    }
}

fn clip_title(title: &str) -> String {
    if title.chars().count() <= TITLE_CHARS {
        return title.to_owned();
    }
    let mut out: String = title.chars().take(TITLE_CHARS - 1).collect();
    out.push('…');
    out
}

// ---- devices ----

/// Registers a device token for a session. The same token may be registered
/// by several sessions (one phone paired twice); each owns only its own row.
pub fn register(db: &Db, token: &str, environment: &str, session_hash: &str) -> rusqlite::Result<()> {
    db.with(|c| {
        c.execute(
            "INSERT INTO push_devices (token, session_hash, environment, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(token, session_hash) DO UPDATE SET environment = excluded.environment",
            params![token, session_hash, environment, iso_from_millis(now_millis())],
        )
        .map(drop)
    })
}

/// Removes a session's own registration of a token.
pub fn unregister(db: &Db, token: &str, session_hash: &str) -> rusqlite::Result<()> {
    db.with(|c| c.execute("DELETE FROM push_devices WHERE token = ?1 AND session_hash = ?2", params![token, session_hash]).map(drop))
}

/// Forgets a token Apple no longer knows, whoever registered it.
fn forget_token(db: &Db, token: &str) -> rusqlite::Result<()> {
    db.with(|c| c.execute("DELETE FROM push_devices WHERE token = ?1", params![token]).map(drop))
}

fn set_environment(db: &Db, token: &str, sandbox: bool) -> rusqlite::Result<()> {
    db.with(|c| c.execute("UPDATE push_devices SET environment = ?2 WHERE token = ?1", params![token, if sandbox { "sandbox" } else { "production" }]).map(drop))
}

/// Drops the devices of revoked sessions.
pub fn forget_sessions(db: &Db, session_hashes: &[String]) -> rusqlite::Result<()> {
    db.with(|c| {
        for h in session_hashes {
            c.execute("DELETE FROM push_devices WHERE session_hash = ?1", params![h])?;
        }
        Ok(())
    })
}

/// Drops devices whose session no longer exists (expired or revoked).
pub fn forget_orphans(db: &Db) -> rusqlite::Result<usize> {
    db.with(|c| c.execute("DELETE FROM push_devices WHERE session_hash NOT IN (SELECT token_hash FROM auth_sessions)", []))
}

/// A device to push to: its token, the sessions that registered it, and its
/// APNs host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushDevice {
    pub token: String,
    pub sessions: Vec<String>,
    pub sandbox: bool,
}

/// Devices with at least one live session, one entry per token.
pub fn devices(db: &Db) -> rusqlite::Result<Vec<PushDevice>> {
    let now = now_millis();
    let rows: Vec<(String, String, bool)> = db.with(|c| {
        let mut st = c.prepare(
            "SELECT d.token, d.session_hash, d.environment FROM push_devices d
             JOIN auth_sessions s ON s.token_hash = d.session_hash WHERE s.expires_at_ms > ?1 ORDER BY d.token",
        )?;
        let rows = st.query_map(params![now], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)? == "sandbox")))?;
        rows.collect()
    })?;
    let mut out: Vec<PushDevice> = Vec::new();
    for (token, session, sandbox) in rows {
        match out.last_mut() {
            Some(last) if last.token == token => last.sessions.push(session),
            _ => out.push(PushDevice { token, sessions: vec![session], sandbox }),
        }
    }
    Ok(out)
}

pub fn device_count(db: &Db) -> usize {
    devices(db).map(|d| d.len()).unwrap_or(0)
}

pub fn valid_token(token: &str) -> bool {
    (32..=200).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_hexdigit())
}

// ---- the notifier ----

/// The alert for a thread that needs the user. `badge` is how many threads
/// wait for an approval.
pub fn payload(environment_id: &str, thread_id: &str, title: &str, body: &str, badge: usize) -> Value {
    json!({
        "aps": { "alert": { "title": clip_title(title), "body": body }, "sound": "default", "thread-id": collapse(thread_id), "badge": badge },
        "environmentId": environment_id,
        "threadId": thread_id,
    })
}

// ---- Live Activities ----

/// A Live Activity the app started for a thread, and where its updates go.
#[derive(Clone, Debug, PartialEq)]
pub struct ActivityToken {
    pub token: String,
    pub sandbox: bool,
}

/// Activity push tokens by thread. Kept in memory: an app re-registers its
/// activities when it comes back.
static ACTIVITIES: std::sync::LazyLock<Mutex<HashMap<String, Vec<ActivityToken>>>> = std::sync::LazyLock::new(Default::default);

pub fn register_activity(thread_id: &str, token: &str, sandbox: bool) {
    let mut all = locked(&ACTIVITIES);
    let list = all.entry(thread_id.to_owned()).or_default();
    list.retain(|t| t.token != token);
    list.push(ActivityToken { token: token.to_owned(), sandbox });
}

pub fn unregister_activity(token: &str) {
    let mut all = locked(&ACTIVITIES);
    for list in all.values_mut() {
        list.retain(|t| t.token != token);
    }
    all.retain(|_, l| !l.is_empty());
}

fn activity_tokens(thread_id: &str) -> Vec<ActivityToken> {
    locked(&ACTIVITIES).get(thread_id).cloned().unwrap_or_default()
}

/// How long a finished activity stays on the Lock Screen.
const ACTIVITY_LINGER_S: i64 = 15 * 60;

/// A Live Activity update for a thread's status (the app's
/// `AgentActivityAttributes.ContentState`); an activity ends with the
/// thread's turn.
pub fn activity_payload(status: &str, detail: &str, started_ms: i64, now_ms: i64) -> Value {
    let ended = !matches!(status, "working" | "approval");
    let mut aps = json!({
        "timestamp": now_ms / 1000,
        "event": if ended { "end" } else { "update" },
        "content-state": { "status": status, "detail": detail, "startedAtMs": started_ms as f64 },
    });
    if ended {
        aps["dismissal-date"] = json!(now_ms / 1000 + ACTIVITY_LINGER_S);
    }
    if status == "approval" {
        aps["alert"] = json!({ "title": "Needs your approval", "body": detail });
    }
    json!({ "aps": aps })
}

/// The most of a request's prompt an approval alert shows.
const PROMPT_CHARS: usize = 160;

/// An approval the user can answer from the alert (Allow / Decline in the
/// app's `APPROVAL` category): its request id rides along, and the body says
/// what is asked unless alerts are private.
pub fn approval_payload(mut base: Value, request_id: &str, prompt: Option<&str>) -> Value {
    base["aps"]["category"] = json!("APPROVAL");
    base["requestId"] = json!(request_id);
    if let Some(prompt) = prompt.map(str::trim).filter(|p| !p.is_empty()) {
        let short: String = prompt.chars().take(PROMPT_CHARS).collect();
        let short = if short.len() < prompt.len() { format!("{}…", short.trim_end()) } else { short };
        let body = base["aps"]["alert"]["body"].as_str().unwrap_or_default().to_owned();
        base["aps"]["alert"]["body"] = json!(format!("{body}: {short}"));
    }
    base
}

/// A question with one free-text answer, which the alert can take as a
/// typed reply (the app's `QUESTION` category): request and question ids
/// ride along, and the body is the question unless alerts are private.
pub fn question_payload(mut base: Value, request_id: &str, question_id: &str, question: Option<&str>) -> Value {
    base["aps"]["category"] = json!("QUESTION");
    base["requestId"] = json!(request_id);
    base["questionId"] = json!(question_id);
    if let Some(question) = question.map(str::trim).filter(|q| !q.is_empty()) {
        let short: String = question.chars().take(PROMPT_CHARS).collect();
        let short = if short.len() < question.len() { format!("{}…", short.trim_end()) } else { short };
        base["aps"]["alert"]["body"] = json!(short);
    }
    base
}

/// Watches thread events and pushes status changes to closed apps.
pub fn spawn(state: SharedState, apns: Apns) {
    tokio::spawn(async move {
        let apns = Arc::new(apns);
        let sends = Arc::new(tokio::sync::Semaphore::new(SENDS_AT_ONCE));
        let mut events = state.store.subscribe();
        let mut statuses = {
            let state = state.clone();
            tokio::task::spawn_blocking(move || seed(&state)).await.unwrap_or_default()
        };
        let mut dirty: HashSet<String> = HashSet::new();
        // When the current burst of events is looked at; `None` when idle.
        let mut due: Option<tokio::time::Instant> = None;
        loop {
            let wait = async {
                match due {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                () = state.shutdown.cancelled() => return,
                event = events.recv() => match event {
                    Ok(e) if e.aggregate_kind == "thread" => {
                        dirty.insert(e.aggregate_id.clone());
                        due.get_or_insert_with(|| tokio::time::Instant::now() + DEBOUNCE);
                    }
                    Ok(_) => {}
                    // Events were missed: look at every thread, and announce
                    // what changed meanwhile like any other change.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        dirty.extend(statuses.keys().cloned());
                        dirty.extend(thread_ids(&state));
                        due.get_or_insert_with(|| tokio::time::Instant::now() + DEBOUNCE);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                () = wait => {
                    due = None;
                    for thread_id in std::mem::take(&mut dirty) {
                        check(&state, &apns, &sends, &mut statuses, &thread_id);
                    }
                }
            }
        }
    });
}

/// Pushes a status change to the thread's Live Activities, ending them when
/// its turn is over.
fn update_activities(state: &SharedState, apns: &Arc<Apns>, shell: &ThreadShell, thread_id: &str, status: ShellStatus) {
    let tokens = activity_tokens(thread_id);
    if tokens.is_empty() {
        return;
    }
    let detail = match status {
        ShellStatus::Approval if !apns.private_alerts => {
            shell.pending_runtime_request.as_ref().and_then(|r| request_prompt(state, thread_id, r.id.as_str())).unwrap_or_default()
        }
        ShellStatus::Approval => "Needs your approval".into(),
        ShellStatus::Failed => "Failed".into(),
        ShellStatus::Working => String::new(),
        _ => "Finished".into(),
    };
    let started = shell.activity_run_started_at.as_ref().and_then(|at| at.as_ref());
    let started = started.and_then(|at| domovoi_contracts::time_fmt::millis_from_iso(at.as_str())).unwrap_or_else(now_millis);
    let payload = activity_payload(status.as_str(), &detail, started, now_millis());
    if payload["aps"]["event"] == "end" {
        for t in &tokens {
            unregister_activity(&t.token);
        }
    }
    for t in tokens {
        let (apns, payload, thread_id) = (apns.clone(), payload.clone(), thread_id.to_owned());
        tokio::spawn(async move {
            match apns.send_live(&t.token, t.sandbox, &payload).await {
                Ok(()) => tracing::info!(thread = %thread_id, "live activity updated"),
                Err(e) => {
                    tracing::warn!(thread = %thread_id, "live activity update failed: {e}");
                    unregister_activity(&t.token);
                }
            }
        });
    }
}

/// What an approval asks ("cargo test"), from its turn item.
fn request_prompt(state: &SharedState, thread_id: &str, request_id: &str) -> Option<String> {
    match &*request_item(state, thread_id, request_id)? {
        TurnItem::ApprovalRequest(asked) => asked.prompt.clone(),
        _ => None,
    }
}

/// The turn item that shows the request `request_id`: an approval, or questions.
fn request_item(state: &SharedState, thread_id: &str, request_id: &str) -> Option<Arc<TurnItem>> {
    let items = state.store.records::<TurnItem>(thread_id).ok()?;
    items.into_iter().find(|item| match &**item {
        TurnItem::ApprovalRequest(asked) => asked.request_id.as_str() == request_id,
        TurnItem::UserInputRequest(asked) => asked.request_id.as_str() == request_id,
        _ => false,
    })
}

/// A pending question answered by typing: exactly one question, without
/// options. Its id and text.
fn free_text_question(state: &SharedState, thread_id: &str, request_id: &str) -> Option<(String, String)> {
    match &*request_item(state, thread_id, request_id)? {
        TurnItem::UserInputRequest(asked) => only_free_text(&asked.questions),
        _ => None,
    }
}

fn only_free_text(questions: &[UserInputQuestion]) -> Option<(String, String)> {
    let [q] = questions else { return None };
    if !q.options.is_empty() {
        return None;
    }
    Some((q.id.clone(), q.question.clone()))
}

fn thread_ids(state: &SharedState) -> Vec<String> {
    state.store.thread_ids().unwrap_or_default().into_iter().map(|(id, _)| id).collect()
}

fn seed(state: &SharedState) -> HashMap<String, ShellStatus> {
    let mut out = HashMap::new();
    for id in thread_ids(state) {
        if let Ok(Some(shell)) = crate::orchestration::projection::thread_shell(&state.store, &id) {
            out.insert(id, shell_status(&shell));
        }
    }
    out
}

/// Compares a thread's status with the last one seen and pushes the change,
/// if it deserves one, to every device whose app is closed. Sending happens
/// on its own tasks, so a slow APNs never holds up the event stream.
fn check(state: &SharedState, apns: &Arc<Apns>, sends: &Arc<tokio::sync::Semaphore>, statuses: &mut HashMap<String, ShellStatus>, thread_id: &str) {
    let Ok(Some(shell)) = crate::orchestration::projection::thread_shell(&state.store, thread_id) else { return };
    let status = shell_status(&shell);
    let previous = statuses.insert(thread_id.to_owned(), status);
    // A thread created after startup starts as `idle`, so its first run counts.
    let previous = previous.unwrap_or(ShellStatus::Idle);
    if previous != status {
        update_activities(state, apns, &shell, thread_id, status);
    }
    let Some(body) = notice(Some(previous), status) else { return };
    let title = if apns.private_alerts { state.environment.label.clone() } else { shell.title.clone() };
    let waiting = statuses.values().filter(|s| **s == ShellStatus::Approval).count();
    let mut alert = payload(&state.environment.environment_id, thread_id, &title, body, waiting);
    // An approval (not a question, which needs the app) can be answered from the alert.
    let pending = shell.pending_runtime_request.as_ref().filter(|_| status == ShellStatus::Approval);
    if let Some(pending) = pending.filter(|p| p.kind != RuntimeRequestKind::UserInput) {
        let request_id = pending.id.as_str();
        let prompt = (!apns.private_alerts).then(|| request_prompt(state, thread_id, request_id)).flatten();
        alert = approval_payload(alert, request_id, prompt.as_deref());
    } else if let Some(pending) = pending
        && let Some((question_id, question)) = free_text_question(state, thread_id, pending.id.as_str())
    {
        // One question with a typed answer: replied to from the alert.
        let shown = (!apns.private_alerts).then_some(question.as_str());
        alert = question_payload(alert, pending.id.as_str(), &question_id, shown);
    }
    let payload = Arc::new(alert);
    let db = state.store.db();
    for device in devices(&db).unwrap_or_default() {
        // An open app (any of the device's sessions has a socket) notifies itself.
        if device.sessions.iter().any(|s| state.connections.is_live(s)) {
            continue;
        }
        let (apns, sends, payload, db, thread_id) = (apns.clone(), sends.clone(), payload.clone(), db.clone(), thread_id.to_owned());
        tokio::spawn(async move {
            let Ok(_permit) = sends.acquire().await else { return };
            match apns.deliver(&device.token, device.sandbox, &payload, &thread_id).await {
                Delivery::Sent => tracing::info!(thread = %thread_id, "push sent: {body}"),
                Delivery::SentOnOtherHost => {
                    tracing::info!(thread = %thread_id, "push sent: {body} (the device was registered for the other APNs host)");
                    let _ = set_environment(&db, &device.token, !device.sandbox);
                }
                Delivery::Gone(reason) => {
                    tracing::info!("push token dropped ({reason})");
                    let _ = forget_token(&db, &device.token);
                }
                Delivery::Failed(e) => tracing::warn!("push failed: {e}"),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A P-256 PKCS#8 key generated for tests only.
    fn test_key() -> String {
        let rng = ring::rand::SystemRandom::new();
        let doc = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(doc.as_ref());
        format!("-----BEGIN PRIVATE KEY-----\n{b64}\n-----END PRIVATE KEY-----\n")
    }

    #[test]
    fn provider_tokens_are_es256_jwts_and_cached() {
        let apns = Apns::from_pem(&test_key(), "KEY123", "TEAM456", "com.crayen.domovoi").unwrap();
        let jwt = apns.jwt().unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: Value = serde_json::from_slice(&base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header, json!({ "alg": "ES256", "kid": "KEY123" }));
        let claims: Value = serde_json::from_slice(&base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["iss"], "TEAM456");
        assert_eq!(base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[2]).unwrap().len(), 64, "raw r||s signature");
        assert_eq!(apns.jwt().unwrap(), jwt, "reused until it ages");
        assert!(Apns::from_pem("not a key", "K", "T", "t").is_err());
    }

    /// The token's age is wall-clock time, so a night asleep (when a
    /// monotonic clock stands still) ages it like any other eight hours.
    #[test]
    fn provider_tokens_age_in_wall_clock_time() {
        let apns = Apns::from_pem(&test_key(), "KEY123", "TEAM456", "com.crayen.domovoi").unwrap();
        let t0 = 1_800_000_000_000;
        let first = apns.jwt_at(t0).unwrap();
        assert_eq!(apns.jwt_at(t0 + 39 * 60 * 1000).unwrap(), first);
        let after_sleep = apns.jwt_at(t0 + 8 * 60 * 60 * 1000).unwrap();
        assert_ne!(after_sleep, first, "re-signed after eight hours");
        // The clock was set back: the cached token's age is negative, so it is re-signed too.
        assert_ne!(apns.jwt_at(t0).unwrap(), after_sleep);
    }

    #[test]
    fn tokens_and_payloads() {
        assert!(valid_token(&"ab".repeat(32)));
        assert!(!valid_token("xyz"));
        let live = activity_payload("working", "", 1_000, 5_000);
        assert_eq!((live["aps"]["event"].as_str(), live["aps"]["content-state"]["status"].as_str()), (Some("update"), Some("working")));
        assert_eq!(live["aps"]["content-state"]["startedAtMs"], 1000.0);
        assert!(live["aps"]["dismissal-date"].is_null());
        let done = activity_payload("completed", "Finished", 1_000, 5_000);
        assert_eq!((done["aps"]["event"].as_str(), done["aps"]["dismissal-date"].as_i64()), (Some("end"), Some(5 + ACTIVITY_LINGER_S)));
        register_activity("tA", "tok1", true);
        register_activity("tA", "tok1", true);
        assert_eq!(activity_tokens("tA").len(), 1, "registering twice keeps one");
        unregister_activity("tok1");
        assert!(activity_tokens("tA").is_empty());
        let approval = approval_payload(payload("env", "t1", "Fix tests", "Needs your approval", 1), "rq", Some(&format!("cargo test {}", "x".repeat(300))));
        assert_eq!(approval["aps"]["category"], "APPROVAL");
        assert_eq!(approval["requestId"], "rq");
        let body = approval["aps"]["alert"]["body"].as_str().unwrap();
        assert!(body.starts_with("Needs your approval: cargo test") && body.ends_with('…') && body.chars().count() < 200, "{body}");
        assert_eq!(approval_payload(payload("env", "t1", "T", "Needs your approval", 1), "rq", None)["aps"]["alert"]["body"], "Needs your approval");
    }

    #[test]
    fn a_free_text_question_can_be_answered_from_the_alert() {
        let q = question_payload(payload("env", "t1", "Fix tests", "Needs your answer", 1), "rq", "name", Some("What should the branch be called?"));
        assert_eq!(q["aps"]["category"], "QUESTION");
        assert_eq!((q["requestId"].as_str(), q["questionId"].as_str()), (Some("rq"), Some("name")));
        assert_eq!(q["aps"]["alert"]["body"], "What should the branch be called?");
        let private = question_payload(payload("env", "t1", "Env", "Needs your answer", 1), "rq", "name", None);
        assert_eq!(private["aps"]["alert"]["body"], "Needs your answer", "private alerts keep the question to themselves");

        let questions = |asked: Value| -> Vec<UserInputQuestion> { serde_json::from_value(asked).unwrap() };
        let question = |id: &str, text: &str, options: Value| json!({ "id": id, "header": "Question", "question": text, "options": options });
        assert_eq!(only_free_text(&questions(json!([question("name", "Name?", json!([]))]))), Some(("name".into(), "Name?".into())));
        let pick = question("pick", "Pick", json!([{ "label": "A", "description": "A" }]));
        assert_eq!(only_free_text(&questions(json!([pick]))), None, "options need the app");
        let two = json!([question("a", "A?", json!([])), question("b", "B?", json!([]))]);
        assert_eq!(only_free_text(&questions(two)), None, "several questions need the app");
        let p = payload("env", "t1", "Fix tests", "Finished", 2);
        assert_eq!(p["aps"]["alert"]["body"], "Finished");
        assert_eq!(p["aps"]["thread-id"], "t1");
        assert_eq!(p["aps"]["badge"], 2);
        assert_eq!(p["threadId"], "t1");
    }

    #[test]
    fn long_and_non_ascii_text_is_cut_on_character_boundaries() {
        // 40 two-byte characters: a byte slice at 64 would split one and panic.
        let id = format!("a{}", "é".repeat(40));
        let stand_in = collapse(&id);
        assert!(stand_in.len() == 32 && stand_in.is_ascii(), "{stand_in}");
        assert_eq!(collapse(&id), stand_in, "stable for one id");
        assert_eq!(collapse("short"), "short");
        assert_eq!(collapse("żółw").len(), 32, "a header cannot carry non-ASCII text");
        let title = "ż".repeat(500);
        let p = payload("env", &id, &title, "Failed", 0);
        assert_eq!(p["aps"]["alert"]["title"].as_str().unwrap().chars().count(), TITLE_CHARS);
        assert!(p["aps"]["alert"]["title"].as_str().unwrap().ends_with('…'));
        assert_eq!(p["threadId"], id, "the app still gets the whole id");
        assert!(p.to_string().len() < 4096, "inside APNs' payload limit");
    }

    #[test]
    fn devices_belong_to_the_sessions_that_registered_them() {
        let db = Db::open_in_memory().unwrap();
        let session = |hash: &str| {
            db.with(|c| {
                c.execute(
                    "INSERT INTO auth_sessions (token_hash, subject, scopes, permissions, created_at, expires_at, expires_at_ms) VALUES (?1, 's', '[]', '[]', '', '', ?2)",
                    params![hash, now_millis() + 60_000],
                )
            })
            .unwrap();
        };
        session("a");
        session("b");
        let token = "ab".repeat(32);
        register(&db, &token, "sandbox", "a").unwrap();
        // The same phone paired again: both sessions hold the token, one device.
        register(&db, &token, "sandbox", "b").unwrap();
        assert_eq!(devices(&db).unwrap(), vec![PushDevice { token: token.clone(), sessions: vec!["a".into(), "b".into()], sandbox: true }]);
        // Another session cannot remove a registration that is not its own.
        unregister(&db, &token, "nobody").unwrap();
        assert_eq!(device_count(&db), 1);
        unregister(&db, &token, "a").unwrap();
        assert_eq!(devices(&db).unwrap()[0].sessions, vec!["b".to_owned()]);
        // Signing "b" out drops its device; nothing is left behind.
        forget_sessions(&db, &["b".into()]).unwrap();
        assert_eq!(device_count(&db), 0);
        register(&db, &token, "production", "gone").unwrap();
        assert_eq!(forget_orphans(&db).unwrap(), 1);
    }
}
