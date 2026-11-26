//! `pullRequests.reviews.*` RPC handlers for evidence-checked AI code reviews.

use super::pull_request_failure::{Failure, Provider};
use super::pull_requests::{PrRef, head_sha, require_project};
use super::{github, gitlab, review_guides};
use crate::review::{self, StartReviewInput, publish, store};
use crate::rpc::Never;
use crate::state::ConnCtx;
use domovoi_model::typed::{PrReviewProgressEvent, PrReviewResult, PrReviewRun};
use domovoi_rpc::Router;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartInput {
    #[serde(flatten)]
    reference: PrRef,
    #[serde(default)]
    force_full: bool,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ListInput {
    #[serde(default)]
    pr_key: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunIdInput {
    run_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToggleFindingInput {
    run_id: String,
    finding_id: String,
    selected: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublishInput {
    #[serde(flatten)]
    reference: PrRef,
    run_id: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SubscribeInput {
    #[serde(default)]
    pr_key: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CancelResult {
    cancelled: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToggleResult {
    updated: bool,
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.unary("pullRequests.reviews.get", |ctx: Arc<ConnCtx>, r: PrRef| async move {
        let p = require_project(&ctx.state, &r).await?;
        let pr_key = review_guides::key(&p.host, &p.repository, r.number);
        let head = head_sha(&p, r.number).await.ok().flatten();
        Ok::<PrReviewResult, Failure>(store::latest_for_pr(ctx.state.store.db(), &pr_key, head))
    })
    .unary("pullRequests.reviews.list", |ctx: Arc<ConnCtx>, input: ListInput| async move {
        let runs = store::list_runs(
            ctx.state.store.db(),
            input.pr_key.as_deref(),
            input.project_id.as_deref(),
            input.limit.unwrap_or(20),
        );
        Ok::<Vec<PrReviewRun>, Never>(runs)
    })
    .unary("pullRequests.reviews.start", |ctx: Arc<ConnCtx>, input: StartInput| async move {
        let p = require_project(&ctx.state, &input.reference).await?;
        let source = match p.provider {
            Provider::GitHub => github::guide_source(&p, input.reference.number).await,
            Provider::GitLab => gitlab::guide_source(&p, input.reference.number).await,
        }
        .map_err(|e| Failure::operation("startReview", e.detail))?;
        let head = source
            .head
            .ok_or_else(|| Failure::operation("startReview", "Pull request has no head commit."))?;
        let is_stacked = !matches!(source.base_branch.as_str(), "main" | "master" | "develop" | "trunk");
        review::start_review(
            ctx.state.clone(),
            StartReviewInput {
                project: p,
                number: input.reference.number,
                title: source.title,
                author: String::new(),
                description: source.body,
                base_ref: source.base_branch,
                head_sha: head,
                force_full: input.force_full,
                is_stacked_base: is_stacked,
            },
        )
        .await
        .map_err(|e| Failure::operation("startReview", e))
    })
    .unary("pullRequests.reviews.cancel", |ctx: Arc<ConnCtx>, input: RunIdInput| async move {
        let cancelled = review::cancel_run(ctx.state.store.db(), &input.run_id);
        Ok::<CancelResult, Never>(CancelResult { cancelled })
    })
    .unary("pullRequests.reviews.setFindingSelected", |ctx: Arc<ConnCtx>, input: ToggleFindingInput| async move {
        let updated = store::set_finding_selected(ctx.state.store.db(), &input.run_id, &input.finding_id, input.selected)
            .map_err(|e| Failure::operation("setFindingSelected", e))?;
        Ok::<ToggleResult, Failure>(ToggleResult { updated })
    })
    .unary("pullRequests.reviews.publish", |ctx: Arc<ConnCtx>, input: PublishInput| async move {
        let p = require_project(&ctx.state, &input.reference).await?;
        publish::publish_run(ctx.state.store.db(), &p, &input.run_id)
            .await
            .map_err(|e| Failure::operation("publishReview", e))
    })
    .stream("pullRequests.reviews.subscribe", |_ctx: Arc<ConnCtx>, input: SubscribeInput| {
        let filter_key = input.pr_key;
        BroadcastStream::new(review::subscribe())
            .filter_map(move |item| {
                let filter_key = filter_key.clone();
                async move {
                    let ev = item.ok()?;
                    if filter_key.as_ref().is_some_and(|k| k != &ev.pr_key) {
                        return None;
                    }
                    Some(Ok::<PrReviewProgressEvent, Never>(ev))
                }
            })
            .boxed()
    })
}
