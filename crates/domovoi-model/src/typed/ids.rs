//! Identifier newtypes: one per t3code id brand, so a run id cannot be passed
//! where a thread id is expected.

use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::fmt;
use std::sync::Arc;

macro_rules! id_types {
    ($($(#[$meta:meta])* $name:ident),+ $(,)?) => {$(
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Arc<str>);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(id: &str) -> Self {
                Self(Arc::from(id))
            }
        }

        impl From<String> for $name {
            fn from(id: String) -> Self {
                Self(Arc::from(id))
            }
        }
    )+};
}

id_types! {
    /// A thread: the durable conversation and work history for a project.
    ThreadId,
    /// A project: a workspace rooted at a directory.
    ProjectId,
    /// An environment: one running server and the machine it owns.
    EnvironmentId,
    /// A command a client or the server dispatched.
    CommandId,
    /// An orchestration event.
    EventId,
    /// The provider's own event an orchestration event was translated from.
    RawEventId,
    /// A conversation message.
    MessageId,
    /// A run: one user-to-agent cycle of a thread.
    RunId,
    /// One attempt at carrying out a run.
    RunAttemptId,
    /// A node of a run's execution tree; subagents are identified by their node.
    NodeId,
    /// A guest share link of a thread.
    GuestShareId,
    /// A message a guest proposed through a share link.
    GuestMessageId,
    /// A live provider process the server talks to.
    ProviderSessionId,
    /// A provider's own conversation backing a thread.
    ProviderThreadId,
    /// One native turn of a provider thread.
    ProviderTurnId,
    /// An entry of a thread's timeline.
    TurnItemId,
    /// A question a provider is waiting on: an approval or user input.
    RuntimeRequestId,
    /// A scheduled task that sends messages on its own.
    ScheduledTaskId,
    /// The hidden git ref a checkpoint is stored under.
    CheckpointRef,
    /// A checkpoint: the workspace as it was after a run.
    CheckpointId,
    /// The directory and execution scope checkpoints are taken in.
    CheckpointScopeId,
    /// A summary handed to a provider thread that did not see earlier runs.
    ContextHandoffId,
    /// A transfer of context between threads or providers.
    ContextTransferId,
    /// A plan or todo list an agent produced.
    PlanId,
    /// A configured provider instance, such as `codex` or a second Claude account.
    ProviderInstanceId,
    /// The driver behind a provider instance, such as `codex` or `claudeAgent`.
    ProviderDriverKind,
}
