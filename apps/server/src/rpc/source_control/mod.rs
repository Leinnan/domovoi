//! Source control RPCs: repositories, sign-in and project clones
//! (`repositories`), pull requests (`pull_requests`, answered from GitHub by
//! `github` and from GitLab by `gitlab` with the records of
//! `pull_request_records` and the failures of `pull_request_failure`; review
//! guides in `review_guides`) and the pull request side of git (`git_workflow`).

pub(crate) mod git_workflow;
pub(crate) mod github;
pub(crate) mod gitlab;
pub(crate) mod pull_request_failure;
pub(crate) mod pull_request_records;
pub(crate) mod pull_requests;
mod repositories;
pub(crate) mod review_guides;
mod reviews;

use crate::state::ConnCtx;
use domovoi_rpc::Router;

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    let r = repositories::register(r);
    let r = pull_requests::register(r);
    let r = reviews::register(r);
    git_workflow::register(r)
}
