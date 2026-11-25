//! Browser tools (t3code `mcp/toolkits/preview` and `previewControls`): the
//! agent shares the server's headless browser with the user
//! (`crate::preview::host`), opening, reading and driving server tabs of its
//! own thread. `preview_snapshot` results are bounded as t3code's
//! `registerPreviewSnapshot` bounds them (20 KB of text; logs shed first),
//! and a requested screenshot comes as image content or a PNG saved under
//! `<userdata>/browser-artifacts/`.
//!
//! The tools need "Agent browser access" (`enableAgentBrowserAccess`, per
//! project overridable); without it they answer t3code's
//! `PreviewAutomationUnavailableError` message. The calling agent is named
//! by its thread (the broker's agent session).
//!
//! Limits: no `preview_recording_start` / `preview_recording_stop` (the
//! browser does not record); argument checks are this module's, not
//! t3code's schemas; a failure is the tool's text (its t3code tag and
//! message), not structured content.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the MCP wire: tool schemas, tool arguments and tool results")]

use super::{Call, FULL_RESULT, b, n, one_of, s, tool};
use crate::preview::host::{self, OpError, Request};
use crate::rpc::preview::{CloseInput, Runtime, manager};
use base64::Engine;
use serde_json::{Map, Value, json};
use std::time::Duration;

const MAX_SNAPSHOT_TEXT_BYTES: usize = 20_000;
const MAX_SNAPSHOT_VISIBLE_TEXT_CHARS: usize = 8_000;
const SNAPSHOT_TEXT_FLOOR_CHARS: usize = 2_000;
const MAX_SNAPSHOT_ELEMENT_NAME_CHARS: usize = 200;
const MAX_SNAPSHOT_LOG_ENTRIES: usize = 40;
const MAX_SNAPSHOT_LOG_TEXT_CHARS: usize = 500;
const MAX_SNAPSHOT_IDENTIFIER_CHARS: usize = 2_048;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
/// The first open may start the browser.
const OPEN_TIMEOUT: Duration = Duration::from_secs(60);
const GRACE: Duration = Duration::from_secs(1);

fn tab_id() -> Value {
    s("Server tab from preview_open or preview_status tabs. Omit for your current tab.")
}

fn timeout_ms() -> Value {
    n("Default 15000, at most 60000.")
}

fn target_schema() -> Value {
    json!({
        "type": "object",
        "description": "A dev server here: {kind:'environment-port', port, protocol?, path?}, or {kind:'url', url}.",
        "properties": {
            "kind": one_of(&["environment-port", "url"], "What the target is."),
            "port": n("Dev-server port here."),
            "protocol": one_of(&["http", "https"], "Default http."),
            "path": s("Path, query, and fragment, such as /settings?tab=account."),
            "url": s("Website URL, for kind 'url'."),
        },
        "required": ["kind"],
    })
}

fn strings(d: &str) -> Value {
    json!({ "type": "array", "items": { "type": "string" }, "description": d })
}

/// A browser tool's description: t3code's hints, `open_world` and `destructive` apart from `read_only`.
fn browser_tool(name: &str, description: &str, properties: Value, required: &[&str], read_only: bool, destructive: bool) -> Value {
    let mut t = tool(name, description, properties, required, read_only);
    t["annotations"]["destructiveHint"] = json!(destructive);
    t["annotations"]["openWorldHint"] = json!(true);
    t
}

pub fn tools() -> Vec<Value> {
    const URL: &str = "Absolute http(s) URL, or a bare host (https; http for loopback).";
    vec![
        browser_tool("preview_status",
            "Report preview tab state: owner, pending dialog, URL, title, loading, viewport, and size. `tabs` lists this thread's tabs. `profiles` lists logins for `preview_open`. Read any listed tab. Act only on your tab, or an unclaimed tab with no human in control.",
            json!({ "tabId": tab_id() }), &[], true, false),
        browser_tool("preview_open",
            "Open a preview tab in this thread and show it. For a dev server, pass `target` instead of `url`. `open=false` keeps it in the background. Reuses your current tab unless `tabId` or `reuseExistingTab=false` says otherwise. Parallel subagents in one session must each use `reuseExistingTab=false`, then their own returned `tabId`.",
            json!({
                "tabId": tab_id(),
                "url": s(&format!("First page URL. {URL} Omit to open a blank tab.")),
                "target": target_schema(),
                "open": b("Show the preview to the user. Default true. False for background only."),
                "show": b("Old alias for `open`."),
                "reuseExistingTab": b("Reuse `tabId`, or your current tab. Default true. False makes a new tab."),
                "profileId": s("Profile id or name from preview_status, for a new tab. Default: the user's. A tab keeps its profile."),
            }), &[], false, false),
        browser_tool("preview_dialog",
            "Accept or dismiss the dialog `preview_status` reports. Pass `promptText` to accept a prompt. Needs your own tab, or an unclaimed one.",
            json!({ "tabId": tab_id(), "accept": b("Accept (true) or dismiss (false)."), "promptText": s("Text for a prompt dialog.") }), &["accept"], false, true),
        browser_tool("preview_navigate",
            "Navigate the chosen tab. Pass `url` for a website, or `target:{kind:'environment-port',port}` for a dev server. Exactly one of `url` and `target`.",
            json!({
                "tabId": tab_id(),
                "url": s(&format!("Website URL. {URL} For public pages.")),
                "target": target_schema(),
                "readiness": one_of(&["load", "domContentLoaded", "none"], "Default 'load'. 'none': return at once."),
                "timeoutMs": timeout_ms(),
            }), &[], false, false),
        browser_tool("preview_resize",
            "Resize the chosen tab: {mode:'fill'}, {mode:'freeform',width,height}, or {mode:'preset',preset,orientation}. Changes CSS size only, not the user agent.",
            json!({
                "tabId": tab_id(),
                "mode": one_of(&["fill", "freeform", "preset"], "How the size is chosen."),
                "preset": s("A device preset id, such as iphone-se, pixel-7 or ipad-air."),
                "width": n("CSS pixels, 240 to 3840."),
                "height": n("CSS pixels, 240 to 3840."),
                "orientation": one_of(&["portrait", "landscape"], "For a preset."),
                "timeoutMs": timeout_ms(),
            }), &["mode"], false, false),
        browser_tool("preview_set_appearance",
            "Set `prefers-color-scheme` for the chosen tab: 'dark', 'light', or 'system' to clear the override.",
            json!({ "tabId": tab_id(), "colorScheme": one_of(&["system", "light", "dark"], "The scheme.") }), &["colorScheme"], false, false),
        browser_tool("preview_snapshot",
            "Use it only when the current request asks for browser work, including checking an edit in the browser. Never use it unasked to verify your own edit, or to repeat a snapshot from an earlier turn. Works on a tab the user is driving. Returns page state, elements, diagnostics (console and network entries), and screenshot size. Target one element with `locator='aria-ref=<ref>'` from `accessibilityTree`. Refs die on navigation, a new snapshot, or handoff. Text caps near 20 KB; on your own or an unclaimed tab, `preview_evaluate` reads more. To show a screenshot when asked: `save=true`, then embed ![alt](screenshotPath).",
            json!({ "tabId": tab_id(), "includeImage": b("Default false. True only to see the page."), "save": b("Write PNG, return `screenshotPath`.") }), &[], false, false),
        browser_tool("preview_act",
            "Act on the chosen tab. Target with `locator='aria-ref=<ref>'` from `preview_snapshot`; `x` and `y` go together. `timeoutMs` defaults to 15000. click: `button`, `clickCount` 1 to 3. type: literal `text`, `clear=true` replaces it. hover: shows menus or tooltips. select: `values` in one native <select>, by value or label; for a custom dropdown, click it open, then click the option. drag: `source` onto `target`. upload: absolute `paths` set on an <input type=file> `locator`. press: one `key`, such as 'Enter', with `modifiers`, to the focused element; click a field first to focus it. scroll: the tab, or one container with a locator; positive `deltaY` scrolls down, positive `deltaX` right.",
            json!({
                "action": one_of(&["click", "type", "hover", "select", "drag", "upload", "press", "scroll"], "The action."),
                "tabId": tab_id(),
                "locator": s("aria-ref=<ref> from the latest snapshot, role=<role>[name=\"...\"], text=<text>, or CSS."),
                "selector": s("CSS selector (legacy; prefer locator)."),
                "x": n("CSS pixels from the left of the viewport."),
                "y": n("CSS pixels from the top of the viewport."),
                "button": one_of(&["left", "right", "middle"], "Button. Default left. Right opens the menu."),
                "clickCount": n("1 click, 2 double, 3 triple. Default 1."),
                "text": s("Text to type."),
                "clear": b("Clear the input first. Default false."),
                "values": strings("Option values or labels to select. Empty clears it."),
                "source": s("Drag from this locator."),
                "target": s("Drop on this locator."),
                "paths": strings("Absolute file paths to upload."),
                "key": s("A key, such as Enter, Tab, ArrowDown or a."),
                "modifiers": { "type": "array", "items": { "type": "string", "enum": ["Alt", "Control", "Meta", "Shift"] }, "description": "Modifiers held for the key." },
                "deltaX": n("Pixels to scroll right."),
                "deltaY": n("Pixels to scroll down."),
                "timeoutMs": timeout_ms(),
            }), &["action"], false, true),
        browser_tool("preview_evaluate",
            "Run JavaScript in the page. It can change page state. Returns `{value}`, up to 64 KB. Needs your tab, or an unclaimed one. To read a tab the user drives, use `preview_snapshot`. Page tools write only on user request. Show results with `html_render`, never page tools.",
            json!({ "tabId": tab_id(), "expression": s("A JavaScript expression; top-level await works."), "awaitPromise": b("Default true.") }), &["expression"], false, true),
        browser_tool("preview_wait_for",
            "Wait until all given locator, selector, text, and URL conditions match in the chosen tab.",
            json!({
                "tabId": tab_id(),
                "locator": s("An element to wait for."),
                "selector": s("CSS selector (legacy; prefer locator)."),
                "text": s("Page text to wait for. Case counts."),
                "urlIncludes": s("A URL part to wait for."),
                "timeoutMs": timeout_ms(),
            }), &[], true, false),
        browser_tool("t3_preview_list",
            "List this thread's preview tabs. Only `runtime:'server'` entries can be automated here. Others are in the user's local desktop app. Never pass a non-server `tabId` to a `preview_*` tool. Tell the user that tab is on their local desktop and this environment cannot reach it.",
            json!({ "cursor": n("Index to start at (nextCursor of the last page)."), "limit": n("At most this many (1 to 50, default 20).") }), &[], true, false),
        browser_tool("t3_preview_close",
            "Close one preview tab this thread owns. Use it on a tab you opened for your own check when you finish.",
            json!({ "tabId": s("The tab to close.") }), &["tabId"], false, true),
    ]
}

pub async fn call(c: &Call<'_>, name: &str) -> Option<Result<Value, String>> {
    if !(name.starts_with("preview_") || name.starts_with("t3_preview_")) {
        return None;
    }
    if !tools().iter().any(|t| t["name"] == name) {
        return None;
    }
    if let Err(e) = browser_access(c).await {
        return Some(Err(e));
    }
    Some(match name {
        "t3_preview_list" => Ok(list(c)),
        "t3_preview_close" => close(c).await,
        "preview_snapshot" => snapshot(c).await,
        "preview_open" => open(c).await,
        "preview_evaluate" => invoke(c, "evaluate", without_tab(c.args), DEFAULT_TIMEOUT).await.map(|value| json!({ "value": value })),
        "preview_act" => act(c).await,
        other => {
            let (operation, timeout) = match other {
                "preview_status" => ("status", DEFAULT_TIMEOUT),
                "preview_dialog" => ("dialog", DEFAULT_TIMEOUT),
                "preview_navigate" => ("navigate", input_timeout(c.args)),
                "preview_resize" => ("resize", input_timeout(c.args)),
                "preview_set_appearance" => ("setColorScheme", DEFAULT_TIMEOUT),
                _ => ("waitFor", input_timeout(c.args)),
            };
            if let Err(e) = check(operation, c.args) {
                return Some(Err(e));
            }
            invoke(c, operation, without_tab(c.args), timeout).await
        }
    })
}

/// t3code `requireThreadMcpCapability("preview")`: "Agent browser access", the project's override first.
async fn browser_access(c: &Call<'_>) -> Result<(), String> {
    let settings = c.state.settings.read().await;
    let project = settings.project_settings_overrides.as_ref().and_then(|p| p.get(c.caller.project_id.as_str())).and_then(|p| p.enable_agent_browser_access);
    if project.or(settings.enable_agent_browser_access) == Some(false) {
        return Err("PreviewAutomationUnavailableError: MCP credential does not grant the preview capability: browser preview tools are off for this thread. Do not retry them. To check a page, use a headless browser from the shell, such as Playwright, or curl. The user can turn on \"Agent browser access\" in Settings; it applies when the agent session next starts.".into());
    }
    Ok(())
}

fn input_timeout(args: &Value) -> Duration {
    args["timeoutMs"].as_u64().map(|ms| Duration::from_millis(ms.clamp(1, 60_000))).unwrap_or(DEFAULT_TIMEOUT)
}

fn without_tab(args: &Value) -> Value {
    let mut input = args.as_object().cloned().unwrap_or_default();
    input.remove("tabId");
    Value::Object(input)
}

fn failed(e: OpError) -> String {
    format!("{}: {}", e.tag, e.message)
}

fn invalid(message: &str) -> String {
    format!("invalid_request: {message}")
}

/// The checks t3code's input schemas make before an operation runs.
fn check(operation: &str, a: &Value) -> Result<(), String> {
    let has = |k: &str| !a[k].is_null();
    let one_target = || {
        let selectors = usize::from(has("selector")) + usize::from(has("locator"));
        if has("x") != has("y") {
            return Err(invalid("Coordinates require both x and y."));
        }
        if selectors + usize::from(has("x")) != 1 { Err(invalid("Provide exactly one target.")) } else { Ok(()) }
    };
    let at_most_one_selector = || if has("selector") && has("locator") { Err(invalid("Provide at most one of selector or locator.")) } else { Ok(()) };
    match operation {
        "navigate" if usize::from(has("url")) + usize::from(has("target")) != 1 => Err(invalid("Provide exactly one of url or target.")),
        "dialog" if !a["accept"].is_boolean() => Err(invalid("accept is required.")),
        "setColorScheme" if !matches!(a["colorScheme"].as_str(), Some("system" | "light" | "dark")) => Err(invalid("colorScheme is one of system, light or dark.")),
        "waitFor" => {
            at_most_one_selector()?;
            if ["selector", "locator", "text", "urlIncludes"].iter().any(|k| has(k)) { Ok(()) } else { Err(invalid("Provide at least one wait condition.")) }
        }
        "click" | "hover" => {
            one_target()?;
            match a["clickCount"].as_u64() {
                Some(n) if !(1..=3).contains(&n) => Err(invalid("clickCount is 1 to 3.")),
                _ => Ok(()),
            }
        }
        "type" if !a["text"].is_string() => Err(invalid("text is required.")),
        "type" | "upload" => at_most_one_selector(),
        "select" if !a["values"].is_array() => Err(invalid("values is required.")),
        "select" if usize::from(has("selector")) + usize::from(has("locator")) != 1 => Err(invalid("Provide exactly one of selector or locator.")),
        "drag" if !(a["source"].is_string() && a["target"].is_string()) => Err(invalid("drag needs source and target locators.")),
        "press" if a["key"].as_str().is_none_or(|k| k.trim().is_empty()) => Err(invalid("key is required.")),
        "scroll" => {
            at_most_one_selector()?;
            if has("deltaX") || has("deltaY") { Ok(()) } else { Err(invalid("Provide deltaX or deltaY.")) }
        }
        _ => Ok(()),
    }
}

fn request(c: &Call<'_>, operation: &'static str, input: Value, timeout: Duration) -> Request {
    Request { thread_id: c.caller.thread_id.clone(), agent: c.caller.thread_id.clone(), tab_id: c.str("tabId"), operation, input, timeout }
}

/// Runs an operation; an action's result gains the page's `toolIcon` (t3code `invoke`).
async fn invoke(c: &Call<'_>, operation: &'static str, input: Value, timeout: Duration) -> Result<Value, String> {
    let result = host::invoke(c.state, request(c, operation, input, timeout + GRACE)).await.map_err(failed)?;
    if ["status", "open", "navigate", "snapshot"].contains(&operation) {
        return Ok(result);
    }
    let tab = if operation == "evaluate" { None } else { result["tabId"].as_str().map(str::to_owned) };
    let mut status = request(c, "status", json!({}), Duration::from_millis(500));
    if tab.is_some() {
        status.tab_id = tab;
    }
    let page = host::invoke(c.state, status).await.ok();
    let mut out = if result.is_object() { result } else if operation == "evaluate" { return Ok(result) } else { json!({}) };
    if let Some(url) = page.as_ref().and_then(|p| p["url"].as_str()).filter(|u| (u.starts_with("http://") || u.starts_with("https://")) && u.len() <= 4096) {
        out["toolIcon"] = json!({ "_tag": "website", "pageUrl": url });
    }
    Ok(out)
}

async fn open(c: &Call<'_>) -> Result<Value, String> {
    let mut input = without_tab(c.args);
    if c.args["tabId"].is_string() && c.args["reuseExistingTab"] == false {
        return Err(invalid("tabId cannot be combined with reuseExistingTab=false."));
    }
    // Models often pass a dev server as both `url` and `target`; the target knows the environment.
    let target = input.as_object_mut().and_then(|m| m.remove("target")).filter(|t| !t.is_null());
    if target.is_some()
        && let Some(m) = input.as_object_mut()
    {
        m.remove("url");
    }
    let opened = invoke(c, "open", input, OPEN_TIMEOUT).await?;
    let (Some(target), Some(tab)) = (target, opened["tabId"].as_str()) else { return Ok(opened) };
    let mut navigate = request(c, "navigate", json!({ "target": target }), DEFAULT_TIMEOUT + GRACE);
    navigate.tab_id = Some(tab.to_owned());
    host::invoke(c.state, navigate).await.map_err(failed)
}

async fn act(c: &Call<'_>) -> Result<Value, String> {
    let action: &'static str = match c.args["action"].as_str() {
        Some("click") => "click",
        Some("type") => "type",
        Some("hover") => "hover",
        Some("select") => "select",
        Some("drag") => "drag",
        Some("upload") => "upload",
        Some("press") => "press",
        Some("scroll") => "scroll",
        _ => return Err(invalid("action is one of click, type, hover, select, drag, upload, press or scroll.")),
    };
    check(action, c.args)?;
    let mut input = without_tab(c.args);
    if let Some(m) = input.as_object_mut() {
        m.remove("action");
    }
    invoke(c, action, input, input_timeout(c.args)).await
}

fn list(c: &Call<'_>) -> Value {
    let all = manager().list(&c.state.server_epoch, &c.caller.thread_id);
    let start = c.args["cursor"].as_u64().unwrap_or(0) as usize;
    let end = start.saturating_add(c.limit(20, 50));
    let next = (end < all.sessions.len()).then_some(end);
    let mut out = serde_json::to_value(&all).unwrap_or(Value::Null);
    out["sessions"] = json!(all.sessions.iter().skip(start).take(end - start).collect::<Vec<_>>());
    out["nextCursor"] = json!(next);
    out
}

async fn close(c: &Call<'_>) -> Result<Value, String> {
    let tab = c.required("tabId")?;
    let server = manager().snapshot(&c.caller.thread_id, &tab).is_some_and(|s| s.runtime == Some(Runtime::Server));
    if server {
        host::invoke(c.state, request(c, "close", json!({}), DEFAULT_TIMEOUT)).await.map_err(failed)?;
    } else {
        manager().close(&c.state.server_epoch, CloseInput { thread_id: c.caller.thread_id.clone(), tab_id: Some(tab) });
    }
    Ok(json!({}))
}

// ---- preview_snapshot (t3code `registerPreviewSnapshot`) ----

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max { text.to_owned() } else { format!("{}…", text.chars().take(max).collect::<String>()) }
}

fn cut_entry(entry: &Value) -> Value {
    match entry {
        Value::Object(m) => Value::Object(m.iter().map(|(k, v)| (k.clone(), v.as_str().map_or_else(|| v.clone(), |s| json!(cut(s, MAX_SNAPSHOT_LOG_TEXT_CHARS))))).collect()),
        other => other.clone(),
    }
}

/// Where a saved screenshot goes: `browser-screenshot-<site>-<time>-<id>.png`.
fn screenshot_path(c: &Call<'_>, url: &str) -> std::path::PathBuf {
    let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)).unwrap_or_default();
    let mut slug = String::new();
    for ch in host.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(40).collect::<String>().trim_end_matches('-').to_owned();
    let slug = if slug.is_empty() { "site".to_owned() } else { slug };
    let millis = domovoi_contracts::time_fmt::now_millis().max(0) as u64;
    let mut base36 = String::new();
    let mut v = millis;
    while v > 0 {
        base36.insert(0, std::char::from_digit((v % 36) as u32, 36).unwrap_or('0'));
        v /= 36;
    }
    let unique: String = crate::util::new_uuid().chars().take(8).collect();
    c.state.paths.userdata.join("browser-artifacts").join(format!("browser-screenshot-{slug}-{base36}-{unique}.png"))
}

async fn snapshot(c: &Call<'_>) -> Result<Value, String> {
    let include_image = c.flag("includeImage");
    let save = c.flag("save");
    let mut input = json!({});
    if save {
        input["fullScreenshot"] = json!(true);
    }
    if !include_image && !save {
        input["pixelsRequired"] = json!(false);
    }
    let snap = invoke(c, "snapshot", input, DEFAULT_TIMEOUT).await.map_err(|e| format!("Preview snapshot failed: {e}"))?;
    let url = snap["url"].as_str().unwrap_or_default().to_owned();
    let png = if include_image || save {
        let data = snap["screenshot"]["data"].as_str().ok_or_else(|| invalid("Preview snapshot did not contain requested PNG pixels."))?;
        Some(data.to_owned())
    } else {
        None
    };
    let mut saved_path = None;
    if save {
        let full = snap["fullScreenshot"]["data"].as_str().or(png.as_deref()).unwrap_or_default();
        let bytes = base64::engine::general_purpose::STANDARD.decode(full).map_err(|e| e.to_string())?;
        let path = screenshot_path(c, &url);
        let written = path.parent().map(std::fs::create_dir_all).unwrap_or(Ok(())).and_then(|()| std::fs::write(&path, bytes));
        if written.is_err() {
            return Err(format!("PreviewScreenshotSaveError: Could not save preview screenshot to {}.", path.display()));
        }
        saved_path = Some(path.to_string_lossy().into_owned());
    }
    if let (Some(path), false) = (&saved_path, include_image) {
        let saved = json!({ "url": cut(&url, MAX_SNAPSHOT_IDENTIFIER_CHARS), "screenshotPath": path });
        return Ok(json!({ FULL_RESULT: { "isError": false, "structuredContent": saved, "content": [{ "type": "text", "text": saved.to_string() }] } }));
    }
    let mut page = snap.as_object().cloned().unwrap_or_default();
    page.remove("fullScreenshot");
    page.insert("screenshot".into(), json!({ "mimeType": "image/png", "width": snap["screenshot"]["width"], "height": snap["screenshot"]["height"] }));
    if let Some(path) = &saved_path {
        page.insert("screenshotPath".into(), json!(path));
    }
    let (value, text, omitted) = bound_snapshot(page);
    let mut structured = value.clone();
    if !omitted.is_empty() {
        structured["omitted"] = json!(omitted);
    }
    let mut content = vec![json!({ "type": "text", "text": json!({ "url": cut(&url, MAX_SNAPSHOT_IDENTIFIER_CHARS) }).to_string() }), json!({ "type": "text", "text": text })];
    if !omitted.is_empty() {
        content.push(json!({ "type": "text", "text": format!("Snapshot text was bounded. Omitted: {}.", omitted.join("; ")) }));
    }
    if let (true, Some(png)) = (include_image, png) {
        content.push(json!({ "type": "image", "data": png, "mimeType": "image/png" }));
    }
    Ok(json!({ FULL_RESULT: { "isError": false, "structuredContent": structured, "content": content } }))
}

/// `boundSnapshotMetadata`: per-field caps, then shedding (logs first, oldest
/// first) until the JSON is at most 20 KB. Answers the value, its text and what was omitted.
fn bound_snapshot(mut page: Map<String, Value>) -> (Value, String, Vec<String>) {
    let mut omitted = vec![];
    let tree = page.remove("accessibilityTree").and_then(|t| t.as_str().map(str::to_owned));
    let visible = page.remove("visibleText").and_then(|t| t.as_str().map(str::to_owned)).unwrap_or_default();
    for key in ["url", "title"] {
        if let Some(text) = page.get(key).and_then(Value::as_str).map(str::to_owned) {
            if text.chars().count() > MAX_SNAPSHOT_IDENTIFIER_CHARS {
                omitted.push(format!("url or title after {MAX_SNAPSHOT_IDENTIFIER_CHARS} characters"));
            }
            page.insert(key.into(), json!(cut(&text, MAX_SNAPSHOT_IDENTIFIER_CHARS)));
        }
    }
    let mut lists: Vec<(&str, Vec<Value>)> = vec![];
    for (key, label) in [("actionTimeline", "action timeline entries"), ("networkEntries", "network entries"), ("consoleEntries", "console entries")] {
        let entries = page.remove(key).and_then(|v| v.as_array().cloned()).unwrap_or_default();
        if entries.len() > MAX_SNAPSHOT_LOG_ENTRIES {
            omitted.push(format!("{} older {label}", entries.len() - MAX_SNAPSHOT_LOG_ENTRIES));
        }
        let kept: Vec<Value> = entries[entries.len().saturating_sub(MAX_SNAPSHOT_LOG_ENTRIES)..].to_vec();
        if kept.iter().any(|e| e.as_object().is_some_and(|m| m.values().any(|v| v.as_str().is_some_and(|s| s.chars().count() > MAX_SNAPSHOT_LOG_TEXT_CHARS)))) {
            omitted.push(format!("{label} text after {MAX_SNAPSHOT_LOG_TEXT_CHARS} characters"));
        }
        lists.push((key, kept.iter().map(cut_entry).collect()));
    }
    let mut elements: Vec<Value> = page.remove("interactiveElements").and_then(|v| v.as_array().cloned()).unwrap_or_default();
    if elements.iter().any(|e| e["name"].as_str().is_some_and(|n| n.chars().count() > MAX_SNAPSHOT_ELEMENT_NAME_CHARS)) {
        omitted.push(format!("element names longer than {MAX_SNAPSHOT_ELEMENT_NAME_CHARS} characters"));
    }
    for e in &mut elements {
        if let Some(name) = e["name"].as_str().map(str::to_owned) {
            e["name"] = json!(cut(&name, MAX_SNAPSHOT_ELEMENT_NAME_CHARS));
        }
    }
    let originals: Vec<usize> = lists.iter().map(|(_, l)| l.len()).chain([elements.len()]).collect();
    let mut visible_chars = visible.chars().count().min(MAX_SNAPSHOT_VISIBLE_TEXT_CHARS);
    let mut tree_chars = tree.as_ref().map_or(0, |t| t.chars().count().min(20_000));
    let halve = |chars: usize| if chars > SNAPSHOT_TEXT_FLOOR_CHARS { SNAPSHOT_TEXT_FLOOR_CHARS.max(chars / 2) } else { chars / 2 };
    let build = |lists: &[(&str, Vec<Value>)], elements: &[Value], visible_chars: usize, tree_chars: usize| {
        let mut v = Value::Object(page.clone());
        if let Some(t) = &tree {
            v["accessibilityTree"] = json!(cut(t, tree_chars));
        }
        v["visibleText"] = json!(cut(&visible, visible_chars));
        v["interactiveElements"] = json!(elements);
        for (k, l) in lists {
            v[*k] = json!(l);
        }
        v
    };
    let mut value = build(&lists, &elements, visible_chars, tree_chars);
    let mut text = value.to_string();
    while text.len() > MAX_SNAPSHOT_TEXT_BYTES {
        if let Some((_, list)) = lists.iter_mut().find(|(_, l)| !l.is_empty()) {
            let keep = list.len() / 2;
            list.drain(..list.len() - keep);
        } else if visible_chars > SNAPSHOT_TEXT_FLOOR_CHARS {
            visible_chars = halve(visible_chars);
        } else if tree_chars > SNAPSHOT_TEXT_FLOOR_CHARS {
            tree_chars = halve(tree_chars);
        } else if visible_chars > 0 {
            visible_chars = halve(visible_chars);
        } else if !elements.is_empty() {
            elements.truncate(elements.len() / 2);
        } else if tree_chars > 0 {
            tree_chars = halve(tree_chars);
        } else {
            break;
        }
        value = build(&lists, &elements, visible_chars, tree_chars);
        text = value.to_string();
    }
    if visible_chars < visible.chars().count() {
        omitted.push(format!("visibleText after {visible_chars} characters (use preview_evaluate for more)"));
    }
    if let Some(t) = &tree
        && tree_chars < t.chars().count()
    {
        omitted.push(format!("accessibilityTree after {tree_chars} characters"));
    }
    for (i, (key, list)) in lists.iter().enumerate() {
        if list.len() < originals[i] {
            omitted.push(format!("{} of {} {key}", originals[i] - list.len(), originals[i]));
        }
    }
    if elements.len() < originals[3] {
        omitted.push(format!("{} of {} interactiveElements", originals[3] - elements.len(), originals[3]));
    }
    (value, text, omitted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn act_inputs_are_checked_like_t3code_schemas() {
        assert!(check("click", &json!({ "locator": "aria-ref=e1" })).is_ok());
        assert!(check("click", &json!({ "x": 1 })).is_err());
        assert!(check("click", &json!({ "x": 1, "y": 2, "locator": "a" })).is_err());
        assert!(check("click", &json!({ "selector": "a", "clickCount": 4 })).is_err());
        assert!(check("navigate", &json!({ "url": "a", "target": { "kind": "url", "url": "a" } })).is_err());
        assert!(check("waitFor", &json!({})).is_err());
        assert!(check("scroll", &json!({ "deltaY": 100 })).is_ok());
        assert!(check("press", &json!({ "key": " " })).is_err());
        assert!(check("select", &json!({ "values": [], "locator": "x" })).is_ok());
    }

    #[test]
    fn snapshots_are_bounded_logs_first() {
        let logs: Vec<Value> = (0..60).map(|i| json!({ "level": "log", "text": format!("{i}: {}", "x".repeat(600)), "timestamp": "t" })).collect();
        let page = json!({ "url": "http://a/", "title": "A", "loading": false, "visibleText": "y".repeat(30_000), "accessibilityTree": "- button \"Go\" [ref=e1]",
            "interactiveElements": [{ "name": "Go" }], "consoleEntries": logs, "networkEntries": [], "actionTimeline": [], "screenshot": { "width": 1, "height": 1 } });
        let (value, text, omitted) = bound_snapshot(page.as_object().cloned().unwrap());
        assert!(text.len() <= MAX_SNAPSHOT_TEXT_BYTES, "{}", text.len());
        assert_eq!(value["interactiveElements"][0]["name"], "Go", "elements go last");
        assert!(omitted.iter().any(|o| o == "20 older console entries"), "{omitted:?}");
        assert!(omitted.iter().any(|o| o.starts_with("visibleText after")), "{omitted:?}");
        let kept = value["consoleEntries"].as_array().unwrap();
        assert!(kept.last().unwrap()["text"].as_str().unwrap().starts_with("59:"), "the newest entries stay");
    }
}

#[cfg(test)]
fn _calls_are_send(c: &'static Call<'static>) {
    fn send<T: Send>(_: T) {}
    send(call(c, "preview_status"));
}
