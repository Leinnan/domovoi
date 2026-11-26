//! What the window asks in a dialog: a folder to add, a name, which sessions
//! to import, a project's icon, a server to pair with, whether to remove or
//! forget something.
//!
//! A window has one dialog. It is always attached and shut while there is
//! nothing to ask, so the window's content keeps the same attachment
//! (as lwa_fm does it). What is typed in a dialog is the window's own, kept
//! in the [`Overlay`]; the core hears of it when the dialog is confirmed.

use std::collections::BTreeSet;
use std::rc::Rc;

use domovoi_core::Event;
use domovoi_core::app::ProviderInstanceEdit;
use domovoi_core::view::{AgentSessionsView, ComposerView, FolderBrowserView, ProjectIconView, ProviderView, SettingsView, SidebarView};
use domovoi_desktop_host::palette;
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, CheckBox, ContentDialog, ContentDialogButton, ContentDialogResult, FontWeight, Grid, GridLength, HorizontalAlignment,
    Image, InfoBar, InfoBarSeverity, Orientation, PasswordBox, ProgressBar, ProgressRing, ScrollBarVisibility, ScrollViewer, SelectorBar, SelectorBarItem, StackPanel, Stretch,
    TextBlock, TextBox, TextTrimming, TextWrapping, Thickness, ToggleSwitch, TooltipExt as _, VerticalAlignment, View,
};

use crate::core_link::CoreLink;
use crate::views::settings::task::{self, TaskDraft, TaskEdit};
use crate::views::sidebar::Icons;
use crate::{icons, theme};

/// What the dialog asks now.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Overlay {
    #[default]
    None,
    /// A folder on the server, typed in.
    AddByPath { path: String },
    /// A new project folder, by name.
    CreateProject { name: String },
    /// The server's folders, walked (`FolderBrowserView`); `path` is one typed to jump to.
    FolderBrowser { path: String },
    /// Folders with other agents' sessions; `picked` are the ones to import.
    /// `seeded`: the recent ones were ticked once the scan came back.
    ImportSessions { picked: BTreeSet<String>, seeded: bool },
    RenameProject { project_id: String, title: String },
    RemoveProject { project_id: String, title: String },
    /// A project's icon: `kind` is `auto`, `monogram`, `emoji`, `symbol` or `image`
    /// (a picture among the project's files, found with `query`).
    IconPicker { project_id: String, kind: String, text: String, symbol: String, color: String, query: String },
    /// Another server to pair with: its link, or its address and a code.
    AddServer { link: String, url: String, code: String },
    RenameServer { id: String, label: String },
    ForgetServer { id: String, label: String },
    DeleteThread { thread_id: String, title: String },
    RenameThread { thread_id: String, title: String },
    /// A plan's text saved into the workspace, at `path` (empty: where the server puts plans).
    SavePlan { item_id: String, path: String },
    /// The agent runs and edits without asking.
    ConfirmFullAccess,
    /// The project's files go back to how they were before a turn.
    ConfirmRevert { checkpoint_id: String, label: String },
    /// A provider account's settings (Antigravity's), changed or new.
    ProviderEditor(Box<ProviderDraft>),
    /// A scheduled task, new or changed.
    TaskEditor(Box<TaskDraft>),
    RemoveProvider { instance_id: String, name: String },
    DeleteTask { id: String, name: String },
    /// A webhook task's address gets a new token.
    RotateToken { id: String, name: String },
}

/// A provider account as its editor holds it (`instance_id` empty: a new one).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderDraft {
    pub instance_id: String,
    pub driver: String,
    pub display_name: String,
    pub enabled: bool,
    pub auth_method: String,
    /// As typed; empty keeps the key the server has.
    pub api_key: String,
    pub gcp_project: String,
    pub gcp_location: String,
    pub binary_path: String,
    /// The sign-in methods to choose from: what the core names them, and what they are called.
    pub methods: Vec<(String, String)>,
    /// The driver's own account: it has no name of its own.
    pub built_in: bool,
    /// The server has a key (it is never shown).
    pub has_key: bool,
}

impl ProviderDraft {
    /// The editor of `provider`'s settings as they are.
    pub fn of(provider: &ProviderView) -> Self {
        let config = &provider.config;
        Self {
            instance_id: provider.instance_id.clone(),
            driver: provider.driver.clone(),
            display_name: config.display_name.clone(),
            enabled: provider.enabled,
            auth_method: config.auth_method.clone(),
            api_key: String::new(),
            gcp_project: config.gcp_project.clone(),
            gcp_location: config.gcp_location.clone(),
            binary_path: config.binary_path.clone(),
            methods: config.auth_methods.iter().map(|m| (m.value.clone(), m.label.clone())).collect(),
            built_in: config.built_in,
            has_key: config.has_api_key,
        }
    }

    /// A new Antigravity account: an API key or Vertex (the Google sign-in is the built-in one's).
    pub fn new_antigravity() -> Self {
        Self {
            driver: "antigravity".into(),
            enabled: true,
            auth_method: "gemini-api-key".into(),
            methods: vec![("gemini-api-key".into(), "Gemini API key".into()), ("agent-platform".into(), "Agent Platform (Vertex AI)".into())],
            ..Self::default()
        }
    }

    fn is_new(&self) -> bool {
        self.instance_id.is_empty()
    }

    fn uses_key(&self) -> bool {
        matches!(self.auth_method.as_str(), "gemini-api-key" | "agent-platform")
    }

    fn uses_project(&self) -> bool {
        matches!(self.auth_method.as_str(), "oauth-business" | "agent-platform")
    }

    /// A new account signing in with a key needs the key.
    fn can_save(&self) -> bool {
        !(self.is_new() && self.auth_method == "gemini-api-key" && self.api_key.trim().is_empty())
    }

    fn saved(&self) -> ProviderInstanceEdit {
        ProviderInstanceEdit {
            instance_id: self.instance_id.clone(),
            driver: self.driver.clone(),
            display_name: self.display_name.trim().to_owned(),
            enabled: self.enabled,
            auth_method: self.auth_method.clone(),
            api_key: self.api_key.trim().to_owned(),
            gcp_project: self.gcp_project.trim().to_owned(),
            gcp_location: self.gcp_location.trim().to_owned(),
            binary_path: self.binary_path.trim().to_owned(),
        }
    }
}

/// What the dialog's own controls tell the window.
#[derive(Clone, Debug)]
pub enum Edit {
    /// The dialog's text box.
    Text(Rc<str>),
    /// A session folder ticked or unticked.
    Toggle(String),
    /// One of a form's other text boxes (a server's address: 1, its code: 2; a provider's key: 1,
    /// project: 2, location: 3, binary: 4).
    Field(u8, Rc<str>),
    /// The icon picker: its kind, a symbol, a colour, or a picture file chosen.
    Kind(String),
    Symbol(String),
    Color(String),
    File(String),
    /// A change in the scheduled task's form.
    Task(TaskEdit),
}

impl Overlay {
    pub fn edit(&mut self, edit: Edit) {
        match (self, edit) {
            (Overlay::AddByPath { path } | Overlay::FolderBrowser { path } | Overlay::SavePlan { path, .. }, Edit::Text(text)) => *path = text.to_string(),
            (Overlay::CreateProject { name }, Edit::Text(text)) => *name = text.to_string(),
            (Overlay::RenameProject { title, .. } | Overlay::RenameThread { title, .. }, Edit::Text(text)) => *title = text.to_string(),
            (Overlay::RenameServer { label, .. }, Edit::Text(text)) => *label = text.to_string(),
            (Overlay::AddServer { link, .. }, Edit::Text(text)) => *link = text.to_string(),
            (Overlay::AddServer { url, .. }, Edit::Field(1, text)) => *url = text.to_string(),
            (Overlay::AddServer { code, .. }, Edit::Field(2, text)) => *code = text.to_string(),
            (Overlay::TaskEditor(draft), Edit::Task(change)) => draft.apply(change),
            (Overlay::ProviderEditor(draft), edit) => match edit {
                Edit::Text(text) => draft.display_name = text.to_string(),
                Edit::Field(1, text) => draft.api_key = text.to_string(),
                Edit::Field(2, text) => draft.gcp_project = text.to_string(),
                Edit::Field(3, text) => draft.gcp_location = text.to_string(),
                Edit::Field(4, text) => draft.binary_path = text.to_string(),
                Edit::Kind(method) => draft.auth_method = method,
                Edit::Toggle(_) => draft.enabled = !draft.enabled,
                _ => {}
            },
            (Overlay::IconPicker { kind, query, .. }, Edit::Text(text)) if kind == "image" => *query = text.to_string(),
            (Overlay::IconPicker { text: letters, .. }, Edit::Text(text)) => *letters = text.to_string(),
            (Overlay::IconPicker { kind, text, .. }, Edit::Kind(to)) => {
                // Letters and an emoji are different texts: one does not carry over as the other.
                if *kind != to && (to == "emoji" || to == "monogram") {
                    text.clear();
                }
                *kind = to;
            }
            (Overlay::IconPicker { kind, symbol, .. }, Edit::Symbol(to)) => {
                *kind = "symbol".into();
                *symbol = to;
            }
            (Overlay::IconPicker { color, .. }, Edit::Color(to)) => *color = to,
            (Overlay::ImportSessions { picked, .. }, Edit::Toggle(path)) => {
                if !picked.remove(&path) {
                    picked.insert(path);
                }
            }
            _ => {}
        }
    }

    /// Ticks the folders worth importing, once, when the scan has come back.
    pub fn seed(&mut self, sessions: &AgentSessionsView) {
        if let Overlay::ImportSessions { picked, seeded } = self
            && !*seeded
            && sessions.scanned
        {
            *seeded = true;
            picked.extend(sessions.candidates.iter().filter(|c| c.recent && !c.already_imported).map(|c| c.path.clone()));
        }
    }

    /// What confirming the dialog tells the core (`None`: nothing, as for a dialog only closed).
    pub fn confirmed(&self, folders: &FolderBrowserView) -> Option<Event> {
        let filled = |text: &str| Some(text.trim().to_owned()).filter(|t| !t.is_empty());
        match self {
            Overlay::AddByPath { path } => filled(path).map(|path| Event::AddProject { path }),
            Overlay::CreateProject { name } => filled(name).map(|name| Event::CreateProject { name }),
            Overlay::FolderBrowser { .. } => (!folders.path.is_empty() && !folders.is_project).then(|| Event::AddProject { path: folders.path.clone() }),
            Overlay::RenameProject { project_id, title } => filled(title).map(|title| Event::RenameProject { project_id: project_id.clone(), title }),
            Overlay::RemoveProject { project_id, .. } => Some(Event::RemoveProject { project_id: project_id.clone() }),
            Overlay::IconPicker { project_id, kind, text, symbol, color, .. } => {
                let kind = match kind.as_str() {
                    "auto" => "",
                    // A picture is set when it is picked, not here.
                    "image" => return None,
                    other => other,
                };
                Some(Event::SetProjectIcon { project_id: project_id.clone(), kind: kind.into(), text: text.trim().into(), symbol: symbol.clone(), color: color.clone() })
            }
            Overlay::AddServer { link, url, code } => match filled(link) {
                Some(link) => Some(Event::ConnectRemote { link }),
                None => filled(url).map(|server_url| Event::Connect { server_url, pairing_code: code.trim().to_owned() }),
            },
            Overlay::RenameServer { id, label } => filled(label).map(|label| Event::RenameEnvironment { id: id.clone(), label }),
            Overlay::ForgetServer { id, .. } => Some(Event::ForgetEnvironment { id: id.clone() }),
            Overlay::DeleteThread { thread_id, .. } => Some(Event::DeleteThread { thread_id: thread_id.clone() }),
            Overlay::SavePlan { item_id, path } => Some(Event::SavePlan { item_id: item_id.clone(), path: path.trim().to_owned() }),
            Overlay::ConfirmRevert { checkpoint_id, .. } => Some(Event::Revert { checkpoint_id: checkpoint_id.clone() }),
            Overlay::ConfirmFullAccess => Some(Event::SetRuntimeMode { mode: "full-access".into() }),
            Overlay::RenameThread { thread_id, title } => filled(title).map(|title| Event::RenameThread { thread_id: thread_id.clone(), title }),
            Overlay::ProviderEditor(draft) => draft.can_save().then(|| Event::SaveProviderInstance { edit: Box::new(draft.saved()) }),
            // A task is more than one event: the window sends what `TaskDraft::saved` gives.
            Overlay::TaskEditor(_) => None,
            Overlay::RemoveProvider { instance_id, .. } => Some(Event::RemoveProviderInstance { instance_id: instance_id.clone() }),
            Overlay::DeleteTask { id, .. } => Some(Event::DeleteScheduled { id: id.clone() }),
            Overlay::RotateToken { id, .. } => Some(Event::RotateWebhookToken { id: id.clone() }),
            Overlay::None | Overlay::ImportSessions { .. } => None,
        }
    }

    /// The icon picker as a project would show it: what its preview draws.
    fn icon(&self, automatic: Option<&ProjectIconView>) -> ProjectIconView {
        let Overlay::IconPicker { kind, text, symbol, color, .. } = self else { return ProjectIconView::default() };
        match kind.as_str() {
            "monogram" | "emoji" | "symbol" => ProjectIconView { kind: kind.clone(), text: text.trim().into(), symbol: symbol.clone(), color: color.clone(), ..ProjectIconView::default() },
            _ => automatic.cloned().unwrap_or_default(),
        }
    }

    /// Whether what the icon picker holds can be saved (the core takes one or two letters, or one emoji).
    fn icon_saves(&self) -> bool {
        let Overlay::IconPicker { kind, text, .. } = self else { return false };
        let text = text.trim();
        match kind.as_str() {
            "auto" | "symbol" => true,
            "monogram" => (1..=2).contains(&text.chars().count()) && text.chars().all(char::is_alphanumeric),
            "emoji" => !text.is_empty() && !text.chars().any(char::is_alphanumeric),
            _ => false,
        }
    }
}

/// What the dialog tells the window.
pub struct Hooks<'a> {
    /// Made once by the window: the dialog was shut, by a button or otherwise.
    pub on_closed: &'a Callback<ContentDialogResult>,
    pub on_edit: Callback<Edit>,
}

/// The view model the dialogs show.
pub struct Shown<'a> {
    pub folders: &'a FolderBrowserView,
    pub sessions: &'a AgentSessionsView,
    pub settings: &'a SettingsView,
    pub sidebar: &'a SidebarView,
    /// The models a scheduled task can use.
    pub composer: &'a ComposerView,
    pub icons: Icons<'a>,
}

pub fn dialog(overlay: &Overlay, core: &CoreLink, shown: &Shown<'_>, hooks: Hooks<'_>) -> ContentDialog {
    let dialog = ContentDialog::new().is_open(*overlay != Overlay::None).on_closed(hooks.on_closed.clone());
    let field = |name: &str, hint: &str, text: &str| -> View {
        let on_edit = hooks.on_edit.clone();
        TextBox::new().automation_name(name).placeholder_text(hint).text(text).on_text_changed(move |text: Rc<str>| on_edit.call(Edit::Text(text))).into()
    };
    let (dialog, content): (ContentDialog, View) = match overlay {
        Overlay::None => (dialog, Border::new().into()),
        Overlay::AddByPath { path } => (
            dialog.title("Add a Server Folder").primary_button_text("Add Project").is_primary_button_enabled(!path.trim().is_empty()).close_button_text("Cancel").default_button(ContentDialogButton::Primary),
            lines(vec![note("The folder's path on the server. It becomes a project."), field("Server folder", r"C:\src\my-project", path)]),
        ),
        Overlay::CreateProject { name } => (
            dialog.title("Create a Project").primary_button_text("Create").is_primary_button_enabled(!name.trim().is_empty()).close_button_text("Cancel").default_button(ContentDialogButton::Primary),
            lines(vec![note("A new folder on the server, where the server keeps new projects."), field("Project name", "my-project", name)]),
        ),
        Overlay::RenameProject { title, .. } => (
            dialog.title("Rename Project").primary_button_text("Rename").is_primary_button_enabled(!title.trim().is_empty()).close_button_text("Cancel").default_button(ContentDialogButton::Primary),
            field("Project name", "Name", title),
        ),
        Overlay::RemoveProject { title, .. } => (
            dialog.title(format!("Remove {title}?")).primary_button_text("Remove").close_button_text("Cancel").default_button(ContentDialogButton::Close),
            note("The project and its threads leave Domovoi. The folder on the server stays as it is."),
        ),
        Overlay::FolderBrowser { path } => (
            dialog
                .title("Browse Server Folders")
                .primary_button_text(if shown.folders.is_project { "Already a Project" } else { "Add This Folder" })
                .is_primary_button_enabled(!shown.folders.path.is_empty() && !shown.folders.is_project)
                .close_button_text("Cancel")
                .default_button(ContentDialogButton::None),
            folder_browser(core, shown.folders, path, field("Go to folder", "A folder's path on the server", path)),
        ),
        Overlay::ImportSessions { picked, .. } => (dialog.title("Import Agent Sessions").close_button_text("Close"), import_sessions(core, shown.sessions, picked, &hooks.on_edit)),
        Overlay::IconPicker { kind, .. } => {
            let dialog = dialog.title("Project Icon").close_button_text(if kind == "image" { "Close" } else { "Cancel" });
            let dialog = if kind == "image" { dialog } else { dialog.primary_button_text("Save Icon").is_primary_button_enabled(overlay.icon_saves()).default_button(ContentDialogButton::Primary) };
            (dialog, icon_picker(overlay, shown, &hooks.on_edit))
        }
        Overlay::AddServer { link, url, code } => {
            let other = |index: u8, name: &str, hint: &str, text: &str| -> View {
                let on_edit = hooks.on_edit.clone();
                TextBox::new().automation_name(name).placeholder_text(hint).text(text).on_text_changed(move |text: Rc<str>| on_edit.call(Edit::Field(index, text))).into()
            };
            (
                dialog
                    .title("Add a Server")
                    .primary_button_text("Connect")
                    .is_primary_button_enabled(link.trim().starts_with("domovoi://") || link.trim().is_empty() && !url.trim().is_empty())
                    .close_button_text("Cancel")
                    .default_button(ContentDialogButton::Primary),
                lines(vec![
                    note("Run `domovoi-server pair` on the machine your agents work on, then paste the link it prints."),
                    field("Pairing link", "domovoi://connect?ticket=…", link),
                    note("Or a server on this network: its address and pairing code."),
                    other(1, "Server address", "http://192.168.1.20:13773", url),
                    other(2, "Pairing code", "Pairing code", code),
                ]),
            )
        }
        Overlay::RenameServer { label, .. } => (
            dialog.title("Rename Server").primary_button_text("Rename").is_primary_button_enabled(!label.trim().is_empty()).close_button_text("Cancel").default_button(ContentDialogButton::Primary),
            field("Server name", "Name", label),
        ),
        Overlay::ForgetServer { label, .. } => (
            dialog.title(format!("Forget {label}?")).primary_button_text("Forget").close_button_text("Cancel").default_button(ContentDialogButton::Close),
            note("This device stops knowing the server, and has to be paired again to reach it. The server and its threads stay as they are."),
        ),
        Overlay::ProviderEditor(draft) => (
            dialog
                .title(if draft.is_new() { "New Provider Account" } else { "Configure Provider" })
                .primary_button_text(if draft.is_new() { "Add" } else { "Save" })
                .is_primary_button_enabled(draft.can_save())
                .close_button_text("Cancel")
                .default_button(ContentDialogButton::Primary),
            provider_editor(draft, &hooks.on_edit),
        ),
        Overlay::TaskEditor(draft) => {
            let on_edit = hooks.on_edit.clone();
            let on_task = Callback::new(move |change: TaskEdit| on_edit.call(Edit::Task(change)));
            (
                dialog
                    .title(if draft.is_new() { "New Scheduled Task" } else { "Edit Scheduled Task" })
                    .primary_button_text(if draft.is_new() { "Create" } else { "Save" })
                    .is_primary_button_enabled(draft.can_save())
                    .close_button_text("Cancel"),
                task::form(draft, shown.settings, shown.composer, &on_task),
            )
        }
        Overlay::RemoveProvider { name, .. } => (
            dialog.title(format!("Remove {name}?")).primary_button_text("Remove").close_button_text("Cancel").default_button(ContentDialogButton::Close),
            note("The account leaves this server, with its saved key. Threads that used it keep their messages."),
        ),
        Overlay::DeleteTask { name, .. } => (
            dialog.title(format!("Delete {name}?")).primary_button_text("Delete").close_button_text("Cancel").default_button(ContentDialogButton::Close),
            note("The task stops running. The threads its runs made stay."),
        ),
        Overlay::RotateToken { name, .. } => (
            dialog.title("Give the webhook a new address?").primary_button_text("Rotate Token").close_button_text("Cancel").default_button(ContentDialogButton::Close),
            note(&format!("“{name}” gets a new address. Whatever calls the old one stops working until it is given the new one.")),
        ),
        Overlay::SavePlan { path, .. } => (
            dialog.title("Save the Plan").primary_button_text("Save").close_button_text("Cancel").default_button(ContentDialogButton::Primary),
            lines(vec![note("A path in the thread's workspace. Left empty, the plan goes to plans/<its title>.md."), field("Path in the workspace", "plans/my-plan.md", path)]),
        ),
        Overlay::ConfirmRevert { label, .. } => (
            dialog.title("Revert the files?").primary_button_text("Revert").close_button_text("Cancel").default_button(ContentDialogButton::Close),
            note(&format!("The project's files go back to how they were before “{label}”. What was changed since, by the agent or by you, is lost.")),
        ),
        Overlay::ConfirmFullAccess => (
            dialog.title("Give the agent full access?").primary_button_text("Allow Full Access").close_button_text("Cancel").default_button(ContentDialogButton::Close),
            note("It will run commands and change files without asking first."),
        ),
        Overlay::RenameThread { title, .. } => (
            dialog.title("Rename Thread").primary_button_text("Rename").is_primary_button_enabled(!title.trim().is_empty()).close_button_text("Cancel").default_button(ContentDialogButton::Primary),
            lines(vec![field("Thread title", "Title", title)]),
        ),
        Overlay::DeleteThread { title, .. } => (
            dialog.title("Delete this thread?").primary_button_text("Delete").close_button_text("Cancel").default_button(ContentDialogButton::Close),
            note(&format!("“{title}” is deleted from the server for good, with its messages.")),
        ),
    };
    dialog.content(content)
}

/// The server's folders: the one listed, the way up, and the folders in it.
fn folder_browser(core: &CoreLink, folders: &FolderBrowserView, typed: &str, field: View) -> View {
    let open = |label: &str, icon: &str, path: &str| -> View {
        let (core, path) = (core.clone(), path.to_owned());
        Button::new()
            .style(ButtonStyle::Subtle)
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .horizontal_content_alignment(HorizontalAlignment::Left)
            .automation_name(label)
            .content(StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children(vec![
                theme::glyph(icon, 14.0).opacity(0.8).into(),
                TextBlock::new().text(label).text_trimming(TextTrimming::CharacterEllipsis).into(),
            ]))
            .on_click(move || core.send(Event::BrowseFolders { path: path.clone() }))
            .into()
    };
    let mut rows: Vec<View> = Vec::new();
    if !folders.parent_path.is_empty() {
        rows.push(open("..", "\u{E74A}", &folders.parent_path));
    }
    rows.extend(folders.entries.iter().map(|entry| open(&entry.name, "\u{E8B7}", &entry.full_path)));
    if rows.is_empty() && !folders.loading && folders.error.is_none() {
        rows.push(note("No folders here."));
    }
    let go = {
        let (core, typed) = (core.clone(), typed.trim().to_owned());
        Button::new().grid_column(1).automation_name("Go").is_enabled(!typed.is_empty()).content(TextBlock::new().text("Go")).on_click(move || core.send(Event::BrowseFolders { path: typed.clone() }))
    };
    let mut body: Vec<View> = vec![
        Grid::new().columns([GridLength::STAR, GridLength::Auto]).column_spacing(8.0).children((field, go)).into(),
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .column_spacing(8.0)
            .children((
                TextBlock::new().text(&folders.path).font_family(theme::MONO).font_size(theme::CAPTION).text_trimming(TextTrimming::CharacterEllipsis).vertical_alignment(VerticalAlignment::Center),
                ProgressRing::new().grid_column(1).is_active(folders.loading).width(16.0).height(16.0),
            ))
            .into(),
    ];
    if let Some(error) = &folders.error {
        body.push(InfoBar::new().is_open(true).is_closable(false).severity(InfoBarSeverity::Error).message(error).into());
    }
    body.push(scroll(rows, 300.0));
    StackPanel::new().orientation(Orientation::Vertical).spacing(8.0).min_width(440.0).children(body).into()
}

/// Folders where Claude Code, Codex or Pi worked: tick the ones to bring in.
fn import_sessions(core: &CoreLink, sessions: &AgentSessionsView, picked: &BTreeSet<String>, on_edit: &Callback<Edit>) -> View {
    let mut body: Vec<View> = vec![note("Folders on the server where Claude Code, Codex or Pi worked. Each becomes a project with its recent sessions as threads.")];
    if let Some(error) = &sessions.error {
        body.push(InfoBar::new().is_open(true).is_closable(false).severity(InfoBarSeverity::Error).message(error).into());
    }
    if sessions.scanning || !sessions.scanned && sessions.error.is_none() {
        body.push(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children(vec![ProgressRing::new().is_active(true).width(16.0).height(16.0).into(), note("Looking through the server…")])
                .into(),
        );
    } else if sessions.importing && sessions.total > 0 {
        let done = sessions.done.min(sessions.total);
        body.push(note(&format!("Importing {done} of {}…", sessions.total)));
        body.push(ProgressBar::new().minimum(0.0).maximum(f64::from(sessions.total)).value(f64::from(done)).into());
    } else if sessions.candidates.is_empty() {
        body.push(note("No sessions of other agents were found on the server."));
    } else {
        let rows: Vec<View> = sessions
            .candidates
            .iter()
            .map(|candidate| {
                let (on_edit, path) = (on_edit.clone(), candidate.path.clone());
                let detail = [candidate.sources.as_str(), candidate.detail.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                let mut name: Vec<View> = vec![TextBlock::new().text(&candidate.title).font_weight(FontWeight::SEMI_BOLD).text_trimming(TextTrimming::CharacterEllipsis).into()];
                if !detail.is_empty() {
                    name.push(TextBlock::new().text(detail).font_size(theme::CAPTION).opacity(0.65).text_trimming(TextTrimming::CharacterEllipsis).into());
                }
                let mut label: Vec<View> = vec![StackPanel::new().orientation(Orientation::Vertical).children(name).into()];
                if candidate.already_imported {
                    label.push(TextBlock::new().text("Imported").font_size(theme::CAPTION).opacity(0.65).vertical_alignment(VerticalAlignment::Center).into());
                }
                CheckBox::new()
                    .automation_name(&candidate.title)
                    .is_checked(picked.contains(&candidate.path))
                    .content(StackPanel::new().orientation(Orientation::Horizontal).spacing(12.0).children(label))
                    .on_click(move || on_edit.call(Edit::Toggle(path.clone())))
                    .into()
            })
            .collect();
        body.push(scroll(rows, 300.0));
        let (core, paths) = (core.clone(), picked.iter().cloned().collect::<Vec<_>>());
        body.push(
            Button::new()
                .style(ButtonStyle::Accent)
                .horizontal_alignment(HorizontalAlignment::Right)
                .is_enabled(!picked.is_empty())
                .content(TextBlock::new().text(format!("Import ({})", picked.len())))
                .on_click(move || core.send(Event::ImportAgentSessions { paths: paths.clone() }))
                .into(),
        );
    }
    StackPanel::new().orientation(Orientation::Vertical).spacing(8.0).min_width(440.0).children(body).into()
}

/// A provider account's settings: how it signs in, and what that needs.
fn provider_editor(draft: &ProviderDraft, on_edit: &Callback<Edit>) -> View {
    let labelled = |label: &str, control: View| -> View {
        Grid::new()
            .columns([GridLength::Pixel(120.0), GridLength::STAR])
            .column_spacing(8.0)
            .children((TextBlock::new().text(label).opacity(0.75).vertical_alignment(VerticalAlignment::Center), Border::new().grid_column(1).content(control)))
            .into()
    };
    let typed = |index: u8, name: &str, hint: &str, value: &str| -> View {
        let on_edit = on_edit.clone();
        TextBox::new()
            .automation_name(name)
            .placeholder_text(hint)
            .text(value)
            .on_text_changed(move |text: Rc<str>| on_edit.call(if index == 0 { Edit::Text(text) } else { Edit::Field(index, text) }))
            .into()
    };
    let mut rows: Vec<View> = Vec::new();
    if !draft.built_in {
        rows.push(labelled("Name", typed(0, "Name", "Antigravity (work)", &draft.display_name)));
    }
    if draft.methods.len() > 1 {
        let chosen = draft.methods.iter().position(|(value, _)| *value == draft.auth_method);
        let (on_edit, values) = (on_edit.clone(), draft.methods.iter().map(|(value, _)| value.clone()).collect::<Vec<_>>());
        rows.push(labelled(
            "Sign-in method",
            windows_reactor::ComboBox::new()
                .automation_name("Sign-in method")
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .items_source(draft.methods.iter().map(|(_, label)| label.clone()))
                .selected_index(chosen)
                .on_selection_changed(move |index: Option<usize>| {
                    if let Some(value) = index.filter(|i| Some(*i) != chosen).and_then(|i| values.get(i)) {
                        on_edit.call(Edit::Kind(value.clone()));
                    }
                })
                .into(),
        ));
    }
    if draft.uses_key() {
        let hint = if draft.has_key {
            "Saved (type to replace)"
        } else if draft.auth_method == "agent-platform" {
            "Optional"
        } else {
            "Required"
        };
        let on_edit = on_edit.clone();
        rows.push(labelled(
            "API key",
            PasswordBox::new().automation_name("API key").placeholder_text(hint).password(&draft.api_key).on_password_changed(move |text: Rc<str>| on_edit.call(Edit::Field(1, text))).into(),
        ));
    }
    if draft.uses_project() {
        rows.push(labelled("GCP project", typed(2, "GCP project", "my-project-id", &draft.gcp_project)));
        rows.push(labelled("GCP location", typed(3, "GCP location", "us-central1", &draft.gcp_location)));
    }
    rows.push(labelled("Binary path", typed(4, "Binary path", "Automatic", &draft.binary_path)));
    let on_edit = on_edit.clone();
    let was = draft.enabled;
    rows.push(labelled(
        "Enabled",
        ToggleSwitch::new()
            .min_width(0.0)
            .is_on(was)
            .on_content(TextBlock::new())
            .off_content(TextBlock::new())
            .automation_name("Enabled")
            .on_toggled(move |on: bool| {
                if on != was {
                    on_edit.call(Edit::Toggle(String::new()));
                }
            })
            .into(),
    ));
    StackPanel::new().orientation(Orientation::Vertical).spacing(10.0).min_width(440.0).children(rows).into()
}

/// The kinds of icon, as the picker names them.
const ICON_KINDS: [(&str, &str); 5] = [("auto", "Automatic"), ("monogram", "Letters"), ("emoji", "Emoji"), ("symbol", "Symbol"), ("image", "Image")];

/// A project's icon: how it looks now, the kind to make it, and that kind's choices.
fn icon_picker(overlay: &Overlay, shown: &Shown<'_>, on_edit: &Callback<Edit>) -> View {
    let Overlay::IconPicker { project_id, kind, text, symbol, color, query } = overlay else { return Border::new().into() };
    let project = shown.sidebar.projects.iter().find(|p| p.id == *project_id);
    let dark = shown.icons.dark;
    let preview = if kind == "image" { project.map(|p| p.icon.clone()).unwrap_or_default() } else { overlay.icon(project.map(|p| &p.automatic_icon)) };
    let kinds = {
        let (on_edit, now) = (on_edit.clone(), kind.clone());
        SelectorBar::new().items(ICON_KINDS.iter().map(|(id, label)| SelectorBarItem::new().text(*label).is_selected(*id == kind).into()).collect::<Vec<View>>()).on_selected_text_changed(
            move |label: Option<Rc<str>>| {
                // Only a change the user made: showing the kind selects it too.
                if let Some((id, _)) = ICON_KINDS.iter().find(|(_, l)| label.as_deref() == Some(*l)).filter(|(id, _)| *id != now) {
                    on_edit.call(Edit::Kind((*id).to_owned()));
                }
            },
        )
    };
    let typed = |name: &str, hint: &str, value: &str| -> View {
        let on_edit = on_edit.clone();
        TextBox::new().automation_name(name).placeholder_text(hint).text(value).on_text_changed(move |text: Rc<str>| on_edit.call(Edit::Text(text))).into()
    };
    let swatches = || -> View {
        let dots: Vec<View> = palette::PROJECT_KEYS
            .iter()
            .enumerate()
            .map(|(index, key)| {
                let (on_edit, to) = (on_edit.clone(), (*key).to_owned());
                let chosen = *key == color;
                Button::new()
                    .grid_column((index % 9) as i32)
                    .grid_row((index / 9) as i32)
                    .style(ButtonStyle::Subtle)
                    .automation_name(*key)
                    .content(
                        Border::new()
                            .width(22.0)
                            .height(22.0)
                            .corner_radius(11.0)
                            .background(theme::rgb(palette::project_rgb(key, dark)))
                            .border_brush(if chosen { theme::primary() } else { theme::stroke() })
                            .border_thickness(Thickness::uniform(if chosen { 3.0 } else { 1.0 })),
                    )
                    .on_click(move || on_edit.call(Edit::Color(to.clone())))
                    .tooltip(*key)
            })
            .collect();
        Grid::new().columns(vec![GridLength::Auto; 9]).rows([GridLength::Auto, GridLength::Auto]).horizontal_alignment(HorizontalAlignment::Left).children(dots).into()
    };
    let mut body: Vec<View> = vec![
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(12.0)
            .children(vec![
                shown.icons.tile(&preview, 48.0),
                TextBlock::new().text(project.map_or("", |p| p.title.as_str())).font_weight(FontWeight::SEMI_BOLD).vertical_alignment(VerticalAlignment::Center).into(),
            ])
            .into(),
        kinds.into(),
    ];
    match kind.as_str() {
        "monogram" => {
            body.push(typed("Letters", "One or two letters or digits", text));
            body.push(swatches());
        }
        "emoji" => {
            body.push(typed("Emoji", "One emoji (Win + . opens the emoji panel)", text));
        }
        "symbol" => {
            let marks: Vec<View> = icons::SYMBOLS
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    let (on_edit, to) = (on_edit.clone(), (*name).to_owned());
                    let chosen = *name == symbol;
                    let button = Button::new()
                        .grid_column((index % 8) as i32)
                        .grid_row((index / 8) as i32)
                        .style(ButtonStyle::Subtle)
                        .automation_name(*name)
                        .content(Image::new().source_svg(icons::symbol(name, palette::project_rgb(color, dark))).stretch(Stretch::Uniform).width(20.0).height(20.0))
                        .on_click(move || on_edit.call(Edit::Symbol(to.clone())));
                    if chosen { button.background(theme::selected()) } else { button }.tooltip(*name)
                })
                .collect();
            body.push(Grid::new().columns(vec![GridLength::Auto; 8]).rows(vec![GridLength::Auto; 3]).horizontal_alignment(HorizontalAlignment::Left).children(marks).into());
            body.push(swatches());
        }
        "image" => {
            let files = &shown.settings.project_icon_files;
            body.push(typed("Search pictures", "Search the project's pictures (png, svg, ico…)", query));
            if let Some(error) = &files.error {
                body.push(InfoBar::new().is_open(true).is_closable(false).severity(InfoBarSeverity::Error).message(error).into());
            }
            let mut rows: Vec<View> = files
                .files
                .iter()
                .map(|file| {
                    let (on_edit, path) = (on_edit.clone(), file.path.clone());
                    let thumbnail: View = match file.url.as_deref().and_then(|url| shown.icons.media.image(url)) {
                        Some(picture) => Image::new().source_data(picture).stretch(Stretch::Uniform).width(28.0).height(28.0).into(),
                        None => Border::new().width(28.0).height(28.0).content(theme::glyph("\u{EB9F}", 16.0).opacity(0.5).horizontal_alignment(HorizontalAlignment::Center)).into(),
                    };
                    Button::new()
                        .style(ButtonStyle::Subtle)
                        .horizontal_alignment(HorizontalAlignment::Stretch)
                        .horizontal_content_alignment(HorizontalAlignment::Left)
                        .automation_name(&file.path)
                        .is_enabled(!file.unavailable)
                        .content(StackPanel::new().orientation(Orientation::Horizontal).spacing(10.0).children(vec![
                            thumbnail,
                            TextBlock::new().text(&file.path).vertical_alignment(VerticalAlignment::Center).text_trimming(TextTrimming::CharacterEllipsis).into(),
                        ]))
                        .on_click(move || on_edit.call(Edit::File(path.clone())))
                        .into()
                })
                .collect();
            if files.loading {
                rows.insert(0, StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children(vec![ProgressRing::new().is_active(true).width(16.0).height(16.0).into(), note("Looking through the project…")]).into());
            } else if rows.is_empty() && files.error.is_none() {
                rows.push(note("No pictures found in the project."));
            }
            if files.truncated {
                rows.push(note("More pictures match: search to narrow them."));
            }
            body.push(scroll(rows, 240.0));
        }
        _ => body.push(note("The project's own picture when it has one (a favicon or an app icon), else its initials in a colour of its own.")),
    }
    StackPanel::new().orientation(Orientation::Vertical).spacing(12.0).min_width(440.0).children(body).into()
}

fn lines(lines: Vec<View>) -> View {
    StackPanel::new().orientation(Orientation::Vertical).spacing(8.0).min_width(400.0).children(lines).into()
}

fn note(text: &str) -> View {
    TextBlock::new().text(text).text_wrapping(TextWrapping::WrapWholeWords).opacity(0.75).into()
}

fn scroll(rows: Vec<View>, height: f64) -> View {
    ScrollViewer::new()
        .height(height)
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
        .content(StackPanel::new().orientation(Orientation::Vertical).spacing(2.0).children(rows))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use domovoi_core::view::AgentSessionCandidateView;

    fn candidate(path: &str, recent: bool, already_imported: bool) -> AgentSessionCandidateView {
        AgentSessionCandidateView { path: path.into(), recent, already_imported, ..AgentSessionCandidateView::default() }
    }

    #[test]
    fn the_recent_folders_are_ticked_once_when_the_scan_comes_back() {
        let mut overlay = Overlay::ImportSessions { picked: BTreeSet::new(), seeded: false };
        let mut sessions = AgentSessionsView { candidates: vec![candidate("a", true, false), candidate("b", false, false), candidate("c", true, true)], ..AgentSessionsView::default() };
        overlay.seed(&sessions);
        assert_eq!(overlay, Overlay::ImportSessions { picked: BTreeSet::new(), seeded: false }, "nothing before the scan is back");
        sessions.scanned = true;
        overlay.seed(&sessions);
        assert_eq!(overlay, Overlay::ImportSessions { picked: BTreeSet::from(["a".to_owned()]), seeded: true });
        // What the user unticks stays unticked.
        overlay.edit(Edit::Toggle("a".into()));
        overlay.edit(Edit::Toggle("b".into()));
        overlay.seed(&sessions);
        assert_eq!(overlay, Overlay::ImportSessions { picked: BTreeSet::from(["b".to_owned()]), seeded: true });
    }

    #[test]
    fn a_dialog_tells_the_core_only_what_is_filled_in() {
        let folders = FolderBrowserView::default();
        assert!(Overlay::AddByPath { path: "  ".into() }.confirmed(&folders).is_none());
        let mut overlay = Overlay::AddByPath { path: String::new() };
        overlay.edit(Edit::Text(r" C:\src\app ".into()));
        assert!(matches!(overlay.confirmed(&folders), Some(Event::AddProject { path }) if path == r"C:\src\app"));
        let rename = Overlay::RenameProject { project_id: "p1".into(), title: "New name".into() };
        assert!(matches!(rename.confirmed(&folders), Some(Event::RenameProject { project_id, title }) if project_id == "p1" && title == "New name"));
    }

    fn picker(kind: &str, text: &str) -> Overlay {
        Overlay::IconPicker { project_id: "p1".into(), kind: kind.into(), text: text.into(), symbol: "rocket".into(), color: "green".into(), query: String::new() }
    }

    #[test]
    fn an_icon_saves_as_the_core_takes_it() {
        let folders = FolderBrowserView::default();
        assert!(matches!(picker("auto", "ab").confirmed(&folders), Some(Event::SetProjectIcon { kind, color, .. }) if kind.is_empty() && color == "green"));
        assert!(matches!(picker("monogram", " Ab ").confirmed(&folders), Some(Event::SetProjectIcon { kind, text, .. }) if kind == "monogram" && text == "Ab"));
        assert!(picker("image", "").confirmed(&folders).is_none(), "a picture is set when it is picked");
        assert!(picker("monogram", "A1").icon_saves() && !picker("monogram", "abc").icon_saves() && !picker("monogram", "").icon_saves());
        assert!(picker("emoji", "🦀").icon_saves() && !picker("emoji", "a").icon_saves());
        assert!(picker("symbol", "").icon_saves() && !picker("image", "").icon_saves());
    }

    #[test]
    fn the_picker_keeps_letters_and_emoji_apart_and_a_symbol_picks_its_kind() {
        let mut overlay = picker("monogram", "AB");
        overlay.edit(Edit::Kind("emoji".into()));
        assert_eq!(overlay, picker("emoji", ""));
        overlay.edit(Edit::Text("🦀".into()));
        overlay.edit(Edit::Symbol("bot".into()));
        assert!(matches!(&overlay, Overlay::IconPicker { kind, symbol, .. } if kind == "symbol" && symbol == "bot"));
        overlay.edit(Edit::Kind("image".into()));
        overlay.edit(Edit::Text("logo".into()));
        assert!(matches!(&overlay, Overlay::IconPicker { query, text, .. } if query == "logo" && text == "🦀"));
    }

    #[test]
    fn a_new_provider_account_needs_its_key_and_saves_what_was_typed() {
        let folders = FolderBrowserView::default();
        let mut overlay = Overlay::ProviderEditor(Box::new(ProviderDraft::new_antigravity()));
        assert!(overlay.confirmed(&folders).is_none(), "a key sign-in with no key");
        overlay.edit(Edit::Text(" Work ".into()));
        overlay.edit(Edit::Field(1, " key-123 ".into()));
        overlay.edit(Edit::Toggle(String::new()));
        let Some(Event::SaveProviderInstance { edit }) = overlay.confirmed(&folders) else { panic!("it saves") };
        assert_eq!((edit.driver.as_str(), edit.display_name.as_str(), edit.api_key.as_str(), edit.enabled), ("antigravity", "Work", "key-123", false));
        // Vertex needs no key, and asks for the project.
        overlay.edit(Edit::Kind("agent-platform".into()));
        overlay.edit(Edit::Field(1, "".into()));
        overlay.edit(Edit::Field(2, "my-project".into()));
        assert!(matches!(overlay.confirmed(&folders), Some(Event::SaveProviderInstance { edit }) if edit.gcp_project == "my-project" && edit.api_key.is_empty()));
    }

    #[test]
    fn a_server_is_added_by_its_link_or_by_its_address_and_code() {
        let folders = FolderBrowserView::default();
        let by_link = Overlay::AddServer { link: " domovoi://connect?ticket=x ".into(), url: "http://ignored".into(), code: String::new() };
        assert!(matches!(by_link.confirmed(&folders), Some(Event::ConnectRemote { link }) if link == "domovoi://connect?ticket=x"));
        let by_code = Overlay::AddServer { link: String::new(), url: "http://10.0.0.2:13773".into(), code: " ABCD ".into() };
        assert!(matches!(by_code.confirmed(&folders), Some(Event::Connect { server_url, pairing_code }) if server_url == "http://10.0.0.2:13773" && pairing_code == "ABCD"));
        assert!(Overlay::AddServer { link: String::new(), url: String::new(), code: "ABCD".into() }.confirmed(&folders).is_none());
    }

    #[test]
    fn files_are_reverted_only_from_the_dialog() {
        let asked = Overlay::ConfirmRevert { checkpoint_id: "c7".into(), label: "Turn 7".into() };
        assert!(matches!(asked.confirmed(&FolderBrowserView::default()), Some(Event::Revert { checkpoint_id }) if checkpoint_id == "c7"));
    }

    #[test]
    fn the_folder_shown_is_added_unless_it_is_a_project_already() {
        let browsing = Overlay::FolderBrowser { path: String::new() };
        let mut folders = FolderBrowserView { path: r"C:\src\app".into(), ..FolderBrowserView::default() };
        assert!(matches!(browsing.confirmed(&folders), Some(Event::AddProject { path }) if path == r"C:\src\app"));
        folders.is_project = true;
        assert!(browsing.confirmed(&folders).is_none());
    }
}
