//! Device hub RPCs (`device.*`, `subscribeDeviceState`), after t3code's
//! `DeviceService`.
//!
//! t3code drives iOS simulators and Android emulators through a device hub and
//! agent tools it installs from npm (locally or over SSH), and through Expo's
//! EAS cloud and simlock gateways. domovoi-server installs nothing and reaches no
//! remote service, so every host is unavailable. What is real:
//! - the settings `device.configure` writes (`enableDeviceSupport`,
//!   `enableAgentDeviceAccess`, `deviceOnboardingCompleted`), and the state
//!   stream subscribers see change with them (revision increments);
//! - with support turned on, the local host is listed as `failed` with both
//!   platforms unavailable and the reason, and no devices or sessions;
//! - `device.close` with no matching session succeeds, as in t3code.
//!
//! Everything that needs a running host answers `DeviceHostUnavailableError`;
//! EAS and simlock identity checks answer their typed errors; SSH host tests
//! answer `DeviceHostUnavailableError` without connecting.

use super::Never;
use super::admin_failure::{Cause, DeviceOperationReason, Failure};
use crate::state::{AppState, ConnCtx};
use effect_rpc::Router;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

const LOCAL_HOST_ID: &str = "local";
const UNAVAILABLE: &str = "Device support is not available on this Domovoi server: it does not install or run the device hub.";

static REVISION: AtomicU64 = AtomicU64::new(0);
static UPDATES: LazyLock<broadcast::Sender<DeviceState>> = LazyLock::new(|| broadcast::channel(16).0);

/// Where a device host is in its life (`DeviceHostStatus`): Domovoi's is off, or has failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum HostStatus {
    Disabled,
    Failed,
}

/// The status of one host, with why it failed.
#[derive(Debug, Clone, Serialize)]
struct HostStatusEntry {
    status: HostStatus,
    detail: &'static str,
}

/// The kind of devices a host could run (`DevicePlatform`).
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum Platform {
    Ios,
    Android,
}

/// Whether a host can run a platform's devices, and why not (`DevicePlatformAvailability`).
#[derive(Debug, Clone, Serialize)]
struct PlatformAvailability {
    platform: Platform,
    available: bool,
    reason: &'static str,
}

/// Which kind of host this is: only ever this machine.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum HostKind {
    Local,
}

/// A device host as listed (`DeviceHostSummary`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HostSummary {
    id: &'static str,
    label: &'static str,
    platforms: [PlatformAvailability; 2],
    hub_installed: bool,
    agent_device_installed: bool,
    kind: HostKind,
}

/// What the device hub knows (`DeviceServiceState`, as it is written): no
/// device, session or remote host ever, and nothing it can retry or update.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceState {
    supports_host_retry: bool,
    supports_tool_update: bool,
    supports_tool_inspection: bool,
    supports_eas_hosts: bool,
    supports_simlock_hosts: bool,
    host_status: HostStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    host_status_detail: Option<&'static str>,
    /// By host id.
    host_statuses: BTreeMap<&'static str, HostStatusEntry>,
    devices: [Never; 0],
    sessions: [Never; 0],
    booting_devices: [Never; 0],
    onboarding_completed: bool,
    agent_access_enabled: bool,
    hub_base_path: &'static str,
    /// Counts the changes `device.configure` made.
    revision: u64,
    hosts: Vec<HostSummary>,
    eas_hosts: [Never; 0],
    simlock_hosts: [Never; 0],
}

/// The settings the device hub goes by.
#[derive(Debug, Clone, Copy, Default)]
pub struct Flags {
    /// `enableDeviceSupport`.
    pub enabled: bool,
    /// `enableAgentDeviceAccess`.
    pub agent_access: bool,
    /// `deviceOnboardingCompleted`.
    pub onboarding_completed: bool,
}

/// The `DeviceServiceState` for the settings as they are.
pub fn state_for(flags: Flags, revision: u64) -> DeviceState {
    let mut state = DeviceState {
        supports_host_retry: false,
        supports_tool_update: false,
        supports_tool_inspection: false,
        supports_eas_hosts: false,
        supports_simlock_hosts: false,
        host_status: HostStatus::Disabled,
        host_status_detail: None,
        host_statuses: BTreeMap::new(),
        devices: [],
        sessions: [],
        booting_devices: [],
        onboarding_completed: flags.onboarding_completed,
        agent_access_enabled: flags.agent_access,
        hub_base_path: "/api/device-hub",
        revision,
        hosts: vec![],
        eas_hosts: [],
        simlock_hosts: [],
    };
    if flags.enabled {
        let platforms = [Platform::Ios, Platform::Android].map(|platform| PlatformAvailability { platform, available: false, reason: UNAVAILABLE });
        state.host_status = HostStatus::Failed;
        state.host_status_detail = Some(UNAVAILABLE);
        state.host_statuses.insert(LOCAL_HOST_ID, HostStatusEntry { status: HostStatus::Failed, detail: UNAVAILABLE });
        state.hosts.push(HostSummary { id: LOCAL_HOST_ID, label: "This machine", platforms, hub_installed: false, agent_device_installed: false, kind: HostKind::Local });
    }
    state
}

async fn current(state: &AppState) -> DeviceState {
    let settings = state.settings.read().await;
    let flags = Flags {
        enabled: settings.enable_device_support.unwrap_or(false),
        agent_access: settings.enable_agent_device_access.unwrap_or(false),
        onboarding_completed: settings.device_onboarding_completed.unwrap_or(false),
    };
    state_for(flags, REVISION.load(Ordering::Relaxed))
}

fn host_unavailable(host_id: Option<&str>) -> Failure {
    let host_id = host_id.unwrap_or(LOCAL_HOST_ID);
    let host_label = (host_id == LOCAL_HOST_ID).then(|| "This machine".to_owned());
    Failure::DeviceHostUnavailable { host_id: host_id.to_owned(), host_label, reason: UNAVAILABLE.to_owned() }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfigureInput {
    enabled: Option<bool>,
    agent_access_enabled: Option<bool>,
    onboarding_completed: Option<bool>,
}

async fn configure(state: &AppState, input: ConfigureInput) -> DeviceState {
    {
        let mut settings = state.settings.write().await;
        settings.update(|s| {
            s.enable_device_support = input.enabled.or(s.enable_device_support);
            s.enable_agent_device_access = input.agent_access_enabled.or(s.enable_agent_device_access);
            s.device_onboarding_completed = input.onboarding_completed.or(s.device_onboarding_completed);
        });
        let _ = crate::settings::save(&state.paths.userdata, &settings);
    }
    REVISION.fetch_add(1, Ordering::Relaxed);
    let next = current(state).await;
    let _ = UPDATES.send(next.clone());
    next
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListInput {
    update_tool: Option<String>,
    retry_host_id: Option<String>,
}

async fn list(state: &AppState, input: ListInput) -> Result<DeviceState, Failure> {
    if let Some(tool) = input.update_tool {
        return Err(Failure::DeviceOperation { operation: format!("update {tool}"), reason: DeviceOperationReason::CommandFailed, cause: Cause::of(UNAVAILABLE) });
    }
    if let Some(host) = input.retry_host_id.filter(|h| h != LOCAL_HOST_ID) {
        return Err(host_unavailable(Some(&host)));
    }
    Ok(current(state).await)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HostRef {
    host_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TestHostInput {
    id: String,
    label: String,
}

fn state_stream(ctx: Arc<ConnCtx>) -> BoxStream<'static, Result<DeviceState, Never>> {
    let mut rx = UPDATES.subscribe();
    async_stream::stream! {
        yield Ok(current(&ctx.state).await);
        loop {
            match rx.recv().await {
                Ok(state) => yield Ok(state),
                Err(RecvError::Lagged(_)) => yield Ok(current(&ctx.state).await),
                Err(RecvError::Closed) => break,
            }
        }
    }
    .boxed()
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.unary("device.configure", |ctx: Arc<ConnCtx>, input: ConfigureInput| async move {
        Ok::<_, Never>(configure(&ctx.state, input).await)
    })
    .unary("device.list", |ctx: Arc<ConnCtx>, input: ListInput| async move { list(&ctx.state, input).await })
    .unary("device.testHost", |_, input: TestHostInput| async move {
        Err::<Never, _>(Failure::DeviceHostUnavailable {
            host_id: input.id,
            host_label: Some(input.label),
            reason: "This Domovoi server does not connect to SSH device hosts.".to_owned(),
        })
    })
    .unary("device.easAccount", |_, _: IgnoredAny| async {
        Err::<Never, _>(Failure::DeviceEasAccount { reason: "This Domovoi server does not connect to Expo EAS." })
    })
    .unary("device.simlockIdentity", |_, _: IgnoredAny| async {
        Err::<Never, _>(Failure::DeviceSimlockIdentity { reason: "This Domovoi server does not connect to simlock gateways." })
    })
    .unary("device.open", |_, input: HostRef| async move { Err::<Never, _>(host_unavailable(input.host_id.as_deref())) })
    // No session is ever open, so there is nothing to close.
    .unary("device.close", |_, _: HostRef| async { Ok::<_, Never>(()) })
    .unary("device.shutdown", |_, input: HostRef| async move { Err::<Never, _>(host_unavailable(input.host_id.as_deref())) })
    .unary("device.detail", |_, input: HostRef| async move { Err::<Never, _>(host_unavailable(input.host_id.as_deref())) })
    .unary("device.action", |_, input: HostRef| async move { Err::<Never, _>(host_unavailable(input.host_id.as_deref())) })
    .stream("subscribeDeviceState", |ctx, _: IgnoredAny| state_stream(ctx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn state_follows_settings() {
        let off = state_for(Flags { onboarding_completed: true, ..Flags::default() }, 3);
        assert_eq!(
            json!(off),
            json!({ "supportsHostRetry": false, "supportsToolUpdate": false, "supportsToolInspection": false, "supportsEasHosts": false,
                "supportsSimlockHosts": false, "hostStatus": "disabled", "hostStatuses": {}, "devices": [], "sessions": [], "bootingDevices": [],
                "onboardingCompleted": true, "agentAccessEnabled": false, "hubBasePath": "/api/device-hub", "revision": 3,
                "hosts": [], "easHosts": [], "simlockHosts": [] })
        );
        let on = json!(state_for(Flags { enabled: true, agent_access: true, onboarding_completed: false }, 4));
        assert_eq!(on["hostStatus"], "failed");
        assert_eq!(on["hostStatusDetail"], UNAVAILABLE);
        assert_eq!(on["hostStatuses"], json!({ "local": { "status": "failed", "detail": UNAVAILABLE } }));
        assert_eq!(
            on["hosts"],
            json!([{ "id": "local", "label": "This machine", "hubInstalled": false, "agentDeviceInstalled": false, "kind": "local", "platforms": [
                { "platform": "ios", "available": false, "reason": UNAVAILABLE }, { "platform": "android", "available": false, "reason": UNAVAILABLE }] }])
        );
        assert_eq!((&on["agentAccessEnabled"], &on["onboardingCompleted"], &on["revision"]), (&json!(true), &json!(false), &json!(4)));
    }

    #[test]
    fn a_host_other_than_this_machine_has_no_label() {
        assert_eq!(json!(host_unavailable(None)), json!({ "_tag": "DeviceHostUnavailableError", "hostId": "local", "hostLabel": "This machine", "reason": UNAVAILABLE }));
        assert_eq!(json!(host_unavailable(Some("ssh-1"))), json!({ "_tag": "DeviceHostUnavailableError", "hostId": "ssh-1", "reason": UNAVAILABLE }));
    }
}
