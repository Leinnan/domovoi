//! One shutdown signal for the whole process: Ctrl-C, SIGTERM (launchd,
//! `kill`), a closing console or session on Windows, the status bar icon's
//! Quit and `domovoi-server stop` all cancel the same token, so every path runs
//! the same cleanup (terminals, the iroh endpoint, the saved ticket).

use tokio_util::sync::CancellationToken;

/// Cancels `shutdown` on the first termination signal. A second one exits
/// at once, in case cleanup hangs.
pub fn watch_signals(shutdown: CancellationToken) {
    tokio::spawn(async move {
        tokio::select! {
            () = signal() => {}
            () = shutdown.cancelled() => {}
        }
        if !shutdown.is_cancelled() {
            tracing::info!("Shutting down");
            shutdown.cancel();
        }
        signal().await;
        std::process::exit(130);
    });
}

/// Waits on a signal stream; never resolves when it could not be set up.
#[cfg(unix)]
async fn recv(stream: &mut Option<tokio::signal::unix::Signal>) {
    match stream.as_mut() {
        Some(s) => {
            s.recv().await;
        }
        None => std::future::pending().await,
    }
}

#[cfg(unix)]
async fn signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).ok();
    let mut hup = signal(SignalKind::hangup()).ok();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        () = recv(&mut term) => {}
        () = recv(&mut hup) => {}
    }
}

#[cfg(windows)]
async fn signal() {
    use tokio::signal::windows;
    let mut brk = windows::ctrl_break().ok();
    let mut close = windows::ctrl_close().ok();
    let mut off = windows::ctrl_shutdown().ok();
    let mut logoff = windows::ctrl_logoff().ok();
    macro_rules! recv {
        ($s:expr) => {
            async {
                match $s.as_mut() {
                    Some(s) => {
                        s.recv().await;
                    }
                    None => std::future::pending().await,
                }
            }
        };
    }
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        () = recv!(brk) => {}
        () = recv!(close) => {}
        () = recv!(off) => {}
        () = recv!(logoff) => {}
    }
}

#[cfg(not(any(unix, windows)))]
async fn signal() {
    let _ = tokio::signal::ctrl_c().await;
}
