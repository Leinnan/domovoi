//! Shared wire types for local codebase indexing, multi-instance failover,
//! and evidence-checked AI pull request reviews.

use crate::typed::{ModelSelection, ProviderInstanceId};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Failover pools & live instance availability
// ---------------------------------------------------------------------------

/// A priority-ordered failover pool of provider instances.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailoverPoolConfig {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub instance_ids: Vec<ProviderInstanceId>,
    /// Try other healthy instances of the requested driver before crossing to another driver.
    #[serde(default = "default_true")]
    pub same_driver_first: bool,
    /// Allow failing over to instances of a different driver once the primary driver is exhausted.
    #[serde(default = "default_true")]
    pub allow_cross_provider: bool,
}

fn default_true() -> bool {
    true
}

/// Live failover availability of a single provider instance.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceAvailability {
    pub instance_id: String,
    pub driver: String,
    pub display_name: String,
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limited_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default)]
    pub active_jobs: u32,
}

/// Result of `providers.failoverStatus`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailoverStatusResult {
    pub pools: Vec<FailoverPoolConfig>,
    pub instances: Vec<InstanceAvailability>,
}

// ---------------------------------------------------------------------------
// Local codebase index
// ---------------------------------------------------------------------------

/// Summary of a project's persisted SQLite codebase index.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectIndexStatus {
    pub project_id: String,
    /// `idle` | `indexing` | `ready` | `failed`
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_at: Option<String>,
    #[serde(default)]
    pub file_count: u64,
    #[serde(default)]
    pub package_count: u64,
    #[serde(default)]
    pub edge_count: u64,
    #[serde(default)]
    pub symbol_count: u64,
    #[serde(default)]
    pub convention_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Evidence-checked AI pull request reviews
// ---------------------------------------------------------------------------

/// Exact code reference anchoring a review finding, suggestion, or question.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrReviewEvidenceRef {
    pub path: String,
    pub line: u64,
    pub quote: String,
}

/// A single factor contributing to a PR's risk score.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrReviewRiskDriver {
    pub points: i64,
    pub label: String,
    pub detail: String,
}

/// Token / runtime accounting for a review run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrReviewUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub stages: u32,
    #[serde(default)]
    pub failovers: u32,
}

/// One validated finding, suggestion, or question produced by an AI PR review.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrReviewFinding {
    pub id: String,
    pub run_id: String,
    /// `finding` | `suggestion` | `question`
    pub kind: String,
    /// `high` | `medium` | `low`
    pub severity: String,
    pub title: String,
    pub detail: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    /// `new` | `old`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<String>,
    #[serde(default)]
    pub evidence: Vec<PrReviewEvidenceRef>,
    #[serde(default)]
    pub counter_evidence: Vec<PrReviewEvidenceRef>,
    #[serde(default = "default_true")]
    pub selected: bool,
    /// `primary` | `lens` | `reused`
    pub source_stage: String,
}

/// A single AI review run for a pull request.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrReviewRun {
    pub id: String,
    pub pr_key: String,
    pub project_id: String,
    pub host: String,
    pub repository: String,
    pub number: u64,
    pub head_sha: String,
    pub base_ref: String,
    pub base_sha: String,
    pub merge_base_sha: String,
    /// `full` | `incremental` | `base_sync` | `stacked`
    pub scope_mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_run_id: Option<String>,
    /// `queued` | `running` | `validated` | `failed` | `cancelled` | `published` | `superseded`
    pub status: String,
    pub phase: String,
    #[serde(default)]
    pub risk_score: u32,
    #[serde(default)]
    pub risk_drivers: Vec<PrReviewRiskDriver>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_markdown: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_selection: Option<ModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_review_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<PrReviewUsage>,
    pub created_at: String,
    pub updated_at: String,
}

/// Result of `pullRequests.reviews.get`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrReviewResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<PrReviewRun>,
    #[serde(default)]
    pub findings: Vec<PrReviewFinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_head_sha: Option<String>,
}

/// Streamed event from `pullRequests.reviews.subscribe`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrReviewProgressEvent {
    pub pr_key: String,
    pub run_id: String,
    pub status: String,
    pub phase: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<PrReviewResult>,
}
