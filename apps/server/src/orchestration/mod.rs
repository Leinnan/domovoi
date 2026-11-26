//! Orchestration v2: commands decide domain events; the store persists them and
//! their projection atomically; subscribers stream them.

pub mod audit;
pub mod checkpoints;
pub mod commands;
pub mod context;
pub mod effects;
pub mod naming;
pub mod events;
pub mod failure;
pub mod projection;
pub mod recovery;
pub mod rollback;
pub mod stored;
pub mod stream;
pub mod thread_cache;
