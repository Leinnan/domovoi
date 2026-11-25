//! The pull requests linked to a thread (t3code `Orchestrator.ts`
//! `thread.pull-request.link` / `unlink` / `watch`): a link is keyed by
//! host, repository and number; unlinking a member of a stack leaves a
//! `stack-dismissed` tombstone so no sync adds it back; a watch records what
//! the agent was already told, so watching again repeats no news.
//!
//! Limits: keys are compared lowercased, without t3code's host alias table
//! (`canonicalRepositoryKey`); an agent's watch is not refused for racing a
//! stop of its run. Nothing reads a watch yet: no pull request watcher
//! polls the host and wakes the thread.

use super::{DispatchResult, Orchestrator, Refusal, now};
use domovoi_model::typed::command::{ThreadPullRequestLink, ThreadPullRequestUnlink, ThreadPullRequestWatch};
use domovoi_model::typed::{
    AppThread, LinkedPullRequest, ParentRelationship, PullRequestChecksState, PullRequestLink, PullRequestLinkSource, PullRequestWatch,
    PullRequestWatchUntil, SettledOverride, ThreadChange, Timestamp, WatchBaseline,
};

/// What names one pull request: lowercased host and repository, and its number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestKey {
    pub host: String,
    pub repository: String,
    pub number: u64,
}

impl PullRequestKey {
    pub fn new(host: &str, repository: &str, number: u64) -> Self {
        PullRequestKey { host: host.trim().to_lowercase(), repository: repository.trim().to_lowercase(), number }
    }

    pub fn of(link: &PullRequestLink) -> Self {
        Self::new(&link.host, &link.repository, link.number)
    }

    /// `legacyThreadPullRequestKey`: the single link older threads held, keyed by its URL's host.
    pub fn of_legacy(linked: &LinkedPullRequest) -> Self {
        let host = linked.url.split("://").nth(1).and_then(|rest| rest.split(['/', ':']).next()).filter(|h| !h.is_empty()).unwrap_or("unknown");
        Self::new(host, &linked.repository, linked.number)
    }

    pub fn names(&self, link: &PullRequestLink) -> bool {
        Self::of(link) == *self
    }
}

/// `threadPullRequestsOf`: the thread's links, or the one older threads kept in `linkedPullRequest`.
pub fn links_of(thread: &AppThread) -> Vec<PullRequestLink> {
    if let Some(links) = &thread.pull_requests {
        return links.clone();
    }
    match thread.linked_pull_request.as_ref().and_then(Option::as_ref) {
        Some(linked) => vec![new_link(&PullRequestKey::of_legacy(linked), &linked.url, None, PullRequestLinkSource::Manual, Timestamp::from("1970-01-01T00:00:00.000Z"))],
        None => vec![],
    }
}

/// A link the thread shows (a `stack-dismissed` one is a tombstone).
pub fn is_visible(link: &PullRequestLink) -> bool {
    link.source != PullRequestLinkSource::StackDismissed
}

fn new_link(key: &PullRequestKey, url: &str, repository_web_url: Option<String>, source: PullRequestLinkSource, linked_at: Timestamp) -> PullRequestLink {
    PullRequestLink {
        host: key.host.clone(),
        repository: key.repository.clone(),
        number: key.number,
        url: url.trim().to_owned(),
        repository_web_url,
        source,
        linked_at,
        snapshot: None,
        stack: None,
        watch: None,
        ended_watch: None,
    }
}

/// `withPullRequestWatch`: a new watch replaces any ended one; no watch keeps the last as ended.
fn with_watch(mut link: PullRequestLink, watch: Option<PullRequestWatch>) -> PullRequestLink {
    match watch {
        Some(watch) => {
            link.watch = Some(watch);
            link.ended_watch = None;
        }
        None => {
            let ended = link.watch.take().or(link.ended_watch.take());
            link.ended_watch = ended;
        }
    }
    link
}

/// `withUntil`: only `merged` is written; `checks` is the default, so it is left out.
fn with_until(mut watch: PullRequestWatch, until: Option<&PullRequestWatchUntil>) -> PullRequestWatch {
    match until {
        Some(PullRequestWatchUntil::Merged) => watch.until = Some(PullRequestWatchUntil::Merged),
        Some(PullRequestWatchUntil::Checks) => watch.until = None,
        _ => {}
    }
    watch
}

/// A new watch: what the last one told carries over, so watching again repeats no news.
fn fresh_watch(link: &PullRequestLink, checks_pending: Option<bool>, at: &Timestamp) -> PullRequestWatch {
    let ended = link.ended_watch.as_ref();
    let pending = checks_pending.unwrap_or_else(|| {
        link.snapshot.as_ref().is_some_and(|s| s.checks_state == Some(Some(PullRequestChecksState::Pending))) || ended.is_some_and(|e| e.checks_pending == Some(true))
    });
    PullRequestWatch {
        started_at: at.clone(),
        head_sha: ended.and_then(|e| e.head_sha.clone()),
        failed_checks: ended.map(|e| e.failed_checks.clone()).unwrap_or_default(),
        passed: ended.is_some_and(|e| e.passed),
        passed_checks: Some(ended.and_then(|e| e.passed_checks.clone()).unwrap_or_default()),
        owed_checks: ended.and_then(|e| e.owed_checks.clone()),
        awaiting_approval: ended.and_then(|e| e.awaiting_approval.clone()),
        approval_announced: ended.and_then(|e| e.approval_announced),
        remarks_through: at.clone(),
        remark_ids: vec![],
        conflicting: ended.is_some_and(|e| e.conflicting),
        rebase_required: ended.and_then(|e| e.rebase_required),
        merge_queue_removal_id: None,
        wakes: 0,
        awaiting_baseline: match ended {
            Some(e) => e.awaiting_baseline.clone(),
            None => Some(WatchBaseline::All),
        },
        checks_awaited_since: ended.and_then(|e| e.checks_awaited_since.clone()),
        checks_pending: pending.then_some(true),
        until: None,
    }
}

/// The thread's single older-style link stays only while a visible link names it.
fn keep_linked(thread: &AppThread, links: &[PullRequestLink]) -> Option<Option<LinkedPullRequest>> {
    let linked = thread.linked_pull_request.as_ref().and_then(Option::as_ref)?;
    let key = PullRequestKey::of_legacy(linked);
    Some(links.iter().any(|link| is_visible(link) && key.names(link)).then(|| linked.clone()))
}

impl Orchestrator {
    fn write_links(&self, command_id: &domovoi_model::typed::CommandId, mut thread: AppThread, links: Vec<PullRequestLink>) -> Result<DispatchResult, Refusal> {
        if thread.linked_pull_request.is_some() {
            thread.linked_pull_request = keep_linked(&thread, &links).or(Some(None));
        }
        thread.pull_requests = Some(links);
        self.write_thread(command_id, ThreadChange::PullRequestSynced, thread, true)
    }

    /// `thread.pull-request.link`: links the pull request, unless it is
    /// linked already. Linking a dismissed stack member again (not as part of
    /// its stack) brings it back. A first manual link also keeps the pull
    /// request of the thread's branch, which the thread showed until then.
    pub(super) fn pull_request_link(&self, c: &ThreadPullRequestLink) -> Result<DispatchResult, Refusal> {
        let thread = self.thread(&c.thread_id)?;
        let key = PullRequestKey::new(&c.host, &c.repository, c.number);
        let links = links_of(&thread);
        let existing = links.iter().find(|link| key.names(link));
        let undismisses = existing.is_some_and(|link| link.source == PullRequestLinkSource::StackDismissed)
            && !matches!(c.source, PullRequestLinkSource::Stack | PullRequestLinkSource::StackDismissed);
        if existing.is_some() && !undismisses {
            return self.answer(None);
        }
        let at = now();
        let link = match existing {
            Some(held) => {
                let mut link = held.clone();
                link.source = c.source.clone();
                link.url = c.url.trim().to_owned();
                if c.repository_web_url.is_some() {
                    link.repository_web_url = c.repository_web_url.clone();
                }
                link
            }
            None => new_link(&key, &c.url, c.repository_web_url.clone(), c.source.clone(), at.clone()),
        };
        let branch = thread.branch_pull_request.as_ref().and_then(Option::as_ref);
        let branch_kept = branch.filter(|branch| {
            let branch_key = PullRequestKey::of_legacy(branch);
            c.source == PullRequestLinkSource::Manual && !links.iter().any(is_visible) && branch_key != key && !links.iter().any(|l| branch_key.names(l))
        });
        let mut next: Vec<PullRequestLink> = branch_kept.map(|b| new_link(&PullRequestKey::of_legacy(b), &b.url, None, PullRequestLinkSource::Manual, at.clone())).into_iter().collect();
        next.extend(links.iter().filter(|l| !key.names(l)).cloned());
        next.push(link);
        self.write_links(&c.command_id, thread, next)
    }

    /// `thread.pull-request.unlink`: a member of a stack stays as a
    /// `stack-dismissed` tombstone (its watch ended); any other link goes.
    pub(super) fn pull_request_unlink(&self, c: &ThreadPullRequestUnlink) -> Result<DispatchResult, Refusal> {
        let thread = self.thread(&c.thread_id)?;
        let key = PullRequestKey::new(&c.host, &c.repository, c.number);
        let links = links_of(&thread);
        let Some(existing) = links.iter().position(|link| key.names(link)) else { return self.answer(None) };
        let held = &links[existing];
        let in_stack = held.source == PullRequestLinkSource::Stack
            || held.stack.is_some()
            || links.iter().any(|link| {
                link.host.eq_ignore_ascii_case(&key.host)
                    && link.repository.eq_ignore_ascii_case(&key.repository)
                    && link.stack.as_ref().is_some_and(|stack| stack.layers.iter().any(|layer| layer.number == key.number))
            });
        let mut next = links.clone();
        if in_stack {
            let mut tombstone = with_watch(next[existing].clone(), None);
            tombstone.source = PullRequestLinkSource::StackDismissed;
            next[existing] = tombstone;
        } else {
            next.remove(existing);
        }
        self.write_links(&c.command_id, thread, next)
    }

    /// `thread.pull-request.watch`: starts (linking the pull request first,
    /// when the command says how) or stops the thread's watch on it.
    pub(super) fn pull_request_watch(&self, c: &ThreadPullRequestWatch) -> Result<DispatchResult, Refusal> {
        let thread = self.thread(&c.thread_id)?;
        if c.watching {
            let settled = thread.settled_override == Some(Some(SettledOverride::Settled)) || thread.settled_at.as_ref().and_then(Option::as_ref).is_some();
            if settled || thread.archived_at.is_some() {
                return Err(Refusal::WatchOnSettledThread(c.thread_id.clone()));
            }
            // Child tasks report to their parent, which owns watching pull requests.
            if thread.lineage.relationship_to_parent == Some(ParentRelationship::Subagent) {
                return Err(Refusal::WatchFromSubagent(c.thread_id.clone()));
            }
        }
        let key = PullRequestKey::new(&c.host, &c.repository, c.number);
        let at = now();
        let linked = links_of(&thread);
        let visible = |link: &PullRequestLink| is_visible(link) && key.names(link);
        let mut links = linked.clone();
        if let (true, Some(to_link), false) = (c.watching, &c.link, linked.iter().any(visible)) {
            let dismissed = linked.iter().find(|l| key.names(l));
            let web = to_link.repository_web_url.clone().or_else(|| dismissed.and_then(|d| d.repository_web_url.clone()));
            let mut link = new_link(&key, &to_link.url, web, to_link.source.clone(), at.clone());
            link.ended_watch = dismissed.and_then(|d| d.ended_watch.clone());
            links.retain(|l| !key.names(l));
            links.push(link);
        }
        let Some(index) = links.iter().position(visible) else { return self.answer(None) };
        let existing = &links[index];
        let watch = c.watching.then(|| with_until(existing.watch.clone().unwrap_or_else(|| fresh_watch(existing, c.checks_pending, &at)), c.until.as_ref()));
        if watch == existing.watch && links == linked {
            return self.answer(None);
        }
        links[index] = with_watch(links[index].clone(), watch);
        self.write_links(&c.command_id, thread, links)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_ignore_case_and_older_links_key_by_their_url() {
        assert_eq!(PullRequestKey::new(" GitHub.com ", "Acme/App", 7), PullRequestKey::new("github.com", "acme/app", 7));
        let linked = LinkedPullRequest { project_id: "p".into(), repository: "acme/app".into(), number: 7, url: "https://github.com/acme/app/pull/7".into() };
        assert_eq!(PullRequestKey::of_legacy(&linked), PullRequestKey::new("github.com", "acme/app", 7));
    }

    #[test]
    fn a_new_watch_carries_over_what_the_last_one_told() {
        let at = Timestamp::from("2026-01-01T00:00:00.000Z");
        let mut link = new_link(&PullRequestKey::new("github.com", "a/b", 1), "https://github.com/a/b/pull/1", None, PullRequestLinkSource::Agent, at.clone());
        let first = fresh_watch(&link, Some(true), &at);
        assert_eq!(first.awaiting_baseline, Some(WatchBaseline::All));
        assert_eq!(first.checks_pending, Some(true));
        let mut told = first.clone();
        told.failed_checks = vec!["lint".into()];
        link.watch = Some(told);
        let link = with_watch(link, None);
        assert!(link.watch.is_none());
        let again = fresh_watch(&link, None, &at);
        assert_eq!(again.failed_checks, vec!["lint".to_owned()]);
        assert_eq!(again.checks_pending, Some(true), "the ended watch's pending checks");
        assert_eq!(with_until(again.clone(), Some(&PullRequestWatchUntil::Merged)).until, Some(PullRequestWatchUntil::Merged));
        assert_eq!(with_until(with_until(again, Some(&PullRequestWatchUntil::Merged)), Some(&PullRequestWatchUntil::Checks)).until, None);
    }
}
