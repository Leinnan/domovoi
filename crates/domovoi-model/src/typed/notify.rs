//! When a thread deserves the user's attention: the client posts a local
//! notification, the server a push to devices whose app is closed.

use super::shell::{ActivityRunStatus, ShellThreadStatus, ThreadShell};

/// What a thread's row in a list says the thread is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShellStatus {
    /// It waits on the user: an approval or a question.
    Approval,
    Working,
    Failed,
    /// Its latest run was interrupted or cancelled.
    Interrupted,
    Completed,
    Idle,
}

impl ShellStatus {
    /// The name the apps know this status by.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::Working => "working",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Completed => "completed",
            Self::Idle => "idle",
        }
    }
}

/// The status a thread's shell shows. A pending request comes first, then
/// work under way, then how the latest run ended.
pub fn shell_status(shell: &ThreadShell) -> ShellStatus {
    let busy = matches!(
        shell.activity_run_status,
        Some(Some(ActivityRunStatus::Waiting | ActivityRunStatus::Running | ActivityRunStatus::Starting))
    );
    if shell.pending_runtime_request.is_some() {
        ShellStatus::Approval
    } else if shell.active_run_id.is_some() || busy {
        ShellStatus::Working
    } else {
        match shell.status {
            ShellThreadStatus::Failed => ShellStatus::Failed,
            ShellThreadStatus::Interrupted | ShellThreadStatus::Cancelled => ShellStatus::Interrupted,
            ShellThreadStatus::Completed => ShellStatus::Completed,
            ShellThreadStatus::Running | ShellThreadStatus::Starting | ShellThreadStatus::Waiting | ShellThreadStatus::Preparing => {
                ShellStatus::Working
            }
            ShellThreadStatus::Idle | ShellThreadStatus::Queued | ShellThreadStatus::RolledBack | ShellThreadStatus::Other(_) => {
                ShellStatus::Idle
            }
        }
    }
}

/// The notification body for a thread moving from `previous` to `status`, if
/// the move deserves one. The first status seen (`previous` is `None`) never
/// does: it is the starting state, not a change.
pub fn notice(previous: Option<ShellStatus>, status: ShellStatus) -> Option<&'static str> {
    let previous = previous?;
    if previous == status {
        return None;
    }
    match status {
        ShellStatus::Approval => Some("Needs your approval"),
        ShellStatus::Completed if previous == ShellStatus::Working => Some("Finished"),
        ShellStatus::Failed => Some("Failed"),
        _ => None,
    }
}
