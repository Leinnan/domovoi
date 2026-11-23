//! Commands on the thread's own record: what it is called, where it works,
//! how it is shown in the sidebar (pinned, archived, settled, snoozed,
//! tagged), and when it was last read.

use super::{DispatchResult, Effect, Orchestrator, Refusal, is_active, now};
use serde_json::Number;
use domovoi_contracts::time_fmt::{iso_from_millis, millis_from_iso, now_millis};
use domovoi_model::typed::command::{
    ThreadAutoSettleSet, ThreadDelete, ThreadMarkUnread, ThreadMetadataUpdate, ThreadProjectAdoptFolder, ThreadSnooze, ThreadTagSet,
    ThreadTitleRegenerationComplete, ThreadUnsnooze, ThreadVisit,
};
use domovoi_model::typed::{
    AppThread, CommandId, EventBody, LastTitleRegeneration, ModelSelection, OpenPick, RunStatus, SettledOverride, ThreadChange, ThreadId,
    ThreadOpens, Timestamp, TitleRegeneration,
};

/// `recordThreadOpen` (t3code paletteSearch.ts): frecency for the palette.
/// `at` is epoch milliseconds; `query` is what the user typed to find the thread.
pub fn record_open(previous: Option<&ThreadOpens>, at: i64, query: Option<&str>) -> ThreadOpens {
    const TEN_MIN: i64 = 10 * 60 * 1000;
    const HALF_LIFE: f64 = 14.0 * 24.0 * 3600.0 * 1000.0;
    const PICK_TTL: i64 = 21 * 24 * 3600 * 1000;
    let score_before = |otherwise: f64| previous.and_then(|p| p.score.as_f64()).unwrap_or(otherwise);
    let (opened_at, score) = match previous.and_then(|p| p.opened_at.as_i64()) {
        // Opened again within ten minutes: the same visit.
        Some(opened) if at - opened < TEN_MIN => (opened, score_before(1.0)),
        Some(opened) => (at, score_before(0.0) * 2f64.powf(-((at - opened) as f64) / HALF_LIFE) + 1.0),
        None => (at, 1.0),
    };
    let mut picks: Vec<OpenPick> =
        previous.map(|p| p.picks.iter().filter(|pick| at - pick.at.as_i64().unwrap_or(0) <= PICK_TTL).cloned().collect()).unwrap_or_default();
    if let Some(query) = query.map(|q| q.trim().to_lowercase()).filter(|q| !q.is_empty()) {
        let query: String = query.chars().take(32).collect();
        picks.retain(|pick| pick.query != query);
        picks.insert(0, OpenPick { query, at: Number::from(at) });
    }
    picks.truncate(3);
    // A score is finite: it is a sum of halvings of finite scores.
    ThreadOpens { opened_at: Number::from(opened_at), score: Number::from_f64(score).unwrap_or_else(|| Number::from(0)), picks }
}

/// Whether the thread's snooze has run out by `now_ms`. A wake time that is
/// not a time has.
fn snooze_is_over(thread: &AppThread, now_ms: i64) -> bool {
    let until = thread.snoozed_until.as_ref().and_then(Option::as_ref);
    thread.deleted_at.is_none() && until.is_some_and(|until| !millis_from_iso(until.as_str()).is_some_and(|until| until > now_ms))
}

impl Orchestrator {
    /// Appends the thread's record as `change`, on the command's behalf;
    /// `touch` moves its `updatedAt` to now.
    pub(super) fn write_thread(&self, command_id: &CommandId, change: ThreadChange, mut thread: AppThread, touch: bool) -> Result<DispatchResult, Refusal> {
        if touch {
            thread.updated_at = now();
        }
        let thread_id = thread.id.clone();
        let origin = Self::origin(command_id, Some(&thread.provider_instance_id));
        let sequence = self.store.append_event(thread_id.as_str(), EventBody::Thread { change, thread: Box::new(thread) }, &origin)?.sequence;
        if matches!(change, ThreadChange::RuntimeModeUpdated | ThreadChange::InteractionModeUpdated) {
            self.send_effect(Effect::RuntimeModeChanged { thread_id: thread_id.as_str().to_owned() });
        }
        Ok(DispatchResult { sequence })
    }

    /// Reads the thread, lets `edit` change its record, and appends it as `change`.
    pub(super) fn edit_thread(
        &self,
        thread_id: &ThreadId,
        command_id: &CommandId,
        change: ThreadChange,
        touch: bool,
        edit: impl FnOnce(&mut AppThread),
    ) -> Result<DispatchResult, Refusal> {
        let mut thread = self.thread(thread_id)?;
        edit(&mut thread);
        self.write_thread(command_id, change, thread, touch)
    }

    pub(super) fn thread_visit(&self, c: &ThreadVisit) -> Result<DispatchResult, Refusal> {
        self.edit_thread(&c.thread_id, &c.command_id, ThreadChange::Visited, false, |thread| {
            // The read watermark only moves forward.
            let visited = thread.last_visited_at.as_ref().and_then(Option::as_ref);
            if visited.is_none_or(|visited| c.visited_at.as_str() > visited.as_str()) {
                thread.last_visited_at = Some(Some(c.visited_at.clone()));
            }
            if c.opened == Some(true) {
                let previous = thread.opens.as_ref().and_then(Option::as_ref);
                thread.opens = Some(Some(record_open(previous, now_millis(), c.search_query.as_deref())));
            }
        })
    }

    /// `thread.metadata.update`: title, branch, worktree, or a request to
    /// regenerate the title.
    pub(super) fn metadata_update(&self, c: &ThreadMetadataUpdate) -> Result<DispatchResult, Refusal> {
        let regenerate = c.regenerate_title == Some(true);
        // Limit recovery (t3code): only for the thread's latest run, failed on a
        // usage limit that resets at the time given, with nothing pending.
        let recovery = match &c.limit_recovery {
            None => None,
            Some(None) => Some(None),
            Some(Some(update)) => {
                let tid = c.thread_id.as_str();
                let runs = self.store.records::<domovoi_model::typed::Run>(tid)?;
                let latest = runs.iter().max_by_key(|run| run.ordinal).ok_or(Refusal::NotUsageLimited)?;
                let failure = crate::usage_recovery::usage_limit_reset(&self.store, tid, latest);
                let pending = self.store.records::<domovoi_model::typed::RuntimeRequest>(tid)?.iter().any(|r| r.status.as_str() == "pending");
                if latest.id != update.run_id || failure.is_none() || pending {
                    return Err(Refusal::NotUsageLimited);
                }
                Some(Some(domovoi_model::typed::LimitRecovery {
                    request_id: Some(c.command_id.clone()),
                    run_id: update.run_id.clone(),
                    reset_at: update.reset_at.clone(),
                    auto_resume: update.auto_resume == Some(true),
                    snooze: update.snooze,
                }))
            }
        };
        let answer = self.edit_thread(&c.thread_id, &c.command_id, ThreadChange::MetadataUpdated, true, |thread| {
            if let Some(title) = &c.title {
                thread.title = title.clone();
            }
            if let Some(branch) = &c.branch {
                thread.branch = branch.clone();
            }
            if let Some(worktree_path) = &c.worktree_path {
                thread.worktree_path = worktree_path.clone();
            }
            if regenerate {
                thread.title_regeneration = Some(Some(TitleRegeneration { request_id: c.command_id.clone(), started_at: now() }));
            }
            if let Some(recovery) = &recovery {
                if let Some(r) = recovery.as_ref().filter(|r| r.snooze == Some(true)) {
                    thread.snoozed_until = Some(Some(r.reset_at.clone()));
                }
                thread.limit_recovery = Some(recovery.clone());
            }
        })?;
        if regenerate {
            self.send_effect(Effect::GenerateTitle {
                thread_id: c.thread_id.as_str().to_owned(),
                request_id: c.command_id.as_str().to_owned(),
                regenerate: true,
            });
        }
        Ok(answer)
    }

    /// The model's title for a pending regeneration (ignored once another
    /// request replaced it).
    pub(super) fn title_regeneration_complete(&self, c: &ThreadTitleRegenerationComplete) -> Result<DispatchResult, Refusal> {
        let mut thread = self.thread(&c.thread_id)?;
        let pending = thread.title_regeneration.as_ref().and_then(Option::as_ref).map(|pending| &pending.request_id);
        if pending != Some(&c.request_id) {
            return self.answer(None);
        }
        if let Some(title) = &c.title {
            thread.title = title.clone();
        }
        thread.title_regeneration = Some(None);
        if let Some(outcome) = &c.outcome {
            thread.last_title_regeneration = Some(Some(LastTitleRegeneration {
                status: outcome.status.clone(),
                message: outcome.message.clone(),
                request_id: c.request_id.clone(),
                completed_at: now(),
            }));
        }
        self.write_thread(&c.command_id, ThreadChange::MetadataUpdated, thread, true)
    }

    /// A new model on the same provider instance, or a switch to another
    /// (`thread.model-selection.set` and `provider.switch` both do either).
    pub(super) fn model_selection_set(&self, thread_id: &ThreadId, command_id: &CommandId, selection: &ModelSelection) -> Result<DispatchResult, Refusal> {
        let mut thread = self.thread(thread_id)?;
        let change =
            if selection.instance_id == thread.provider_instance_id { ThreadChange::ModelSelectionUpdated } else { ThreadChange::ProviderSwitched };
        if change == ThreadChange::ProviderSwitched {
            let outline = self.store.thread_outline(thread_id.as_str())?;
            self.close_message_questions(thread_id, &outline.runtime_requests, &Self::origin(command_id, None))?;
        }
        thread.provider_instance_id = selection.instance_id.clone();
        thread.model_selection = selection.clone();
        self.write_thread(command_id, change, thread, true)
    }

    /// Moves a thread into the project that owns its folder (scratch promotion).
    pub(super) fn adopt_folder(&self, c: &ThreadProjectAdoptFolder) -> Result<DispatchResult, Refusal> {
        let mut thread = self.thread(&c.thread_id)?;
        let target = self
            .store
            .get_project(c.project_id.as_str())?
            .ok_or_else(|| Refusal::AdoptMissingProject { thread: c.thread_id.clone(), project: c.project_id.clone() })?;
        let current = self.store.get_project(thread.project_id.as_str())?;
        let cwd = thread.worktree_path.clone().or(current.map(|project| project.workspace_root));
        if cwd.as_deref() != Some(target.workspace_root.as_str()) {
            return Err(Refusal::AdoptOtherFolder { thread: c.thread_id.clone(), root: target.workspace_root });
        }
        thread.project_id = target.id.into();
        thread.worktree_path = None;
        let written = self.write_thread(&c.command_id, ThreadChange::MetadataUpdated, thread, true)?;
        self.send_effect(Effect::MoveThreadTasks { thread_id: c.thread_id.as_str().to_owned(), project_id: c.project_id.as_str().to_owned() });
        Ok(written)
    }

    pub(super) fn auto_settle_set(&self, c: &ThreadAutoSettleSet) -> Result<DispatchResult, Refusal> {
        let mut thread = self.thread(&c.thread_id)?;
        let disabled_at = thread.auto_settle_disabled_at.take().flatten();
        let unchanged = c.enabled == disabled_at.is_none();
        thread.auto_settle_disabled_at = Some(if c.enabled { None } else { Some(disabled_at.unwrap_or_else(now)) });
        self.write_thread(&c.command_id, ThreadChange::AutoSettleSet, thread, !unchanged)
    }

    pub(super) fn tag_set(&self, c: &ThreadTagSet) -> Result<DispatchResult, Refusal> {
        let mut thread = self.thread(&c.thread_id)?;
        let tag = c.tag.trim();
        if tag.is_empty() || tag.chars().count() > 64 || tag.chars().any(char::is_control) {
            return Err(Refusal::InvalidTag);
        }
        let mut tags = thread.tags.take().unwrap_or_default();
        tags.retain(|held| held != tag);
        if c.tagged {
            tags.push(tag.to_owned());
        }
        thread.tags = Some(tags);
        self.write_thread(&c.command_id, ThreadChange::TagsUpdated, thread, false)
    }

    /// Deleting a thread also stops its work: the active run is interrupted,
    /// queued runs are cancelled and its terminals closed (t3code ThreadDeletion).
    pub(super) fn delete_thread(&self, c: &ThreadDelete) -> Result<DispatchResult, Refusal> {
        self.thread(&c.thread_id)?;
        let thread_id = c.thread_id.as_str();
        let outline = self.store.thread_outline(thread_id)?;
        for run in &outline.runs {
            if run.status == RunStatus::Queued {
                self.cancel_run(thread_id, &outline, (**run).clone(), &Self::run_origin(&c.command_id, run))?;
            } else if is_active(&run.status) {
                self.send_effect(Effect::Interrupt { thread_id: thread_id.to_owned(), run_id: run.id.as_str().to_owned() });
            }
        }
        self.send_effect(Effect::CloseTerminals { thread_id: thread_id.to_owned() });
        self.send_effect(Effect::DeleteThreadTasks { thread_id: thread_id.to_owned() });
        // Read again: cancelling its runs moved the thread's `updatedAt`.
        self.edit_thread(&c.thread_id, &c.command_id, ThreadChange::Deleted, true, |thread| thread.deleted_at = Some(now()))
    }

    /// Hides the thread until `snoozedUntil`; then it wakes (`wake_snoozed`).
    pub(super) fn snooze(&self, c: &ThreadSnooze) -> Result<DispatchResult, Refusal> {
        if millis_from_iso(c.snoozed_until.as_str()).is_none() {
            return Err(Refusal::SnoozeNotATime);
        }
        self.edit_thread(&c.thread_id, &c.command_id, ThreadChange::Snoozed, true, |thread| {
            thread.snoozed_until = Some(Some(c.snoozed_until.clone()));
            thread.snoozed_at = Some(Some(now()));
        })
    }

    pub(super) fn unsnooze(&self, c: &ThreadUnsnooze) -> Result<DispatchResult, Refusal> {
        let mut thread = self.thread(&c.thread_id)?;
        let awake = thread.snoozed_until.as_ref().and_then(Option::as_ref).is_none();
        thread.snoozed_until = Some(None);
        thread.snoozed_at = Some(None);
        // A thread that was not snoozed keeps when it last woke.
        thread.last_snooze_wake_at = Some(if awake { thread.last_snooze_wake_at.take().flatten() } else { Some(now()) });
        self.write_thread(&c.command_id, ThreadChange::Unsnoozed, thread, !awake)
    }

    /// Rewinds the visit to just before the thread's last activity.
    pub(super) fn mark_unread(&self, c: &ThreadMarkUnread) -> Result<DispatchResult, Refusal> {
        self.edit_thread(&c.thread_id, &c.command_id, ThreadChange::MarkedUnread, false, |thread| {
            let last = millis_from_iso(thread.updated_at.as_str()).unwrap_or_else(now_millis);
            thread.last_visited_at = Some(Some(Timestamp::from(iso_from_millis(last - 1))));
        })
    }

    /// Wakes threads whose snooze ran out: they come back at the top. Returns
    /// how many woke.
    pub fn wake_snoozed(&self) -> usize {
        let Ok(threads) = self.store.app_threads() else { return 0 };
        let now_ms = now_millis();
        let mut woke = 0;
        for thread in threads.iter().filter(|thread| snooze_is_over(thread, now_ms)) {
            let origin = Self::origin(&CommandId::from("snooze-wake"), Some(&thread.provider_instance_id));
            // Re-checked on the record as it is when written: a snooze changed
            // or a thread renamed since the list was read is kept.
            let woken = self.store.update_thread(thread.id.as_str(), ThreadChange::Unsnoozed, &origin, |thread| {
                if !snooze_is_over(thread, now_ms) {
                    return false;
                }
                thread.last_snooze_wake_at = Some(thread.snoozed_until.take().flatten());
                thread.snoozed_until = Some(None);
                thread.snoozed_at = Some(None);
                thread.updated_at = now();
                true
            });
            if matches!(woken, Ok(Some(_))) {
                woke += 1;
            }
        }
        woke
    }

    /// `thread.settle` and `thread.auto-settle` settle the thread;
    /// `thread.unsettle` keeps it active whatever its runs say.
    pub(super) fn settle(&self, thread_id: &ThreadId, command_id: &CommandId, settle: bool) -> Result<DispatchResult, Refusal> {
        let at = now();
        let (change, kept) = if settle { (ThreadChange::Settled, SettledOverride::Settled) } else { (ThreadChange::Unsettled, SettledOverride::Active) };
        let answer = self.edit_thread(thread_id, command_id, change, true, |thread| {
            thread.settled_override = Some(Some(kept));
            if settle {
                thread.settled_at = Some(Some(at));
            } else {
                thread.settled_at = Some(None);
                thread.unsettled_at = Some(Some(at));
            }
        })?;
        if settle {
            self.send_effect(Effect::ThreadSettled { thread_id: thread_id.as_str().to_owned() });
        }
        Ok(answer)
    }
}
