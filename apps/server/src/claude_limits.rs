//! A Claude account's usage limits read on demand (t3code's capability probe
//! `usage_EXPERIMENTAL…`): Claude Code's `get_usage` control request, which
//! names the five-hour and weekly windows before any turn reports them. Asked
//! of a running session, or of a Claude Code started for it alone: no prompt,
//! no settings, hooks or MCP servers, and no session kept, so it costs nothing.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "Claude Code's control requests and answers, in its format")]

use serde_json::{Value, json};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const DEADLINE: Duration = Duration::from_secs(20);

static READING: AtomicBool = AtomicBool::new(false);

/// `skip_behaviors`: only the limits, not the scan of local transcripts.
fn request() -> Value {
    json!({ "subtype": "get_usage", "skip_behaviors": true })
}

/// Keeps a `get_usage` answer (a control response's `response`) as the
/// instance's limits; an answer that doesn't say whether the account has
/// limits (a Claude Code without `get_usage`) changes nothing.
fn keep(instance: &str, answer: &Value) {
    if answer["subtype"] == "success" && answer["response"]["rate_limits_available"].is_boolean() {
        crate::usage_limits::set(instance, crate::usage_limits::claude_usage_limits(&answer["response"]));
    }
}

/// Asks a running session for the account's limits, in the background.
pub fn read_from(cli: Arc<crate::claude_cli::ClaudeCli>, instance: &str) {
    let instance = instance.to_owned();
    tokio::spawn(async move {
        if let Ok(Ok(answer)) = tokio::time::timeout(DEADLINE, cli.control(request())).await {
            keep(&instance, &answer);
        }
    });
}

/// Reads the account's limits with a Claude Code of its own, in the
/// background (one read at a time); the provider list shows them when they arrive.
pub fn refresh() {
    if READING.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::spawn(async move {
        if let Ok(Some(answer)) = tokio::time::timeout(DEADLINE, read()).await {
            keep(crate::providers::CLAUDE, &answer);
        }
        READING.store(false, Ordering::SeqCst);
    });
}

async fn read() -> Option<Value> {
    let exe = crate::providers::executable(crate::providers::CLAUDE_TOOL).ok()?;
    let mut child = crate::process::command(&exe)
        .args([
            "--output-format", "stream-json", "--verbose", "--input-format", "stream-json",
            "--no-session-persistence", "--strict-mcp-config", "--setting-sources=",
        ])
        .current_dir(std::env::temp_dir())
        .env("CLAUDE_CODE_ENTRYPOINT", "sdk-ts")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let _tree = crate::process::Tree::of(child.id());
    let mut stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let mut lines = String::new();
    for (id, request) in [("init", json!({ "subtype": "initialize" })), ("usage", request())] {
        lines.push_str(&json!({ "request_id": id, "type": "control_request", "request": request }).to_string());
        lines.push('\n');
    }
    stdin.write_all(lines.as_bytes()).await.ok()?;
    stdin.flush().await.ok()?;
    let mut out = BufReader::new(stdout).lines();
    let mut answer = None;
    while let Ok(Some(line)) = out.next_line().await {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        if msg["type"] == "control_response" && msg["response"]["request_id"] == "usage" {
            answer = Some(msg["response"].clone());
            break;
        }
    }
    drop(stdin);
    let _ = child.kill().await;
    answer
}
