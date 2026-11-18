//! The records a thread is made of, and the collection each kind is kept in.
//!
//! A [`Records`] collection holds one kind of record at its latest version:
//!
//! - records stay in the order they first arrived, except turn items, which
//!   are kept in timeline order (`ordinal`, then id);
//! - a record is found by its id without a search;
//! - records are shared (`Arc`), so copying a collection copies pointers, not text.

use super::checkpoint::{Checkpoint, CheckpointScope};
use super::common::Timestamp;
use super::context::{ContextHandoff, ContextTransfer};
use super::ids::{
    CheckpointId, CheckpointScopeId, ContextHandoffId, ContextTransferId, MessageId, NodeId, PlanId, ProviderSessionId,
    ProviderThreadId, ProviderTurnId, RunAttemptId, RunId, RuntimeRequestId, TurnItemId,
};
use super::message::ConversationMessage;
use super::plan::PlanArtifact;
use super::provider::{ProviderSession, ProviderThread, ProviderTurn, RuntimeRequest};
use super::run::{ExecutionNode, Run, RunAttempt, Subagent};
use super::turn_item::TurnItem;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Deserializer};
use std::borrow::Borrow;
use std::cmp::Ordering;
use std::collections::hash_map::Entry;
use std::fmt;
use std::hash::Hash;
use std::ops::Range;
use std::sync::Arc;

/// A record a thread keeps a collection of.
pub trait Record: Sized {
    type Id: Clone + Eq + Ord + Hash;

    fn id(&self) -> &Self::Id;

    /// The record's place in its collection, for a kind kept in an order of
    /// its own (`ordinal`, then id). `None` keeps the order of arrival.
    fn ordinal(&self) -> Option<u64> {
        None
    }

    /// Whether `new` is `old` again with more of the text that grows while a
    /// reply streams: every other field equal, `updatedAt` aside. Always
    /// `false` for a kind that does not stream.
    ///
    /// `old` is the version about to be replaced, and may be left without its text.
    fn only_text_grew(_old: &mut Arc<Self>, _new: &mut Self) -> bool {
        false
    }
}

/// Whether `old` and `new` are equal once their `growing` text and their
/// `updatedAt` are set aside. Leaves `old` without its text.
fn same_apart_from<T: PartialEq, G: Default>(
    old: &mut T,
    new: &mut T,
    growing: fn(&mut T) -> &mut G,
    updated_at: fn(&mut T) -> &mut Timestamp,
) -> bool {
    let grown = std::mem::take(growing(new));
    *growing(old) = G::default();
    *updated_at(old) = updated_at(new).clone();
    let same = old == new;
    *growing(new) = grown;
    same
}

impl Record for ConversationMessage {
    type Id = MessageId;

    fn id(&self) -> &MessageId {
        &self.id
    }

    fn only_text_grew(old: &mut Arc<Self>, new: &mut Self) -> bool {
        same_apart_from(Arc::make_mut(old), new, |message| &mut message.text, |message| &mut message.updated_at)
    }
}

impl Record for TurnItem {
    type Id = TurnItemId;

    fn id(&self) -> &TurnItemId {
        TurnItem::id(self)
    }

    fn ordinal(&self) -> Option<u64> {
        Some(TurnItem::ordinal(self))
    }

    /// The items that stream: an assistant message and reasoning grow in
    /// `text`, a proposed plan in `markdown`, a command in `output` (which
    /// must already be there: its first output adds a part to the row).
    fn only_text_grew(old: &mut Arc<Self>, new: &mut Self) -> bool {
        let streams = matches!(
            (&**old, &*new),
            (Self::AssistantMessage(_), Self::AssistantMessage(_))
                | (Self::Reasoning(_), Self::Reasoning(_))
                | (Self::ProposedPlan(_), Self::ProposedPlan(_))
                | (Self::CommandExecution(_), Self::CommandExecution(_))
        );
        if !streams {
            return false;
        }
        match (Arc::make_mut(old), new) {
            (Self::AssistantMessage(old), Self::AssistantMessage(new)) => {
                same_apart_from(&mut **old, &mut **new, |item| &mut item.text, |item| &mut item.updated_at)
            }
            (Self::Reasoning(old), Self::Reasoning(new)) => {
                same_apart_from(&mut **old, &mut **new, |item| &mut item.text, |item| &mut item.updated_at)
            }
            (Self::ProposedPlan(old), Self::ProposedPlan(new)) => {
                same_apart_from(&mut **old, &mut **new, |item| &mut item.markdown, |item| &mut item.updated_at)
            }
            (Self::CommandExecution(old), Self::CommandExecution(new)) => {
                old.output.is_some() == new.output.is_some()
                    && same_apart_from(&mut **old, &mut **new, |item| &mut item.output, |item| &mut item.updated_at)
            }
            _ => false,
        }
    }
}

impl Record for PlanArtifact {
    type Id = PlanId;

    fn id(&self) -> &PlanId {
        PlanArtifact::id(self)
    }
}

/// Declares the kinds of record a thread keeps: for each, its record type, its
/// id type and the name of its collection in a thread's projection.
macro_rules! record_kinds {
    ($($(#[$meta:meta])* $kind:ident($record:ty, $id:ty) = $collection:literal,)+) => {
        /// A kind of record a thread keeps a collection of.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum RecordKind {
            $($(#[$meta])* $kind,)+
        }

        impl RecordKind {
            /// Every kind, in the order a thread's projection lists its collections.
            pub const ALL: [Self; 15] = [$(Self::$kind,)+];

            /// The name of this kind's collection in a thread's projection, such as `turnItems`.
            pub const fn collection(self) -> &'static str {
                match self {
                    $(Self::$kind => $collection,)+
                }
            }

            /// The kind kept in the projection's collection `name`.
            pub fn of_collection(name: &str) -> Option<Self> {
                match name {
                    $($collection => Some(Self::$kind),)+
                    _ => None,
                }
            }
        }

        /// Names one record of a thread: its kind and its id.
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        pub enum RecordRef {
            $($kind($id),)+
        }

        impl RecordRef {
            pub fn kind(&self) -> RecordKind {
                match self {
                    $(Self::$kind(_) => RecordKind::$kind,)+
                }
            }

            /// The record's id, whatever its kind.
            pub fn id(&self) -> &str {
                match self {
                    $(Self::$kind(id) => id.as_str(),)+
                }
            }
        }

        /// One record of a thread, of whichever kind, as the server stores it:
        /// a row of `(kind, record)`.
        #[derive(Clone, Debug, PartialEq)]
        pub enum StoredRecord {
            $($kind(Arc<$record>),)+
        }

        impl StoredRecord {
            /// Decodes a record of the given kind.
            pub fn decode<'de, D: Deserializer<'de>>(kind: RecordKind, record: D) -> Result<Self, D::Error> {
                match kind {
                    $(RecordKind::$kind => <$record>::deserialize(record).map(|record| Self::$kind(Arc::new(record))),)+
                }
            }

            /// Which record this is.
            pub fn reference(&self) -> RecordRef {
                match self {
                    $(Self::$kind(record) => RecordRef::$kind(Record::id(&**record).clone()),)+
                }
            }
        }
    };
}

record_kinds! {
    Run(Run, RunId) = "runs",
    Attempt(RunAttempt, RunAttemptId) = "attempts",
    Node(ExecutionNode, NodeId) = "nodes",
    /// Subagents are identified by the node that started them.
    Subagent(Subagent, NodeId) = "subagents",
    ProviderSession(ProviderSession, ProviderSessionId) = "providerSessions",
    ProviderThread(ProviderThread, ProviderThreadId) = "providerThreads",
    ProviderTurn(ProviderTurn, ProviderTurnId) = "providerTurns",
    RuntimeRequest(RuntimeRequest, RuntimeRequestId) = "runtimeRequests",
    Message(ConversationMessage, MessageId) = "messages",
    Plan(PlanArtifact, PlanId) = "plans",
    TurnItem(TurnItem, TurnItemId) = "turnItems",
    CheckpointScope(CheckpointScope, CheckpointScopeId) = "checkpointScopes",
    Checkpoint(Checkpoint, CheckpointId) = "checkpoints",
    ContextHandoff(ContextHandoff, ContextHandoffId) = "contextHandoffs",
    ContextTransfer(ContextTransfer, ContextTransferId) = "contextTransfers",
}

impl StoredRecord {
    /// Decodes a record of the given kind from its JSON text.
    pub fn from_json(kind: RecordKind, record: &str) -> serde_json::Result<Self> {
        Self::decode(kind, &mut serde_json::Deserializer::from_str(record))
    }
}

/// Implements [`Record`] for the kinds identified by their `id` field and kept
/// in order of arrival.
macro_rules! records_by_id_field {
    ($($record:ty => $id:ty,)+) => {$(
        impl Record for $record {
            type Id = $id;

            fn id(&self) -> &$id {
                &self.id
            }
        }
    )+};
}

records_by_id_field! {
    Run => RunId,
    RunAttempt => RunAttemptId,
    ExecutionNode => NodeId,
    Subagent => NodeId,
    ProviderSession => ProviderSessionId,
    ProviderThread => ProviderThreadId,
    ProviderTurn => ProviderTurnId,
    RuntimeRequest => RuntimeRequestId,
    CheckpointScope => CheckpointScopeId,
    Checkpoint => CheckpointId,
    ContextHandoff => ContextHandoffId,
    ContextTransfer => ContextTransferId,
}

/// The order a collection keeps: by ordinal then id for a kind that has
/// ordinals, the order of arrival (every record equal) for one that does not.
fn order<T: Record>(a: &T, b: &T) -> Ordering {
    match (a.ordinal(), b.ordinal()) {
        (Some(a_ordinal), Some(b_ordinal)) => a_ordinal.cmp(&b_ordinal).then_with(|| a.id().cmp(b.id())),
        _ => Ordering::Equal,
    }
}

/// What putting a record into its collection did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Put {
    /// The id was new. `at_end` is false when the record went in before others.
    Added { at_end: bool },
    /// The record replaced its earlier version, and `moved` if it took another place.
    Replaced { moved: bool, only_text_grew: bool },
}

/// One kind of a thread's records, each at its latest version.
#[derive(Clone)]
pub struct Records<T: Record> {
    records: Vec<Arc<T>>,
    /// Where each record is in `records`.
    index: FxHashMap<T::Id, usize>,
}

impl<T: Record> Default for Records<T> {
    fn default() -> Self {
        Self { records: Vec::new(), index: FxHashMap::default() }
    }
}

impl<T: Record + fmt::Debug> fmt::Debug for Records<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(&self.records).finish()
    }
}

impl<T: Record + PartialEq> PartialEq for Records<T> {
    fn eq(&self, other: &Self) -> bool {
        self.records == other.records
    }
}

/// Iterates a collection's records in its order.
pub type RecordsIter<'a, T> = std::iter::Map<std::slice::Iter<'a, Arc<T>>, fn(&'a Arc<T>) -> &'a T>;

impl<T: Record> Records<T> {
    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// The record with this id.
    pub fn get<Q>(&self, id: &Q) -> Option<&T>
    where
        T::Id: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.shared(id).map(|record| &**record)
    }

    /// The record with this id, to hold on to without copying it.
    pub fn shared<Q>(&self, id: &Q) -> Option<&Arc<T>>
    where
        T::Id: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.position(id).and_then(|at| self.records.get(at))
    }

    /// Where the record with this id is in the collection's order.
    pub fn position<Q>(&self, id: &Q) -> Option<usize>
    where
        T::Id: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.index.get(id).copied()
    }

    pub fn contains<Q>(&self, id: &Q) -> bool
    where
        T::Id: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.index.contains_key(id)
    }

    /// The records in the collection's order.
    pub fn iter(&self) -> RecordsIter<'_, T> {
        self.records.iter().map(|record| &**record)
    }

    /// The records in the collection's order, as they are shared.
    pub fn as_slice(&self) -> &[Arc<T>] {
        &self.records
    }

    pub fn first(&self) -> Option<&T> {
        self.records.first().map(|record| &**record)
    }

    pub fn last(&self) -> Option<&T> {
        self.records.last().map(|record| &**record)
    }

    /// A collection of `records`: put in the collection's order, and where an
    /// id comes twice, the later version in the earlier one's place.
    pub(crate) fn from_shared(records: Vec<Arc<T>>) -> Self {
        let mut collection = Self::default();
        collection.records.reserve(records.len());
        collection.index.reserve(records.len());
        for record in records {
            match collection.index.entry(record.id().clone()) {
                Entry::Occupied(at) => collection.records[*at.get()] = record,
                Entry::Vacant(at) => {
                    at.insert(collection.records.len());
                    collection.records.push(record);
                }
            }
        }
        if !collection.records.is_sorted_by(|a, b| order(&**a, &**b) != Ordering::Greater) {
            collection.records.sort_by(|a, b| order(&**a, &**b));
            collection.reindex(0..collection.records.len());
        }
        collection
    }

    /// Adds `record`, or replaces the version of it the collection holds.
    pub(crate) fn put(&mut self, mut record: T) -> Put {
        match self.index.get(record.id()).copied() {
            Some(at) => {
                let held = &mut self.records[at];
                let only_text_grew = T::only_text_grew(held, &mut record);
                let reordered = held.ordinal() != record.ordinal();
                *held = Arc::new(record);
                Put::Replaced { moved: reordered && self.reposition(at), only_text_grew }
            }
            None => {
                let at = self.records.partition_point(|held| order(&**held, &record) != Ordering::Greater);
                let at_end = at == self.records.len();
                self.records.insert(at, Arc::new(record));
                self.reindex(at..self.records.len());
                Put::Added { at_end }
            }
        }
    }

    /// Changes the record with this id in place; `false` if there is none.
    pub(crate) fn change(&mut self, id: &T::Id, change: impl FnOnce(&mut T)) -> bool
    where
        T: Clone,
    {
        match self.index.get(id) {
            Some(&at) => {
                change(Arc::make_mut(&mut self.records[at]));
                true
            }
            None => false,
        }
    }

    /// Takes the record with this id out of the collection.
    pub(crate) fn remove(&mut self, id: &T::Id) -> Option<Arc<T>> {
        let at = self.index.remove(id)?;
        let record = self.records.remove(at);
        self.reindex(at..self.records.len());
        Some(record)
    }

    /// Moves the record at `from` to where the collection's order puts it now
    /// that its ordinal changed; whether that is another place.
    fn reposition(&mut self, from: usize) -> bool {
        let record = self.records.remove(from);
        let to = self.records.partition_point(|held| order(&**held, &*record) != Ordering::Greater);
        self.records.insert(to, record);
        self.reindex(from.min(to)..from.max(to) + 1);
        to != from
    }

    /// Notes where each of the records at `positions` now is.
    fn reindex(&mut self, positions: Range<usize>) {
        for at in positions {
            self.index.insert(self.records[at].id().clone(), at);
        }
    }
}

impl<'a, T: Record> IntoIterator for &'a Records<T> {
    type Item = &'a T;
    type IntoIter = RecordsIter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
