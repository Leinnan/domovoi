//! Worktree setup progress per thread (t3code `WorktreeSetupTracker`): the
//! stages of provisioning a thread's worktree, streamed to
//! `subscribeWorktreeSetup`, and cancellation while it runs.

use crate::rpc::Never;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use domovoi_model::typed::Timestamp;
use tokio::sync::broadcast;

/// A step of provisioning a worktree (`WorktreeSetupStageId`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StageId {
    Fetch,
    Checkout,
    Submodules,
    SetupScript,
    Agent,
}

pub const STAGES: [StageId; 5] = [StageId::Fetch, StageId::Checkout, StageId::Submodules, StageId::SetupScript, StageId::Agent];

/// How far a step is (`WorktreeSetupStageStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StageStatus {
    Pending,
    Running,
    Done,
    Skipped,
    Warning,
    Failed,
}

/// How far a setup is (`WorktreeSetupPhase`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Running,
    Done,
    Failed,
    Cancelled,
}

/// One step of a setup (`WorktreeSetupStage`). This server reports no
/// percentage, detail or output of a step.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stage {
    pub id: StageId,
    pub status: StageStatus,
    pub started_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    pub percent: Option<Never>,
    pub detail: Option<Never>,
    pub tail: [Never; 0],
}

/// The setup of a thread's worktree as it stands (`WorktreeSetupSnapshot`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub thread_id: String,
    pub phase: Phase,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub branch: Option<String>,
    pub base_ref: Option<String>,
    pub worktree_path: Option<String>,
    /// The project's setup script runs in a terminal of the thread; it is not named here.
    pub setup_script: Option<Never>,
    pub stages: Vec<Stage>,
    /// Why the setup failed.
    pub error: Option<String>,
    /// Counts the versions of this record: 1 when the setup starts.
    pub sequence: u64,
}

pub struct Tracker {
    records: Mutex<HashMap<String, Snapshot>>,
    cancelled: Mutex<std::collections::HashSet<String>>,
    tx: broadcast::Sender<(String, Snapshot)>,
}

pub fn tracker() -> &'static Tracker {
    static T: OnceLock<Tracker> = OnceLock::new();
    T.get_or_init(|| Tracker { records: Mutex::default(), cancelled: Mutex::default(), tx: broadcast::channel(256).0 })
}

fn now() -> Timestamp {
    crate::time_fmt::iso_now().into()
}

impl Tracker {
    fn update(&self, thread_id: &str, f: impl FnOnce(&mut Snapshot)) {
        let mut records = crate::util::locked(&self.records);
        let Some(r) = records.get_mut(thread_id) else { return };
        f(r);
        r.sequence += 1;
        let _ = self.tx.send((thread_id.to_owned(), r.clone()));
    }

    pub fn start(&self, thread_id: &str, branch: Option<&str>, base_ref: Option<&str>) {
        let stages = STAGES
            .iter()
            .map(|&id| Stage { id, status: StageStatus::Pending, started_at: None, ended_at: None, percent: None, detail: None, tail: [] })
            .collect();
        let record = Snapshot {
            thread_id: thread_id.to_owned(),
            phase: Phase::Running,
            started_at: now(),
            ended_at: None,
            branch: branch.map(str::to_owned),
            base_ref: base_ref.map(str::to_owned),
            worktree_path: None,
            setup_script: None,
            stages,
            error: None,
            sequence: 1,
        };
        crate::util::locked(&self.cancelled).remove(thread_id);
        crate::util::locked(&self.records).insert(thread_id.to_owned(), record.clone());
        let _ = self.tx.send((thread_id.to_owned(), record));
    }

    pub fn stage(&self, thread_id: &str, stage: StageId, status: StageStatus) {
        let now = now();
        self.update(thread_id, |r| {
            if let Some(s) = r.stages.iter_mut().find(|s| s.id == stage) {
                if status == StageStatus::Running && s.started_at.is_none() {
                    s.started_at = Some(now.clone());
                }
                if matches!(status, StageStatus::Done | StageStatus::Skipped | StageStatus::Warning | StageStatus::Failed) {
                    s.ended_at = Some(now);
                }
                s.status = status;
            }
        });
    }

    pub fn set_worktree(&self, thread_id: &str, path: &str, branch: &str) {
        self.update(thread_id, |r| {
            r.worktree_path = Some(path.to_owned());
            r.branch = Some(branch.to_owned());
        });
    }

    /// Ends the setup; stages still pending are skipped.
    pub fn finish(&self, thread_id: &str, phase: Phase, error: Option<&str>) {
        let now = now();
        self.update(thread_id, |r| {
            r.phase = phase;
            r.ended_at = Some(now.clone());
            r.error = error.map(str::to_owned);
            for s in &mut r.stages {
                match s.status {
                    StageStatus::Pending => s.status = StageStatus::Skipped,
                    StageStatus::Running => {
                        s.status = if phase == Phase::Failed { StageStatus::Failed } else { StageStatus::Done };
                        s.ended_at = Some(now.clone());
                    }
                    _ => {}
                }
            }
        });
    }

    pub fn is_cancelled(&self, thread_id: &str) -> bool {
        crate::util::locked(&self.cancelled).contains(thread_id)
    }

    /// Requests cancellation of a running setup; false when none is running.
    pub fn cancel(&self, thread_id: &str) -> bool {
        let running = crate::util::locked(&self.records).get(thread_id).is_some_and(|r| r.phase == Phase::Running);
        if running {
            crate::util::locked(&self.cancelled).insert(thread_id.to_owned());
        }
        running
    }

    pub fn current(&self, thread_id: &str) -> Option<Snapshot> {
        crate::util::locked(&self.records).get(thread_id).cloned()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<(String, Snapshot)> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stages_progress_and_finish() {
        let t = tracker();
        t.start("wt-1", Some("t3/abc"), Some("main"));
        let started = t.current("wt-1").unwrap();
        let pending = |id: &str| json!({ "id": id, "status": "pending", "startedAt": null, "endedAt": null, "percent": null, "detail": null, "tail": [] });
        let stages = ["fetch", "checkout", "submodules", "setup-script", "agent"].map(pending);
        assert_eq!(
            json!(started),
            json!({
                "threadId": "wt-1", "phase": "running", "startedAt": started.started_at, "endedAt": null,
                "branch": "t3/abc", "baseRef": "main", "worktreePath": null, "setupScript": null,
                "stages": stages, "error": null, "sequence": 1,
            })
        );
        assert!(t.cancel("wt-1"));
        assert!(t.is_cancelled("wt-1"));
        t.stage("wt-1", StageId::Fetch, StageStatus::Skipped);
        t.stage("wt-1", StageId::Checkout, StageStatus::Running);
        let running = json!(t.current("wt-1").unwrap());
        assert_eq!((&running["stages"][1]["status"], running["stages"][1]["startedAt"].is_string(), &running["stages"][1]["endedAt"]), (&json!("running"), true, &json!(null)));
        t.finish("wt-1", Phase::Done, None);
        let r = t.current("wt-1").unwrap();
        assert_eq!(r.phase, Phase::Done);
        let status: Vec<StageStatus> = r.stages.iter().map(|s| s.status).collect();
        assert_eq!(status, [StageStatus::Skipped, StageStatus::Done, StageStatus::Skipped, StageStatus::Skipped, StageStatus::Skipped]);
        assert_eq!(r.sequence, 4, "start, two stages, finish");
        assert!(!t.cancel("wt-1"), "only running setups cancel");

        t.start("wt-2", None, None);
        t.stage("wt-2", StageId::Checkout, StageStatus::Running);
        t.set_worktree("wt-2", "/w/x", "feature/x");
        t.stage("wt-2", StageId::Submodules, StageStatus::Warning);
        t.finish("wt-2", Phase::Failed, Some("it broke"));
        let failed = json!(t.current("wt-2").unwrap());
        assert_eq!(
            (&failed["phase"], &failed["error"], &failed["worktreePath"], &failed["branch"], &failed["baseRef"]),
            (&json!("failed"), &json!("it broke"), &json!("/w/x"), &json!("feature/x"), &json!(null))
        );
        let status: Vec<&str> = failed["stages"].as_array().unwrap().iter().map(|s| s["status"].as_str().unwrap()).collect();
        assert_eq!(status, ["skipped", "failed", "warning", "skipped", "skipped"]);
        assert_eq!(json!(Phase::Cancelled), json!("cancelled"));
    }
}
