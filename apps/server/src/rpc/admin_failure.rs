//! Why a call of the administration RPC groups failed, as the client is told:
//! each a tagged error of t3code's, with the fields that error carries. The
//! groups are preview tabs and local servers (`preview.ts`), devices
//! (`device.ts`), what reaches beyond this machine (`relayClient.ts`,
//! `peerLink.ts`, `guestShare.ts`, `secretRequest.ts`, `mcpApps.ts`) and the
//! server's own administration (`server.ts`, `settings.ts`, `acpRegistry.ts`).

use serde::Serialize;

/// What caused a failure, written as t3code writes a defect (`Schema.Defect`):
/// an object with the cause's message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Cause {
    pub message: String,
}

impl Cause {
    pub fn of(message: impl Into<String>) -> Self {
        Cause { message: message.into() }
    }
}

/// The cause of a refused preview URL: t3code's `PreviewUrlNormalizationError`, by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UrlCause {
    pub name: &'static str,
    pub message: String,
}

/// Why a preview URL was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum InvalidUrlReason {
    Empty,
    Parse,
    UnsupportedProtocol,
}

impl InvalidUrlReason {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            InvalidUrlReason::Empty => "empty",
            InvalidUrlReason::Parse => "parse",
            InvalidUrlReason::UnsupportedProtocol => "unsupported-protocol",
        }
    }
}

/// Why a local server was not stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StopLocalServerReason {
    /// No listed server has this pid and port.
    NotFound,
    /// Its process group also holds this server, an agent or a terminal shell.
    SharedGroup,
    /// This platform has no process groups to signal.
    Unsupported,
    SignalFailed,
}

/// Why a device tool was not updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceOperationReason {
    CommandFailed,
}

/// Why the relay client was not installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayInstallReason {
    UnsupportedPlatform,
    /// Domovoi downloads nothing: the client is for whoever runs the server to provide.
    ManagedByPolicy,
}

/// Why no environment was linked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerLinkReason {
    Disabled,
}

/// Why a link request card was not answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerLinkRequestReason {
    LoadFailed,
    NotFound,
    AlreadyAnswered,
    RecordFailed,
}

/// Why no guest share was created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GuestShareReason {
    Disabled,
    /// Sharing is on, but Domovoi has no T3 Connect to share through.
    ConnectUnavailable,
}

/// Why a secret request card was not answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretRequestReason {
    LoadFailed,
    NotFound,
    AlreadyAnswered,
    /// The run that asked is over.
    AgentStopped,
    RecordFailed,
    StoreFailed,
}

/// Why an MCP App's request was not carried out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpAppReason {
    /// The item is an app's, but Domovoi has no bridge to its provider.
    ProviderUnsupported,
    NotAnApp,
}

/// Why the ACP registry did nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpRegistryReason {
    RegistryUnavailable,
}

/// A failed call of the administration groups, written as its tagged error.
#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "_tag")]
pub enum Failure {
    #[error("Unknown preview session: thread={thread_id}, tab={tab_id}")]
    #[serde(rename = "PreviewSessionLookupError", rename_all = "camelCase")]
    PreviewSessionLookup { thread_id: String, tab_id: String },

    #[error("{}", cause.message)]
    #[serde(rename = "PreviewInvalidUrlError", rename_all = "camelCase")]
    PreviewInvalidUrl {
        /// In UTF-16 code units, as t3code counts a string's length.
        input_length: usize,
        reason: InvalidUrlReason,
        #[serde(skip_serializing_if = "Option::is_none")]
        protocol: Option<String>,
        cause: UrlCause,
    },

    #[error("the tab {tab_id} is driven by the server's browser")]
    #[serde(rename = "PreviewControlRequiredError", rename_all = "camelCase")]
    PreviewControlRequired { tab_id: String },

    #[error("the local server was not stopped ({reason:?})")]
    #[serde(rename = "PreviewStopLocalServerError")]
    StopLocalServer {
        reason: StopLocalServerReason,
        #[serde(skip_serializing_if = "Option::is_none")]
        cause: Option<Cause>,
    },

    #[error("{reason}")]
    #[serde(rename = "DeviceHostUnavailableError", rename_all = "camelCase")]
    DeviceHostUnavailable {
        host_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        host_label: Option<String>,
        reason: String,
    },

    #[error("{operation}: {}", cause.message)]
    #[serde(rename = "DeviceOperationError")]
    DeviceOperation { operation: String, reason: DeviceOperationReason, cause: Cause },

    #[error("{reason}")]
    #[serde(rename = "DeviceEasAccountError")]
    DeviceEasAccount { reason: &'static str },

    #[error("{reason}")]
    #[serde(rename = "DeviceSimlockIdentityError")]
    DeviceSimlockIdentity { reason: &'static str },

    #[error("{message}")]
    #[serde(rename = "RelayClientInstallFailedError")]
    RelayClientInstall { reason: RelayInstallReason, message: String },

    #[error("{message}")]
    #[serde(rename = "PeerLinkError")]
    PeerLink { reason: PeerLinkReason, message: &'static str },

    #[error("the link request was not answered ({reason:?})")]
    #[serde(rename = "PeerLinkRequestError")]
    PeerLinkRequest {
        reason: PeerLinkRequestReason,
        #[serde(skip_serializing_if = "Option::is_none")]
        cause: Option<Cause>,
    },

    #[error("no guest share was created ({reason:?})")]
    #[serde(rename = "GuestShareCreateError")]
    GuestShareCreate { reason: GuestShareReason },

    #[error("the secret request was not answered ({reason:?})")]
    #[serde(rename = "SecretRequestError")]
    SecretRequest {
        reason: SecretRequestReason,
        #[serde(skip_serializing_if = "Option::is_none")]
        cause: Option<Cause>,
    },

    #[error("the MCP App request of thread {thread_id} was not carried out ({reason:?})")]
    #[serde(rename = "McpAppRequestError", rename_all = "camelCase")]
    McpAppRequest { thread_id: String, reason: McpAppReason },

    #[error("{reason}")]
    #[serde(rename = "DatabaseReclaimError")]
    DatabaseReclaim { reason: String },

    #[error("{detail}")]
    #[serde(rename = "TextGeneratorError")]
    TextGenerator { detail: &'static str },

    #[error("Server update failed: {reason}")]
    #[serde(rename = "ServerSelfUpdateError")]
    ServerSelfUpdate { reason: &'static str },

    #[error("{message}")]
    #[serde(rename = "AcpRegistryOperationError")]
    AcpRegistry { reason: AcpRegistryReason, message: &'static str },

    /// The one failure t3code has no tagged error for: its telemetry stream
    /// cannot fail this way. Written as the message alone.
    #[error("{message}")]
    #[serde(untagged)]
    TelemetryUnavailable { message: &'static str },
}

impl Failure {
    /// No tab `tab_id` is open in the thread.
    pub fn no_session(thread_id: &str, tab_id: &str) -> Self {
        Failure::PreviewSessionLookup { thread_id: thread_id.to_owned(), tab_id: tab_id.to_owned() }
    }

    /// The local server was not stopped, with what the system said when there is something.
    pub fn not_stopped(reason: StopLocalServerReason, cause: Option<String>) -> Self {
        Failure::StopLocalServer { reason, cause: cause.map(Cause::of) }
    }

    /// The text generator's model list could not be had.
    pub fn text_generator(detail: &'static str) -> Self {
        Failure::TextGenerator { detail }
    }

    /// Domovoi is not installed in a way it could update itself.
    pub fn self_update() -> Self {
        Failure::ServerSelfUpdate { reason: "This server is not installed from npm or the desktop app, so it cannot update itself." }
    }

    /// Domovoi runs no ACP agents.
    pub fn acp_registry() -> Self {
        Failure::AcpRegistry { reason: AcpRegistryReason::RegistryUnavailable, message: "This server does not run ACP registry agents." }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn failures_are_written_as_the_tagged_errors_of_t3code() {
        assert_eq!(json!(Failure::no_session("t1", "tab_1")), json!({ "_tag": "PreviewSessionLookupError", "threadId": "t1", "tabId": "tab_1" }));
        let cause = UrlCause { name: "PreviewUrlNormalizationError", message: "Invalid".into() };
        assert_eq!(
            json!(Failure::PreviewInvalidUrl { input_length: 3, reason: InvalidUrlReason::Empty, protocol: None, cause: cause.clone() }),
            json!({ "_tag": "PreviewInvalidUrlError", "inputLength": 3, "reason": "empty", "cause": { "name": "PreviewUrlNormalizationError", "message": "Invalid" } })
        );
        for (reason, spelled) in [(InvalidUrlReason::Parse, "parse"), (InvalidUrlReason::UnsupportedProtocol, "unsupported-protocol"), (InvalidUrlReason::Empty, "empty")] {
            assert_eq!(reason.as_str(), spelled);
            assert_eq!(
                json!(Failure::PreviewInvalidUrl { input_length: 9, reason, protocol: Some("file:".into()), cause: cause.clone() }),
                json!({ "_tag": "PreviewInvalidUrlError", "inputLength": 9, "reason": spelled, "protocol": "file:",
                    "cause": { "name": "PreviewUrlNormalizationError", "message": "Invalid" } })
            );
        }
        assert_eq!(json!(Failure::PreviewControlRequired { tab_id: "tab_1".into() }), json!({ "_tag": "PreviewControlRequiredError", "tabId": "tab_1" }));

        let reasons = [
            (StopLocalServerReason::NotFound, "not-found"), (StopLocalServerReason::SharedGroup, "shared-group"),
            (StopLocalServerReason::Unsupported, "unsupported"), (StopLocalServerReason::SignalFailed, "signal-failed"),
        ];
        for (reason, spelled) in reasons {
            assert_eq!(json!(Failure::not_stopped(reason, None)), json!({ "_tag": "PreviewStopLocalServerError", "reason": spelled }));
        }
        assert_eq!(
            json!(Failure::not_stopped(StopLocalServerReason::SignalFailed, Some("ps failed".into()))),
            json!({ "_tag": "PreviewStopLocalServerError", "reason": "signal-failed", "cause": { "message": "ps failed" } })
        );

        assert_eq!(
            json!(Failure::DeviceHostUnavailable { host_id: "ssh-1".into(), host_label: None, reason: "no hub".into() }),
            json!({ "_tag": "DeviceHostUnavailableError", "hostId": "ssh-1", "reason": "no hub" })
        );
        assert_eq!(
            json!(Failure::DeviceHostUnavailable { host_id: "local".into(), host_label: Some("This machine".into()), reason: "no hub".into() }),
            json!({ "_tag": "DeviceHostUnavailableError", "hostId": "local", "hostLabel": "This machine", "reason": "no hub" })
        );
        assert_eq!(
            json!(Failure::DeviceOperation { operation: "update hub".into(), reason: DeviceOperationReason::CommandFailed, cause: Cause::of("no hub") }),
            json!({ "_tag": "DeviceOperationError", "operation": "update hub", "reason": "command_failed", "cause": { "message": "no hub" } })
        );
        assert_eq!(json!(Failure::DeviceEasAccount { reason: "no EAS" }), json!({ "_tag": "DeviceEasAccountError", "reason": "no EAS" }));
        assert_eq!(json!(Failure::DeviceSimlockIdentity { reason: "no simlock" }), json!({ "_tag": "DeviceSimlockIdentityError", "reason": "no simlock" }));

        for (reason, spelled) in [(RelayInstallReason::UnsupportedPlatform, "unsupported_platform"), (RelayInstallReason::ManagedByPolicy, "managed_by_policy")] {
            assert_eq!(
                json!(Failure::RelayClientInstall { reason, message: "Install it.".into() }),
                json!({ "_tag": "RelayClientInstallFailedError", "reason": spelled, "message": "Install it." })
            );
        }
        assert_eq!(
            json!(Failure::PeerLink { reason: PeerLinkReason::Disabled, message: "Off." }),
            json!({ "_tag": "PeerLinkError", "reason": "disabled", "message": "Off." })
        );
        let reasons = [
            (PeerLinkRequestReason::LoadFailed, "load_failed"), (PeerLinkRequestReason::NotFound, "not_found"),
            (PeerLinkRequestReason::AlreadyAnswered, "already_answered"), (PeerLinkRequestReason::RecordFailed, "record_failed"),
        ];
        for (reason, spelled) in reasons {
            assert_eq!(json!(Failure::PeerLinkRequest { reason, cause: None }), json!({ "_tag": "PeerLinkRequestError", "reason": spelled }));
        }
        assert_eq!(
            json!(Failure::PeerLinkRequest { reason: PeerLinkRequestReason::LoadFailed, cause: Some(Cause::of("gone")) }),
            json!({ "_tag": "PeerLinkRequestError", "reason": "load_failed", "cause": { "message": "gone" } })
        );
        for (reason, spelled) in [(GuestShareReason::Disabled, "disabled"), (GuestShareReason::ConnectUnavailable, "connect_unavailable")] {
            assert_eq!(json!(Failure::GuestShareCreate { reason }), json!({ "_tag": "GuestShareCreateError", "reason": spelled }));
        }
        let reasons = [
            (SecretRequestReason::LoadFailed, "load_failed"), (SecretRequestReason::NotFound, "not_found"),
            (SecretRequestReason::AlreadyAnswered, "already_answered"), (SecretRequestReason::AgentStopped, "agent_stopped"),
            (SecretRequestReason::RecordFailed, "record_failed"), (SecretRequestReason::StoreFailed, "store_failed"),
        ];
        for (reason, spelled) in reasons {
            assert_eq!(json!(Failure::SecretRequest { reason, cause: None }), json!({ "_tag": "SecretRequestError", "reason": spelled }));
        }
        assert_eq!(
            json!(Failure::SecretRequest { reason: SecretRequestReason::StoreFailed, cause: Some(Cause::of("no store")) }),
            json!({ "_tag": "SecretRequestError", "reason": "store_failed", "cause": { "message": "no store" } })
        );
        for (reason, spelled) in [(McpAppReason::ProviderUnsupported, "provider-unsupported"), (McpAppReason::NotAnApp, "not-an-app")] {
            assert_eq!(
                json!(Failure::McpAppRequest { thread_id: "t1".into(), reason }),
                json!({ "_tag": "McpAppRequestError", "threadId": "t1", "reason": spelled })
            );
        }

        assert_eq!(json!(Failure::DatabaseReclaim { reason: "database is locked".into() }), json!({ "_tag": "DatabaseReclaimError", "reason": "database is locked" }));
        assert_eq!(json!(Failure::text_generator("Not configured.")), json!({ "_tag": "TextGeneratorError", "detail": "Not configured." }));
        assert_eq!(
            json!(Failure::self_update()),
            json!({ "_tag": "ServerSelfUpdateError", "reason": "This server is not installed from npm or the desktop app, so it cannot update itself." })
        );
        assert_eq!(
            json!(Failure::acp_registry()),
            json!({ "_tag": "AcpRegistryOperationError", "reason": "registry_unavailable", "message": "This server does not run ACP registry agents." })
        );
        assert_eq!(json!(Failure::TelemetryUnavailable { message: "No telemetry." }), json!({ "message": "No telemetry." }));
    }
}
