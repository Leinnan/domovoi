//! `scheduledTasks.*` RPCs over [`crate::scheduled_tasks`].

use super::Never;
use crate::scheduled_tasks::{self as tasks, ScheduledTaskError, UpsertInput, task_error};
use crate::state::ConnCtx;
use effect_rpc::Router;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use domovoi_model::typed::{ScheduledTask, ScheduledTaskList};
use tokio::sync::broadcast::error::RecvError;

#[derive(Deserialize)]
struct IdInput {
    id: String,
}

#[derive(Deserialize)]
struct SetEnabledInput {
    id: String,
    enabled: bool,
}

/// The task a call saved, changed or ran, as it is stored afterwards
/// (`ScheduledTaskMutationResult`, `ScheduledTaskRunNowResult`).
#[derive(Serialize)]
struct TaskAnswer {
    task: ScheduledTask,
}

/// `ScheduledTaskDeleteResult`.
#[derive(Serialize)]
struct Deleted {
    id: String,
}

/// `ScheduledTaskListWebhookDeliveriesResult`. Domovoi serves no webhook route, so
/// no delivery is ever logged.
#[derive(Serialize)]
struct Deliveries {
    deliveries: [Never; 0],
}

/// Webhook URLs start agents, so only operate-scoped sessions see them.
fn visible(ctx: &ConnCtx, list: ScheduledTaskList) -> ScheduledTaskList {
    if ctx.session.scopes.iter().any(|s| s == "orchestration:operate") { list } else { without_webhooks(list) }
}

/// The list as a session that may not operate gets it: no task says where its webhook is.
fn without_webhooks(mut list: ScheduledTaskList) -> ScheduledTaskList {
    for task in list.tasks.items_mut() {
        task.webhook = None;
    }
    list
}

/// The full list, then again after every change.
fn subscribe(ctx: Arc<ConnCtx>) -> BoxStream<'static, Result<ScheduledTaskList, ScheduledTaskError>> {
    let service = tasks::service(&ctx.state);
    async_stream::stream! {
        // Subscribe before reading so concurrent changes are not missed.
        let mut changes = service.subscribe();
        yield tasks::list_result(service.store()).map(|l| visible(&ctx, l));
        loop {
            match changes.recv().await {
                Ok(()) | Err(RecvError::Lagged(_)) => {
                    // Coalesce a burst into one list.
                    while changes.try_recv().is_ok() {}
                    yield tasks::list_result(service.store()).map(|l| visible(&ctx, l));
                }
                Err(RecvError::Closed) => return,
            }
        }
    }
    .boxed()
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.stream("scheduledTasks.subscribe", |ctx: Arc<ConnCtx>, _input: IgnoredAny| subscribe(ctx))
        .unary("scheduledTasks.upsert", |ctx: Arc<ConnCtx>, input: UpsertInput| async move {
            let service = tasks::service(&ctx.state);
            let task = tasks::upsert(service.store(), input, &jiff::Zoned::now())?;
            service.notify_changed();
            Ok::<_, ScheduledTaskError>(TaskAnswer { task })
        })
        .unary("scheduledTasks.setEnabled", |ctx: Arc<ConnCtx>, input: SetEnabledInput| async move {
            let service = tasks::service(&ctx.state);
            let (task, changed) = tasks::set_enabled(service.store(), &input.id, input.enabled, &jiff::Zoned::now())?;
            if changed {
                service.notify_changed();
            }
            Ok::<_, ScheduledTaskError>(TaskAnswer { task })
        })
        .unary("scheduledTasks.delete", |ctx: Arc<ConnCtx>, input: IdInput| async move {
            let service = tasks::service(&ctx.state);
            tasks::delete(service.store(), &input.id)?;
            service.notify_changed();
            Ok::<_, ScheduledTaskError>(Deleted { id: input.id })
        })
        .unary("scheduledTasks.runNow", |ctx: Arc<ConnCtx>, input: IdInput| async move {
            let task = tasks::service(&ctx.state).run_now(&input.id).await?;
            Ok::<_, ScheduledTaskError>(TaskAnswer { task })
        })
        .unary("scheduledTasks.rotateWebhookToken", |ctx: Arc<ConnCtx>, input: IdInput| async move {
            let service = tasks::service(&ctx.state);
            let task = tasks::rotate_webhook_token(service.store(), &input.id, &jiff::Zoned::now())?;
            service.notify_changed();
            Ok::<_, ScheduledTaskError>(TaskAnswer { task })
        })
        .unary("scheduledTasks.listWebhookDeliveries", |_ctx: Arc<ConnCtx>, _input: IdInput| async move { Ok::<_, Never>(Deliveries { deliveries: [] }) })
        // With no delivery logged, none is ever found (`ScheduledTaskGetWebhookDeliveryResult` is never answered).
        .unary("scheduledTasks.getWebhookDelivery", |_ctx: Arc<ConnCtx>, input: IdInput| async move {
            Err::<Never, _>(task_error("Webhook delivery not found.", Some(&input.id)))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_session_that_may_not_operate_is_not_told_where_webhooks_are() {
        let task = |id: &str, schedule: serde_json::Value, webhook: serde_json::Value| {
            let mut task = json!({
                "id": id, "title": "T", "prompt": "P", "enabled": true, "schedule": schedule, "projectId": "p1", "threadId": null, "threadMode": "new_thread",
                "workspaceStrategy": { "type": "root" }, "modelSelection": { "instanceId": "codex", "model": "gpt-5" }, "runtimeMode": "full-access",
                "interactionMode": "default", "createdBy": "user", "creationSource": "web", "createdAt": "2026-11-13T12:00:00.000Z",
                "updatedAt": "2026-11-13T12:00:00.000Z", "nextRunAt": null, "lastRunAt": null, "lastRunStatus": "never", "lastRunError": null, "runCount": 0,
            });
            if !webhook.is_null() {
                task["webhook"] = webhook;
            }
            task
        };
        let hook = json!({ "type": "webhook", "signature": null, "maxDeliveryAgeMinutes": null });
        let endpoint = json!({ "path": "/api/hooks/hook/token", "url": null, "hasSecret": false });
        let sent = json!({ "tasks": [task("hook", hook.clone(), endpoint), task("timed", json!({ "type": "interval", "everyMs": 60000 }), json!(null))], "timeZone": null });
        let list: ScheduledTaskList = serde_json::from_value(sent.clone()).unwrap();
        assert_eq!(json!(list), sent, "the list as an operating session gets it");
        let hidden = json!({ "tasks": [task("hook", hook, json!(null)), task("timed", json!({ "type": "interval", "everyMs": 60000 }), json!(null))], "timeZone": null });
        assert_eq!(json!(without_webhooks(list)), hidden);
        assert_eq!(json!(Deliveries { deliveries: [] }), json!({ "deliveries": [] }));
        assert_eq!(json!(Deleted { id: "hook".into() }), json!({ "id": "hook" }));
    }
}
