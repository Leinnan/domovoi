use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use std::sync::Arc;
use domovoi_model::typed::{ModelSelection, ProjectIcon, ProjectScript, RepositoryIdentity, ThreadShell};

pub const SHELL_SCHEMA_VERSION: u32 = 2;

/// `OrchestrationProjectShell`, and the project's stored row.
///
/// A row an older server wrote may hold an identity, a model selection, an
/// icon or a script in a shape the records no longer read: that field is then
/// read as not set (that script left out), so the project is still listed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectShell {
    pub id: String,
    pub title: String,
    pub has_custom_title: bool,
    pub workspace_root: String,
    #[serde(default, deserialize_with = "fitting")]
    pub repository_identity: Option<RepositoryIdentity>,
    #[serde(default, deserialize_with = "fitting")]
    pub default_model_selection: Option<ModelSelection>,
    pub default_thread_env_mode: Option<String>,
    pub auto_pull: bool,
    pub favicon_path: Option<String>,
    #[serde(default, deserialize_with = "fitting")]
    pub project_icon: Option<ProjectIcon>,
    #[serde(default, deserialize_with = "each_fitting")]
    pub scripts: Vec<ProjectScript>,
    pub pinned_at: Option<String>,
    pub archived_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A stored field as its record, or none when what is stored does not fit it.
#[expect(clippy::disallowed_types, reason = "a stored field is held as JSON only long enough to try it as its record")]
fn fitting<'de, D: Deserializer<'de>, T: DeserializeOwned>(stored: D) -> Result<Option<T>, D::Error> {
    Ok(T::deserialize(serde_json::Value::deserialize(stored)?).ok())
}

/// The stored entries that fit their record.
#[expect(clippy::disallowed_types, reason = "a stored entry is held as JSON only long enough to try it as its record")]
fn each_fitting<'de, D: Deserializer<'de>, T: DeserializeOwned>(stored: D) -> Result<Vec<T>, D::Error> {
    Ok(Vec::<serde_json::Value>::deserialize(stored)?.into_iter().filter_map(|entry| T::deserialize(entry).ok()).collect())
}

/// The `projects.mutate` success value (`OrchestrationProject`): the shell
/// fields minus `hasCustomTitle`, plus `deletedAt`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRecord {
    pub id: String,
    pub title: String,
    pub workspace_root: String,
    pub repository_identity: Option<RepositoryIdentity>,
    pub favicon_path: Option<String>,
    pub project_icon: Option<ProjectIcon>,
    pub default_model_selection: Option<ModelSelection>,
    pub default_thread_env_mode: Option<String>,
    pub auto_pull: bool,
    pub scripts: Vec<ProjectScript>,
    pub pinned_at: Option<String>,
    pub archived_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub deleted_at: Option<String>,
}

impl From<&ProjectShell> for ProjectRecord {
    fn from(p: &ProjectShell) -> Self {
        ProjectRecord {
            id: p.id.clone(),
            title: p.title.clone(),
            workspace_root: p.workspace_root.clone(),
            repository_identity: p.repository_identity.clone(),
            favicon_path: p.favicon_path.clone(),
            project_icon: p.project_icon.clone(),
            default_model_selection: p.default_model_selection.clone(),
            default_thread_env_mode: p.default_thread_env_mode.clone(),
            auto_pull: p.auto_pull,
            scripts: p.scripts.clone(),
            pinned_at: p.pinned_at.clone(),
            archived_at: p.archived_at.clone(),
            created_at: p.created_at.clone(),
            updated_at: p.updated_at.clone(),
            deleted_at: None,
        }
    }
}

/// `OrchestrationV2ShellSnapshot`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellSnapshot {
    pub schema_version: u32,
    pub snapshot_sequence: u64,
    /// Shared with whoever derived them: a snapshot copies no shell.
    pub threads: Vec<Arc<ThreadShell>>,
    pub archived_threads: Vec<Arc<ThreadShell>>,
    pub projects: Vec<ProjectShell>,
}

/// `OrchestrationV2ShellStreamItem`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
pub enum ShellStreamItem {
    #[serde(rename = "synchronized")]
    Synchronized,
    #[serde(rename = "snapshot-required")]
    SnapshotRequired,
    #[serde(rename = "snapshot")]
    Snapshot {
        /// Boxed: the other items are small and many.
        snapshot: Box<ShellSnapshot>,
        #[serde(rename = "resolvedRepositoryIdentityRoots", skip_serializing_if = "Option::is_none")]
        resolved_repository_identity_roots: Option<Vec<String>>,
    },
    #[serde(rename = "project.updated")]
    ProjectUpdated { sequence: u64, project: Box<ProjectShell> },
    #[serde(rename = "project.removed")]
    ProjectRemoved {
        sequence: u64,
        #[serde(rename = "projectId")]
        project_id: String,
    },
    #[serde(rename = "thread.updated")]
    ThreadUpdated { sequence: u64, location: String, thread: Arc<ThreadShell> },
    #[serde(rename = "thread.removed")]
    ThreadRemoved {
        sequence: u64,
        location: String,
        #[serde(rename = "threadId")]
        thread_id: String,
    },
}
