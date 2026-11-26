//! What a menu choice tells the core, where that is all it does. A choice is
//! a key, `what:id` (`t-pin:<thread id>`, `more:<section>`); the menus that
//! offer them are built where they are shown, and the window acts on the
//! choices that open a dialog or touch the window itself.

use domovoi_core::Event;

const HOUR_MS: i64 = 3_600_000;

/// The event for the choice `what` on `id`, at the time `now_ms` (a snooze
/// counts from it). `None` for a choice the window has to act on.
pub fn event(what: &str, id: &str, now_ms: i64) -> Option<Event> {
    let thread_id = || id.to_owned();
    let snooze = |hours: i64| Event::SnoozeThread { thread_id: thread_id(), until_ms: if hours == 0 { 0 } else { now_ms + hours * HOUR_MS } };
    Some(match what {
        "t-pin" => Event::PinThread { thread_id: thread_id(), pinned: true },
        "t-unpin" => Event::PinThread { thread_id: thread_id(), pinned: false },
        "t-settle" => Event::SettleThread { thread_id: thread_id(), settled: true },
        "t-unsettle" => Event::SettleThread { thread_id: thread_id(), settled: false },
        "t-snooze-1h" => snooze(1),
        "t-snooze-3h" => snooze(3),
        "t-snooze-1d" => snooze(24),
        "t-snooze-7d" => snooze(7 * 24),
        "t-snooze-wake" => snooze(0),
        "t-unread" => Event::MarkThreadUnread { thread_id: thread_id() },
        "t-retitle" => Event::RegenerateTitle { thread_id: thread_id() },
        "t-fork" => Event::ForkThread { thread_id: thread_id() },
        "t-archive" => Event::ArchiveThread { thread_id: thread_id(), archived: true },
        "t-unarchive" => Event::ArchiveThread { thread_id: thread_id(), archived: false },
        "t-forget" => Event::ForgetThreadPermissions { thread_id: thread_id() },
        "t-promote" => Event::PromoteScratchThread { thread_id: thread_id() },
        "effort" => Event::SelectEffort { value: id.to_owned() },
        // Full access is asked about first (the window's).
        "perm" if id != "full-access" => Event::SetRuntimeMode { mode: id.to_owned() },
        "stash-restore" => Event::RestoreStash { index: id.parse().ok()? },
        "stash-drop" => Event::DropStash { index: id.parse().ok()? },
        "more" => Event::ShowAllInSection { section: id.to_owned() },
        "more-project" => Event::ShowAllThreads { project_id: id.to_owned() },
        "new" => Event::NewThread { project_id: Some(id.to_owned()) },
        "route" => Event::ShowRoute { route: id.to_owned() },
        "group" => Event::SetSidebarGrouping { grouping: id.to_owned() },
        "env-select" => Event::SelectEnvironment { id: id.to_owned() },
        "env-up" => Event::MoveEnvironment { id: id.to_owned(), delta: -1 },
        "env-down" => Event::MoveEnvironment { id: id.to_owned(), delta: 1 },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snooze_counts_from_now_and_waking_is_zero() {
        assert!(matches!(event("t-snooze-3h", "t1", 1_000), Some(Event::SnoozeThread { thread_id, until_ms }) if thread_id == "t1" && until_ms == 1_000 + 3 * HOUR_MS));
        assert!(matches!(event("t-snooze-7d", "t1", 0), Some(Event::SnoozeThread { until_ms, .. }) if until_ms == 7 * 24 * HOUR_MS));
        assert!(matches!(event("t-snooze-wake", "t1", 1_000), Some(Event::SnoozeThread { until_ms: 0, .. })));
    }

    #[test]
    fn a_choice_and_its_opposite_differ_only_in_the_flag() {
        assert!(matches!(event("t-pin", "t1", 0), Some(Event::PinThread { pinned: true, .. })));
        assert!(matches!(event("t-unpin", "t1", 0), Some(Event::PinThread { pinned: false, .. })));
        assert!(matches!(event("t-archive", "t1", 0), Some(Event::ArchiveThread { archived: true, .. })));
        assert!(matches!(event("t-unarchive", "t1", 0), Some(Event::ArchiveThread { archived: false, .. })));
        assert!(matches!(event("env-up", "e1", 0), Some(Event::MoveEnvironment { delta: -1, .. })));
    }

    #[test]
    fn full_access_is_not_given_from_the_menu_alone() {
        assert!(matches!(event("perm", "approval-required", 0), Some(Event::SetRuntimeMode { mode }) if mode == "approval-required"));
        assert!(event("perm", "full-access", 0).is_none());
        assert!(matches!(event("stash-drop", "2", 0), Some(Event::DropStash { index: 2 })));
        assert!(event("stash-drop", "two", 0).is_none());
    }

    #[test]
    fn a_choice_that_opens_a_dialog_is_the_windows() {
        for what in ["t-rename", "thread-delete", "t-copy", "icon", "earlier", "no-such-choice"] {
            assert!(event(what, "x", 0).is_none(), "{what}");
        }
    }
}
