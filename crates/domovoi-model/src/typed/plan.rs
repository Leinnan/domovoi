//! Plans and todo lists an agent produced, and the questions it asks the user.

use super::common::Timestamp;
use super::ids::{NodeId, PlanId, RunId, ThreadId};
use super::macros::string_enum;
use serde::{Deserialize, Serialize};

/// A plan an agent produced (`OrchestrationV2PlanArtifact`): a proposal in
/// markdown, or a todo list it works through.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanArtifact {
    ProposedPlan(ProposedPlan),
    TodoList(TodoList),
}

impl PlanArtifact {
    pub fn id(&self) -> &PlanId {
        match self {
            Self::ProposedPlan(plan) => &plan.id,
            Self::TodoList(list) => &list.id,
        }
    }
}

/// A plan in markdown, proposed for the user to accept.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposedPlan {
    pub id: PlanId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: NodeId,
    pub status: PlanStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_in_turn_item: Option<bool>,
    pub markdown: String,
}

/// The steps an agent set itself, with their progress.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoList {
    pub id: PlanId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: NodeId,
    pub status: PlanStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_in_turn_item: Option<bool>,
    pub steps: Vec<PlanStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
}

string_enum! {
    /// Where a plan is in its life.
    PlanStatus {
        Draft = "draft",
        Active = "active",
        Completed = "completed",
        Superseded = "superseded",
    }
}

/// One step of a todo list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    pub id: String,
    pub text: String,
    pub status: PlanStepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_anchor_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

string_enum! {
    /// How far a todo step has got.
    PlanStepStatus {
        Pending = "pending",
        Running = "running",
        Completed = "completed",
    }
}

/// A question an agent asks the user, with the answers it offers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_select: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_custom_answer: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

/// One answer a question offers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}
