//! The server's images this window shows (project icons, the pictures the
//! icon picker offers, a thread's pictures and diagrams): asked for
//! through the host, kept by URL, and handed to the views as the same bytes
//! each render so the toolkit decodes each once.
//!
//! A project's icon is a signed link that the core renews every few minutes.
//! Its last image stays up until the renewed one has loaded.

use std::collections::{HashMap, HashSet};

use domovoi_core::Event;
use domovoi_core::view::{ProjectIconView, SettingsView, SidebarView};
use windows_reactor::EncodedImage;

use crate::core_link::CoreLink;

#[derive(Default)]
pub struct Media {
    images: HashMap<String, EncodedImage>,
    /// Fetches the host has not answered yet.
    asked: HashSet<String>,
    /// Fetches that failed: not asked again until the link changes.
    failed: HashSet<String>,
    /// The project each icon link is for.
    icon_of: HashMap<String, String>,
    /// Each project's last loaded icon.
    last_icon: HashMap<String, EncodedImage>,
    /// The `(project, load key)` pairs the core was already asked to load.
    icon_loads: HashSet<(String, String)>,
    /// The icon picker's pictures the core was already asked for a link to (`revision:path`).
    thumbnail_loads: HashSet<String>,
}

/// What a view of the sidebar needs from the server.
#[derive(Debug, Default, PartialEq, Eq)]
struct Wanted {
    /// Projects whose icon link the core should make or renew, with the key it asked under.
    loads: Vec<(String, String)>,
    /// Links to fetch; an icon's with its project, another picture's with none.
    icons: Vec<(String, String)>,
    /// The icon picker's pictures with no link yet: what tells them apart, their project and path.
    thumbnails: Vec<(String, String, String)>,
}

fn wanted(sidebar: &SidebarView, settings: &SettingsView, others: &[String]) -> Wanted {
    let mut out = Wanted::default();
    let mut seen = HashSet::new();
    let rows = sidebar.sections.iter().flat_map(|s| &s.rows).map(|r| &r.project_icon);
    for icon in sidebar.projects.iter().map(|p| &p.icon).chain(rows).chain(settings.projects.iter().map(|p| &p.icon)) {
        if let Some(url) = icon.image_url.as_deref().filter(|u| !u.is_empty())
            && seen.insert(url)
        {
            out.icons.push((url.to_owned(), icon.project_id.clone()));
        }
    }
    for project in &sidebar.projects {
        if let Some(key) = &project.icon.load_key {
            out.loads.push((project.id.clone(), key.clone()));
        }
    }
    // The open thread's pictures and diagrams.
    for url in others.iter().filter(|u| !u.is_empty()) {
        if seen.insert(url.as_str()) {
            out.icons.push((url.clone(), String::new()));
        }
    }
    let files = &settings.project_icon_files;
    for file in &files.files {
        if let Some(url) = file.url.as_deref().filter(|u| !u.is_empty())
            && seen.insert(url)
        {
            out.icons.push((url.to_owned(), String::new()));
        }
        if file.refresh && !files.project_id.is_empty() {
            out.thumbnails.push((format!("{}:{}", files.revision, file.path), files.project_id.clone(), file.path.clone()));
        }
    }
    out
}

impl Media {
    /// Asks for what the sidebar, Settings and the open thread (`others`: its
    /// pictures' links) show and this window does not have yet.
    pub fn refresh(&mut self, core: &CoreLink, sidebar: &SidebarView, settings: &SettingsView, others: &[String]) {
        let wanted = wanted(sidebar, settings, others);
        // The core is asked once per key: it hands out a new one when the
        // link runs out or the connection is made again.
        self.icon_loads.retain(|load| wanted.loads.contains(load));
        for load in wanted.loads {
            if self.icon_loads.insert(load.clone()) {
                core.send(Event::LoadProjectIcon { project_id: load.0 });
            }
        }
        self.thumbnail_loads.retain(|key| wanted.thumbnails.iter().any(|(of, _, _)| of == key));
        for (key, project_id, path) in wanted.thumbnails {
            if self.thumbnail_loads.insert(key) {
                core.send(Event::LoadProjectIconThumbnail { project_id, path });
            }
        }
        let shown: HashSet<&str> = wanted.icons.iter().map(|(url, _)| url.as_str()).collect();
        self.images.retain(|url, _| shown.contains(url.as_str()));
        self.failed.retain(|url| shown.contains(url.as_str()));
        self.icon_of.retain(|url, _| shown.contains(url.as_str()));
        for (url, project) in wanted.icons {
            if self.images.contains_key(&url) || self.failed.contains(&url) || !self.asked.insert(url.clone()) {
                continue;
            }
            if !project.is_empty() {
                self.icon_of.insert(url.clone(), project);
            }
            core.fetch_image(url);
        }
    }

    pub fn loaded(&mut self, url: String, bytes: Vec<u8>) {
        self.asked.remove(&url);
        let image = EncodedImage::new(bytes);
        if let Some(project) = self.icon_of.get(&url) {
            self.last_icon.insert(project.clone(), image.clone());
        }
        self.images.insert(url, image);
    }

    pub fn failed(&mut self, url: String) {
        self.asked.remove(&url);
        self.failed.insert(url);
    }

    /// The picture at `url`, once loaded.
    /// The picture at `url` could not be fetched.
    pub fn gave_up(&self, url: &str) -> bool {
        self.failed.contains(url)
    }

    pub fn image(&self, url: &str) -> Option<EncodedImage> {
        self.images.get(url).cloned()
    }

    /// The image to draw for `icon`: its own once loaded, else the project's
    /// last one while a renewed link loads. `None` for an icon that is not a
    /// picture (a monogram, an emoji or a symbol), or has none yet.
    pub fn project_icon(&self, icon: &ProjectIconView) -> Option<EncodedImage> {
        if let Some(image) = icon.image_url.as_deref().and_then(|url| self.images.get(url)) {
            return Some(image.clone());
        }
        let renewing = icon.load_key.is_some() || icon.image_url.as_deref().is_some_and(|url| !self.failed.contains(url));
        if renewing && !icon.project_id.is_empty() { self.last_icon.get(&icon.project_id).cloned() } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domovoi_core::view::{SidebarProjectView, SidebarSectionView, ThreadRowView};

    fn picture(project: &str, url: Option<&str>, key: Option<&str>) -> ProjectIconView {
        ProjectIconView { project_id: project.into(), image_url: url.map(str::to_owned), load_key: key.map(str::to_owned), ..ProjectIconView::default() }
    }

    fn project(id: &str, icon: ProjectIconView) -> SidebarProjectView {
        SidebarProjectView { id: id.into(), icon, ..SidebarProjectView::default() }
    }

    #[test]
    fn each_link_is_wanted_once_and_each_key_names_its_project() {
        let row = ThreadRowView { project_icon: picture("a", Some("http://s/a.png"), None), ..ThreadRowView::default() };
        let sidebar = SidebarView {
            projects: vec![project("a", picture("a", Some("http://s/a.png"), None)), project("b", picture("b", None, Some("k1")))],
            sections: vec![SidebarSectionView { rows: vec![row], ..SidebarSectionView::default() }],
            ..SidebarView::default()
        };
        let got = wanted(&sidebar, &SettingsView::default(), &["http://s/shot.png".to_owned(), "http://s/a.png".to_owned(), String::new()]);
        assert_eq!(got.icons, vec![("http://s/a.png".to_owned(), "a".to_owned()), ("http://s/shot.png".to_owned(), String::new())]);
        assert_eq!(got.loads, vec![("b".to_owned(), "k1".to_owned())]);
    }

    #[test]
    fn the_icon_pickers_pictures_are_fetched_and_the_ones_without_a_link_asked_for() {
        use domovoi_core::view::{ProjectIconFileView, ProjectIconFilesView};
        let file = |path: &str, url: Option<&str>, refresh: bool| ProjectIconFileView { path: path.into(), url: url.map(str::to_owned), refresh, ..ProjectIconFileView::default() };
        let settings = SettingsView {
            project_icon_files: ProjectIconFilesView {
                project_id: "a".into(),
                revision: "r1".into(),
                files: vec![file("logo.png", Some("http://s/logo"), false), file("icon.svg", None, true)],
                ..ProjectIconFilesView::default()
            },
            ..SettingsView::default()
        };
        let got = wanted(&SidebarView::default(), &settings, &[]);
        assert_eq!(got.icons, vec![("http://s/logo".to_owned(), String::new())]);
        assert_eq!(got.thumbnails, vec![("r1:icon.svg".to_owned(), "a".to_owned(), "icon.svg".to_owned())]);
    }

    #[test]
    fn the_last_icon_stays_while_a_renewed_link_loads() {
        let mut media = Media::default();
        media.icon_of.insert("http://s/old.png".into(), "a".into());
        media.loaded("http://s/old.png".into(), vec![1, 2, 3]);
        let old = media.project_icon(&picture("a", Some("http://s/old.png"), None));
        assert!(old.is_some());
        // The link ran out: the core asks for a new one, then names it.
        assert_eq!(media.project_icon(&picture("a", None, Some("k2"))), old);
        assert_eq!(media.project_icon(&picture("a", Some("http://s/new.png"), None)), old);
        // The project's icon became an emoji: no picture any more.
        assert_eq!(media.project_icon(&ProjectIconView { kind: "emoji".into(), text: "🦀".into(), custom: true, ..ProjectIconView::default() }), None);
        // The new link cannot be fetched: the monogram shows, not a stale picture.
        media.failed("http://s/new.png".into());
        assert_eq!(media.project_icon(&picture("a", Some("http://s/new.png"), None)), None);
    }
}
