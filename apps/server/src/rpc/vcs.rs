//! VCS RPCs. Subscriptions poll git every few seconds and emit on change.

use super::Never;
use crate::state::ConnCtx;
use crate::vcs;
use crate::vcs_ops::sent;
use effect_rpc::Router;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::Deserialize;
use serde::de::IgnoredAny;
use std::sync::Arc;
use std::time::Duration;
use domovoi_model::typed::{VcsBranchStatus, VcsStatusLocalUpdated, VcsStatusRemoteUpdated, VcsStatusSnapshot, VcsStatusStreamEvent};

const POLL: Duration = Duration::from_secs(4);

#[derive(Deserialize)]
struct CwdInput {
    cwd: String,
}

#[derive(Deserialize)]
struct ListRefsInput {
    cwd: String,
    query: Option<String>,
    limit: Option<usize>,
}

/// `ReviewBranchCommitsInput`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BranchCommitsInput {
    #[serde(default, deserialize_with = "sent::text")]
    cwd: String,
    #[serde(default, deserialize_with = "sent::text_if_any")]
    base_ref: Option<String>,
}

fn branch_stream(input: CwdInput) -> BoxStream<'static, Result<VcsBranchStatus, Never>> {
    async_stream::stream! {
        let mut last = None;
        loop {
            let now = vcs::branch_status(&input.cwd).await;
            if last.as_ref() != Some(&now) {
                yield Ok(now.clone());
                last = Some(now);
            }
            tokio::time::sleep(POLL).await;
        }
    }
    .boxed()
}

fn status_stream(input: CwdInput) -> BoxStream<'static, Result<VcsStatusStreamEvent, Never>> {
    async_stream::stream! {
        let mut local = vcs::local_status(&input.cwd).await;
        let mut remote = vcs::remote_status(&input.cwd).await;
        yield Ok(VcsStatusStreamEvent::Snapshot(VcsStatusSnapshot { local: local.clone(), remote: remote.clone() }));
        loop {
            tokio::time::sleep(POLL).await;
            let l = vcs::local_status(&input.cwd).await;
            if l != local {
                yield Ok(VcsStatusStreamEvent::LocalUpdated(VcsStatusLocalUpdated { local: l.clone() }));
                local = l;
            }
            let r = vcs::remote_status(&input.cwd).await;
            if r != remote {
                yield Ok(VcsStatusStreamEvent::RemoteUpdated(VcsStatusRemoteUpdated { remote: r.clone() }));
                remote = r;
            }
        }
    }
    .boxed()
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.stream("subscribeVcsBranch", |_, input: CwdInput| branch_stream(input))
        .stream("subscribeVcsStatus", |_, input: CwdInput| status_stream(input))
        .unary("vcs.refreshBranch", |_, _: IgnoredAny| async { Ok::<_, Never>(()) })
        .unary("vcs.listRefs", |_: Arc<ConnCtx>, input: ListRefsInput| async move {
            Ok::<_, Never>(vcs::list_refs(&input.cwd, input.query.as_deref(), input.limit.unwrap_or(100)).await)
        })
        .unary("server.discoverSourceControl", |_, _: IgnoredAny| async { Ok::<_, Never>(vcs::discover_source_control().await) })
        .unary("review.getBranchCommits", |_: Arc<ConnCtx>, input: BranchCommitsInput| async move {
            Ok::<_, Never>(vcs::branch_commits(&input.cwd, input.base_ref.as_deref()).await)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The two later items of `subscribeVcsStatus`, which arrive only when a
    /// checkout changes while it is watched.
    #[tokio::test]
    async fn status_updates_are_written_as_their_tagged_events() {
        let dir = std::env::temp_dir().join(format!("domovoi-vcs-events-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        let cwd = dir.to_string_lossy().into_owned();
        let local = vcs::local_status(&cwd).await;
        let local_json = json!({ "isRepo": false, "hasPrimaryRemote": false, "isDefaultRef": false, "refName": null, "hasWorkingTreeChanges": false,
            "workingTree": { "files": [], "insertions": 0, "deletions": 0 } });
        assert_eq!(
            json!(VcsStatusStreamEvent::Snapshot(VcsStatusSnapshot { local: local.clone(), remote: vcs::remote_status(&cwd).await })),
            json!({ "_tag": "snapshot", "local": local_json, "remote": null })
        );
        assert_eq!(json!(VcsStatusStreamEvent::LocalUpdated(VcsStatusLocalUpdated { local })), json!({ "_tag": "localUpdated", "local": local_json }));
        assert_eq!(json!(VcsStatusStreamEvent::RemoteUpdated(VcsStatusRemoteUpdated { remote: None })), json!({ "_tag": "remoteUpdated", "remote": null }));
        let remote = domovoi_model::typed::VcsStatusRemote { has_upstream: true, ahead_count: 2, behind_count: 1, ahead_of_default_count: Some(0), pr: None };
        assert_eq!(
            json!(VcsStatusStreamEvent::RemoteUpdated(VcsStatusRemoteUpdated { remote: Some(remote) })),
            json!({ "_tag": "remoteUpdated", "remote": { "hasUpstream": true, "aheadCount": 2, "behindCount": 1, "aheadOfDefaultCount": 0, "pr": null } })
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
