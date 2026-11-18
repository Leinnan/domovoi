//! What a client reads of a project's workspace besides its threads: search
//! across threads and inside one (`threadSearch.ts`, `orchestrationV2.ts`),
//! the workspace's files (`project.ts`, `filesystem.ts`), its terminals
//! (`terminal.ts`), asset links (`assets.ts`, `chatAttachment.ts`) and diffs
//! (`checkpointDiff.ts`, `review.ts`).
//!
//! A terminal stream item that is already parsed JSON is best decoded with
//! its `from_value`, which takes the JSON apart. `Deserialize` on a
//! `serde_json::Value` gives the same item but rebuilds the JSON first.

use super::attachment::ChatAttachment;
use super::common::Timestamp;
use super::ids::{MessageId, ProjectId, RunId, ThreadId};
use super::macros::string_enum;
use super::project::{restore_tag, take_tag, unknown_member};
use super::shell::ThreadShell;
use super::wire::{optional, required};
use serde::de::Error;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Number, Value};

// ---- search ----

string_enum! {
    /// Whose message a thread search matched (`OrchestrationThreadSearchSource`).
    ThreadSearchSource {
        User = "user",
        Assistant = "assistant",
    }
}

/// A thread whose messages match a search (`OrchestrationThreadSearchMatch`),
/// with the best matching message's text around the match.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSearchMatch {
    pub thread_id: ThreadId,
    pub project_id: ProjectId,
    pub source: ThreadSearchSource,
    /// At most 240 characters.
    pub snippet: String,
    pub message_created_at: Option<Timestamp>,
    /// How well the message matches, from 0 to 1.25. Older servers omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<Number>,
    /// How many of the thread's newest messages match. Older servers omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hits: Option<u64>,
    /// The message the snippet is from. Older servers omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
}

/// The result of `orchestration.searchThreads` (`OrchestrationV2SearchThreadsResult`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchThreadsResult {
    pub matches: Vec<ThreadSearchMatch>,
    /// The sequence `threads` are as of; present with `threads`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_sequence: Option<u64>,
    /// The best matching older settled threads, which a shell that omits them
    /// does not have. Present when the search named `olderSettledBefore`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threads: Option<Vec<ThreadShell>>,
}

/// One match of a search inside a thread (`OrchestrationV2ThreadFindMatch`):
/// the timeline entry it is in and which of the entry's matches it is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadFindMatch {
    /// A message id, or a turn item id for an entry that is not a message.
    pub entry_id: String,
    pub run_id: Option<RunId>,
    pub occurrence: u64,
}

/// A timeline entry with matches of a search inside a thread: one element of
/// `OrchestrationV2SearchThreadResult.navigation`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadFindEntry {
    /// A message id, or a turn item id for an entry that is not a message.
    pub entry_id: String,
    pub run_id: Option<RunId>,
    /// The index, among all the thread's matches, of the entry's first.
    pub start_index: u64,
    pub count: u64,
}

/// The result of `orchestration.searchThread`, the search inside one thread
/// (`OrchestrationV2SearchThreadResult`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindInThreadResult {
    /// Whether the whole thread was searched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complete: Option<bool>,
    pub snapshot_sequence: u64,
    pub total_matches: u64,
    pub active_index: u64,
    /// The match at `active_index`; `null` when nothing matches.
    #[serde(rename = "match")]
    pub active_match: Option<ThreadFindMatch>,
    /// The entries with matches around the active one, at most 17.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub navigation: Option<Vec<ThreadFindEntry>>,
}

// ---- files ----

string_enum! {
    /// Whether a workspace entry is a file or a directory (`ProjectEntryKind`).
    ProjectEntryKind {
        File = "file",
        Directory = "directory",
    }
}

/// A file or directory of a workspace (`ProjectEntry`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectEntry {
    /// Relative to the workspace root.
    pub path: String,
    pub kind: ProjectEntryKind,
    /// Whether git ignores the entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignored: Option<bool>,
}

/// The result of `projects.searchEntries` (`ProjectSearchEntriesResult`) and of
/// `projects.listEntries` (`ProjectListEntriesResult`), which have one shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectEntriesResult {
    pub entries: Vec<ProjectEntry>,
    /// Whether there were more entries than the result holds.
    pub truncated: bool,
}

/// The result of `projects.readFile` (`ProjectReadFileResult`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectReadFileResult {
    pub relative_path: String,
    pub contents: String,
    pub byte_length: u64,
    pub truncated: bool,
    /// `false` only for a file the request allowed to be missing. Older servers omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
}

/// A directory offered while a path is typed (`FilesystemBrowseEntry`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesystemBrowseEntry {
    pub name: String,
    pub full_path: String,
}

/// The result of `filesystem.browse` (`FilesystemBrowseResult`): the
/// directories of `parent_path` that match what was typed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesystemBrowseResult {
    pub parent_path: String,
    pub entries: Vec<FilesystemBrowseEntry>,
}

// ---- terminals ----

string_enum! {
    /// What a terminal's process is doing (`TerminalSessionStatus`).
    TerminalSessionStatus {
        Starting = "starting",
        Running = "running",
        Exited = "exited",
        Error = "error",
    }
}

/// A terminal with the output it has kept (`TerminalSessionSnapshot`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSessionSnapshot {
    pub thread_id: String,
    pub terminal_id: String,
    pub cwd: String,
    pub worktree_path: Option<String>,
    pub status: TerminalSessionStatus,
    pub pid: Option<u64>,
    /// The kept tail of the terminal's output, escape sequences included.
    pub history: String,
    pub exit_code: Option<i64>,
    pub exit_signal: Option<i64>,
    pub label: String,
    /// When the terminal last changed; the schema leaves its format open.
    pub updated_at: String,
    /// The last event the history includes; events after it follow on the stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
    /// The width of the grid every renderer of the terminal draws. Older servers omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<u64>,
    /// The height of that grid. Older servers omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
}

/// A terminal as lists show it (`TerminalSummary`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSummary {
    pub thread_id: String,
    pub terminal_id: String,
    pub cwd: String,
    pub worktree_path: Option<String>,
    pub status: TerminalSessionStatus,
    pub pid: Option<u64>,
    pub exit_code: Option<i64>,
    pub exit_signal: Option<i64>,
    pub has_running_subprocess: bool,
    pub label: String,
    /// When the terminal last changed; the schema leaves its format open.
    pub updated_at: String,
}

/// One item of `subscribeTerminalMetadata` (`TerminalMetadataStreamEvent`).
///
/// `Other` is an event of a type this build does not know, kept as it was sent.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum TerminalMetadataEvent {
    /// Every terminal there is.
    Snapshot { terminals: Vec<TerminalSummary> },
    /// A terminal that is new or changed.
    Upsert { terminal: Box<TerminalSummary> },
    /// A terminal that was closed.
    Remove { thread_id: String, terminal_id: String },
    #[serde(untagged)]
    Other(UnknownTerminalMetadataEvent),
}

unknown_member! {
    /// A terminal metadata event of a type this build does not know, kept as it was sent.
    UnknownTerminalMetadataEvent by "type"
}

impl TerminalMetadataEvent {
    /// The event that parsed JSON holds. The JSON is taken apart, not read:
    /// nothing in it is copied.
    pub fn from_value(event: Value) -> serde_json::Result<Self> {
        match event {
            Value::Object(fields) => Self::from_fields(fields),
            _ => Err(serde_json::Error::custom("a terminal metadata event is an object")),
        }
    }

    /// The event whose JSON object held `fields`.
    fn from_fields<E: Error>(mut fields: Map<String, Value>) -> Result<Self, E> {
        let event_type = take_tag(&mut fields, "type")?;
        match Self::of_known_type::<E>(&event_type, &mut fields) {
            Ok(Some(event)) => Ok(event),
            Ok(None) => Ok(Self::Other(restore_tag(fields, "type", event_type).into())),
            Err(error) => Err(E::custom(format_args!("terminal metadata event `{event_type}`: {error}"))),
        }
    }

    /// The event of `event_type` in `fields`, or `None` for a type this build does not know.
    fn of_known_type<E: Error>(event_type: &str, fields: &mut Map<String, Value>) -> Result<Option<Self>, E> {
        Ok(Some(match event_type {
            "snapshot" => Self::Snapshot { terminals: required(fields, "terminals")? },
            "upsert" => Self::Upsert { terminal: required(fields, "terminal")? },
            "remove" => Self::Remove { thread_id: required(fields, "threadId")?, terminal_id: required(fields, "terminalId")? },
            _ => return Ok(None),
        }))
    }
}

impl<'de> Deserialize<'de> for TerminalMetadataEvent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_fields(Map::deserialize(deserializer)?)
    }
}

/// One item of `terminal.attach` and of `terminal.observe`
/// (`TerminalAttachStreamEvent`): the terminal as it is, then what happens to it.
///
/// Every event but the snapshot names its terminal and, from servers that
/// number events, its `sequence` in the terminal's own count. `Other` is an
/// event of a type this build does not know, kept as it was sent.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum TerminalAttachEvent {
    /// The terminal as it is: the stream's first item.
    Snapshot { snapshot: Box<TerminalSessionSnapshot> },
    /// Bytes the terminal printed, escape sequences included.
    Output {
        thread_id: String,
        terminal_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        sequence: Option<u64>,
        data: String,
    },
    /// The terminal's process ended.
    Exited {
        thread_id: String,
        terminal_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        sequence: Option<u64>,
        exit_code: Option<i64>,
        exit_signal: Option<i64>,
    },
    /// The terminal was closed: the stream's last item.
    Closed {
        thread_id: String,
        terminal_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        sequence: Option<u64>,
    },
    /// The terminal failed.
    Error {
        thread_id: String,
        terminal_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        sequence: Option<u64>,
        message: String,
    },
    /// The terminal's kept output was cleared.
    Cleared {
        thread_id: String,
        terminal_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        sequence: Option<u64>,
    },
    /// The terminal's process was started again; the snapshot replaces what was shown.
    Restarted {
        thread_id: String,
        terminal_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        sequence: Option<u64>,
        snapshot: Box<TerminalSessionSnapshot>,
    },
    /// What the terminal is running changed, or its grid was resized.
    Activity {
        thread_id: String,
        terminal_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        sequence: Option<u64>,
        has_running_subprocess: bool,
        label: String,
        /// The new width of the grid, when it changed.
        #[serde(skip_serializing_if = "Option::is_none")]
        cols: Option<u64>,
        /// The new height of the grid, when it changed.
        #[serde(skip_serializing_if = "Option::is_none")]
        rows: Option<u64>,
    },
    #[serde(untagged)]
    Other(UnknownTerminalAttachEvent),
}

unknown_member! {
    /// A terminal event of a type this build does not know, kept as it was sent.
    UnknownTerminalAttachEvent by "type"
}

impl TerminalAttachEvent {
    /// The event that parsed JSON holds. The JSON is taken apart, not read:
    /// nothing in it is copied.
    pub fn from_value(event: Value) -> serde_json::Result<Self> {
        match event {
            Value::Object(fields) => Self::from_fields(fields),
            _ => Err(serde_json::Error::custom("a terminal event is an object")),
        }
    }

    /// The event whose JSON object held `fields`.
    fn from_fields<E: Error>(mut fields: Map<String, Value>) -> Result<Self, E> {
        let event_type = take_tag(&mut fields, "type")?;
        match Self::of_known_type::<E>(&event_type, &mut fields) {
            Ok(Some(event)) => Ok(event),
            Ok(None) => Ok(Self::Other(restore_tag(fields, "type", event_type).into())),
            Err(error) => Err(E::custom(format_args!("terminal event `{event_type}`: {error}"))),
        }
    }

    /// The event of `event_type` in `fields`, or `None` for a type this build does not know.
    fn of_known_type<E: Error>(event_type: &str, fields: &mut Map<String, Value>) -> Result<Option<Self>, E> {
        if event_type == "snapshot" {
            return Ok(Some(Self::Snapshot { snapshot: required(fields, "snapshot")? }));
        }
        if !matches!(event_type, "output" | "exited" | "closed" | "error" | "cleared" | "restarted" | "activity") {
            return Ok(None);
        }
        let thread_id = required(fields, "threadId")?;
        let terminal_id = required(fields, "terminalId")?;
        let sequence = optional(fields, "sequence")?;
        Ok(Some(match event_type {
            "output" => Self::Output { thread_id, terminal_id, sequence, data: required(fields, "data")? },
            "exited" => Self::Exited {
                thread_id,
                terminal_id,
                sequence,
                exit_code: required(fields, "exitCode")?,
                exit_signal: required(fields, "exitSignal")?,
            },
            "closed" => Self::Closed { thread_id, terminal_id, sequence },
            "error" => Self::Error { thread_id, terminal_id, sequence, message: required(fields, "message")? },
            "cleared" => Self::Cleared { thread_id, terminal_id, sequence },
            "restarted" => Self::Restarted { thread_id, terminal_id, sequence, snapshot: required(fields, "snapshot")? },
            _ => Self::Activity {
                thread_id,
                terminal_id,
                sequence,
                has_running_subprocess: required(fields, "hasRunningSubprocess")?,
                label: required(fields, "label")?,
                cols: optional(fields, "cols")?,
                rows: optional(fields, "rows")?,
            },
        }))
    }

    /// The event's place in the terminal's own count, when it has one: its
    /// `sequence`, or the sequence a snapshot is as of. For an event of an
    /// unknown type, its `sequence` if it carries one.
    pub fn sequence(&self) -> Option<u64> {
        match self {
            Self::Snapshot { snapshot } => snapshot.sequence,
            Self::Output { sequence, .. }
            | Self::Exited { sequence, .. }
            | Self::Closed { sequence, .. }
            | Self::Error { sequence, .. }
            | Self::Cleared { sequence, .. }
            | Self::Restarted { sequence, .. }
            | Self::Activity { sequence, .. } => *sequence,
            Self::Other(unknown) => unknown.record().get("sequence").and_then(Value::as_u64),
        }
    }
}

impl<'de> Deserialize<'de> for TerminalAttachEvent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_fields(Map::deserialize(deserializer)?)
    }
}

// ---- assets ----

/// The pixel size of an image asset (`AssetImageDimensions`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetImageDimensions {
    pub width: u64,
    pub height: u64,
}

/// The result of `assets.createUrl` (`AssetCreateUrlResult`): a signed link
/// to a file the server holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetCreateUrlResult {
    /// Relative to the server's origin; signed anew on every request.
    pub relative_url: String,
    /// When the link stops working, in milliseconds since the Unix epoch.
    pub expires_at: Number,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_dimensions: Option<AssetImageDimensions>,
    /// Changes only when the file's contents or identity change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_revision: Option<String>,
}

/// The result of `assets.persistChatAttachments` (`PersistChatAttachmentsResult`):
/// the uploaded files as attachments a message can carry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistChatAttachmentsResult {
    pub attachments: Vec<ChatAttachment>,
}

// ---- diffs ----

/// What a thread's turns changed in its workspace (`ThreadTurnDiff`): the
/// result of `orchestration.getTurnDiff` and of `orchestration.getFullThreadDiff`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTurnDiff {
    pub from_turn_count: u64,
    pub to_turn_count: u64,
    pub thread_id: ThreadId,
    /// A unified diff; one the server cut off ends with `\n\n[truncated]`.
    pub diff: String,
}

string_enum! {
    /// What a previewed diff compares (`ReviewDiffPreviewSourceKind`).
    ReviewDiffPreviewSourceKind {
        WorkingTree = "working-tree",
        BranchRange = "branch-range",
        Commit = "commit",
    }
}

/// How much a previewed diff changes one file (`ReviewDiffFileStat`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDiffFileStat {
    pub path: String,
    /// The path the file was renamed from.
    pub previous_path: Option<String>,
    pub additions: Number,
    pub deletions: Number,
}

/// One diff of a preview (`ReviewDiffPreviewSource`): the uncommitted
/// changes, a branch against its base, or a commit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDiffPreviewSource {
    pub id: String,
    pub kind: ReviewDiffPreviewSourceKind,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_subject: Option<String>,
    pub base_ref: Option<String>,
    pub head_ref: Option<String>,
    /// A unified diff; empty when the patches were deferred.
    pub diff: String,
    pub diff_hash: String,
    /// Whether the server cut the diff off at its size limit.
    pub truncated: bool,
    /// Whether `diff` was left out, to be asked for per file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patches_deferred: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<ReviewDiffFileStat>>,
}

/// The result of `review.getDiffPreview` (`ReviewDiffPreviewResult`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDiffPreviewResult {
    pub cwd: String,
    pub generated_at: Timestamp,
    pub sources: Vec<ReviewDiffPreviewSource>,
    /// Whether a source could not be read in full.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete: Option<bool>,
}
