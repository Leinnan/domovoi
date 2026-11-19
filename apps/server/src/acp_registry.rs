//! The ACP registry (t3code `provider-acp-registry`): the agents published at
//! `https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json`,
//! and installing them under `<home>/acp-registry/` for instances that name
//! them (`agentId`).
//!
//! An agent comes as a binary per platform (`darwin-aarch64`, `linux-x86_64`,
//! ...: an archive, its sha256 checked when the registry gives one, unpacked),
//! an npm package (installed into the agent's own folder) or a Python
//! package run with `uvx`. A binary for this platform is preferred, then npm,
//! then uv, each only when its tool is on PATH. An install writes a receipt
//! (`installs/<agent>.json`) naming the command an instance starts.
//!
//! The registry is read again after 5 minutes for a search, and always before
//! an install; a copy is kept for when it can't be reached.

#![expect(clippy::disallowed_types, reason = "the ACP registry's JSON, read as the registry publishes it")]

use crate::adapters::acp::Spawn;
use crate::rpc::admin_failure::{AcpRegistryReason, Failure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const REGISTRY_URL: &str = "https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json";
const FRESH: std::time::Duration = std::time::Duration::from_secs(5 * 60);
const MAX_AGENTS: usize = 512;

fn fail(reason: AcpRegistryReason, message: impl Into<String>) -> Failure {
    Failure::AcpRegistry { reason, message: message.into() }
}

fn root() -> PathBuf {
    crate::state::STATE.get().map_or_else(|| std::env::temp_dir().join("domovoi-acp-registry"), |s| s.paths.home.join("acp-registry"))
}

/// The registry's URL: `DOMOVOI_ACP_REGISTRY_URL` names another (a test's).
fn registry_url() -> String {
    std::env::var("DOMOVOI_ACP_REGISTRY_URL").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| REGISTRY_URL.to_owned())
}

/// The registry, read again when the kept copy is older than 5 minutes (or `force`).
pub async fn registry(force: bool) -> Result<Value, Failure> {
    let file = root().join("registry.json");
    let fresh = std::fs::metadata(&file).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age < FRESH);
    let kept = || std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
    if fresh && !force && let Some(v) = kept() {
        return Ok(v);
    }
    let url = registry_url();
    let fetched = if let Some(path) = url.strip_prefix("file://") {
        std::fs::read_to_string(path).map_err(|e| e.to_string())
    } else {
        let _ = rustls::crypto::ring::default_provider().install_default();
        match reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).build() {
            Err(e) => Err(e.to_string()),
            Ok(client) => match client.get(&url).send().await {
                Ok(r) if r.status().is_success() => r.text().await.map_err(|e| e.to_string()),
                Ok(r) => Err(format!("the registry answered {}", r.status())),
                Err(e) => Err(e.to_string()),
            },
        }
    };
    match fetched.ok().and_then(|t| serde_json::from_str::<Value>(&t).ok().filter(|v| v["agents"].is_array())) {
        Some(v) => {
            let _ = std::fs::create_dir_all(root());
            let _ = std::fs::write(&file, v.to_string());
            Ok(v)
        }
        None => kept().ok_or_else(|| fail(AcpRegistryReason::RegistryUnavailable, "The ACP registry could not be reached.")),
    }
}

/// This machine as the registry names platforms.
pub fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => other,
    };
    format!("{os}-{arch}")
}

/// Whether `tool` is on the server's PATH.
fn on_path(tool: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(tool).is_file()))
}

/// The distribution to use for `agent` here: a binary for this platform, else npm, else uv.
pub fn pick(agent: &Value, wanted: Option<&str>, tools: &dyn Fn(&str) -> bool) -> Option<&'static str> {
    let d = &agent["distribution"];
    let binary = !d["binary"][platform()].is_null();
    let npx = !d["npx"].is_null() && tools("npm");
    let uvx = !d["uvx"].is_null() && tools("uvx");
    let have = |k: &str| match k {
        "binary" => binary,
        "npx" => npx,
        "uvx" => uvx,
        _ => false,
    };
    match wanted.filter(|w| *w != "auto") {
        Some(w) => ["binary", "npx", "uvx"].into_iter().find(|k| *k == w && have(k)),
        None => ["binary", "npx", "uvx"].into_iter().find(|k| have(k)),
    }
}

/// An agent as `searchAcpRegistry` lists it (`AcpRegistrySearchAgent`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Listed {
    id: String,
    name: String,
    version: String,
    description: String,
    authors: Vec<String>,
    license: Option<String>,
    website: Option<String>,
    repository: Option<String>,
    icon: Option<String>,
    distribution: &'static str,
    integrity: &'static str,
}

/// `server.searchAcpRegistry`: the agents that run here whose id, name or description has `query`.
pub async fn search(query: &str, limit: Option<usize>) -> Result<Vec<Listed>, Failure> {
    let reg = registry(false).await?;
    Ok(search_in(&reg, query, limit, &on_path))
}

pub fn search_in(reg: &Value, query: &str, limit: Option<usize>, tools: &dyn Fn(&str) -> bool) -> Vec<Listed> {
    let q = query.trim().to_lowercase();
    let text = |v: &Value, max: usize| v.as_str().map(|s| s.chars().take(max).collect::<String>());
    reg["agents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| q.is_empty() || ["id", "name", "description"].iter().any(|k| a[*k].as_str().is_some_and(|s| s.to_lowercase().contains(&q))))
        .filter_map(|a| {
            let distribution = pick(a, None, tools)?;
            let id = a["id"].as_str().filter(|id| valid_id(id))?.to_owned();
            let sha = distribution == "binary" && a["distribution"]["binary"][platform()]["sha256"].is_string();
            Some(Listed {
                name: text(&a["name"], 160).unwrap_or_else(|| id.clone()),
                version: text(&a["version"], 128).unwrap_or_else(|| "0".into()),
                description: text(&a["description"], 1024).unwrap_or_default(),
                authors: a["authors"].as_array().into_iter().flatten().filter_map(|s| text(s, 256)).take(16).collect(),
                license: text(&a["license"], 128),
                website: text(&a["website"], 2048),
                repository: text(&a["repository"], 2048),
                icon: text(&a["icon"], 2048).filter(|i| i.starts_with("https://cdn.agentclientprotocol.com/")),
                distribution,
                integrity: if sha { "sha256" } else { "registry" },
                id,
            })
        })
        .take(limit.unwrap_or(MAX_AGENTS).clamp(1, MAX_AGENTS))
        .collect()
}

/// t3code's agent id pattern: `^[a-z0-9][a-z0-9._-]*$`, at most 128.
fn valid_id(id: &str) -> bool {
    id.len() <= 128 && id.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

/// What an install leaves: the command an instance of the agent starts.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub agent_id: String,
    pub version: String,
    pub distribution: String,
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

fn receipt_file(agent_id: &str) -> PathBuf {
    root().join("installs").join(format!("{agent_id}.json"))
}

fn agent_dir(agent_id: &str) -> PathBuf {
    root().join("agents").join(agent_id)
}

/// The installed command of a registry agent, as its receipt says.
pub fn installed_spawn(agent_id: &str, _distribution: Option<&str>) -> Result<Spawn, String> {
    if !valid_id(agent_id) {
        return Err(format!("{agent_id} is not an ACP registry agent id."));
    }
    let receipt: Receipt = std::fs::read_to_string(receipt_file(agent_id))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .ok_or_else(|| format!("The ACP agent {agent_id} is not installed: prepare it in Settings → Providers first."))?;
    Ok(Spawn { program: receipt.program, args: receipt.args, env: receipt.env })
}

/// The `env` object of a distribution as pairs.
fn env_of(v: &Value) -> Vec<(String, String)> {
    v.as_object().into_iter().flatten().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned()))).collect()
}

fn args_of(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(|a| a.as_str().map(str::to_owned)).collect()
}

async fn run(program: &str, args: &[&str], cwd: &Path, env: &[(&str, String)]) -> Result<String, String> {
    let mut cmd = crate::process::command(program);
    cmd.args(args).current_dir(cwd);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = crate::process::output(cmd, std::time::Duration::from_secs(10 * 60)).await.map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("{program} failed: {}", String::from_utf8_lossy(&out.stderr).trim().chars().take(500).collect::<String>()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// What `prepareAcpRegistryAgent` answers (`AcpRegistryPrepareResult`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Prepared {
    agent_id: String,
    version: String,
    distribution: String,
    prepared: bool,
}

/// `server.prepareAcpRegistryAgent`: installs the agent (again, if its version changed).
pub async fn prepare(agent_id: &str, wanted: Option<&str>) -> Result<Prepared, Failure> {
    if !valid_id(agent_id) {
        return Err(fail(AcpRegistryReason::AgentNotFound, format!("{agent_id} is not an ACP registry agent id.")));
    }
    let reg = registry(true).await?;
    let agent = reg["agents"].as_array().into_iter().flatten().find(|a| a["id"] == agent_id).cloned().ok_or_else(|| fail(AcpRegistryReason::AgentNotFound, format!("The ACP registry has no agent {agent_id}.")))?;
    let distribution = pick(&agent, wanted, &on_path).ok_or_else(|| {
        if agent["distribution"]["binary"].is_object() && agent["distribution"]["npx"].is_null() && agent["distribution"]["uvx"].is_null() {
            fail(AcpRegistryReason::UnsupportedPlatform, format!("{agent_id} has no build for {}.", platform()))
        } else {
            fail(AcpRegistryReason::RunnerUnavailable, format!("{agent_id} needs npm or uv, and neither is on PATH."))
        }
    })?;
    let version = agent["version"].as_str().unwrap_or("0").to_owned();
    if let Ok(t) = std::fs::read_to_string(receipt_file(agent_id))
        && let Ok(r) = serde_json::from_str::<Receipt>(&t)
        && r.version == version
        && r.distribution == distribution
    {
        return Ok(Prepared { agent_id: agent_id.to_owned(), version, distribution: distribution.to_owned(), prepared: true });
    }
    let dir = agent_dir(agent_id).join(&version);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| fail(AcpRegistryReason::InstallFailed, e.to_string()))?;
    let d = &agent["distribution"][distribution];
    let receipt = match distribution {
        "npx" => install_npm(agent_id, &version, d, &dir).await?,
        "uvx" => Receipt {
            agent_id: agent_id.to_owned(),
            version: version.clone(),
            distribution: "uvx".into(),
            program: "uvx".into(),
            args: std::iter::once(d["package"].as_str().unwrap_or_default().to_owned()).chain(args_of(&d["args"])).collect(),
            env: env_of(&d["env"]),
        },
        _ => install_binary(agent_id, &version, &agent["distribution"]["binary"][platform()], &dir).await?,
    };
    let file = receipt_file(agent_id);
    let _ = std::fs::create_dir_all(file.parent().unwrap_or(Path::new(".")));
    std::fs::write(&file, serde_json::to_string_pretty(&receipt).unwrap_or_default()).map_err(|e| fail(AcpRegistryReason::InstallFailed, e.to_string()))?;
    Ok(Prepared { agent_id: agent_id.to_owned(), version, distribution: distribution.to_owned(), prepared: true })
}

/// The npm package installed into the agent's folder; its first `bin` runs.
async fn install_npm(agent_id: &str, version: &str, d: &Value, dir: &Path) -> Result<Receipt, Failure> {
    let package = d["package"].as_str().ok_or_else(|| fail(AcpRegistryReason::InstallFailed, "The registry names no npm package."))?;
    let prefix = dir.to_string_lossy().into_owned();
    run("npm", &["install", "--prefix", &prefix, "--no-audit", "--no-fund", "--loglevel=error", package], dir, &[])
        .await
        .map_err(|e| fail(AcpRegistryReason::InstallFailed, e))?;
    let name = package.rsplit_once('@').filter(|(n, _)| !n.is_empty()).map_or(package, |(n, _)| n);
    let manifest: Value = std::fs::read_to_string(dir.join("node_modules").join(name).join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .ok_or_else(|| fail(AcpRegistryReason::InstallFailed, format!("{name} did not install.")))?;
    let bin = match &manifest["bin"] {
        Value::String(_) => name.rsplit('/').next().unwrap_or(name).to_owned(),
        Value::Object(m) => m.keys().next().cloned().unwrap_or_default(),
        _ => String::new(),
    };
    let program = dir.join("node_modules").join(".bin").join(&bin);
    if bin.is_empty() || !program.exists() {
        return Err(fail(AcpRegistryReason::InstallFailed, format!("{name} has no command to run.")));
    }
    Ok(Receipt {
        agent_id: agent_id.to_owned(),
        version: version.to_owned(),
        distribution: "npx".into(),
        program: program.to_string_lossy().into_owned(),
        args: args_of(&d["args"]),
        env: env_of(&d["env"]),
    })
}

/// The archive downloaded, its sha256 checked when given, and unpacked; `cmd` runs.
async fn install_binary(agent_id: &str, version: &str, b: &Value, dir: &Path) -> Result<Receipt, Failure> {
    let url = b["archive"].as_str().ok_or_else(|| fail(AcpRegistryReason::DownloadFailed, "The registry names no archive."))?;
    let cmd = b["cmd"].as_str().ok_or_else(|| fail(AcpRegistryReason::ArchiveInvalid, "The registry names no command."))?;
    let bytes = download(url).await?;
    if let Some(want) = b["sha256"].as_str() {
        let got: String = Sha256::digest(&bytes).iter().map(|x| format!("{x:02x}")).collect();
        if !got.eq_ignore_ascii_case(want) {
            return Err(fail(AcpRegistryReason::ChecksumMismatch, format!("The download of {agent_id} does not match its checksum.")));
        }
    }
    let lower = url.to_lowercase();
    let path_part = lower.split(['?', '#']).next().unwrap_or(&lower).to_owned();
    let cmd_path = dir.join(cmd.trim_start_matches("./"));
    if path_part.ends_with(".zip") || path_part.ends_with(".tar.gz") || path_part.ends_with(".tgz") || path_part.ends_with(".tar.bz2") || path_part.ends_with(".tar.xz") || path_part.ends_with(".tar") {
        let archive = dir.join(if path_part.ends_with(".zip") { "download.zip" } else { "download.tar" });
        std::fs::write(&archive, &bytes).map_err(|e| fail(AcpRegistryReason::InstallFailed, e.to_string()))?;
        let a = archive.to_string_lossy().into_owned();
        let unpacked = if path_part.ends_with(".zip") { run("unzip", &["-o", "-q", &a], dir, &[]).await } else { run("tar", &["-xf", &a], dir, &[]).await };
        unpacked.map_err(|e| fail(AcpRegistryReason::ArchiveInvalid, e))?;
        let _ = std::fs::remove_file(&archive);
    } else {
        // A bare executable.
        std::fs::write(&cmd_path, &bytes).map_err(|e| fail(AcpRegistryReason::InstallFailed, e.to_string()))?;
    }
    if !cmd_path.is_file() {
        return Err(fail(AcpRegistryReason::ArchiveInvalid, format!("The archive of {agent_id} has no {cmd}.")));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&cmd_path, std::fs::Permissions::from_mode(0o755));
    }
    Ok(Receipt {
        agent_id: agent_id.to_owned(),
        version: version.to_owned(),
        distribution: "binary".into(),
        program: cmd_path.to_string_lossy().into_owned(),
        args: args_of(&b["args"]),
        env: env_of(&b["env"]),
    })
}

async fn download(url: &str) -> Result<Vec<u8>, Failure> {
    if let Some(path) = url.strip_prefix("file://") {
        return std::fs::read(path).map_err(|e| fail(AcpRegistryReason::DownloadFailed, e.to_string()));
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10 * 60)).build().map_err(|e| fail(AcpRegistryReason::DownloadFailed, e.to_string()))?;
    let res = client.get(url).send().await.map_err(|e| fail(AcpRegistryReason::DownloadFailed, e.to_string()))?;
    if !res.status().is_success() {
        return Err(fail(AcpRegistryReason::DownloadFailed, format!("The download answered {}.", res.status())));
    }
    res.bytes().await.map(|b| b.to_vec()).map_err(|e| fail(AcpRegistryReason::DownloadFailed, e.to_string()))
}

/// What `uninstallAcpRegistryManagedBinary` answers.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Removed {
    agent_id: String,
    removed: bool,
}

/// `server.uninstallAcpRegistryManagedBinary`: refused while an instance uses the agent.
pub fn uninstall(agent_id: &str) -> Result<Removed, Failure> {
    if !valid_id(agent_id) {
        return Err(fail(AcpRegistryReason::AgentNotFound, format!("{agent_id} is not an ACP registry agent id.")));
    }
    let used = crate::providers::instances_of("acpRegistry").into_iter().any(|(_, c)| c.config.as_ref().is_some_and(|c| c["agentId"] == agent_id));
    if used {
        return Err(fail(AcpRegistryReason::AgentNotConfigured, format!("An instance still uses {agent_id}: remove it first.")));
    }
    let had = receipt_file(agent_id).exists() || agent_dir(agent_id).exists();
    let _ = std::fs::remove_file(receipt_file(agent_id));
    let _ = std::fs::remove_dir_all(agent_dir(agent_id));
    Ok(Removed { agent_id: agent_id.to_owned(), removed: had })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reg() -> Value {
        json!({ "version": "1.0.0", "agents": [
            { "id": "bin-agent", "name": "Bin", "version": "1.2.0", "description": "A binary agent", "authors": ["A"], "icon": "https://cdn.agentclientprotocol.com/registry/v1/latest/bin-agent.svg",
              "distribution": { "binary": { platform(): { "archive": "https://x/a.tar.gz", "cmd": "./a", "sha256": "00" } } } },
            { "id": "npm-agent", "name": "Npm", "version": "2.0.0", "description": "Via npm", "icon": "https://evil.example/x.svg", "distribution": { "npx": { "package": "npm-agent@2.0.0", "args": ["--acp"] } } },
            { "id": "py-agent", "name": "Py", "version": "0.1", "description": "Via uv", "distribution": { "uvx": { "package": "py-agent==0.1" } } },
            { "id": "other-os", "name": "Elsewhere", "version": "1", "description": "", "distribution": { "binary": { "plan9-mips": { "archive": "x", "cmd": "y" } } } },
            { "id": "Bad Id", "name": "Bad", "version": "1", "distribution": { "npx": { "package": "x" } } },
        ] })
    }

    #[test]
    fn search_lists_what_runs_here() {
        let only_npm = |t: &str| t == "npm";
        let found = search_in(&reg(), "", None, &only_npm);
        let ids: Vec<(&str, &str, &str)> = found.iter().map(|a| (a.id.as_str(), a.distribution, a.integrity)).collect();
        assert_eq!(ids, [("bin-agent", "binary", "sha256"), ("npm-agent", "npx", "registry")], "no uv, no build for here, no bad id");
        assert!(found[1].icon.is_none(), "icons only from the registry's own host");
        assert_eq!(search_in(&reg(), "via", None, &|_| true).iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["npm-agent", "py-agent"]);
        assert_eq!(pick(&reg()["agents"][0], Some("npx"), &|_| true), None, "a distribution it doesn't have");
    }

    #[tokio::test]
    async fn a_binary_agent_installs_with_its_checksum_checked() {
        let dir = std::env::temp_dir().join(format!("acp-reg-test-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        // A tar.gz holding `./agent`.
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("agent"), "#!/bin/sh\necho hi\n").unwrap();
        let tar = dir.join("a.tar.gz");
        assert!(std::process::Command::new("tar").args(["-czf", tar.to_str().unwrap(), "-C", src.to_str().unwrap(), "agent"]).status().unwrap().success());
        let sha: String = Sha256::digest(std::fs::read(&tar).unwrap()).iter().map(|x| format!("{x:02x}")).collect();
        let target = dir.join("install");
        std::fs::create_dir_all(&target).unwrap();
        let b = json!({ "archive": format!("file://{}", tar.display()), "cmd": "./agent", "args": ["acp"], "sha256": sha });
        let r = install_binary("x", "1", &b, &target).await.unwrap();
        assert!(r.program.ends_with("/install/agent") && r.args == ["acp"], "{r:?}");
        let wrong = json!({ "archive": format!("file://{}", tar.display()), "cmd": "./agent", "sha256": "ff" });
        assert!(matches!(install_binary("x", "1", &wrong, &target).await, Err(Failure::AcpRegistry { reason: AcpRegistryReason::ChecksumMismatch, .. })));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
