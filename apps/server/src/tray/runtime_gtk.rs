//! The menu's event loop on Linux: GTK on the main thread (tray-icon needs
//! it there), polling a channel for messages from the server.

use super::app::TrayApp;
use super::{Notify, TrayMessage};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

pub struct Platform {
    messages: mpsc::Receiver<TrayMessage>,
}

impl Platform {
    /// Fails without a display (a server over SSH): the caller then runs
    /// without an icon.
    pub fn new() -> Result<(Self, Notify), String> {
        gtk::init().map_err(|e| format!("GTK could not start ({e}); is there a desktop session?"))?;
        let (sender, messages) = mpsc::channel();
        let sender = Mutex::new(sender);
        let notify: Notify = Arc::new(move |message| {
            if let Ok(sender) = sender.lock() {
                let _ = sender.send(message);
            }
        });
        let clicks = notify.clone();
        tray_icon::menu::MenuEvent::set_event_handler(Some(move |event: tray_icon::menu::MenuEvent| clicks(TrayMessage::Menu(event.id.0))));
        Ok((Platform { messages }, notify))
    }

    pub fn run(self, mut app: TrayApp) {
        app.build();
        let app = Rc::new(RefCell::new(app));
        let messages = self.messages;
        let mut ticked = Instant::now();
        gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
            let mut app = app.borrow_mut();
            while let Ok(message) = messages.try_recv() {
                app.on_message(message);
            }
            if ticked.elapsed() >= Duration::from_secs(1) {
                ticked = Instant::now();
                app.tick();
            }
            if app.should_exit() {
                gtk::main_quit();
                return gtk::glib::ControlFlow::Break;
            }
            gtk::glib::ControlFlow::Continue
        });
        gtk::main();
    }
}
