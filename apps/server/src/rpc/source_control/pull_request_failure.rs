//! Why a call of the pull request RPC group failed, as the client is told:
//! t3code's `PullRequestUnavailableError` (the host cannot be read at all) and
//! `PullRequestOperationError` (this one request went wrong), with the fields
//! those errors carry; and the hosts a pull request is read from
//! ([`Provider`], t3code's `PullRequestProviderApi.kind` and
//! `unavailableRequirement`).

use crate::gh::{self, GhError, GhReason};
use serde::Serialize;
use domovoi_model::typed::PullRequestListResult;

/// GitHub's provider kind.
pub const PROVIDER: &str = "github";

/// A host a pull request is read from, and the CLI it is read through:
/// GitHub through `gh` (`crate::gh`), GitLab through `glab` (`crate::glab`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    GitHub,
    GitLab,
}

impl Provider {
    /// The provider of a repository identity's kind, when there is a driver for it.
    pub fn of(kind: &str) -> Option<Provider> {
        match kind {
            PROVIDER => Some(Provider::GitHub),
            "gitlab" => Some(Provider::GitLab),
            _ => None,
        }
    }

    /// `SourceControlProviderKind`.
    pub fn kind(self) -> &'static str {
        match self {
            Provider::GitHub => PROVIDER,
            Provider::GitLab => "gitlab",
        }
    }

    /// `unavailableRequirement.missing`.
    pub fn requirement_missing(self) -> &'static str {
        match self {
            Provider::GitHub => gh::REQUIREMENT_MISSING,
            Provider::GitLab => crate::glab::REQUIREMENT_MISSING,
        }
    }

    /// `unavailableRequirement.unauthenticated`.
    pub fn requirement_unauthenticated(self) -> &'static str {
        match self {
            Provider::GitHub => gh::REQUIREMENT_UNAUTHENTICATED,
            Provider::GitLab => crate::glab::REQUIREMENT_UNAUTHENTICATED,
        }
    }
}

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

    /// A failed `gh` or `glab` of `provider`'s host under `operation`
    /// (t3code's `toPullRequestErrorWith`): without the tool or a credential
    /// the host is unavailable, anything else is this operation's failure.
    pub fn from_cli(operation: &str, provider: Provider, e: &GhError) -> Self {
        let unavailable = |reason, requirement| Failure::Unavailable { reason, provider: Some(provider.kind()), requirement: Some(requirement), list_result: None };
        match e.reason {
            GhReason::MissingTool => unavailable(UnavailableReason::CliMissing, provider.requirement_missing()),
            GhReason::Unauthenticated => unavailable(UnavailableReason::CliUnauthenticated, provider.requirement_unauthenticated()),
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

/// What a host's row of a listing says of its failed `gh` or `glab` (`providerDetail`).
pub fn cli_detail(provider: Provider, e: &GhError) -> String {
    match e.reason {
        GhReason::MissingTool => provider.requirement_missing().into(),
        GhReason::Unauthenticated => provider.requirement_unauthenticated().into(),
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
            json!(Failure::from_cli("detail", Provider::GitHub, &gh_error(GhReason::MissingTool))),
            json!({ "_tag": "PullRequestUnavailableError", "reason": "cli-missing", "provider": "github", "requirement": gh::REQUIREMENT_MISSING })
        );
        assert_eq!(
            json!(Failure::from_cli("detail", Provider::GitHub, &gh_error(GhReason::Unauthenticated))),
            json!({ "_tag": "PullRequestUnavailableError", "reason": "cli-unauthenticated", "provider": "github", "requirement": gh::REQUIREMENT_UNAUTHENTICATED })
        );
        assert_eq!(
            json!(Failure::from_cli("detail", Provider::GitHub, &gh_error(GhReason::NotFound))),
            json!({ "_tag": "PullRequestOperationError", "operation": "detail", "detail": "what gh said", "reason": "not-found" })
        );
        for reason in [GhReason::RateLimited, GhReason::Forbidden, GhReason::Failed] {
            assert_eq!(
                json!(Failure::from_cli("comment", Provider::GitHub, &gh_error(reason))),
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
            json!(Failure::from_cli("list", Provider::GitHub, &gh_error(GhReason::Unauthenticated)).with_list(list.clone())),
            json!({ "_tag": "PullRequestUnavailableError", "reason": "cli-unauthenticated", "provider": "github",
                    "requirement": gh::REQUIREMENT_UNAUTHENTICATED, "listResult": shown })
        );
        assert_eq!(
            json!(Failure::from_cli("list", Provider::GitHub, &gh_error(GhReason::NotFound)).with_list(list.clone())),
            json!({ "_tag": "PullRequestOperationError", "operation": "list", "detail": "what gh said", "reason": "not-found", "listResult": shown })
        );
        assert_eq!(
            json!(Failure::from_cli("list", Provider::GitHub, &gh_error(GhReason::Failed)).with_list(list)),
            json!({ "_tag": "PullRequestOperationError", "operation": "list", "detail": "what gh said", "listResult": shown })
        );
    }

    #[test]
    fn gitlab_failures_name_glab() {
        assert_eq!(
            json!(Failure::from_cli("detail", Provider::GitLab, &gh_error(GhReason::MissingTool))),
            json!({ "_tag": "PullRequestUnavailableError", "reason": "cli-missing", "provider": "gitlab", "requirement": crate::glab::REQUIREMENT_MISSING })
        );
        assert_eq!(
            json!(Failure::from_cli("detail", Provider::GitLab, &gh_error(GhReason::Unauthenticated))),
            json!({ "_tag": "PullRequestUnavailableError", "reason": "cli-unauthenticated", "provider": "gitlab", "requirement": crate::glab::REQUIREMENT_UNAUTHENTICATED })
        );
        assert_eq!(cli_detail(Provider::GitLab, &gh_error(GhReason::MissingTool)), crate::glab::REQUIREMENT_MISSING);
        assert_eq!((Provider::of("gitlab"), Provider::of("github"), Provider::of("bitbucket")), (Some(Provider::GitLab), Some(Provider::GitHub), None));
    }

    #[test]
    fn a_host_row_names_what_the_host_needs() {
        assert_eq!(cli_detail(Provider::GitHub, &gh_error(GhReason::MissingTool)), gh::REQUIREMENT_MISSING);
        assert_eq!(cli_detail(Provider::GitHub, &gh_error(GhReason::Unauthenticated)), gh::REQUIREMENT_UNAUTHENTICATED);
        assert_eq!(cli_detail(Provider::GitHub, &gh_error(GhReason::RateLimited)), "what gh said");
    }
}
