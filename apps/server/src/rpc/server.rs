//! Server-level RPCs: config/lifecycle streams, providers and client activity
//! (previews, devices and project clones live in their own modules).

use super::{Never, then_pending};
use crate::settings::Settings;
use crate::state::ConnCtx;
use domovoi_rpc::Router;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::de::IgnoredAny;
use serde::Deserialize;
use std::sync::Arc;
use domovoi_model::typed::{
    EnvironmentTheme, ServerConfigEnvironmentThemesUpdatedPayload, ServerConfigProviderStatusesPayload, ServerConfigSettingsUpdatedPayloadOf,
    ServerConfigStreamEnvironmentThemesUpdatedEvent, ServerConfigStreamEventOf, ServerConfigStreamKeybindingsUpdatedEvent,
    ServerConfigStreamProviderStatusesEvent, ServerConfigStreamSettingsUpdatedEventOf, ServerConfigStreamSnapshotEventOf,
    ServerConfigStreamUsageLimitSourcesUpdatedEvent, ServerConfigUsageLimitSourcesUpdatedPayload, ServerLifecycleReadyPayload,
    ServerLifecycleStreamEvent, ServerLifecycleStreamReadyEvent, ServerLifecycleStreamWelcomeEvent, ServerLifecycleWelcomePayload, ServerProvider,
    ServerProviderUpdatedPayload, UsageLimitSourceSnapshot,
};
use tokio_stream::wrappers::BroadcastStream;

/// One item of `subscribeServerConfig` as the server sends it: the settings
/// in it are the settings file's, keys of another build included.
type ConfigEvent = ServerConfigStreamEventOf<Settings>;

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ConfigSubscription {
    #[serde(default)]
    environment_themes: bool,
    #[serde(default)]
    usage_limit_sources: bool,
}

fn providers_event(providers: Vec<ServerProvider>) -> ConfigEvent {
    ConfigEvent::ProviderStatuses(ServerConfigStreamProviderStatusesEvent { version: 1, payload: ServerConfigProviderStatusesPayload { providers: providers.into() } })
}

fn settings_event(settings: Settings) -> ConfigEvent {
    ConfigEvent::SettingsUpdated(Box::new(ServerConfigStreamSettingsUpdatedEventOf { version: 1, payload: ServerConfigSettingsUpdatedPayloadOf { settings } }))
}

fn keybindings_event(resolved: crate::keybindings::Resolved) -> ConfigEvent {
    ConfigEvent::KeybindingsUpdated(ServerConfigStreamKeybindingsUpdatedEvent { version: 1, payload: resolved.into() })
}

/// Domovoi reads no usage limit sources: the list a client asks for is empty.
fn no_sources_event() -> ConfigEvent {
    let sources: Vec<UsageLimitSourceSnapshot> = vec![];
    ConfigEvent::UsageLimitSourcesUpdated(ServerConfigStreamUsageLimitSourcesUpdatedEvent { version: 1, payload: ServerConfigUsageLimitSourcesUpdatedPayload { sources: sources.into() } })
}

/// Domovoi publishes no themes: the list a client asks for is empty.
fn no_themes_event() -> ConfigEvent {
    let themes: Vec<EnvironmentTheme> = vec![];
    ConfigEvent::EnvironmentThemesUpdated(ServerConfigStreamEnvironmentThemesUpdatedEvent { version: 1, payload: ServerConfigEnvironmentThemesUpdatedPayload { themes } })
}

fn config_stream(ctx: Arc<ConnCtx>, input: ConfigSubscription) -> BoxStream<'static, Result<ConfigEvent, Never>> {
    let state = ctx.state.clone();
    async_stream::stream! {
        let updates = state.provider_updates.subscribe();
        let settings_rx = crate::settings::subscribe();
        let keybindings_rx = crate::keybindings::subscribe();
        let config = state.server_config().await;
        yield Ok(ConfigEvent::Snapshot(Box::new(ServerConfigStreamSnapshotEventOf { version: 1, config })));
        if input.usage_limit_sources {
            yield Ok(no_sources_event());
        }
        if input.environment_themes {
            yield Ok(no_themes_event());
        }
        let mut providers = BroadcastStream::new(updates);
        let mut settings = BroadcastStream::new(settings_rx);
        let mut keybindings = BroadcastStream::new(keybindings_rx);
        loop {
            tokio::select! {
                Some(next) = providers.next() => if let Ok(providers) = next {
                    yield Ok(providers_event(providers));
                },
                Some(next) = settings.next() => if let Ok(settings) = next {
                    yield Ok(settings_event(settings));
                },
                Some(next) = keybindings.next() => if let Ok(resolved) = next {
                    yield Ok(keybindings_event(resolved));
                },
                else => return,
            }
        }
    }
    .boxed()
}

fn lifecycle_stream(ctx: Arc<ConnCtx>) -> BoxStream<'static, Result<ServerLifecycleStreamEvent, Never>> {
    let s = &ctx.state;
    let project_name = std::path::Path::new(&s.cwd)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "workspace".into());
    then_pending(vec![
        ServerLifecycleStreamEvent::Welcome(Box::new(ServerLifecycleStreamWelcomeEvent {
            version: 1,
            sequence: 1,
            payload: ServerLifecycleWelcomePayload {
                environment: (&s.environment).into(),
                cwd: s.cwd.clone(),
                project_name,
                bootstrap_status: None,
                bootstrap_project_id: None,
                bootstrap_thread_id: None,
                bootstrap_project_created: None,
                bootstrap_thread_created: None,
            },
        })),
        ServerLifecycleStreamEvent::Ready(Box::new(ServerLifecycleStreamReadyEvent {
            version: 1,
            sequence: 2,
            payload: ServerLifecycleReadyPayload { at: crate::time_fmt::iso_now().into(), environment: (&s.environment).into(), update_outcome: None },
        })),
    ])
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RefreshProvidersInput {
    instance_id: Option<String>,
}

async fn refresh_providers(ctx: Arc<ConnCtx>, input: RefreshProvidersInput) -> Result<ServerProviderUpdatedPayload, Never> {
    let all = ctx.state.providers.refresh(input.instance_id.as_deref()).await;
    let _ = ctx.state.provider_updates.send(all.clone());
    let providers: Vec<ServerProvider> = match &input.instance_id {
        Some(id) => all.into_iter().filter(|p| p.instance_id.as_str() == id).collect(),
        None => all,
    };
    Ok(ServerProviderUpdatedPayload { providers: providers.into() })
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.stream("subscribeServerConfig", |ctx, input: ConfigSubscription| config_stream(ctx, input))
        .stream("subscribeServerLifecycle", |ctx, _: IgnoredAny| lifecycle_stream(ctx))
        .unary("server.refreshProviders", refresh_providers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The events after the snapshot, as t3code's `ServerConfigStreamEvent` spells them.
    #[test]
    fn config_events_are_written_as_stream_events() {
        assert_eq!(json!(no_sources_event()), json!({ "type": "usageLimitSourcesUpdated", "version": 1, "payload": { "sources": [] } }));
        assert_eq!(json!(no_themes_event()), json!({ "type": "environmentThemesUpdated", "version": 1, "payload": { "themes": [] } }));
        assert_eq!(json!(providers_event(vec![])), json!({ "type": "providerStatuses", "version": 1, "payload": { "providers": [] } }));
        let document = json!({ "defaultAutoPull": true, "futureSetting": { "level": 3 }, "branchNamePrefix": 5 });
        let settings = Settings::from_document(document.as_object().unwrap().clone());
        assert_eq!(json!(settings_event(settings)), json!({ "type": "settingsUpdated", "version": 1, "payload": { "settings": document } }));
        let resolved = crate::keybindings::Resolved { keybindings: vec![], issues: vec![] };
        assert_eq!(json!(keybindings_event(resolved)), json!({ "type": "keybindingsUpdated", "version": 1, "payload": { "keybindings": [], "issues": [] } }));
    }
}
