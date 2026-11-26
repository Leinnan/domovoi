//! When a thread deserves the user's attention: the client posts a local
//! notification, the server a push to devices whose app is closed.

use super::pull_request::{PullRequestChecksState, PullRequestLinkSource, PullRequestReviewDecision, PullRequestState};
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

/// What of a pull request a thread links a notification is made from: as
/// the server last read it from its host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestSeen {
    /// `host/repository#number`, lowercased.
    pub key: String,
    pub number: u64,
    pub state: PullRequestState,
    pub checks: Option<PullRequestChecksState>,
    pub review: Option<PullRequestReviewDecision>,
    pub unresolved_review_threads: u64,
}

/// The pull requests a thread shows that have been read from their host.
pub fn pull_requests_seen(shell: &ThreadShell) -> Vec<PullRequestSeen> {
    shell
        .pull_requests
        .iter()
        .flatten()
        .filter(|link| link.source != PullRequestLinkSource::StackDismissed)
        .filter_map(|link| {
            let snapshot = link.snapshot.as_ref()?;
            Some(PullRequestSeen {
                key: format!("{}/{}#{}", link.host, link.repository, link.number).to_lowercase(),
                number: link.number,
                state: snapshot.state.clone(),
                checks: snapshot.checks_state.clone().flatten(),
                review: snapshot.review_decision.clone().flatten(),
                unresolved_review_threads: snapshot.unresolved_review_threads.unwrap_or(0),
            })
        })
        .collect()
}

/// The notification bodies for a thread's pull requests moving from
/// `previous` to `next`: checks that failed, a review that arrived, a merge,
/// a close. A pull request first seen never notifies (it is the starting
/// state), and neither does anything about one that is no longer open.
pub fn pull_request_notices(previous: &[PullRequestSeen], next: &[PullRequestSeen]) -> Vec<String> {
    let mut notices = vec![];
    for now in next {
        let Some(before) = previous.iter().find(|seen| seen.key == now.key) else { continue };
        let n = now.number;
        if before.state != now.state {
            match now.state {
                PullRequestState::Merged => notices.push(format!("Pull request #{n} merged")),
                PullRequestState::Closed => notices.push(format!("Pull request #{n} was closed")),
                _ => {}
            }
            continue;
        }
        if now.state != PullRequestState::Open {
            continue;
        }
        if now.checks == Some(PullRequestChecksState::Failing) && before.checks != now.checks {
            notices.push(format!("Checks failed on #{n}"));
        }
        if before.review != now.review {
            match now.review {
                Some(PullRequestReviewDecision::ChangesRequested) => notices.push(format!("Changes requested on #{n}")),
                Some(PullRequestReviewDecision::Approved) => notices.push(format!("#{n} was approved")),
                _ => {}
            }
        } else if now.unresolved_review_threads > before.unresolved_review_threads {
            notices.push(format!("New review comments on #{n}"));
        }
    }
    notices
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(number: u64, state: PullRequestState, checks: Option<PullRequestChecksState>, review: Option<PullRequestReviewDecision>, unresolved: u64) -> PullRequestSeen {
        PullRequestSeen { key: format!("github.com/a/b#{number}"), number, state, checks, review, unresolved_review_threads: unresolved }
    }

    #[test]
    fn a_pull_request_notifies_of_what_happened_to_it_once() {
        use PullRequestChecksState::{Failing, Passing, Pending};
        use PullRequestReviewDecision::{Approved, ChangesRequested};
        use PullRequestState::{Closed, Merged, Open};
        let open = seen(7, Open, Some(Pending), None, 0);
        let none: [String; 0] = [];
        // First seen, and seen again unchanged: nothing.
        assert_eq!(pull_request_notices(&[], &[open.clone()]), none);
        assert_eq!(pull_request_notices(&[open.clone()], &[open.clone()]), none);
        // Checks: failing is news, passing is not, failing again after passing is.
        let failing = seen(7, Open, Some(Failing), None, 0);
        assert_eq!(pull_request_notices(&[open.clone()], &[failing.clone()]), ["Checks failed on #7"]);
        assert_eq!(pull_request_notices(&[failing.clone()], &[failing.clone()]), none);
        assert_eq!(pull_request_notices(&[failing.clone()], &[seen(7, Open, Some(Passing), None, 0)]), none);
        // Reviews: a decision, or new conversations without one.
        assert_eq!(pull_request_notices(&[open.clone()], &[seen(7, Open, Some(Pending), Some(ChangesRequested), 2)]), ["Changes requested on #7"]);
        assert_eq!(pull_request_notices(&[open.clone()], &[seen(7, Open, Some(Pending), Some(Approved), 0)]), ["#7 was approved"]);
        assert_eq!(pull_request_notices(&[open.clone()], &[seen(7, Open, Some(Pending), None, 1)]), ["New review comments on #7"]);
        assert_eq!(pull_request_notices(&[seen(7, Open, None, None, 3)], &[seen(7, Open, None, None, 1)]), none, "resolving is not news");
        // The end of it, alone: a merge with failing checks says it merged.
        assert_eq!(pull_request_notices(&[open.clone()], &[seen(7, Merged, Some(Failing), Some(Approved), 0)]), ["Pull request #7 merged"]);
        assert_eq!(pull_request_notices(&[open.clone()], &[seen(7, Closed, None, None, 0)]), ["Pull request #7 was closed"]);
        assert_eq!(pull_request_notices(&[seen(7, Merged, None, None, 0)], &[seen(7, Merged, Some(Failing), None, 4)]), none);
        // Each pull request of a thread by itself.
        let both = pull_request_notices(&[open.clone(), seen(8, Open, None, None, 0)], &[failing, seen(8, Merged, None, None, 0)]);
        assert_eq!(both, ["Checks failed on #7", "Pull request #8 merged"]);
    }
}
