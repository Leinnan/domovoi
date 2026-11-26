//! Typed records: what t3code's schemas put on the wire, field for field.
//!
//! Two groups of records live here. The first is the orchestration domain
//! (`orchestrationV2.ts`): a thread, the records it is made of, the events
//! that change them and the commands that ask for a change. The second is
//! what a client keeps of a server besides its threads: its settings and
//! configuration with their providers and keybindings, the environment it
//! runs in, its projects and their workspaces, a checkout's version control
//! state and its pull requests, what the settings pages show, and the routes
//! a device signs in through. Each module names the schema file it follows.
//!
//! Every record decodes from the JSON the server sends and encodes back to the
//! same JSON:
//!
//! - a field the schema makes optional is an `Option` left out when `None`;
//! - a nullable field is an `Option` written as `null`;
//! - a field that is both is an `Option<Option<T>>`, so absent and `null` stay
//!   distinct;
//! - a closed set of strings is an enum whose `Other` variant keeps a value
//!   from a newer server verbatim;
//! - a timestamp stays the string it was sent as ([`Timestamp`]);
//! - a number the schema calls finite rather than integral is a
//!   [`serde_json::Number`], which keeps `1` and `1.0` apart.
//!
//! Records list every field their schema defines and nothing else: a field
//! this build does not know is dropped on decode, as t3code drops it.
//! `serde_json::Value` appears only where the schema itself is open, and each
//! such field says so.
//!
//! One spelling is normalised. t3code's JSON codec writes an optional field
//! whose key is present but undefined as `null`, and reads `null` back as
//! undefined. Such a `null` decodes as `None` here and is written back absent,
//! which is the same value to every reader of the schema.
//!
//! On top of a thread's records sit the events that change them
//! ([`DomainEvent`]), what a subscription to a thread delivers
//! ([`ThreadStreamItem`]), and a thread held in memory ([`Thread`]): kept
//! current by applying events, and the source of what is derived from its
//! records (its [`ThreadShell`], its timeline as shown). Records the schema's
//! rules do not cover (a field missing, a value of the wrong type) do not
//! decode: nothing is guessed. The one exception is a list a newer server may
//! add to ([`ForwardList`]), which sets aside the elements it cannot read.

mod macros;
mod wire;

mod attachment;
mod checkpoint;
pub mod command;
mod common;
mod context;
mod derive;
mod event;
mod ids;
mod issue;
mod live;
mod message;
mod notify;
mod plan;
mod provider;
mod provider_setup;
mod pull_request;
mod record;
mod run;
mod shell;
mod state;
mod stream;
mod thread;
mod turn_item;

// What a client keeps of a server besides its threads.
mod environment;
mod keybinding;
mod list;
mod pages;
mod project;
mod remote_access;
mod review;
mod server;
mod settings;
mod source_control;
mod vcs;
mod workspace;

pub use attachment::*;
pub use checkpoint::*;
pub use command::{
    Command, DispatchCommandError, DispatchCommandResult, DispatchMode, ForkSourcePoint, InitialMessage, OtherCommand, ThreadLaunchError,
    ThreadLaunchInput, ThreadLaunchResult,
};
pub use common::*;
pub use context::*;
pub use event::*;
pub use ids::*;
pub use live::{ACTION_CHARS, LiveProgress, SUMMARY_CHARS, TaskCount, clip as clip_chars, line as first_line_clipped, live_progress};
pub use message::*;
pub use notify::*;
pub use plan::*;
pub use provider::*;
pub use provider_setup::*;
pub use pull_request::*;
pub use record::{Record, RecordKind, RecordRef, Records, RecordsIter, StoredRecord};
pub use run::*;
pub use shell::*;
pub use state::*;
pub use stream::*;
pub use thread::*;
pub use issue::*;
pub use turn_item::*;

pub use environment::*;
pub use keybinding::*;
pub use list::ForwardList;
pub use pages::*;
pub use project::*;
pub use remote_access::*;
pub use review::*;
pub use server::*;
pub use settings::*;
pub use source_control::*;
pub use vcs::*;
pub use workspace::*;
