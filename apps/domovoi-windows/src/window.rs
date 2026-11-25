//! The window: the title bar, then the pairing form, or the thread list on
//! the left and the open thread with its composer on the right.
//!
//! It owns the core link and the store. The host's reports wake the UI
//! thread (`ui_thread`), which sends `Message::CoreWoke`; `absorb` takes all
//! of them, applies the deltas and acts on the rest.

use std::rc::Rc;
use std::time::Duration;

use domovoi_core::Event;
use domovoi_desktop_host::HostEvent;
use windows_reactor::{
    Border, Component, ComponentContext, Grid, GridLength, InfoBar, InfoBarSeverity, TextBlock, Thickness, TitleBar, View, ViewContext, WindowBackdrop,
    WindowConstraints, WindowTheme, WindowVisuals,
};

use crate::core_link::CoreLink;
use crate::launch::Launch;
use crate::store::Store;
use crate::views::{composer, pairing, sidebar, thread};
use crate::{theme, ui_thread};

const SIZE: (f64, f64) = (1280.0, 820.0);
const MIN_SIZE: (f64, f64) = (720.0, 480.0);

pub struct MainWindow {
    core: Option<CoreLink>,
    /// Why the core could not start (shown instead of anything else).
    failed: Option<String>,
    store: Store,
    pairing: pairing::Draft,
    /// The composer's text as typed here, and the core's revision it started from.
    draft: String,
    draft_revision: u32,
    /// A server folder typed in to add as a project.
    project_path: String,
    /// The reconnect countdown already scheduled.
    scheduled_retry: u64,
    /// The thread the host was last told is on screen.
    focused_thread: Option<String>,
    smoke: bool,
}

pub enum Message {
    CoreWoke,
    PairLink(Rc<str>),
    PairUrl(Rc<str>),
    PairCode(Rc<str>),
    ConnectLink,
    ConnectCode,
    Draft(Rc<str>),
    ProjectPath(Rc<str>),
    AddProject,
    Send,
    Stop,
    DismissToast,
}

impl MainWindow {
    fn send(&self, event: Event) {
        if let Some(core) = &self.core {
            core.send(event);
        }
    }

    /// Takes what the host reported since the last wake-up.
    fn absorb(&mut self) {
        let Some(core) = self.core.clone() else { return };
        for event in core.take() {
            match event {
                HostEvent::Delta(delta) => {
                    let changed = self.store.apply(*delta);
                    if changed.composer && self.store.composer.draft_revision != self.draft_revision {
                        self.draft_revision = self.store.composer.draft_revision;
                        self.draft = self.store.composer.draft.clone();
                    }
                }
                HostEvent::OpenThreadFromNotification { environment_id, thread_id } => core.send(Event::OpenNotification { environment_id, thread_id }),
                HostEvent::RespondFromNotification { environment_id, thread_id, request_id, decision } => {
                    core.send(Event::RespondFromNotification { environment_id, thread_id, request_id, decision });
                }
                // Images are not drawn yet; nothing asks for them.
                HostEvent::ImageLoaded { .. } | HostEvent::ImageFailed { .. } => {}
            }
        }
        let connection = Rc::clone(&self.store.connection);
        // The core counts down; the shell keeps the time.
        if connection.state == "reconnecting" && connection.reconnect_in_seconds > 0 && !connection.offline && self.scheduled_retry != connection.retry_gen {
            self.scheduled_retry = connection.retry_gen;
            core.send_after(Duration::from_secs(u64::from(connection.reconnect_in_seconds)), Event::RetryTimer { retry_gen: connection.retry_gen });
        }
        let shown = self.store.thread.as_ref().map(|t| t.id.clone());
        if shown != self.focused_thread {
            self.focused_thread.clone_from(&shown);
            core.set_focus(true, shown);
        }
        if self.smoke {
            match connection.state.as_str() {
                "connected" => {
                    println!("smoke: connected to {} ({})", connection.environment_label, connection.server_version);
                    std::process::exit(0);
                }
                "failed" => {
                    eprintln!("smoke: the connection failed: {}", connection.error.as_deref().unwrap_or("no reason given"));
                    std::process::exit(1);
                }
                _ => {}
            }
        }
    }
}

impl Component for MainWindow {
    type Input = Launch;
    type Message = Message;

    fn create(launch: &Launch, context: &ComponentContext<Self>) -> Self {
        let sender = context.sender();
        if !ui_thread::install(move || {
            let _ = sender.send(Message::CoreWoke);
        }) {
            tracing::error!("the UI thread's wake window could not be made: the window will not update");
        }
        let (core, failed) = match CoreLink::start() {
            Ok(core) => (Some(core), None),
            Err(e) => (None, Some(format!("Domovoi could not start its runtime: {e}"))),
        };
        if let Some(core) = &core {
            core.send(Event::Tick { now_ms: crate::core_link::now_ms() });
            let (iana, utc_offset_secs) = domovoi_desktop_host::local_timezone();
            core.send(Event::SetTimeZone { iana, utc_offset_secs });
            core.send(Event::Launch { surface: "desktop".into(), device_label: domovoi_desktop_host::device_label(), device_os: domovoi_desktop_host::device_os().into() });
            if let Some(link) = &launch.link {
                core.send(Event::ConnectRemote { link: link.clone() });
            } else if let Some(server) = &launch.server {
                core.send(Event::Connect { server_url: server.clone(), pairing_code: launch.pairing.clone().unwrap_or_default() });
            }
        }
        MainWindow {
            core,
            failed,
            store: Store::default(),
            pairing: pairing::Draft::default(),
            draft: String::new(),
            draft_revision: 0,
            project_path: String::new(),
            scheduled_retry: 0,
            focused_thread: None,
            smoke: launch.smoke,
        }
    }

    fn update(&mut self, message: Message, _context: &ComponentContext<Self>) {
        match message {
            Message::CoreWoke => self.absorb(),
            Message::PairLink(text) => self.pairing.link = text.to_string(),
            Message::PairUrl(text) => self.pairing.url = text.to_string(),
            Message::PairCode(text) => self.pairing.code = text.to_string(),
            Message::ConnectLink => self.send(Event::ConnectRemote { link: self.pairing.link.trim().to_owned() }),
            Message::ConnectCode => self.send(Event::Connect { server_url: self.pairing.url.trim().to_owned(), pairing_code: self.pairing.code.trim().to_owned() }),
            Message::Draft(text) => {
                if *text != *self.draft {
                    self.draft = text.to_string();
                    self.send(Event::SetDraft { text: self.draft.clone() });
                }
            }
            Message::ProjectPath(text) => self.project_path = text.to_string(),
            Message::AddProject => {
                self.send(Event::AddProject { path: self.project_path.trim().to_owned() });
                self.project_path.clear();
            }
            Message::Send => self.send(Event::Send),
            Message::Stop => self.send(Event::Interrupt),
            Message::DismissToast => self.send(Event::DismissToast),
        }
    }

    fn view(&self, _launch: &Launch, context: &mut ViewContext<'_, Self>) -> View {
        let title = self.store.thread.as_ref().map_or_else(|| "Domovoi".to_owned(), |t| format!("{} - Domovoi", t.title));
        context.window_title(title);
        context.window_visuals(
            WindowVisuals::new()
                .backdrop(WindowBackdrop::Mica)
                .theme(WindowTheme::System)
                .client_size(SIZE.0, SIZE.1)
                .constraints(WindowConstraints { min_width: Some(MIN_SIZE.0), min_height: Some(MIN_SIZE.1), ..WindowConstraints::default() }),
        );
        let subtitle = match self.store.connection.state.as_str() {
            "connected" => self.store.connection.environment_label.clone(),
            "" => String::new(),
            other => other.to_owned(),
        };
        let title_bar = TitleBar::new().title("Domovoi").subtitle(subtitle);
        let body: View = match (&self.core, &self.failed) {
            (_, Some(failed)) => TextBlock::new().text(failed).margin(Thickness::uniform(24.0)).into(),
            (None, None) => Border::new().into(),
            (Some(_), None) if self.store.needs_pairing() => pairing::pairing(
                &self.store.connection,
                &self.pairing,
                pairing::Hooks {
                    on_link: context.callback(Message::PairLink),
                    on_url: context.callback(Message::PairUrl),
                    on_code: context.callback(Message::PairCode),
                    on_pair_link: context.callback(|()| Message::ConnectLink),
                    on_pair_code: context.callback(|()| Message::ConnectCode),
                },
            ),
            (Some(core), None) => {
                let main = match &self.store.thread {
                    Some(page) => thread::thread(core, page),
                    None if &*self.store.route == "new-thread" => thread::new_thread(
                        core,
                        &self.store.sidebar,
                        &self.project_path,
                        thread::ProjectHooks { on_path: context.callback(Message::ProjectPath), on_add: context.callback(|()| Message::AddProject) },
                    ),
                    None => thread::empty(),
                };
                let mut right: Vec<View> = Vec::new();
                if let Some(toast) = &self.store.toast {
                    let severity = match toast.kind.as_str() {
                        "error" => InfoBarSeverity::Error,
                        "success" => InfoBarSeverity::Success,
                        _ => InfoBarSeverity::Informational,
                    };
                    right.push(
                        InfoBar::new()
                            .is_open(true)
                            .is_closable(true)
                            .severity(severity)
                            .message(&toast.message)
                            .margin(Thickness::new(theme::GAP * 2.0, theme::GAP, theme::GAP * 2.0, 0.0))
                            .on_closed(context.callback(|()| Message::DismissToast))
                            .into(),
                    );
                }
                let toasts = right.len() as i32;
                right.push(Border::new().grid_row(toasts).content(main).into());
                right.push(
                    Border::new()
                        .grid_row(toasts + 1)
                        .content(composer::composer(
                            &self.store.composer,
                            &self.draft,
                            composer::Hooks { on_draft: context.callback(Message::Draft), on_send: context.callback(|()| Message::Send), on_stop: context.callback(|()| Message::Stop) },
                        ))
                        .into(),
                );
                let rows: Vec<GridLength> = (0..toasts).map(|_| GridLength::Auto).chain([GridLength::STAR, GridLength::Auto]).collect();
                Grid::new()
                    .columns([GridLength::Pixel(theme::SIDEBAR), GridLength::STAR])
                    .children((
                        Border::new().content(sidebar::sidebar(core, &self.store.connection, &self.store.sidebar)),
                        Border::new()
                            .grid_column(1)
                            .margin(Thickness::new(0.0, 0.0, theme::GAP, theme::GAP))
                            .background(theme::card())
                            .border_brush(theme::stroke())
                            .border_thickness(Thickness::uniform(1.0))
                            .corner_radius(theme::RADIUS)
                            .content(Grid::new().rows(rows).children(right)),
                    ))
                    .into()
            }
        };
        Grid::new().rows([GridLength::Auto, GridLength::STAR]).children((title_bar, Border::new().grid_row(1).content(body))).into()
    }
}
