//! Source control RPCs: repositories, sign-in and project clones
//! (`repositories`), pull requests (`pull_requests`, answered from GitHub by
//! `github` with the records of `pull_request_records` and the failures of
//! `pull_request_failure`; review guides in `review_guides`) and the pull
//! request side of git (`git_workflow`).

mod git_workflow;
mod github;
mod pull_request_failure;
mod pull_request_records;
mod pull_requests;
mod repositories;
mod review_guides;

use crate::state::ConnCtx;
use effect_rpc::Router;

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    let r = repositories::register(r);
    let r = pull_requests::register(r);
    git_workflow::register(r)
}
