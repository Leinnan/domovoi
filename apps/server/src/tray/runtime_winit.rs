//! The menu's event loop on macOS and Windows: winit, on the main thread.

use super::app::TrayApp;
use super::{Notify, TrayMessage};
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};

pub struct Platform {
    event_loop: EventLoop<TrayMessage>,
}

impl Platform {
    pub fn new() -> Result<(Self, Notify), String> {
        let mut builder = EventLoop::<TrayMessage>::with_user_event();
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
            // No Dock icon, no app menu, and never steal focus at launch.
            builder.with_activation_policy(ActivationPolicy::Accessory);
            builder.with_activate_ignoring_other_apps(false);
            builder.with_default_menu(false);
        }
        let event_loop = builder.build().map_err(|e| e.to_string())?;
        let proxy = event_loop.create_proxy();
        let notify: Notify = Arc::new(move |message| {
            let _ = proxy.send_event(message);
        });
        // Menu clicks arrive on the menu's own channel; forwarding them
        // through the proxy wakes the loop at once.
        let clicks = notify.clone();
        tray_icon::menu::MenuEvent::set_event_handler(Some(move |event: tray_icon::menu::MenuEvent| clicks(TrayMessage::Menu(event.id.0))));
        Ok((Platform { event_loop }, notify))
    }

    pub fn run(self, app: TrayApp) {
        let mut handler = Handler { app };
        if let Err(e) = self.event_loop.run_app(&mut handler) {
            tracing::warn!("The status bar icon's event loop ended: {e}");
        }
    }
}

struct Handler {
    app: TrayApp,
}

impl ApplicationHandler<TrayMessage> for Handler {
    fn resumed(&mut self, _: &ActiveEventLoop) {}

    fn window_event(&mut self, _: &ActiveEventLoop, _: winit::window::WindowId, _: WindowEvent) {}

    fn new_events(&mut self, _: &ActiveEventLoop, cause: StartCause) {
        // The icon is created once the loop runs, as tray-icon asks.
        if cause == StartCause::Init {
            self.app.build();
        }
    }

    fn user_event(&mut self, _: &ActiveEventLoop, message: TrayMessage) {
        self.app.on_message(message);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.app.tick();
        if self.app.should_exit() {
            event_loop.exit();
            return;
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_secs(1)));
    }
}
