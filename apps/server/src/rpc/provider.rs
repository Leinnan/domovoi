//! `provider.*` setup RPCs, after t3code's `ProviderAuthService`,
//! `ProviderInstallation`, `subscribeChatGptHandoff` and the
//! `provider.uploadFeedback` / `provider.consumeResetCredit` handlers.
//!
//! Antigravity instances sign in here (`crate::antigravity::auth`).
//!
//! Domovoi runs the plain Codex and Claude drivers. In t3code those drivers have no
//! sign-in controller (sign-in happens in the CLIs) and only Codex in managed
//! setup mode has a managed installation, so the auth methods answer with
//! t3code's typed "does not support sign-in" errors. Differences:
//! - managed Codex installation (a download into the server home) is not
//!   implemented: subscribing shows an idle state, starting it fails;
//! - the ChatGPT handoff (an OAuth flow against ChatGPT) is refused;
//! - reset credits are never offered (Domovoi's snapshots carry no usage limits),
//!   so consuming one answers `noCredit`.

#![expect(clippy::disallowed_types, reason = "a provider instance's `config`: the driver's own settings, which the schema leaves open")]

use super::Never;
use crate::config_failure::Failure;
use crate::providers::{CLAUDE, CODEX};
use crate::state::ConnCtx;
use effect_rpc::Router;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use domovoi_model::typed::ProviderThread;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstanceInput {
    instance_id: String,
}

/// An instance Domovoi knows: the built-in Codex and Claude instances, plus
/// configured instances of those drivers.
struct Instance {
    driver: String,
    enabled: bool,
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
    let enabled = configured.and_then(|c| c.enabled).or(of_config("enabled").and_then(Value::as_bool)).unwrap_or(true);
    Some(Instance {
        driver,
        enabled,
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

/// A stream that fails at once.
fn failing_stream(e: Failure) -> BoxStream<'static, Result<Never, Failure>> {
    futures::stream::once(async move { Err(e) }).boxed()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResetCreditInput {
    instance_id: Option<String>,
    source_id: Option<String>,
}

/// The answer of `provider.consumeResetCredit` (`ProviderConsumeResetCreditResult`).
#[derive(Debug, Serialize)]
struct ResetCreditResult {
    outcome: ResetCreditOutcome,
}

/// `ProviderConsumeResetCreditOutcome`: Domovoi offers no reset credits.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
enum ResetCreditOutcome {
    NoCredit,
}

async fn consume_reset_credit(ctx: Arc<ConnCtx>, input: ResetCreditInput) -> Result<ResetCreditResult, Failure> {
    if let Some(source) = input.source_id {
        let settings = ctx.state.settings.read().await;
        // Only a source whose `enabled` is set counts as on here.
        let enabled = settings.usage_limit_sources.as_ref().and_then(|sources| sources.get(&source)).and_then(|config| config.enabled) == Some(true);
        let detail = if enabled { "Usage limit sources are not supported by this server." } else { "The usage limit source is missing or disabled." };
        return Err(Failure::UsageLimitSource { detail: detail.to_owned() });
    }
    let id = input.instance_id.unwrap_or_default();
    match instance(&ctx, &id).await {
        None => Err(Failure::setup(&id, "consume-reset-credit", "Provider instance not found.")),
        Some(i) if !i.enabled => Err(Failure::setup(&id, "consume-reset-credit", "This provider is disabled.")),
        Some(_) => Ok(ResetCreditResult { outcome: ResetCreditOutcome::NoCredit }),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FeedbackInput {
    thread_id: String,
    reason: Option<String>,
}

/// The answer of `provider.uploadFeedback` (`ProviderUploadFeedbackResult`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FeedbackResult {
    feedback_id: String,
}

/// `provider.uploadFeedback`: Codex uploads through its live app-server; the
/// Claude CLI has no feedback upload.
async fn upload_feedback(ctx: Arc<ConnCtx>, input: FeedbackInput) -> Result<FeedbackResult, Failure> {
    let err = |cause: &str| Failure::UploadFeedback { thread_id: input.thread_id.clone(), cause: cause.to_owned() };
    let store = &ctx.state.store;
    let thread = store.app_thread(&input.thread_id).map_err(|e| err(&e.to_string()))?;
    let Some(thread) = thread.filter(|t| t.deleted_at.is_none()) else {
        return Err(err(&format!("Thread {} was not found.", input.thread_id)));
    };
    let threads = store.records::<ProviderThread>(&input.thread_id).map_err(|e| err(&e.to_string()))?;
    let provider_thread = threads.iter().find(|p| Some(&p.id) == thread.active_provider_thread_id.as_ref()).or(threads.last());
    let Some(pt) = provider_thread.filter(|p| p.provider_session_id.is_some()) else {
        return Err(err("No provider session has run in this thread yet."));
    };
    let not_running = "The provider session is no longer running. Send a message first.";
    let host = crate::adapters::HOST.get();
    match pt.driver.as_str() {
        "codex" => {
            let server = match host {
                Some(h) => h.live_codex().await,
                None => None,
            };
            let (Some(server), Some(native)) = (server, pt.native_thread_ref.as_ref().and_then(|native| native.native_id.as_deref())) else {
                return Err(err(not_running));
            };
            let id = server.upload_feedback(native, input.reason.as_deref()).await.map_err(|_| err("Failed to upload Codex thread feedback."))?;
            Ok(FeedbackResult { feedback_id: id })
        }
        driver => {
            let live = match (host, driver) {
                (Some(h), CLAUDE) => h.has_live_claude(&input.thread_id).await,
                (Some(h), crate::providers::PI) => h.has_live_pi(&input.thread_id).await,
                _ => false,
            };
            if live {
                Err(err(&format!("Provider '{driver}' does not support feedback uploads.")))
            } else {
                Err(err(not_running))
            }
        }
    }
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
        .unary("provider.consumeResetCredit", consume_reset_credit)
        .unary("provider.uploadFeedback", upload_feedback)
        .stream("provider.chatgpt.handoff.subscribe", |_ctx: Arc<ConnCtx>, input: InstanceInput| {
            failing_stream(Failure::setup(&input.instance_id, "start", "ChatGPT sign-in handoff is not available on this server."))
        })
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
    // Every other sign-in operation fails at the controller lookup (or, for
    // Antigravity, takes nothing its sign-in asks).
    let auth_ops = [
        ("provider.auth.respond", "respond"),
        ("provider.auth.removeMethod", "removeMethod"),
        ("provider.chatgpt.reconnect-profile", "export"),
        ("provider.chatgpt.import-profile", "import"),
    ];
    let r = auth_ops.into_iter().fold(r, |r, (tag, operation)| {
        r.unary(tag, move |ctx: Arc<ConnCtx>, input: InstanceInput| async move {
            Err::<Never, _>(auth_controller(&ctx, &input.instance_id, operation).await)
        })
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
        assert_eq!(json!(ResetCreditResult { outcome: ResetCreditOutcome::NoCredit }), json!({ "outcome": "noCredit" }));
        assert_eq!(json!(FeedbackResult { feedback_id: "f1".into() }), json!({ "feedbackId": "f1" }));
    }
}
