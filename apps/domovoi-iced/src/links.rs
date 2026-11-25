//! `domovoi://` links from the OS, as an Iced subscription. The single-instance
//! hand-off between launches is in `domovoi_desktop_host::links`; the macOS
//! Apple Event that carries a link is caught here.

use iced::Subscription;

use crate::host::IDENTITY;

pub use domovoi_desktop_host::links::from_args;

/// Run before the event loop: hands `link` to an instance already running
/// (true: this one has nothing left to do), else starts listening for later
/// launches when this is the first.
pub fn start(link: Option<&str>) -> bool {
    #[cfg(target_os = "macos")]
    mac::install();
    #[cfg(windows)]
    domovoi_desktop_host::windows::register_scheme(domovoi_desktop_host::windows::SchemeOwner::Fallback);
    domovoi_desktop_host::links::start(IDENTITY, link)
}

/// Links as they arrive.
pub fn subscription() -> Subscription<String> {
    Subscription::run(incoming)
}

fn incoming() -> impl iced::futures::Stream<Item = String> {
    iced::stream::channel(16, async |mut out| {
        use iced::futures::SinkExt;
        let Some(mut rx) = domovoi_desktop_host::links::take_receiver() else {
            return;
        };
        while let Some(link) = rx.recv().await {
            if out.send(link).await.is_err() {
                break;
            }
        }
    })
}

/// Launch Services delivers a link as a `GetURL` Apple Event, to the running
/// app or to the one it starts; AppKit leaves that event to the app (winit
/// sets no application delegate that would claim it).
#[cfg(target_os = "macos")]
mod mac {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{ClassType, class, define_class, msg_send, sel};
    use objc2_foundation::NSString;

    /// `'GURL'`, the event class and id of a URL to open.
    const GET_URL: u32 = u32::from_be_bytes(*b"GURL");
    /// `'----'`: the event's direct object (the URL).
    const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "DomovoiLinkHandler"]
        struct LinkHandler;

        impl LinkHandler {
            #[unsafe(method(handleGetURLEvent:withReplyEvent:))]
            fn handle_get_url(&self, event: &AnyObject, _reply: &AnyObject) {
                // SAFETY: `event` is an `NSAppleEventDescriptor`; both messages are part of it.
                let direct: Option<Retained<AnyObject>> = unsafe { msg_send![event, paramDescriptorForKeyword: DIRECT_OBJECT] };
                let url: Option<Retained<NSString>> = direct.and_then(|d| unsafe { msg_send![&*d, stringValue] });
                if let Some(url) = url {
                    domovoi_desktop_host::links::deliver(&url.to_string());
                }
            }
        }
    );

    pub fn install() {
        // SAFETY: plain `NSObject` subclass without ivars.
        let handler: Retained<LinkHandler> = unsafe { msg_send![LinkHandler::class(), new] };
        let manager: Retained<AnyObject> = unsafe { msg_send![class!(NSAppleEventManager), sharedAppleEventManager] };
        // SAFETY: the selector is the handler's, with the signature Apple Events expect.
        let () = unsafe {
            msg_send![&*manager, setEventHandler: &*handler, andSelector: sel!(handleGetURLEvent:withReplyEvent:), forEventClass: GET_URL, andEventID: GET_URL]
        };
        // The manager doesn't retain its handlers; this one lives as long as the app.
        std::mem::forget(handler);
    }
}
