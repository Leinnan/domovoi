//! Finding and signing in to the server on this machine (`domovoi_local`),
//! exported to the native shells. The Mac app runs the core's `Local` effect
//! with these; the phone apps have no server beside them and do not call
//! them. Both read and write small files and return at once.

/// The text of the descriptor a server on this machine wrote, for the core's
/// `LocalOutput::Descriptor`; empty when there is none this user may trust
/// (or `DOMOVOI_LOCAL_SERVER=off`).
#[boltffi::export]
#[must_use]
pub fn local_server_descriptor() -> String {
    domovoi_local::locate().and_then(|path| domovoi_local::read_descriptor(&path)).unwrap_or_default()
}

/// Writes a sign-in request beside the descriptor and returns `<id> <secret>`
/// for the core's `LocalOutput::Request`; empty when there is no descriptor
/// or the request could not be written (`LocalOutput::Unavailable`).
#[boltffi::export]
#[must_use]
pub fn local_server_request() -> String {
    let made = domovoi_local::locate().and_then(|path| domovoi_local::requests_beside(&path)).and_then(|dir| domovoi_local::create_request(&dir).ok());
    made.map(|request| format!("{} {}", request.id, request.secret)).unwrap_or_default()
}
