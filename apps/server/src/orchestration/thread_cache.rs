//! Recently used threads held in memory, kept current event by event, so a
//! thread's sidebar shell is derived once per event instead of reloading and
//! re-parsing the whole thread for every subscriber of the shell stream.
//!
//! Each event is applied right after its transaction commits, with the
//! database's writer lock still held, so the cache sees events in commit
//! order. A thread enters the cache under the same lock, loaded from the
//! projection tables, so no event lands between the load and the entry. The
//! rules are `domovoi_model::typed::Thread`'s, the same as the SQL projection's.

use crate::store::StoredEvent;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use domovoi_model::typed::{Thread, ThreadShell};

/// Threads kept; the least recently used one leaves first.
const CAPACITY: usize = 32;

struct Entry {
    /// Shared copy-on-write: a reader deriving a shell holds it without
    /// blocking the writer, which copies the thread (its records stay shared)
    /// only if a reader still has it.
    thread: Arc<Thread>,
    /// The global sequence of the last event applied.
    sequence: u64,
    /// The shell derived at `sequence`, once someone asked for it.
    shell: Option<(u64, Arc<ThreadShell>)>,
    used: u64,
}

#[derive(Default)]
pub struct ThreadCache {
    entries: Mutex<(HashMap<String, Entry>, u64)>,
}

impl ThreadCache {
    /// Applies a committed thread event to its thread, if it is cached. Call
    /// with the writer lock held, after the commit.
    pub fn apply(&self, stored: &StoredEvent) {
        let Some(event) = &stored.thread_event else { return };
        let mut guard = crate::util::locked(&self.entries);
        // An entry loaded after the event (inside the batch that wrote it,
        // before the batch published) already holds it.
        if let Some(e) = guard.0.get_mut(&stored.aggregate_id).filter(|e| stored.sequence > e.sequence) {
            Arc::make_mut(&mut e.thread).apply(event.clone());
            e.sequence = stored.sequence;
        }
    }

    /// Whether the thread is cached.
    pub fn contains(&self, thread_id: &str) -> bool {
        crate::util::locked(&self.entries).0.contains_key(thread_id)
    }

    /// Caches a thread loaded at `sequence`. Call with the writer lock held,
    /// so no event is committed between the load and this.
    pub fn insert(&self, thread_id: &str, thread: Thread, sequence: u64) {
        let mut guard = crate::util::locked(&self.entries);
        let (entries, clock) = &mut *guard;
        *clock += 1;
        if entries.len() >= CAPACITY && !entries.contains_key(thread_id) {
            if let Some(oldest) = entries.iter().min_by_key(|(_, e)| e.used).map(|(k, _)| k.clone()) {
                entries.remove(&oldest);
            }
        }
        entries.insert(thread_id.to_owned(), Entry { thread: Arc::new(thread), sequence, shell: None, used: *clock });
    }

    /// Forgets threads (pruned ones).
    pub fn remove(&self, thread_ids: &[String]) {
        let mut guard = crate::util::locked(&self.entries);
        for id in thread_ids {
            guard.0.remove(id);
        }
    }

    /// The thread's shell as of the latest applied event, derived at most once
    /// per event. `None` if the thread is not cached.
    pub fn shell(&self, thread_id: &str) -> Option<Arc<ThreadShell>> {
        let (thread, sequence) = {
            let mut guard = crate::util::locked(&self.entries);
            let (entries, clock) = &mut *guard;
            *clock += 1;
            let e = entries.get_mut(thread_id)?;
            e.used = *clock;
            if let Some((at, shell)) = &e.shell {
                if *at == e.sequence {
                    return Some(shell.clone());
                }
            }
            (e.thread.clone(), e.sequence)
        };
        // Derived outside the lock: events keep being applied meanwhile.
        let shell = Arc::new(thread.shell());
        let mut guard = crate::util::locked(&self.entries);
        if let Some(e) = guard.0.get_mut(thread_id) {
            if e.sequence == sequence {
                e.shell = Some((sequence, shell.clone()));
            }
        }
        Some(shell)
    }
}
