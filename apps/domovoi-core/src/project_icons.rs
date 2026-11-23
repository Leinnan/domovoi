//! Project icon mutations and short-lived image URLs for native clients.

use crate::{
    Model,
    model::Connection,
    requests,
    rpc::Route,
    update::{Cmd, call, uuid},
    view::{ProjectIconFileView, ProjectIconFilesView, ProjectIconView},
};
use crux_core::{Command, render::render};
use domovoi_model::typed::{
    AssetCreateUrlResult, Project, ProjectEntriesResult, ProjectIcon, ProjectShell,
};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    icon: Option<Option<ProjectIcon>>,
    path: Option<Option<String>>,
}

impl Choice {
    fn from_project(project: &ProjectShell) -> Self {
        Self {
            icon: project.project_icon.clone(),
            path: project.favicon_path.clone(),
        }
    }

    fn apply(&self, project: &mut ProjectShell) {
        project.project_icon = self.icon.clone();
        project.favicon_path = self.path.clone();
    }
}

struct Save {
    before: Choice,
    choice: Choice,
    preview: Option<String>,
    confirmed: bool,
}

#[derive(Default)]
pub struct State {
    assets: HashMap<String, Asset>,
    saves: HashMap<String, Save>,
    picker: Picker,
}

#[derive(Default)]
struct Picker {
    project_id: String,
    request: String,
    paths: Vec<String>,
    assets: HashMap<String, Asset>,
    loading: bool,
    error: Option<String>,
    truncated: bool,
}

struct Asset {
    key: String,
    request: String,
    expires_at: Option<i64>,
    url: Option<String>,
}

impl Asset {
    fn pending(key: String, request: String) -> Self {
        Self {
            key,
            request,
            expires_at: None,
            url: None,
        }
    }

    fn finish(&mut self, value: Option<AssetCreateUrlResult>, now_ms: i64) {
        self.expires_at = Some(
            value
                .as_ref()
                .and_then(|asset| asset.expires_at.as_i64())
                .unwrap_or(now_ms + 60_000),
        );
        self.url = value
            .filter(|asset| {
                asset.source_path.is_some() && asset.relative_url.starts_with("/api/assets/")
            })
            .map(|asset| asset.relative_url);
    }
}

fn asset_key(project: &ProjectShell) -> String {
    format!(
        "{}\0{}",
        project.workspace_root,
        project
            .favicon_path
            .as_ref()
            .and_then(Option::as_deref)
            .unwrap_or_default()
    )
}

fn project<'a>(model: &'a Model, id: &str) -> Option<&'a ProjectShell> {
    model.projects.iter().find(|p| p.id.as_str() == id)
}

pub fn view(model: &Model, project: &ProjectShell, automatic: bool) -> ProjectIconView {
    let id = project.id.as_str();
    let mut icon = crate::sidebar::project_icon(project);
    icon.saving = model.project_icons.saves.contains_key(id);
    if automatic && icon.custom {
        let (text, color) = crate::present::project_identity(&project.title);
        return ProjectIconView {
            kind: "monogram".into(),
            text,
            color: color.into(),
            ..Default::default()
        };
    }
    if icon.custom {
        return icon;
    }
    let path = project.favicon_path.as_ref().and_then(Option::as_deref);
    if !automatic && let Some(path) = path {
        icon.file_path = Some(path.into());
        icon.custom = true;
    }
    if automatic && path.is_some() {
        return icon;
    }
    icon.project_id = id.into();
    let key = asset_key(project);
    let asset = model
        .project_icons
        .assets
        .get(id)
        .filter(|asset| asset.key == key);
    let pending = model.project_icons.saves.get(id);
    icon.image_url = pending
        .and_then(|save| save.preview.as_ref())
        .or_else(|| asset.and_then(|asset| asset.url.as_ref()))
        .map(|url| format!("{}{url}", model.server_url));
    if pending.is_none()
        && model.connection == Connection::Connected
        && asset.is_none_or(|asset| asset.expires_at.is_some_and(|at| at <= model.shown_ms))
    {
        icon.load_key = Some(format!(
            "{}\0{}\0{key}",
            model.attempt,
            model.active_env.as_deref().unwrap_or_default()
        ));
    }
    icon
}

pub fn load(model: &mut Model, id: String) -> Cmd {
    let Some(project) = project(model, &id) else {
        return Command::done();
    };
    let key = asset_key(project);
    if model.connection != Connection::Connected
        || model.project_icons.saves.contains_key(&id)
        || crate::sidebar::project_icon(project).custom
        || model.project_icons.assets.get(&id).is_some_and(|asset| {
            asset.key == key && asset.expires_at.is_none_or(|at| at > model.now_ms)
        })
    {
        return Command::done();
    }
    let request = requests::project_favicon(&project.workspace_root);
    let revision = uuid();
    model
        .project_icons
        .assets
        .insert(id.clone(), Asset::pending(key, revision.clone()));
    call(
        model,
        request,
        Route::ProjectIconAsset {
            project_id: id,
            request: revision,
        },
    )
}

pub fn search(model: &mut Model, project_id: String, query: String) -> Cmd {
    let Some(project) = project(model, &project_id) else {
        return Command::done();
    };
    let call_request = requests::project_icon_files(&project.workspace_root, &query);
    let request = uuid();
    let connected = model.connection == Connection::Connected;
    let assets = if model.project_icons.picker.project_id == project_id {
        std::mem::take(&mut model.project_icons.picker.assets)
    } else {
        HashMap::new()
    };
    model.project_icons.picker = Picker {
        assets,
        project_id,
        request: request.clone(),
        loading: connected,
        error: (!connected).then(|| "Connect to the server to browse project files.".into()),
        ..Default::default()
    };
    call(model, call_request, Route::ProjectIconFiles(request)).and(render())
}

pub fn close(model: &mut Model, id: &str) -> Cmd {
    if model.project_icons.picker.project_id == id {
        model.project_icons.picker = Picker::default();
        return render();
    }
    Command::done()
}

pub fn thumbnail(model: &mut Model, id: String, path: String) -> Cmd {
    let picker = &model.project_icons.picker;
    if model.connection != Connection::Connected
        || picker.project_id != id
        || !picker.paths.contains(&path)
        || picker
            .assets
            .get(&path)
            .is_some_and(|asset| asset.expires_at.is_none_or(|at| at > model.now_ms))
    {
        return Command::done();
    }
    let Some(project) = project(model, &id) else {
        return Command::done();
    };
    let request = requests::project_icon_thumbnail(&project.workspace_root, &path);
    let revision = uuid();
    model.project_icons.picker.assets.insert(
        path.clone(),
        Asset::pending(String::new(), revision.clone()),
    );
    call(
        model,
        request,
        Route::ProjectIconThumbnail {
            path,
            request: revision,
        },
    )
}

pub fn files_view(model: &Model) -> ProjectIconFilesView {
    let picker = &model.project_icons.picker;
    ProjectIconFilesView {
        project_id: picker.project_id.clone(),
        revision: picker.request.clone(),
        loading: picker.loading,
        error: picker.error.clone(),
        truncated: picker.truncated,
        files: picker
            .paths
            .iter()
            .map(|path| {
                let asset = picker.assets.get(path);
                ProjectIconFileView {
                    path: path.clone(),
                    url: asset
                        .and_then(|a| a.url.as_ref())
                        .map(|url| format!("{}{url}", model.server_url)),
                    unavailable: asset.is_some_and(|a| a.expires_at.is_some() && a.url.is_none()),
                    refresh: asset
                        .is_none_or(|a| a.expires_at.is_some_and(|at| at <= model.shown_ms)),
                }
            })
            .collect(),
    }
}

pub fn image_reply(model: &mut Model, route: &Route, result: Option<AssetCreateUrlResult>) {
    let (asset, request) = match route {
        Route::ProjectIconAsset {
            project_id,
            request,
        } => (model.project_icons.assets.get_mut(project_id), request),
        Route::ProjectIconThumbnail { path, request } => {
            (model.project_icons.picker.assets.get_mut(path), request)
        }
        _ => return,
    };
    if let Some(asset) = asset.filter(|asset| &asset.request == request) {
        asset.finish(result, model.now_ms);
    }
}

pub fn files_reply(model: &mut Model, request: &str, result: Result<ProjectEntriesResult, String>) {
    let picker = &mut model.project_icons.picker;
    if picker.request == request {
        picker.loading = false;
        match result {
            Ok(result) => {
                picker.paths = result.entries.into_iter().map(|entry| entry.path).collect();
                picker.truncated = result.truncated;
            }
            Err(error) => picker.error = Some(error),
        }
    }
}

pub fn save(model: &mut Model, id: String, icon: Option<ProjectIcon>, path: Option<String>) -> Cmd {
    if model.connection != Connection::Connected {
        model.toast("error", "Change project icon: not connected to the server.");
        return render();
    }
    if model.project_icons.saves.contains_key(&id) {
        return Command::done();
    }
    let preview = if let Some(path) = &path {
        let picker = &model.project_icons.picker;
        let Some(url) = (picker.project_id == id)
            .then(|| picker.assets.get(path))
            .flatten()
            .and_then(|asset| asset.url.clone())
        else {
            return Command::done();
        };
        Some(url)
    } else {
        None
    };
    let Some(project) = model.projects.iter_mut().find(|p| p.id.as_str() == id) else {
        return Command::done();
    };
    let before = Choice::from_project(project);
    let choice = Choice {
        icon: Some(icon.clone()),
        path: Some(path.clone()),
    };
    choice.apply(project);
    model.project_icons.assets.remove(&id);
    model.project_icons.saves.insert(
        id.clone(),
        Save {
            before,
            choice,
            preview,
            confirmed: false,
        },
    );
    let payload = requests::ProjectChange::SetIcon {
        project: &id.as_str().into(),
        icon,
        path,
    };
    call(
        model,
        requests::projects_mutate(&uuid(), payload),
        Route::ProjectIconChanged(id),
    )
    .and(render())
}

pub fn shell_project(state: &mut State, project: &mut ProjectShell) {
    let id = project.id.as_str();
    if let Some(save) = state.saves.get_mut(id) {
        save.before = Choice::from_project(project);
        save.confirmed |= save.before.icon.as_ref().and_then(Option::as_ref)
            == save.choice.icon.as_ref().and_then(Option::as_ref)
            && save.before.path.as_ref().and_then(Option::as_ref)
                == save.choice.path.as_ref().and_then(Option::as_ref);
        if !save.confirmed {
            save.choice.apply(project);
        }
    }
}

pub fn finish_save(model: &mut Model, id: &str, result: Result<Option<Project>, ()>) {
    if let Some(save) = model.project_icons.saves.remove(id)
        && !save.confirmed
        && let Some(project) = model.projects.iter_mut().find(|p| p.id.as_str() == id)
        && Choice::from_project(project) == save.choice
    {
        match result {
            Err(()) => save.before.apply(project),
            Ok(Some(saved)) if saved.updated_at.as_str() >= project.updated_at.as_str() => {
                Choice {
                    icon: saved.project_icon,
                    path: saved.favicon_path,
                }
                .apply(project);
            }
            Ok(Some(_)) => save.before.apply(project),
            Ok(None) => {}
        }
    }
}

pub fn prune(model: &mut Model) {
    let exists = |id: &str| model.projects.iter().any(|p| p.id.as_str() == id);
    model.project_icons.assets.retain(|id, _| exists(id));
    model.project_icons.saves.retain(|id, _| exists(id));
    if !exists(&model.project_icons.picker.project_id) {
        model.project_icons.picker = Picker::default();
    }
}

pub fn disconnected(model: &mut Model) {
    let ids: Vec<_> = model.project_icons.saves.keys().cloned().collect();
    for id in ids {
        finish_save(model, &id, Err(()));
    }
    model
        .project_icons
        .assets
        .retain(|_, asset| asset.expires_at.is_some());
    let picker = &mut model.project_icons.picker;
    if !picker.project_id.is_empty() {
        picker.loading = false;
        picker.error = Some("The connection dropped. Search again when it reconnects.".into());
        picker.assets.retain(|_, asset| asset.expires_at.is_some());
    }
}
