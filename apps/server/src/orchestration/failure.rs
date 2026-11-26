//! Why a read, a subscription or a stop control of the orchestration RPC group
//! failed, as the client is told: each a tagged error of t3code's
//! (`OrchestrationV2GetShellSnapshotError` and the rest), with the fields that
//! error carries. (Why a command or a launch was not carried out is in
//! `commands::failure`.)

use serde::Serialize;

/// Why `orchestration.forceStopThread` did nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ForceStopReason {
    /// The run the client named is no longer the thread's.
    Superseded,
}

/// Why `orchestration.stopWorkflow` did nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StopWorkflowReason {
    Unavailable,
    Unsupported,
    NotRunning,
}

/// Why a workflow's script was not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkflowScriptReason {
    InvalidPath,
    RootUnavailable,
    NotFound,
    OutsideRoot,
    NotJs,
    ReadFailed,
    NotRegularFile,
}

/// A failed call of the orchestration group, written as its tagged error.
#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "_tag")]
pub enum Failure {
    #[error("{message}")]
    #[serde(rename = "OrchestrationV2GetShellSnapshotError")]
    ShellSnapshot { message: String },

    #[error("{message}")]
    #[serde(rename = "OrchestrationV2GetThreadProjectionError", rename_all = "camelCase")]
    ThreadProjection { thread_id: String, message: String },

    #[error("{cause}")]
    #[serde(rename = "OrchestrationV2SearchThreadError")]
    SearchThread { cause: String },

    #[error("{message}")]
    #[serde(rename = "OrchestrationSearchThreadsError")]
    SearchThreads { message: String },

    #[error("thread {thread_id} was not force-stopped")]
    #[serde(rename = "OrchestrationV2ForceStopThreadError", rename_all = "camelCase")]
    ForceStopThread { thread_id: String, reason: Option<ForceStopReason> },

    #[error("workflow {subagent_id} of thread {thread_id} was not stopped")]
    #[serde(rename = "OrchestrationV2StopWorkflowError", rename_all = "camelCase")]
    StopWorkflow { thread_id: String, subagent_id: String, reason: StopWorkflowReason },

    #[error("the workflow script {script_path} was not read")]
    #[serde(rename = "OrchestrationGetWorkflowScriptError", rename_all = "camelCase")]
    WorkflowScript { reason: WorkflowScriptReason, script_path: String },

    #[error("{message}")]
    #[serde(rename = "OrchestrationV2CommandOutputError", rename_all = "camelCase")]
    CommandOutput { thread_id: String, message: String },
}

impl Failure {
    /// The shell snapshot, or what the server is asked for with it, could not be read.
    pub fn shell(cause: impl std::fmt::Display) -> Self {
        Failure::ShellSnapshot { message: cause.to_string() }
    }

    /// The thread's projection could not be read.
    pub fn projection(thread_id: &str, cause: impl std::fmt::Display) -> Self {
        Failure::ThreadProjection { thread_id: thread_id.to_owned(), message: cause.to_string() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn failures_are_written_as_tagged_errors() {
        assert_eq!(json!(Failure::shell("no disk")), json!({ "_tag": "OrchestrationV2GetShellSnapshotError", "message": "no disk" }));
        assert_eq!(
            json!(Failure::projection("t1", "Unknown thread t1")),
            json!({ "_tag": "OrchestrationV2GetThreadProjectionError", "threadId": "t1", "message": "Unknown thread t1" })
        );
        assert_eq!(json!(Failure::SearchThread { cause: "gone".into() }), json!({ "_tag": "OrchestrationV2SearchThreadError", "cause": "gone" }));
        assert_eq!(json!(Failure::SearchThreads { message: "gone".into() }), json!({ "_tag": "OrchestrationSearchThreadsError", "message": "gone" }));
        assert_eq!(
            json!(Failure::ForceStopThread { thread_id: "t1".into(), reason: None }),
            json!({ "_tag": "OrchestrationV2ForceStopThreadError", "threadId": "t1", "reason": null })
        );
        assert_eq!(
            json!(Failure::ForceStopThread { thread_id: "t1".into(), reason: Some(ForceStopReason::Superseded) }),
            json!({ "_tag": "OrchestrationV2ForceStopThreadError", "threadId": "t1", "reason": "superseded" })
        );
        assert_eq!(
            json!(Failure::StopWorkflow { thread_id: "t1".into(), subagent_id: "s1".into(), reason: StopWorkflowReason::NotRunning }),
            json!({ "_tag": "OrchestrationV2StopWorkflowError", "threadId": "t1", "subagentId": "s1", "reason": "not-running" })
        );
        let reasons = [
            (WorkflowScriptReason::InvalidPath, "invalid-path"), (WorkflowScriptReason::RootUnavailable, "root-unavailable"),
            (WorkflowScriptReason::NotFound, "not-found"), (WorkflowScriptReason::OutsideRoot, "outside-root"), (WorkflowScriptReason::NotJs, "not-js"),
            (WorkflowScriptReason::ReadFailed, "read-failed"), (WorkflowScriptReason::NotRegularFile, "not-regular-file"),
        ];
        for (reason, spelled) in reasons {
            assert_eq!(
                json!(Failure::WorkflowScript { reason, script_path: "/a.js".into() }),
                json!({ "_tag": "OrchestrationGetWorkflowScriptError", "reason": spelled, "scriptPath": "/a.js" })
            );
        }
        assert_eq!(
            json!(Failure::CommandOutput { thread_id: "t1".into(), message: "No such command.".into() }),
            json!({ "_tag": "OrchestrationV2CommandOutputError", "threadId": "t1", "message": "No such command." })
        );
    }
}
