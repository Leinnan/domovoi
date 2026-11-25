//! The `issueTrackers.*` RPCs (t3code `IssueTrackerConnections`,
//! `IssueTrackerHostLive` and `persistence/IssueTrackerBindings`): the
//! standalone trackers this server ships (Linear), one credential per tracker
//! kind, and which of a tracker's scopes each project reads beside its git
//! host's issues.
//!
//! A credential is vetted with the tracker before it is stored, and lives in
//! the secret store (`crate::secrets::set_tracker_token`), never in settings
//! or events. Bindings are rows of the `issue_tracker_bindings` table, as
//! upstream keeps them: a project is bound to one scope at most, and
//! disconnecting a tracker removes every binding to it.
//!
//! Limits: Linear is the only standalone tracker (upstream ships only Linear
//! too); a stored credential is probed afresh on every `issueTrackers.list`.

use super::linear;
use crate::state::{AppState, ConnCtx};
use domovoi_model::typed::{
    IssueTrackerBindProjectInput, IssueTrackerConnectInput, IssueTrackerConnection, IssueTrackerConnectionList, IssueTrackerProjectBinding,
    StandaloneIssueTrackerDescriptor,
};
use effect_rpc::Router;
use rusqlite::params;
use serde::Serialize;
use serde::de::IgnoredAny;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Why a tracker connection call failed (`IssueTrackerConnectionError`).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "_tag")]
pub enum TrackerFailure {
    #[serde(rename = "IssueTrackerConnectionError")]
    Connection {
        tracker: String,
        /// `unknown-tracker`, `rejected` or `failed`.
        reason: &'static str,
        detail: String,
    },
}

impl std::fmt::Display for TrackerFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let TrackerFailure::Connection { tracker, detail, .. } = self;
        write!(f, "{tracker} connection failed: {detail}")
    }
}

impl std::error::Error for TrackerFailure {}

fn failure(tracker: &str, reason: &'static str, detail: impl Into<String>) -> TrackerFailure {
    TrackerFailure::Connection { tracker: tracker.to_owned(), reason, detail: detail.into() }
}

/// The standalone trackers, in registration order.
const STANDALONE: [&str; 1] = [linear::KIND];

fn descriptor(tracker: &str) -> Option<StandaloneIssueTrackerDescriptor> {
    match tracker {
        linear::KIND => Some(linear::descriptor()),
        _ => None,
    }
}

/// Where a standalone tracker is served from, for a kind this server ships.
pub(super) fn standalone_host(tracker: &str) -> Option<&'static str> {
    match tracker {
        linear::KIND => Some(linear::HOST),
        _ => None,
    }
}

/// `requireTracker`.
fn require_tracker(tracker: &str) -> Result<StandaloneIssueTrackerDescriptor, TrackerFailure> {
    descriptor(tracker).ok_or_else(|| failure(tracker, "unknown-tracker", "This server has no such issue tracker."))
}

/// The kind an error about no tracker in particular is filed under.
const ANY_TRACKER: &str = STANDALONE[0];

// ---------------------------------------------------------------------------
// Bindings (`IssueTrackerBindingsRepository`)
// ---------------------------------------------------------------------------

/// Every project's binding, by project id.
pub(crate) fn bindings(state: &AppState) -> rusqlite::Result<BTreeMap<String, IssueTrackerProjectBinding>> {
    state.store.db().read(|conn| {
        let mut stmt = conn.prepare("SELECT project_id, tracker, scope FROM issue_tracker_bindings ORDER BY project_id")?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, IssueTrackerProjectBinding { tracker: row.get(1)?, scope: row.get(2)? })))?;
        rows.collect()
    })
}

/// Binds a project, or unbinds it with `None`.
fn set_binding(state: &AppState, project_id: &str, binding: Option<&IssueTrackerProjectBinding>) -> rusqlite::Result<()> {
    state.store.db().with(|conn| {
        match binding {
            None => conn.execute("DELETE FROM issue_tracker_bindings WHERE project_id = ?1", params![project_id])?,
            Some(b) => conn.execute(
                "INSERT INTO issue_tracker_bindings (project_id, tracker, scope, updated_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (project_id) DO UPDATE SET tracker = excluded.tracker, scope = excluded.scope, updated_at = excluded.updated_at",
                params![project_id, b.tracker, b.scope.trim(), domovoi_contracts::iso_now()],
            )?,
        };
        Ok(())
    })
}

/// Unbinds every project that pointed at the tracker.
fn remove_tracker(state: &AppState, tracker: &str) -> rusqlite::Result<()> {
    state.store.db().with(|conn| conn.execute("DELETE FROM issue_tracker_bindings WHERE tracker = ?1", params![tracker]).map(|_| ()))
}

// ---------------------------------------------------------------------------
// Connections
// ---------------------------------------------------------------------------

fn disconnected(descriptor: StandaloneIssueTrackerDescriptor) -> IssueTrackerConnection {
    IssueTrackerConnection { descriptor, connected: false, account: None, detail: None, workspace: None, scopes: vec![] }
}

/// The tracker's view of a credential, or why it refused it.
async fn probe(tracker: &str, token: &str) -> Result<(String, Option<String>, Vec<domovoi_model::typed::IssueTrackerScope>), super::TrackerError> {
    match tracker {
        linear::KIND => linear::probe(token).await,
        _ => Err(super::TrackerError::new(linear::KIND, "probeCredential", super::TrackerReason::Failed, "This server has no such issue tracker.")),
    }
}

/// A stored credential's status: a refusal is status, not a failure.
async fn current(state: &AppState, descriptor: StandaloneIssueTrackerDescriptor) -> Result<IssueTrackerConnection, TrackerFailure> {
    let tracker = descriptor.tracker.clone();
    let stored = crate::secrets::tracker_token(&state.paths.userdata, &tracker).map_err(|_| failure(&tracker, "failed", "The stored credential could not be read."))?;
    let Some(token) = stored else { return Ok(disconnected(descriptor)) };
    Ok(match probe(&tracker, &token).await {
        Ok((account, workspace, scopes)) => IssueTrackerConnection { descriptor, connected: true, account: Some(account), detail: None, workspace, scopes },
        Err(e) => IssueTrackerConnection { descriptor, connected: true, account: None, detail: Some(e.detail()), workspace: None, scopes: vec![] },
    })
}

async fn list(state: &AppState) -> Result<IssueTrackerConnectionList, TrackerFailure> {
    let mut trackers = vec![];
    for tracker in STANDALONE {
        trackers.push(current(state, require_tracker(tracker)?).await?);
    }
    // No single tracker is at fault; the first registered one carries the report.
    let bindings = bindings(state).map_err(|_| failure(ANY_TRACKER, "failed", "The project bindings could not be read."))?;
    Ok(IssueTrackerConnectionList { trackers, bindings })
}

async fn connect(state: &AppState, input: IssueTrackerConnectInput) -> Result<IssueTrackerConnection, TrackerFailure> {
    let descriptor = require_tracker(&input.tracker)?;
    let tracker = descriptor.tracker.clone();
    let userdata = &state.paths.userdata;
    let Some(token) = input.token.as_deref().map(str::trim).filter(|t| !t.is_empty()) else {
        crate::secrets::remove_tracker_token(userdata, &tracker).map_err(|_| failure(&tracker, "failed", "The credential could not be removed."))?;
        // Bindings to a tracker nobody is signed into would only produce errors in every listing.
        remove_tracker(state, &tracker).map_err(|_| failure(&tracker, "failed", "The project bindings could not be cleared."))?;
        return Ok(disconnected(descriptor));
    };
    let (account, workspace, scopes) = probe(&tracker, token)
        .await
        .map_err(|e| failure(&tracker, if e.reason == super::TrackerReason::Unauthenticated { "rejected" } else { "failed" }, e.detail()))?;
    crate::secrets::set_tracker_token(userdata, &tracker, token).map_err(|_| failure(&tracker, "failed", "The credential could not be stored."))?;
    Ok(IssueTrackerConnection { descriptor, connected: true, account: Some(account), detail: None, workspace, scopes })
}

fn bind_project(state: &AppState, input: IssueTrackerBindProjectInput) -> Result<(), TrackerFailure> {
    // Unbinding names no tracker; the first registered one carries any report.
    let tracker = match &input.binding {
        None => ANY_TRACKER.to_owned(),
        Some(binding) => require_tracker(&binding.tracker)?.tracker,
    };
    set_binding(state, input.project_id.as_str(), input.binding.as_ref()).map_err(|_| failure(&tracker, "failed", "The project binding could not be saved."))
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.unary("issueTrackers.list", |ctx: Arc<ConnCtx>, _: IgnoredAny| async move { list(&ctx.state).await })
        .unary("issueTrackers.connect", |ctx: Arc<ConnCtx>, input: IssueTrackerConnectInput| async move { connect(&ctx.state, input).await })
        .unary("issueTrackers.bindProject", |ctx: Arc<ConnCtx>, input: IssueTrackerBindProjectInput| async move { bind_project(&ctx.state, input) })
}
