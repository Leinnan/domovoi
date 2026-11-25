//! The core and its effects, from `domovoi-desktop-host`, as an Iced
//! subscription.

use std::sync::Arc;

use domovoi_desktop_host::{HostConfig, HostEvent, Identity, SecureStore};
use iced::Subscription;
use iced::futures::SinkExt;

pub use domovoi_desktop_host::{HostHandle, device_label, device_os, local_timezone};

/// This app's data folder, keychain service and Windows app id.
pub const IDENTITY: Identity = Identity::ICED;

#[derive(Clone)]
pub enum HostMsg {
    Ready(HostHandle),
    Delta(Box<domovoi_core::delta::ViewDelta>),
    ImageLoaded { url: String, handle: iced::widget::image::Handle },
    /// The image couldn't be fetched; asking for it again tries again.
    ImageFailed { url: String },
    #[cfg_attr(target_os = "macos", allow(dead_code, reason = "clicks are only reported by freedesktop and Windows notifications"))]
    OpenThreadFromNotification { environment_id: String, thread_id: String },
    /// Allow or Decline on an approval's notification.
    #[cfg_attr(target_os = "macos", allow(dead_code, reason = "buttons are only reported by freedesktop and Windows notifications"))]
    RespondFromNotification { environment_id: String, thread_id: String, request_id: String, decision: String },
}

impl std::fmt::Debug for HostMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostMsg::Ready(_) => f.write_str("Ready"),
            HostMsg::Delta(_) => f.write_str("Delta(..)"),
            HostMsg::ImageLoaded { url, .. } => write!(f, "ImageLoaded({url})"),
            HostMsg::ImageFailed { url } => write!(f, "ImageFailed({url})"),
            HostMsg::OpenThreadFromNotification { thread_id, .. } => write!(f, "OpenThreadFromNotification({thread_id})"),
            HostMsg::RespondFromNotification { request_id, decision, .. } => write!(f, "RespondFromNotification({request_id}, {decision})"),
        }
    }
}

impl From<HostEvent> for HostMsg {
    fn from(event: HostEvent) -> Self {
        match event {
            HostEvent::Delta(delta) => HostMsg::Delta(delta),
            HostEvent::ImageLoaded { url, bytes } => HostMsg::ImageLoaded { url, handle: iced::widget::image::Handle::from_bytes(bytes) },
            HostEvent::ImageFailed { url } => HostMsg::ImageFailed { url },
            HostEvent::OpenThreadFromNotification { environment_id, thread_id } => HostMsg::OpenThreadFromNotification { environment_id, thread_id },
            HostEvent::RespondFromNotification { environment_id, thread_id, request_id, decision } => {
                HostMsg::RespondFromNotification { environment_id, thread_id, request_id, decision }
            }
        }
    }
}

fn worker() -> impl iced::futures::Stream<Item = HostMsg> {
    iced::stream::channel(128, async |mut out| {
        let store = Arc::new(SecureStore::from_env(IDENTITY));
        let (handle, mut events) = domovoi_desktop_host::start(store, HostConfig::new(IDENTITY));
        if out.send(HostMsg::Ready(handle)).await.is_err() {
            return;
        }
        while let Some(event) = events.recv().await {
            if out.send(event.into()).await.is_err() {
                break;
            }
        }
    })
}

pub fn subscription() -> Subscription<HostMsg> {
    Subscription::run(worker)
}

#[cfg(test)]
pub fn start_test_host(store: Arc<SecureStore>) -> (HostHandle, tokio::sync::mpsc::Receiver<HostMsg>) {
    let (handle, mut events) = domovoi_desktop_host::start(store, HostConfig::for_tests(IDENTITY));
    let (tx, rx) = tokio::sync::mpsc::channel(128);
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if tx.send(HostMsg::from(event)).await.is_err() {
                break;
            }
        }
    });
    (handle, rx)
}
