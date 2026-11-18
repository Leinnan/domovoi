//! Why a call of the pull request RPC group failed, as the client is told:
//! t3code's `PullRequestUnavailableError` (the host cannot be read at all) and
//! `PullRequestOperationError` (this one request went wrong), with the fields
//! those errors carry.

use crate::gh::{self, GhError, GhReason};
use serde::Serialize;
use domovoi_model::typed::PullRequestListResult;

/// The host every pull request is read from.
pub const PROVIDER: &str = "github";

/// Why a host cannot be read (`PullRequestUnavailableReason`). The names keep
/// t3code's `cli-` prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnavailableReason {
    CliMissing,
    CliUnauthenticated,
    ProviderUnsupported,
}

/// What an operation's failure was, where the host said (`PullRequestOperationError.reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OperationReason {
    NotFound,
}

/// A failed call of the pull request group, written as its tagged error.
#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "_tag")]
pub enum Failure {
    #[error("pull requests cannot be read here ({reason:?})")]
    #[serde(rename = "PullRequestUnavailableError", rename_all = "camelCase")]
    Unavailable {
        reason: UnavailableReason,
        /// The host's kind, where one was found.
        #[serde(skip_serializing_if = "Option::is_none")]
        provider: Option<&'static str>,
        /// What the host needs on the server, in its own words.
        #[serde(skip_serializing_if = "Option::is_none")]
        requirement: Option<&'static str>,
        /// For `pullRequests.list`: the listing with its hosts and no row.
        #[serde(skip_serializing_if = "Option::is_none")]
        list_result: Option<Box<PullRequestListResult>>,
    },

    #[error("Pull request operation {operation} failed: {detail}")]
    #[serde(rename = "PullRequestOperationError", rename_all = "camelCase")]
    Operation {
        operation: String,
        detail: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<OperationReason>,
        /// For `pullRequests.list`: the listing with its hosts and no row.
        #[serde(skip_serializing_if = "Option::is_none")]
        list_result: Option<Box<PullRequestListResult>>,
    },
}

impl Failure {
    /// The project's host is one whose pull requests cannot be browsed here.
    pub fn unsupported() -> Self {
        Failure::Unavailable { reason: UnavailableReason::ProviderUnsupported, provider: None, requirement: None, list_result: None }
    }

    /// `operation` went wrong, for the reason `detail` gives.
    pub fn operation(operation: &str, detail: impl Into<String>) -> Self {
        Failure::Operation { operation: operation.to_owned(), detail: detail.into(), reason: None, list_result: None }
    }

    /// `operation` asked for something the host does not have.
    pub fn not_found(operation: &str, detail: impl Into<String>) -> Self {
        Failure::Operation { operation: operation.to_owned(), detail: detail.into(), reason: Some(OperationReason::NotFound), list_result: None }
    }

    /// A failed `gh` under `operation` (t3code's `toPullRequestErrorWith`):
    /// without the tool or a credential the host is unavailable, anything
    /// else is this operation's failure.
    pub fn from_gh(operation: &str, e: &GhError) -> Self {
        let unavailable = |reason, requirement| Failure::Unavailable { reason, provider: Some(PROVIDER), requirement: Some(requirement), list_result: None };
        match e.reason {
            GhReason::MissingTool => unavailable(UnavailableReason::CliMissing, gh::REQUIREMENT_MISSING),
            GhReason::Unauthenticated => unavailable(UnavailableReason::CliUnauthenticated, gh::REQUIREMENT_UNAUTHENTICATED),
            GhReason::NotFound => Failure::not_found(operation, e.detail.clone()),
            _ => Failure::operation(operation, e.detail.clone()),
        }
    }

    /// The failure with the listing it leaves a client to show.
    pub fn with_list(mut self, list: PullRequestListResult) -> Self {
        match &mut self {
            Failure::Unavailable { list_result, .. } | Failure::Operation { list_result, .. } => *list_result = Some(Box::new(list)),
        }
        self
    }
}

/// What a host's row of a listing says of a failed `gh`.
pub fn provider_detail(e: &GhError) -> String {
    match e.reason {
        GhReason::MissingTool => gh::REQUIREMENT_MISSING.into(),
        GhReason::Unauthenticated => gh::REQUIREMENT_UNAUTHENTICATED.into(),
        _ => e.detail.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn gh_error(reason: GhReason) -> GhError {
        GhError { reason, detail: "what gh said".into() }
    }

    #[test]
    fn failures_are_written_as_the_tagged_errors_of_t3code() {
        assert_eq!(json!(Failure::unsupported()), json!({ "_tag": "PullRequestUnavailableError", "reason": "provider-unsupported" }));
        assert_eq!(
            json!(Failure::from_gh("detail", &gh_error(GhReason::MissingTool))),
            json!({ "_tag": "PullRequestUnavailableError", "reason": "cli-missing", "provider": "github", "requirement": gh::REQUIREMENT_MISSING })
        );
        assert_eq!(
            json!(Failure::from_gh("detail", &gh_error(GhReason::Unauthenticated))),
            json!({ "_tag": "PullRequestUnavailableError", "reason": "cli-unauthenticated", "provider": "github", "requirement": gh::REQUIREMENT_UNAUTHENTICATED })
        );
        assert_eq!(
            json!(Failure::from_gh("detail", &gh_error(GhReason::NotFound))),
            json!({ "_tag": "PullRequestOperationError", "operation": "detail", "detail": "what gh said", "reason": "not-found" })
        );
        for reason in [GhReason::RateLimited, GhReason::Forbidden, GhReason::Failed] {
            assert_eq!(
                json!(Failure::from_gh("comment", &gh_error(reason))),
                json!({ "_tag": "PullRequestOperationError", "operation": "comment", "detail": "what gh said" })
            );
        }
        assert_eq!(
            json!(Failure::operation("update", "Nothing was changed.")),
            json!({ "_tag": "PullRequestOperationError", "operation": "update", "detail": "Nothing was changed." })
        );
        assert_eq!(
            json!(Failure::not_found("activity", "The pull request was not found.")),
            json!({ "_tag": "PullRequestOperationError", "operation": "activity", "detail": "The pull request was not found.", "reason": "not-found" })
        );
    }

    #[test]
    fn a_failed_listing_carries_what_there_is_to_show() {
        let list = PullRequestListResult {
            viewers: BTreeMap::new(), providers: vec![], entries: vec![], errors: vec![], truncated: false, next_cursors: BTreeMap::new(),
        };
        let shown = json!({ "viewers": {}, "providers": [], "entries": [], "errors": [], "truncated": false, "nextCursors": {} });
        assert_eq!(
            json!(Failure::from_gh("list", &gh_error(GhReason::Unauthenticated)).with_list(list.clone())),
            json!({ "_tag": "PullRequestUnavailableError", "reason": "cli-unauthenticated", "provider": "github",
                    "requirement": gh::REQUIREMENT_UNAUTHENTICATED, "listResult": shown })
        );
        assert_eq!(
            json!(Failure::from_gh("list", &gh_error(GhReason::NotFound)).with_list(list.clone())),
            json!({ "_tag": "PullRequestOperationError", "operation": "list", "detail": "what gh said", "reason": "not-found", "listResult": shown })
        );
        assert_eq!(
            json!(Failure::from_gh("list", &gh_error(GhReason::Failed)).with_list(list)),
            json!({ "_tag": "PullRequestOperationError", "operation": "list", "detail": "what gh said", "listResult": shown })
        );
    }

    #[test]
    fn a_host_row_names_what_the_host_needs() {
        assert_eq!(provider_detail(&gh_error(GhReason::MissingTool)), gh::REQUIREMENT_MISSING);
        assert_eq!(provider_detail(&gh_error(GhReason::Unauthenticated)), gh::REQUIREMENT_UNAUTHENTICATED);
        assert_eq!(provider_detail(&gh_error(GhReason::RateLimited)), "what gh said");
    }
}
