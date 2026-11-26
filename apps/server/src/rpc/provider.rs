//! `provider.*` setup RPCs: signing a provider instance in and out, and its
//! managed installation.
//!
//! Antigravity instances sign in here (`crate::antigravity::auth`).
//!
//! The Codex and Claude drivers sign in through their own CLIs, so for them
//! the sign-in methods answer with a typed "does not support sign-in" error.
//! A managed Codex installation (a download into the server home) is not
//! implemented: subscribing shows an idle state, starting it fails.

#![expect(clippy::disallowed_types, reason = "a provider instance's `config`: the driver's own settings, which the schema leaves open")]

use super::Never;
use crate::config_failure::Failure;
use crate::providers::{CLAUDE, CODEX};
use crate::state::ConnCtx;
use domovoi_rpc::Router;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstanceInput {
    instance_id: String,
}

/// An instance Domovoi knows: the built-in Codex and Claude instances, plus
/// configured instances of those drivers.
struct Instance {
    driver: String,
    /// Codex's `setupMode` is `managed`.
    managed: bool,
    /// The executable the instance names, trimmed; empty when it names none.
    binary_path: String,
}

async fn instance(ctx: &ConnCtx, id: &str) -> Option<Instance> {
    let settings = ctx.state.settings.read().await;
    let configured = settings.provider_instances.as_ref().and_then(|instances| instances.get(id));
    let driver = configured.map(|c| c.driver.as_str().to_owned()).or_else(|| [CODEX, CLAUDE].contains(&id).then(|| id.to_owned()))?;
    if driver != CODEX && driver != CLAUDE {
        return None;
    }
    // The driver's own settings, an open record: the few it shares with the server are read from it.
    let config = configured.and_then(|c| c.config.as_ref());
    let of_config = |key: &str| config.and_then(|c| c.get(key));
    Some(Instance {
        driver,
        managed: of_config("setupMode").and_then(Value::as_str) == Some("managed"),
        binary_path: of_config("binaryPath").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_owned(),
    })
}

/// `ProviderAuthService.getController`: neither driver has a sign-in
/// controller; Antigravity's takes no other operations.
async fn auth_controller(ctx: &ConnCtx, id: &str, operation: &'static str) -> Failure {
    if crate::antigravity::is_instance(id) {
        return Failure::setup(id, operation, "Antigravity's sign-in does not support this operation.");
    }
    let detail = if instance(ctx, id).await.is_some() {
        "This provider does not support sign-in in T3 Code."
    } else {
        "This provider instance is no longer available."
    };
    Failure::setup(id, operation, detail)
}

/// `ProviderInstallation.requireInstance`; `Ok` means managed Codex.
async fn managed_codex(ctx: &ConnCtx, id: &str, operation: &'static str, managed_only: bool) -> Result<Instance, Failure> {
    let Some(inst) = instance(ctx, id).await.filter(|i| i.driver == CODEX) else {
        return Err(Failure::setup(id, operation, "Managed installation is not available for this provider instance."));
    };
    if !inst.managed {
        return Err(Failure::setup(id, operation, "Choose managed setup to install Codex in T3 Code."));
    }
    if managed_only && !inst.binary_path.is_empty() && inst.binary_path != "codex" {
        return Err(Failure::setup(
            id,
            operation,
            "This instance uses a custom executable. Clear its binary path to manage installation in T3 Code.",
        ));
    }
    Ok(inst)
}

/// Where a managed installation of a provider stands (`ProviderInstallState`).
/// Domovoi installs nothing, so the state is always idle: the fields an
/// installation under way would fill are `null`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallState {
    driver: &'static str,
    operation_id: Option<Never>,
    phase: InstallPhase,
    downloaded_bytes: u64,
    total_bytes: Option<Never>,
    version: Option<Never>,
    /// The version of the Codex the server found on the machine.
    installed_version: Option<String>,
    source: Option<InstallSource>,
    can_remove: bool,
    message: Option<Never>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
enum InstallPhase {
    Idle,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
enum InstallSource {
    Local,
}

/// The installation state when nothing is being installed.
async fn idle_install_state(ctx: &ConnCtx) -> InstallState {
    let statuses = ctx.state.providers.snapshot().await;
    let version = statuses.iter().find(|s| s.instance_id.as_str() == CODEX).and_then(|s| s.version.clone());
    InstallState {
        driver: CODEX,
        operation_id: None,
        phase: InstallPhase::Idle,
        downloaded_bytes: 0,
        total_bytes: None,
        version: None,
        source: version.is_some().then_some(InstallSource::Local),
        installed_version: version,
        can_remove: false,
        message: None,
    }
}

/// An install state of either driver that has one.
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum AnyInstall {
    Codex(InstallState),
    Antigravity(crate::antigravity::install::InstallState),
}

/// `ProviderInstallCancelInput`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelInstallInput {
    instance_id: String,
    #[serde(default)]
    operation_id: String,
}

/// `ProviderAuthCompleteInput`, `ProviderAuthCancelInput` (and the instance of any other).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FlowInput {
    instance_id: String,
    #[serde(default)]
    flow_id: String,
    #[serde(default)]
    callback_url: String,
}

/// The sign-in controller of an Antigravity instance (the only driver here
/// with one): `None` for any other instance.
fn antigravity(id: &str) -> Option<&str> {
    crate::antigravity::is_instance(id).then_some(id)
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    let r = r
        .stream("provider.auth.subscribe", |ctx: Arc<ConnCtx>, input: InstanceInput| {
            if let Some(id) = antigravity(&input.instance_id) {
                return crate::antigravity::auth::subscribe(id, ctx.session.token_hash.clone()).map(Ok::<_, Failure>).boxed();
            }
            futures::stream::once(async move { Err(auth_controller(&ctx, &input.instance_id, "subscribe").await) }).boxed()
        })
        .unary("provider.auth.start", |ctx: Arc<ConnCtx>, input: InstanceInput| async move {
            match antigravity(&input.instance_id) {
                Some(id) => crate::antigravity::auth::start(id, &ctx.session.token_hash).await,
                None => Err(auth_controller(&ctx, &input.instance_id, "start").await),
            }
        })
        .unary("provider.auth.complete", |ctx: Arc<ConnCtx>, input: FlowInput| async move {
            match antigravity(&input.instance_id) {
                Some(id) => crate::antigravity::auth::complete(id, &ctx.session.token_hash, &input.flow_id, &input.callback_url).await,
                None => Err(auth_controller(&ctx, &input.instance_id, "complete").await),
            }
        })
        .unary("provider.auth.cancel", |ctx: Arc<ConnCtx>, input: FlowInput| async move {
            match antigravity(&input.instance_id) {
                Some(id) => crate::antigravity::auth::cancel(id, &ctx.session.token_hash, &input.flow_id).await,
                None => Err(auth_controller(&ctx, &input.instance_id, "cancel").await),
            }
        })
        .unary("provider.auth.logout", |ctx: Arc<ConnCtx>, input: InstanceInput| async move {
            match antigravity(&input.instance_id) {
                Some(id) => crate::antigravity::auth::logout(id).await,
                None => Err(auth_controller(&ctx, &input.instance_id, "logout").await),
            }
        });
    r.unary("provider.install.start", |ctx: Arc<ConnCtx>, input: InstanceInput| async move {
        if let Some(id) = antigravity(&input.instance_id) {
            return crate::antigravity::install::start(id).await.map(AnyInstall::Antigravity);
        }
        managed_codex(&ctx, &input.instance_id, "install", true).await?;
        Err(Failure::setup(&input.instance_id, "install", "Managed Codex installation is not available on this server."))
    })
    .unary("provider.install.remove", |ctx: Arc<ConnCtx>, input: InstanceInput| async move {
        if let Some(id) = antigravity(&input.instance_id) {
            return crate::antigravity::install::remove(id).map(AnyInstall::Antigravity);
        }
        managed_codex(&ctx, &input.instance_id, "remove-install", true).await?;
        Err(Failure::setup(&input.instance_id, "remove-install", "There is no managed Codex installation to remove."))
    })
    .unary("provider.install.cancel", |ctx: Arc<ConnCtx>, input: CancelInstallInput| async move {
        if let Some(id) = antigravity(&input.instance_id) {
            return crate::antigravity::install::cancel(id, &input.operation_id).map(AnyInstall::Antigravity);
        }
        managed_codex(&ctx, &input.instance_id, "cancel-install", false).await?;
        // Nothing is ever installing, so there is nothing to cancel.
        Ok::<_, Failure>(AnyInstall::Codex(idle_install_state(&ctx).await))
    })
    .stream("provider.install.subscribe", |ctx: Arc<ConnCtx>, input: InstanceInput| {
        if antigravity(&input.instance_id).is_some() {
            return crate::antigravity::install::subscribe().map(|s| Ok::<_, Failure>(AnyInstall::Antigravity(s))).boxed();
        }
        async_stream::stream! {
            match managed_codex(&ctx, &input.instance_id, "observe-install", false).await {
                Err(e) => yield Err(e),
                Ok(_) => {
                    yield Ok(AnyInstall::Codex(idle_install_state(&ctx).await));
                    futures::future::pending::<()>().await;
                }
            }
        }
        .boxed()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn answers_are_written_as_t3code_records() {
        let idle = |installed_version: Option<String>, source| InstallState {
            driver: CODEX, operation_id: None, phase: InstallPhase::Idle, downloaded_bytes: 0, total_bytes: None, version: None,
            installed_version, source, can_remove: false, message: None,
        };
        assert_eq!(
            json!(idle(Some("0.166.0".into()), Some(InstallSource::Local))),
            json!({ "driver": "codex", "operationId": null, "phase": "idle", "downloadedBytes": 0, "totalBytes": null, "version": null,
                "installedVersion": "0.166.0", "source": "local", "canRemove": false, "message": null })
        );
        assert_eq!(
            json!(idle(None, None)),
            json!({ "driver": "codex", "operationId": null, "phase": "idle", "downloadedBytes": 0, "totalBytes": null, "version": null,
                "installedVersion": null, "source": null, "canRemove": false, "message": null })
        );
    }
}
