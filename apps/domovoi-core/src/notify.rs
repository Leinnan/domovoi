//! System notifications (a thread needs approval, finished or failed while
//! the user looks elsewhere). The core decides; the shell only posts them.

use crux_core::capability::Operation;
use facet::Facet;
use serde::{Deserialize, Serialize};

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct NotifyOperation {
    pub title: String,
    pub body: String,
    /// Selecting the notification sends `SelectThread` for this thread.
    pub thread_id: String,
    /// The server the thread is on (for `RespondFromNotification`).
    pub environment_id: String,
    /// An approval the notification can answer (Allow / Decline) without
    /// opening the app; `None` for questions and other notices.
    pub request_id: Option<String>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum NotifyOutput {
    Posted,
}

impl Operation for NotifyOperation {
    type Output = NotifyOutput;
}
