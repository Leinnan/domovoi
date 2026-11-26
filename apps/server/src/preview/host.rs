//! What the rest of the server asks of the headless browser: the RPC
//! handlers (`preview.open`, `reportProfiles`, `clearProfile`), the preview
//! stream, the agent tools (t3code's `PreviewAutomationBroker` with the
//! server host as its only host) and `html_preview` (t3code `HtmlRender.preview`).
//!
//! With the `preview-browser` feature each call goes to `super::browser`;
//! without it the browser is unavailable and every call says so.

#![expect(clippy::disallowed_types, reason = "agent tool inputs and results pass through as JSON")]

use crate::state::SharedState;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
#[cfg(not(feature = "preview-browser"))]
use tokio::sync::mpsc;

/// A browser profile as clients report it (`BrowserProfile`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrowserProfile {
    pub id: String,
    pub name: String,
    /// `persistent` or `incognito`.
    pub kind: String,
}

/// `PreviewReportProfilesInput`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilesReport {
    #[serde(default)]
    pub profiles: Vec<BrowserProfile>,
    pub default_profile_id: String,
}

/// A failed agent operation, as t3code's `ServerBrowserOperationError`: the
/// `PreviewAutomationError` member it stands for, and its message.
#[derive(Debug, Clone, PartialEq)]
pub struct OpError {
    pub tag: &'static str,
    pub message: String,
}

impl OpError {
    pub fn new(tag: &'static str, message: impl Into<String>) -> Self {
        OpError { tag, message: message.into() }
    }
    pub fn execution(message: impl Into<String>) -> Self {
        OpError::new("PreviewAutomationExecutionError", message)
    }
    pub fn timeout(message: impl Into<String>) -> Self {
        OpError::new("PreviewAutomationTimeoutError", message)
    }
    /// `PreviewAutomationControlInterruptedError` with its reason's message.
    pub fn control(reason: &str) -> Self {
        let message = match reason {
            "agentMismatch" => "This browser tab belongs to another agent session or a human. Open your own tab.",
            "unclaimedTab" => "This tab has no agent owner. Ask the user to close it in the browser panel.",
            "humanControl" => "A human controls this browser tab. Wait until they release control.",
            "tabRequired" => "Multiple browser tabs belong to this session. Pass a tabId from preview_open or from the tabs listed by preview_status.",
            "dialogPending" => "A browser dialog is pending. Read preview_status and resolve it with preview_dialog.",
            "tabLimit" => "Too many server browser tabs are open. Close one with preview_close, or reuse a tabId from preview_status tabs.",
            "closed" => "This browser tab is closed. Call preview_open.",
            _ => "Browser control changed. Take a fresh snapshot before trying again.",
        };
        OpError::new("PreviewAutomationControlInterruptedError", message)
    }
    pub fn unavailable(cause: &super::chrome::LaunchError) -> Self {
        OpError::new("PreviewAutomationNoAvailableHostError", cause.to_string())
    }
}

/// One agent operation (`PreviewAutomationRequest`).
#[derive(Debug, Clone)]
pub struct Request {
    pub thread_id: String,
    /// The calling agent session (the broker's `agentSessionId`).
    pub agent: String,
    pub tab_id: Option<String>,
    /// `PreviewAutomationOperation`.
    pub operation: &'static str,
    pub input: Value,
    pub timeout: Duration,
}

/// What a viewer is sent.
pub enum Output {
    /// A JPEG; `ack` lets the browser send the next one.
    Frame { data: Vec<u8>, ack: Option<Ack> },
    /// A JSON message (`viewport`, `control`, `probe`).
    Text(String),
    /// The tab closed (close code 4404).
    Gone,
}

/// Lets the browser send the next screencast frame.
pub struct Ack(pub BoxFuture<'static, ()>);

impl Ack {
    pub async fn send(self) {
        self.0.await;
    }
}

/// Which tab a viewer streams, and how.
#[derive(Debug, Clone)]
pub struct AttachInput {
    pub thread_id: String,
    pub tab_id: String,
    pub max_width: u32,
    pub max_height: u32,
    pub quality: u32,
    pub can_operate: bool,
}

/// Why a viewer could not attach.
#[derive(Debug)]
pub enum AttachError {
    /// No such server tab (close code 4404).
    Gone,
    /// The browser could not start (HTTP 503).
    Launch(String),
}

/// What `html_preview` captured (t3code `HtmlPreview`).
#[derive(Debug, Clone)]
pub struct HtmlShot {
    /// Base64 PNG.
    pub png: String,
    pub content_height: u64,
    pub captured_height: u64,
    /// `(level, text)`.
    pub console: Vec<(String, String)>,
}

#[cfg(feature = "preview-browser")]
pub use super::browser::Viewer;

/// A viewer of a build without the browser; never made.
#[cfg(not(feature = "preview-browser"))]
pub struct Viewer {
    pub output: mpsc::Receiver<Output>,
}

#[cfg(not(feature = "preview-browser"))]
impl Viewer {
    pub fn take_output(&mut self) -> mpsc::Receiver<Output> {
        std::mem::replace(&mut self.output, mpsc::channel(1).1)
    }
    pub async fn input(&self, _message: Value) {}
    pub async fn close(&self) {}
}

/// Starts the browser service, so it mirrors the tabs the manager announces from now on.
pub fn start(state: &SharedState) {
    #[cfg(feature = "preview-browser")]
    super::browser::service(state);
    #[cfg(not(feature = "preview-browser"))]
    let _ = state;
}

pub fn report_profiles(state: &SharedState, report: ProfilesReport) {
    #[cfg(feature = "preview-browser")]
    super::browser::service(state).report_profiles(report);
    #[cfg(not(feature = "preview-browser"))]
    let _ = (state, report);
}

/// `clearProfile`: closes the profile's browser and deletes its data (the
/// folder is deleted even without the browser feature).
pub async fn clear_profile(state: &SharedState, profile_id: &str) -> Result<(), String> {
    #[cfg(feature = "preview-browser")]
    return super::browser::service(state).clear_profile(profile_id).await;
    #[cfg(not(feature = "preview-browser"))]
    {
        if profile_id == "incognito" {
            return Ok(());
        }
        match std::fs::remove_dir_all(super::chrome::profile_dir(&state.paths.userdata, profile_id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        }
    }
}

/// Runs one agent operation on the server's browser.
pub async fn invoke(state: &SharedState, request: Request) -> Result<Value, OpError> {
    #[cfg(feature = "preview-browser")]
    return super::browser::service(state).invoke(request).await;
    #[cfg(not(feature = "preview-browser"))]
    {
        let _ = (state, request);
        Err(OpError::unavailable(&super::chrome::LaunchError::NotBuilt))
    }
}

/// Attaches a viewer to a server tab.
pub async fn attach(state: &SharedState, input: AttachInput) -> Result<Viewer, AttachError> {
    #[cfg(feature = "preview-browser")]
    return super::browser::service(state).attach(input).await;
    #[cfg(not(feature = "preview-browser"))]
    {
        let _ = (state, input, mpsc::channel::<Output>(1));
        Err(AttachError::Launch(super::chrome::LaunchError::NotBuilt.to_string()))
    }
}

/// Loads `html` at `width` CSS pixels, dark or light, and captures it.
pub async fn html_preview(state: &SharedState, html: String, width: u32, dark: bool) -> Result<HtmlShot, String> {
    #[cfg(feature = "preview-browser")]
    return super::browser::service(state).html_preview(html, width, dark).await;
    #[cfg(not(feature = "preview-browser"))]
    {
        let _ = (state, html, width, dark);
        Err(super::chrome::LaunchError::NotBuilt.to_string())
    }
}
