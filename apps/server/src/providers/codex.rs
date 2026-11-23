//! Codex's current model catalog, queried without creating a thread.
#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "Codex model/list JSON-RPC responses")]

use super::{choice, select};
use crate::jsonrpc::RpcClient;
use domovoi_model::typed::{ModelCapabilities, ServerProviderModel};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

/// A short-lived app-server uses the same executable and account as turns.
/// Bound the entire exchange, and stop the child on success, error or timeout.
pub(super) async fn discover(exe: &Path) -> Result<Vec<ServerProviderModel>, String> {
    let (rpc, mut events) = RpcClient::spawn(&exe.to_string_lossy(), &["app-server"], None, |_, _| {}).map_err(|e| e.to_string())?;
    let drain = tokio::spawn(async move { while events.recv().await.is_some() {} });
    let result = tokio::time::timeout(Duration::from_secs(10), list_models(&rpc)).await;
    // The same app-server reads the account's limits.
    if result.as_ref().is_ok_and(Result::is_ok) {
        let _ = tokio::time::timeout(Duration::from_secs(5), read_limits(&rpc)).await;
    }
    rpc.kill().await;
    drain.abort();
    result.map_err(|_| "Codex model discovery timed out".to_owned())?
}

/// The account's limits (`account/rateLimits/read`), kept as the provider's.
pub(crate) async fn read_limits(rpc: &RpcClient) {
    if let Ok(answer) = rpc.request("account/rateLimits/read", Value::Null).await {
        crate::usage_limits::merge(crate::providers::CODEX, crate::usage_limits::codex_windows(&answer["rateLimits"]));
    }
}

/// Reads the account's limits with an app-server of its own.
pub(crate) async fn refresh_limits(exe: &Path) {
    let Ok((rpc, mut events)) = RpcClient::spawn(&exe.to_string_lossy(), &["app-server"], None, |_, _| {}) else { return };
    let drain = tokio::spawn(async move { while events.recv().await.is_some() {} });
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        initialize(&rpc).await?;
        read_limits(&rpc).await;
        Ok::<(), String>(())
    })
    .await;
    rpc.kill().await;
    drain.abort();
}

async fn initialize(rpc: &RpcClient) -> Result<(), String> {
    rpc.request("initialize", json!({
        "clientInfo": { "name": "domovoi", "version": domovoi_contracts::environment::SERVER_VERSION },
        "capabilities": { "experimentalApi": true },
    })).await.map_err(|e| e.to_string())?;
    rpc.notify("initialized", None).await.map_err(|e| e.to_string())
}

async fn list_models(rpc: &RpcClient) -> Result<Vec<ServerProviderModel>, String> {
    initialize(rpc).await?;
    let mut models = vec![];
    let mut cursor = None;
    let mut cursors = HashSet::new();
    let mut seen = HashSet::new();
    loop {
        let page = rpc.request("model/list", json!({ "limit": 100, "includeHidden": false, "cursor": cursor })).await.map_err(|e| e.to_string())?;
        let data = page["data"].as_array().ok_or("Codex model/list has no data array")?;
        for model in data {
            if model["hidden"] == true {
                continue;
            }
            let model = parse_model(model).ok_or("Codex model/list returned a model without an identity")?;
            if seen.insert(model.slug.clone()) {
                models.push(model);
            }
        }
        match page.get("nextCursor") {
            None | Some(Value::Null) => break,
            Some(Value::String(next)) if !next.is_empty() && cursors.insert(next.clone()) => cursor = Some(next.clone()),
            _ => return Err("Codex model/list returned an invalid or repeated cursor".into()),
        }
    }
    if models.is_empty() {
        return Err("Codex model/list returned no visible models".into());
    }
    Ok(models)
}

fn parse_model(model: &Value) -> Option<ServerProviderModel> {
    // `model` is the identifier accepted by thread/start, which can differ from `id`.
    let slug = model["model"].as_str().filter(|s| !s.trim().is_empty())?;
    let default_effort = model["defaultReasoningEffort"].as_str();
    let efforts: Vec<_> = model["supportedReasoningEfforts"].as_array().into_iter().flatten().filter_map(|e| {
        let id = e["reasoningEffort"].as_str()?;
        let label = match id {
            "low" => "Low", "medium" => "Medium", "high" => "High", "xhigh" => "Extra High", "max" => "Max", "ultra" => "Ultra", _ => id,
        };
        Some(choice(id, label, e["description"].as_str(), Some(id) == default_effort))
    }).collect();
    let mut descriptors = vec![];
    if !efforts.is_empty() {
        descriptors.push(select("reasoningEffort", "Reasoning", efforts, default_effort.map(str::to_owned)));
    }
    let default_tier = model["defaultServiceTier"].as_str().unwrap_or("default");
    let mut tiers = vec![choice("default", "Standard", None, default_tier == "default")];
    for tier in model["serviceTiers"].as_array().into_iter().flatten() {
        if let Some(id) = tier["id"].as_str().filter(|id| *id != "default") {
            tiers.push(choice(id, tier["name"].as_str().unwrap_or(id), tier["description"].as_str(), id == default_tier));
        }
    }
    if tiers.len() > 1 {
        descriptors.push(select("serviceTier", "Service Tier", tiers, Some(default_tier.to_owned())));
    }
    let cyber: Vec<_> = model["availableAccessPrograms"]["cyber"].as_array().into_iter().flatten().filter_map(|p| {
        let id = p.as_str()?;
        Some(choice(id, if id == "standard" { "Standard" } else { id }, None, id == "standard"))
    }).collect();
    if !cyber.is_empty() {
        descriptors.push(select("cyberAccessProgram", "Cyber Access", cyber, None));
    }
    Some(ServerProviderModel {
        slug: slug.into(),
        name: model["displayName"].as_str().filter(|n| !n.trim().is_empty()).unwrap_or(slug).into(),
        short_name: None,
        sub_provider: None,
        aliases: None,
        badge: None,
        is_custom: false,
        is_default: model["isDefault"].as_bool(),
        is_legacy: None,
        supports_text_generation: None,
        capabilities: Some(ModelCapabilities { option_descriptors: Some(descriptors) }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE: &str = r#"
import json, sys
scenario = sys.argv[1]
for line in sys.stdin:
    m = json.loads(line)
    method = m.get('method')
    if method == 'initialize':
        assert m['params']['capabilities']['experimentalApi']
        result = {}
    elif method == 'initialized':
        continue
    elif method == 'model/list':
        assert m['params']['includeHidden'] is False
        if scenario == 'error':
            print(json.dumps({'id': m['id'], 'error': {'code': -32601, 'message': 'unsupported'}}), flush=True)
            continue
        if scenario == 'empty':
            result = {'data': [], 'nextCursor': None}
        elif scenario == 'malformed':
            result = {'data': [{'id': 'missing-model'}]}
        elif m['params']['cursor'] is None:
            result = {'data': [
                {'id': 'display-id', 'model': 'gpt-6.2-sol', 'displayName': 'GPT-6.2-Sol', 'isDefault': True,
                 'supportedReasoningEfforts': [{'reasoningEffort': 'low'}, {'reasoningEffort': 'ultra'}],
                 'defaultReasoningEffort': 'low', 'serviceTiers': [{'id': 'priority', 'name': 'Fast'}]},
                {'model': 'hidden-model', 'hidden': True}], 'nextCursor': 'page2'}
        else:
            assert m['params']['cursor'] == 'page2'
            result = {'data': [{'model': 'gpt-6.2-sol'}, {'model': 'gpt-6.2-astra', 'displayName': 'GPT-6.2-Astra'}],
                      'nextCursor': 'page2' if scenario == 'cycle' else None}
    else:
        raise AssertionError('Discovery must not create a thread: ' + method)
    print(json.dumps({'id': m['id'], 'result': result}), flush=True)
"#;

    async fn fake_catalog(scenario: &str) -> Result<Vec<ServerProviderModel>, String> {
        let (rpc, _events) = RpcClient::spawn("python3", &["-c", FAKE, scenario], None, |_, _| {}).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), list_models(&rpc)).await.unwrap();
        rpc.kill().await;
        result
    }

    #[tokio::test]
    async fn discovers_new_models_across_pages_with_live_capabilities() {
        let models = fake_catalog("pages").await.unwrap();
        assert_eq!(models.iter().map(|m| m.slug.as_str()).collect::<Vec<_>>(), ["gpt-6.2-sol", "gpt-6.2-astra"]);
        assert_eq!(models[0].name, "GPT-6.2-Sol");
        assert_eq!(models[0].is_default, Some(true));
        let options = json!(models[0].capabilities)["optionDescriptors"].clone();
        assert_eq!(options[0]["currentValue"], "low");
        assert_eq!(options[0]["options"][1]["id"], "ultra");
        assert_eq!(options[1]["options"][1]["id"], "priority");
        assert_eq!(options[1]["currentValue"], "default");
    }

    #[tokio::test]
    async fn rejects_unusable_catalogs_so_the_probe_can_keep_its_fallback() {
        for scenario in ["error", "empty", "malformed", "cycle"] {
            assert!(fake_catalog(scenario).await.is_err(), "{scenario}");
        }
    }

    #[tokio::test]
    #[ignore = "queries the Codex installation and account on this machine; creates no threads"]
    async fn installed_codex_catalog() {
        let exe = crate::providers::executable(crate::providers::CODEX_TOOL).unwrap();
        let models = discover(&exe).await.unwrap();
        println!("Codex models: {:?}", models.iter().map(|m| &m.slug).collect::<Vec<_>>());
        assert!(models.iter().any(|m| m.is_default == Some(true)));
    }
}
