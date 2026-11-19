//! `domovoi-server acp-mcp-bridge`: Domovoi's MCP tools for an ACP agent that
//! only starts MCP servers as commands (stdio). Each JSON-RPC line on stdin
//! goes to the server's `/mcp` (`DOMOVOI_MCP_URL`, `DOMOVOI_MCP_TOKEN`), and
//! each answer comes back as a line on stdout.

#![expect(clippy::disallowed_types, reason = "MCP JSON-RPC messages passed through as they are")]

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub async fn run() -> Result<(), String> {
    let url = std::env::var("DOMOVOI_MCP_URL").map_err(|_| "DOMOVOI_MCP_URL is not set.")?;
    let token = std::env::var("DOMOVOI_MCP_TOKEN").map_err(|_| "DOMOVOI_MCP_TOKEN is not set.")?;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let http = reqwest::Client::new();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let answer = http.post(&url).bearer_auth(&token).json(&msg).send().await.map_err(|e| e.to_string())?;
        // A notification gets no answer.
        if msg.get("id").is_none() {
            continue;
        }
        let body = answer.text().await.map_err(|e| e.to_string())?;
        out.write_all(body.trim().as_bytes()).await.map_err(|e| e.to_string())?;
        out.write_all(b"\n").await.map_err(|e| e.to_string())?;
        out.flush().await.map_err(|e| e.to_string())?;
    }
    Ok(())
}
