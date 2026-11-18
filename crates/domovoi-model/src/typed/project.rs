//! Projects as the thread list shows them (`orchestrationProject.ts`,
//! `project.ts`), and the stream that keeps the list current.
//!
//! A shell stream item that is already parsed JSON is best decoded with
//! [`ShellStreamItem::from_value`], which takes the JSON apart. `Deserialize`
//! on a `serde_json::Value` gives the same item but rebuilds the JSON first.

use super::common::{ModelSelection, Timestamp, double_option};
use super::environment::{RepositoryIdentity, ThreadEnvMode};
use super::ids::{ProjectId, ThreadId};
use super::macros::{string_enum, tagged_union_deserialize};
use super::shell::ThreadShell;
use super::wire::{optional, required};
use serde::de::Error;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

/// Defines the fallback member of a union: a record whose tag this build does
/// not know, kept whole so it encodes back to what was sent.
macro_rules! unknown_member {
    ($(#[$meta:meta])* $name:ident by $tag:literal) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(serde_json::Map<String, serde_json::Value>);

        impl $name {
            #[doc = concat!("The `", $tag, "` this build does not know.")]
            pub fn tag(&self) -> &str {
                self.0.get($tag).and_then(serde_json::Value::as_str).unwrap_or_default()
            }

            #[doc = concat!("The whole record as it was sent, `", $tag, "` included.")]
            pub fn record(&self) -> &serde_json::Map<String, serde_json::Value> {
                &self.0
            }
        }

        impl From<serde_json::Map<String, serde_json::Value>> for $name {
            fn from(record: serde_json::Map<String, serde_json::Value>) -> Self {
                Self(record)
            }
        }
    };
}

pub(crate) use unknown_member;

/// Takes the string field `tag` out of `fields`, leaving its key where it is,
/// so the record can be matched on its tag and then taken apart.
pub(crate) fn take_tag<E: Error>(fields: &mut Map<String, Value>, tag: &'static str) -> Result<String, E> {
    match fields.get_mut(tag) {
        Some(Value::String(value)) => Ok(std::mem::take(value)),
        Some(_) => Err(E::custom(format_args!("`{tag}` is not a string"))),
        None => Err(E::missing_field(tag)),
    }
}

/// Puts back what [`take_tag`] took, for a record that is kept whole.
pub(crate) fn restore_tag(mut fields: Map<String, Value>, tag: &'static str, value: String) -> Map<String, Value> {
    if let Some(Value::String(slot)) = fields.get_mut(tag) {
        *slot = value;
    }
    fields
}

string_enum! {
    /// The icon of a project script (`ProjectScriptIcon`).
    ProjectScriptIcon {
        Play = "play",
        Test = "test",
        Lint = "lint",
        Configure = "configure",
        Build = "build",
        Debug = "debug",
    }
}

/// A command a project offers to run (`ProjectScript`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectScript {
    pub id: String,
    pub name: String,
    pub command: String,
    pub icon: ProjectScriptIcon,
    pub run_on_worktree_create: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_on_settle: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_on_settle: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#async: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_open_preview: Option<bool>,
}

string_enum! {
    /// The colour of a project's icon (`ProjectIconColor`).
    ProjectIconColor {
        Gray = "gray",
        Red = "red",
        Orange = "orange",
        Amber = "amber",
        Yellow = "yellow",
        Lime = "lime",
        Green = "green",
        Emerald = "emerald",
        Teal = "teal",
        Cyan = "cyan",
        Sky = "sky",
        Blue = "blue",
        Indigo = "indigo",
        Violet = "violet",
        Purple = "purple",
        Fuchsia = "fuchsia",
        Pink = "pink",
        Rose = "rose",
    }
}

/// The icon a user chose for a project, as a server sends it (`ReceivedProjectIcon`).
///
/// `Other` is an icon kind this build does not know; t3code shows such a
/// project with no chosen icon.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectIcon {
    Lucide(LucideProjectIcon),
    Emoji(EmojiProjectIcon),
    Monogram(MonogramProjectIcon),
    #[serde(untagged)]
    Other(UnknownProjectIcon),
}

tagged_union_deserialize! {
    ProjectIcon by "kind" {
        "lucide" => Lucide,
        "emoji" => Emoji,
        "monogram" => Monogram,
        _ => Other,
    }
}

/// A project icon drawn from the Lucide set (`ProjectLucideIconWithLegacyMonogram`).
///
/// A record written before monograms were a kind of their own carries the
/// monogram's text in `monogram_text` or `monogram`: t3code reads such an
/// icon as a monogram of that text (`monogram_text` first) in this colour.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LucideProjectIcon {
    /// The icon's Lucide name, such as `message-square-dashed`.
    pub name: String,
    pub color: ProjectIconColor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monogram_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monogram: Option<String>,
}

/// A project icon that is an emoji (`ProjectEmojiIcon`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmojiProjectIcon {
    pub emoji: String,
}

/// A project icon that is a few letters on a colour (`ProjectMonogramIcon`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonogramProjectIcon {
    pub text: String,
    pub color: ProjectIconColor,
}

unknown_member! {
    /// A project icon of a kind this build does not know, kept as it was sent.
    UnknownProjectIcon by "kind"
}

/// A project as lists show it (`OrchestrationProjectShell`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectShell {
    pub id: ProjectId,
    pub title: String,
    /// Set once someone renames the project: a folder's default title can equal the repository's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_custom_title: Option<bool>,
    pub workspace_root: String,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub repository_identity: Option<Option<RepositoryIdentity>>,
    pub default_model_selection: Option<ModelSelection>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub default_thread_env_mode: Option<Option<ThreadEnvMode>>,
    /// Whether the server pulls the checkout in the background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_pull: Option<bool>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub favicon_path: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub project_icon: Option<Option<ProjectIcon>>,
    pub scripts: Vec<ProjectScript>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub pinned_at: Option<Option<Timestamp>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<Option<Timestamp>>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// A project's own record (`Project`): what `projects.mutate` answers with
/// after creating, updating or deleting one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: ProjectId,
    pub title: String,
    pub workspace_root: String,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub repository_identity: Option<Option<RepositoryIdentity>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub favicon_path: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub project_icon: Option<Option<ProjectIcon>>,
    pub default_model_selection: Option<ModelSelection>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub default_thread_env_mode: Option<Option<ThreadEnvMode>>,
    /// Whether the server pulls the checkout in the background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_pull: Option<bool>,
    pub scripts: Vec<ProjectScript>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub pinned_at: Option<Option<Timestamp>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<Option<Timestamp>>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub deleted_at: Option<Timestamp>,
}

/// The result of `projects.ensureScratch` (`ProjectEnsureScratchResult`): the
/// project that holds threads started without one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectEnsureScratchResult {
    pub project_id: ProjectId,
}

/// The result of `projects.createNew` (`ProjectCreateNewResult`): the project
/// made in a new folder of its own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCreateNewResult {
    pub project_id: ProjectId,
    pub workspace_root: String,
    /// Why the first commit failed; the project and its files exist either way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_error: Option<String>,
}

/// The result of `projects.createFromThread` (`ProjectCreateFromThreadResult`):
/// the project a scratch thread's folder became.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCreateFromThreadResult {
    pub project_id: ProjectId,
}

/// How many older settled threads a project has (`OrchestrationV2OlderSettledCount`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OlderSettledCount {
    pub project_id: ProjectId,
    pub count: u64,
}

/// The settled threads a snapshot left out (`OrchestrationV2OlderSettledThreads`):
/// those settled before `before`, counted per project as of `as_of_sequence`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OlderSettledThreads {
    pub before: Timestamp,
    pub as_of_sequence: u64,
    pub counts: Vec<OlderSettledCount>,
}

/// Every project and thread as lists show them, as of one point in the
/// server's log (`OrchestrationV2ShellSnapshot`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellSnapshot {
    pub schema_version: u64,
    /// The last event the snapshot includes; changes after it follow on the stream.
    pub snapshot_sequence: u64,
    pub threads: Vec<ThreadShell>,
    pub archived_threads: Vec<ThreadShell>,
    pub projects: Vec<ProjectShell>,
    /// Present when the snapshot leaves out older settled threads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub older_settled: Option<OlderSettledThreads>,
}

/// The shell snapshot `GET /api/orchestration/shell` answers with
/// (`OrchestrationV2ShellHttpSnapshot`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellHttpSnapshot {
    pub schema_version: u64,
    /// The last event the snapshot includes; a subscription resumes after it.
    pub snapshot_sequence: u64,
    pub threads: Vec<ThreadShell>,
    pub archived_threads: Vec<ThreadShell>,
    pub projects: Vec<ProjectShell>,
    /// Present when the snapshot leaves out older settled threads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub older_settled: Option<OlderSettledThreads>,
    /// Names the repository identities the snapshot carries, to hand back as
    /// `knownProjectIdentities` when subscribing. Opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_identities: Option<String>,
}

impl ShellHttpSnapshot {
    /// The snapshot itself and the name of its repository identities.
    pub fn into_parts(self) -> (ShellSnapshot, Option<String>) {
        let Self { schema_version, snapshot_sequence, threads, archived_threads, projects, older_settled, project_identities } = self;
        (ShellSnapshot { schema_version, snapshot_sequence, threads, archived_threads, projects, older_settled }, project_identities)
    }
}

/// What `GET /api/orchestration/shell` answers with: a snapshot
/// (`OrchestrationV2ShellHttpSnapshot`), or, to a request that named the
/// cursor of rows the client holds, that the stream can resume from there
/// (`OrchestrationV2ShellResumable`, `{"resumable": true}`).
#[derive(Clone, Debug, PartialEq)]
pub enum ShellHttpAnswer {
    /// The client keeps its rows and subscribes from its cursor.
    Resumable,
    Snapshot(Box<ShellHttpSnapshot>),
}

impl Serialize for ShellHttpAnswer {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;

        match self {
            Self::Resumable => {
                let mut answer = serializer.serialize_map(Some(1))?;
                answer.serialize_entry("resumable", &true)?;
                answer.end()
            }
            Self::Snapshot(snapshot) => snapshot.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ShellHttpAnswer {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        /// Either answer's fields, read in one pass over the body.
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Body {
            resumable: Option<bool>,
            schema_version: Option<u64>,
            snapshot_sequence: Option<u64>,
            threads: Option<Vec<ThreadShell>>,
            archived_threads: Option<Vec<ThreadShell>>,
            projects: Option<Vec<ProjectShell>>,
            older_settled: Option<OlderSettledThreads>,
            project_identities: Option<String>,
        }

        let body = Body::deserialize(deserializer)?;
        if body.resumable == Some(true) {
            return Ok(Self::Resumable);
        }
        Ok(Self::Snapshot(Box::new(ShellHttpSnapshot {
            schema_version: body.schema_version.ok_or_else(|| D::Error::missing_field("schemaVersion"))?,
            snapshot_sequence: body.snapshot_sequence.ok_or_else(|| D::Error::missing_field("snapshotSequence"))?,
            threads: body.threads.ok_or_else(|| D::Error::missing_field("threads"))?,
            archived_threads: body.archived_threads.ok_or_else(|| D::Error::missing_field("archivedThreads"))?,
            projects: body.projects.ok_or_else(|| D::Error::missing_field("projects"))?,
            older_settled: body.older_settled,
            project_identities: body.project_identities,
        })))
    }
}

/// The archived threads and every project (`OrchestrationV2ArchivedShellSnapshot`):
/// the result of `orchestration.getArchivedShellSnapshot`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchivedShellSnapshot {
    pub schema_version: u64,
    pub snapshot_sequence: u64,
    pub projects: Vec<ProjectShell>,
    /// The archived threads.
    pub threads: Vec<ThreadShell>,
}

/// A page of older settled threads (`OrchestrationV2OlderSettledThreadsPage`),
/// newest settle first: the result of `orchestration.getOlderSettledThreads`.
/// A request with `limit: 0` fills `counts` instead of `threads`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OlderSettledThreadsPage {
    pub snapshot_sequence: u64,
    pub threads: Vec<ThreadShell>,
    pub counts: Vec<OlderSettledCount>,
}

string_enum! {
    /// Which list a thread of the shell stream belongs to.
    ShellThreadLocation {
        Active = "active",
        Archive = "archive",
    }
}

/// One item of a subscription to the shell (`OrchestrationV2ShellStreamItem`).
///
/// `Other` is an item of a kind this build does not know, kept as it was sent.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum ShellStreamItem {
    /// Everything recorded so far has been delivered; what follows is live.
    #[serde(rename = "synchronized")]
    Synchronized,
    /// The server cannot replay from the cursor the client named: load the
    /// HTTP snapshot and subscribe again from its cursor.
    #[serde(rename = "snapshot-required")]
    SnapshotRequired,
    #[serde(rename = "snapshot")]
    Snapshot {
        snapshot: Box<ShellSnapshot>,
        /// The workspace roots whose repository identities this snapshot
        /// resolved. An item that has them only refreshes its projects.
        #[serde(skip_serializing_if = "Option::is_none")]
        resolved_repository_identity_roots: Option<Vec<String>>,
    },
    #[serde(rename = "project.updated")]
    ProjectUpdated { sequence: u64, project: Box<ProjectShell> },
    #[serde(rename = "project.removed")]
    ProjectRemoved { sequence: u64, project_id: ProjectId },
    #[serde(rename = "thread.updated")]
    ThreadUpdated { sequence: u64, location: ShellThreadLocation, thread: Box<ThreadShell> },
    #[serde(rename = "thread.removed")]
    ThreadRemoved { sequence: u64, location: ShellThreadLocation, thread_id: ThreadId },
    #[serde(untagged)]
    Other(UnknownShellStreamItem),
}

unknown_member! {
    /// A shell stream item of a kind this build does not know, kept as it was sent.
    UnknownShellStreamItem by "kind"
}

impl ShellStreamItem {
    /// The item that parsed JSON holds. The JSON is taken apart, not read:
    /// nothing in it is copied.
    pub fn from_value(item: Value) -> serde_json::Result<Self> {
        match item {
            Value::Object(fields) => Self::from_fields(fields),
            _ => Err(serde_json::Error::custom("a shell stream item is an object")),
        }
    }

    /// The item whose JSON object held `fields`.
    fn from_fields<E: Error>(mut fields: Map<String, Value>) -> Result<Self, E> {
        let kind = take_tag(&mut fields, "kind")?;
        match Self::of_known_kind::<E>(&kind, &mut fields) {
            Ok(Some(item)) => Ok(item),
            Ok(None) => Ok(Self::Other(restore_tag(fields, "kind", kind).into())),
            Err(error) => Err(E::custom(format_args!("shell stream item `{kind}`: {error}"))),
        }
    }

    /// The item of `kind` in `fields`, or `None` for a kind this build does not know.
    fn of_known_kind<E: Error>(kind: &str, fields: &mut Map<String, Value>) -> Result<Option<Self>, E> {
        Ok(Some(match kind {
            "synchronized" => Self::Synchronized,
            "snapshot-required" => Self::SnapshotRequired,
            "snapshot" => Self::Snapshot {
                snapshot: required(fields, "snapshot")?,
                resolved_repository_identity_roots: optional(fields, "resolvedRepositoryIdentityRoots")?,
            },
            "project.updated" => Self::ProjectUpdated { sequence: required(fields, "sequence")?, project: required(fields, "project")? },
            "project.removed" => Self::ProjectRemoved { sequence: required(fields, "sequence")?, project_id: required(fields, "projectId")? },
            "thread.updated" => Self::ThreadUpdated {
                sequence: required(fields, "sequence")?,
                location: required(fields, "location")?,
                thread: required(fields, "thread")?,
            },
            "thread.removed" => Self::ThreadRemoved {
                sequence: required(fields, "sequence")?,
                location: required(fields, "location")?,
                thread_id: required(fields, "threadId")?,
            },
            _ => return Ok(None),
        }))
    }

    /// The item's place in the server's log, when it has one: the sequence of
    /// an update, or the sequence a snapshot is as of. For an item of an
    /// unknown kind, its `sequence` if it carries one.
    pub fn sequence(&self) -> Option<u64> {
        match self {
            Self::Synchronized | Self::SnapshotRequired => None,
            Self::Snapshot { snapshot, .. } => Some(snapshot.snapshot_sequence),
            Self::ProjectUpdated { sequence, .. }
            | Self::ProjectRemoved { sequence, .. }
            | Self::ThreadUpdated { sequence, .. }
            | Self::ThreadRemoved { sequence, .. } => Some(*sequence),
            Self::Other(unknown) => unknown.record().get("sequence").and_then(Value::as_u64),
        }
    }
}

impl<'de> Deserialize<'de> for ShellStreamItem {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_fields(Map::deserialize(deserializer)?)
    }
}
