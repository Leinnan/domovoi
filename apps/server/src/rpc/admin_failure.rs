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

/// Why no environment was linked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerLinkReason {
    Unreachable,
    NotAT3Environment,
    Incompatible,
    #[serde(rename = "self")]
    OwnEnvironment,
    PairingRejected,
    Expired,
    UnknownLink,
    Storage,
    Disabled,
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
    /// A `reference` answer: Domovoi has no secret provider to read it from.
    ReferenceUnreadable,
}

/// Why the ACP registry did nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpRegistryReason {
    RegistryUnavailable,
    AgentNotFound,
    AgentNotConfigured,
    UnsupportedPlatform,
    RunnerUnavailable,
    InstallFailed,
    DownloadFailed,
    ArchiveInvalid,
    ChecksumMismatch,
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

    /// t3code `PreviewClearProfileError`.
    #[error("The environment could not delete this browser profile's data.")]
    #[serde(rename = "PreviewClearProfileError", rename_all = "camelCase")]
    PreviewClearProfile { profile_id: String, cause: Cause },

    #[error("the local server was not stopped ({reason:?})")]
    #[serde(rename = "PreviewStopLocalServerError")]
    StopLocalServer {
        reason: StopLocalServerReason,
        #[serde(skip_serializing_if = "Option::is_none")]
        cause: Option<Cause>,
    },

    #[error("{message}")]
    #[serde(rename = "PeerLinkError")]
    PeerLink {
        reason: PeerLinkReason,
        message: String,
        /// The environment that answered at the addresses of the one the user picked.
        #[serde(rename = "answeredAs", skip_serializing_if = "Option::is_none")]
        answered_as: Option<String>,
    },

    #[error("the secret request was not answered ({reason:?})")]
    #[serde(rename = "SecretRequestError")]
    SecretRequest {
        reason: SecretRequestReason,
        #[serde(skip_serializing_if = "Option::is_none")]
        cause: Option<Cause>,
    },

    #[error("{reason}")]
    #[serde(rename = "DatabaseReclaimError")]
    DatabaseReclaim { reason: String },

    #[error("{detail}")]
    #[serde(rename = "TextGeneratorError")]
    TextGenerator { detail: &'static str },

    #[error("{message}")]
    #[serde(rename = "AcpRegistryOperationError")]
    AcpRegistry { reason: AcpRegistryReason, message: String },

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
            json!(Failure::PeerLink { reason: PeerLinkReason::Disabled, message: "Off.".into(), answered_as: None }),
            json!({ "_tag": "PeerLinkError", "reason": "disabled", "message": "Off." })
        );
        assert_eq!(
            json!([PeerLinkReason::NotAT3Environment, PeerLinkReason::OwnEnvironment, PeerLinkReason::PairingRejected, PeerLinkReason::UnknownLink]),
            json!(["not_a_t3_environment", "self", "pairing_rejected", "unknown_link"])
        );
        assert_eq!(
            json!(Failure::PeerLink { reason: PeerLinkReason::Unreachable, message: "m".into(), answered_as: Some("Other".into()) }),
            json!({ "_tag": "PeerLinkError", "reason": "unreachable", "message": "m", "answeredAs": "Other" })
        );
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
        assert_eq!(json!(Failure::DatabaseReclaim { reason: "database is locked".into() }), json!({ "_tag": "DatabaseReclaimError", "reason": "database is locked" }));
        assert_eq!(json!(Failure::text_generator("Not configured.")), json!({ "_tag": "TextGeneratorError", "detail": "Not configured." }));
        assert_eq!(json!(Failure::TelemetryUnavailable { message: "No telemetry." }), json!({ "message": "No telemetry." }));
    }
}
