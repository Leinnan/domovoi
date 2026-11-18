//! Store-backed read models; the derivation rules live in `domovoi_model::typed`.

use crate::store::{Store, StoreError};
use domovoi_model::typed::{Thread, ThreadProjection, ThreadShell};

/// The thread as its stored rows hold it. `None` for a thread with no record,
/// or one whose record does not decode.
pub fn thread(store: &Store, thread_id: &str) -> Result<Option<Thread>, StoreError> {
    store.load_thread(thread_id)
}

pub fn thread_projection(store: &Store, thread_id: &str) -> Result<Option<ThreadProjection>, StoreError> {
    Ok(thread(store, thread_id)?.map(|t| t.projection()))
}

pub fn thread_shell(store: &Store, thread_id: &str) -> Result<Option<ThreadShell>, StoreError> {
    Ok(thread(store, thread_id)?.map(|t| t.shell()))
}
