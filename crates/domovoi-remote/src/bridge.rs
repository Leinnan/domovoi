//! The client side: loopback TCP connections piped into iroh bi-streams.

use crate::frames::{FrameReader, FrameWriter};
use crate::{ALPN, EndpointTicket, Error, Network, PathKind, RPC_ALPN, SecretKey, bind, current_path};
use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr};
use serde::Serialize;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

/// How long one dial may take before the bridge reports a failure.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// After a failed dial, new connections fail at once for this long (doubling
/// up to the cap) instead of each waiting out its own dial.
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(15);
/// Loopback connections forwarded at once; more are closed.
const MAX_PIPES: usize = 128;
/// Accept errors in a row after which the listener is given up on (iOS takes
/// a suspended app's listening socket away).
const ACCEPT_FAILURES: u32 = 10;

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeStatus {
    pub connected: bool,
    pub path: Option<PathKind>,
    pub rtt_ms: Option<u64>,
    /// The last dial error, cleared by the next successful dial.
    pub error: Option<String>,
    /// The loopback port still accepts connections. `false` means the bridge
    /// is dead and must be started again.
    pub listening: bool,
}

/// A device endpoint that can dial several paired servers at once.
pub struct ClientEndpoint(Endpoint);

impl ClientEndpoint {
    pub async fn bind(secret: SecretKey, network: Network) -> Result<Self, Error> {
        bind(secret, &network, Vec::new()).await.map(Self)
    }
}

pub struct Bridge {
    shared: Arc<Shared>,
    port: u16,
    cancel: CancellationToken,
    owns_endpoint: bool,
}

struct Shared {
    endpoint: Endpoint,
    remote: EndpointAddr,
    conn: Mutex<Option<Connection>>,
    /// The connection RPC sockets are streams of ([`RPC_ALPN`]), once one was asked for.
    rpc_conn: Mutex<Option<Connection>>,
    /// Serializes dials so concurrent TCP accepts share one connection.
    dialing: tokio::sync::Mutex<()>,
    error: Mutex<Option<String>>,
    /// When dialing may be tried again, and the wait after the next failure.
    backoff: Mutex<Option<(std::time::Instant, Duration)>>,
    pipes: Arc<tokio::sync::Semaphore>,
    listening: std::sync::atomic::AtomicBool,
}

impl Bridge {
    /// Binds `127.0.0.1:port` (0 picks a free port), dials the server once so
    /// a bad ticket fails here, then forwards every accepted connection.
    pub async fn start(secret: SecretKey, network: Network, ticket: EndpointTicket, port: u16) -> Result<Self, Error> {
        let endpoint = bind(secret, &network, Vec::new()).await?;
        Self::listen(endpoint, ticket, port, true).await
    }

    pub async fn start_on(endpoint: &ClientEndpoint, ticket: EndpointTicket, port: u16) -> Result<Self, Error> {
        Self::listen(endpoint.0.clone(), ticket, port, false).await
    }

    async fn listen(endpoint: Endpoint, ticket: EndpointTicket, port: u16, owns_endpoint: bool) -> Result<Self, Error> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await.map_err(|e| Error::msg("binding the loopback port", e))?;
        let port = listener.local_addr().map_err(|e| Error::msg("reading the loopback port", e))?.port();
        let shared = Arc::new(Shared {
            endpoint,
            remote: ticket.endpoint_addr().clone(),
            conn: Mutex::new(None),
            rpc_conn: Mutex::new(None),
            dialing: tokio::sync::Mutex::new(()),
            error: Mutex::new(None),
            backoff: Mutex::new(None),
            pipes: Arc::new(tokio::sync::Semaphore::new(MAX_PIPES)),
            listening: std::sync::atomic::AtomicBool::new(true),
        });
        if let Err(e) = shared.connection(true).await {
            if owns_endpoint { close(&shared.endpoint).await; }
            return Err(e);
        }
        let cancel = CancellationToken::new();
        tokio::spawn(accept_loop(listener, shared.clone(), cancel.clone()));
        Ok(Self { shared, port, cancel, owns_endpoint })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn status(&self) -> BridgeStatus {
        let conn = self.shared.live();
        let path = conn.as_ref().and_then(current_path);
        BridgeStatus {
            connected: conn.is_some(),
            path: path.map(|(kind, _)| kind),
            rtt_ms: path.map(|(_, rtt)| u64::try_from(rtt.as_millis()).unwrap_or(u64::MAX)),
            error: lock(&self.shared.error).clone(),
            listening: self.shared.listening.load(std::sync::atomic::Ordering::Relaxed),
        }
    }

    /// Dials again now if the connection is gone (e.g. after the app was
    /// suspended); otherwise a no-op.
    pub async fn ensure_connected(&self) -> Result<(), Error> {
        self.shared.connection(true).await.map(drop)
    }

    /// An RPC socket to the server: a stream on a connection of its own
    /// kind ([`RPC_ALPN`]), with no HTTP and no loopback port in between.
    /// The connection is made by the first socket and kept for the next
    /// ones. Fails when the server does not serve RPC this way (an older
    /// one); the socket is then opened through the loopback port as before.
    pub async fn rpc(&self) -> Result<(FrameReader, FrameWriter), Error> {
        let kept = lock(&self.shared.rpc_conn).clone().filter(|conn| conn.close_reason().is_none());
        if let Some(conn) = kept
            && let Ok((send, recv)) = conn.open_bi().await
        {
            return Ok(crate::frames::halves(&conn, send, recv));
        }
        let conn = match tokio::time::timeout(CONNECT_TIMEOUT, self.shared.endpoint.connect(self.shared.remote.clone(), RPC_ALPN)).await {
            Ok(Ok(conn)) => conn,
            Ok(Err(e)) => return Err(Error::msg("connecting to the server for RPC", e)),
            Err(_) => return Err(Error::msg("connecting to the server for RPC", "timed out")),
        };
        let (send, recv) = conn.open_bi().await.map_err(|e| Error::msg("opening the RPC stream", e))?;
        *lock(&self.shared.rpc_conn) = Some(conn.clone());
        Ok(crate::frames::halves(&conn, send, recv))
    }

    /// Tells iroh the network changed (Wi-Fi to cellular), so it re-probes
    /// paths at once instead of waiting for timeouts.
    pub async fn network_change(&self) {
        self.shared.endpoint.network_change().await;
    }

    pub async fn stop(self) {
        self.cancel.cancel();
        if self.owns_endpoint { close(&self.shared.endpoint).await; }
    }
}

/// Dropping a bridge without `stop` still frees its port and its tasks.
impl Drop for Bridge {
    fn drop(&mut self) {
        if self.cancel.is_cancelled() {
            return;
        }
        self.cancel.cancel();
        if self.owns_endpoint && let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let endpoint = self.shared.endpoint.clone();
            runtime.spawn(async move { close(&endpoint).await });
        }
    }
}

/// How long closing the endpoint may wait for the server to acknowledge.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);

/// Closes the endpoint, giving up after `CLOSE_TIMEOUT`: on sockets iOS took
/// away nothing is acknowledged, and a start that replaces this bridge waits
/// for the close.
async fn close(endpoint: &Endpoint) {
    if tokio::time::timeout(CLOSE_TIMEOUT, endpoint.close()).await.is_err() {
        tracing::debug!("bridge: closing the endpoint timed out");
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Shared {
    /// The live connection, dialing when there is none. Right after a failed
    /// dial this fails at once (unless `force`d), so a queue of loopback
    /// connections does not wait out one dial each while the server is away.
    async fn connection(&self, force: bool) -> Result<Connection, Error> {
        if let Some(conn) = self.live() {
            return Ok(conn);
        }
        let _dialing = self.dialing.lock().await;
        if let Some(conn) = self.live() {
            return Ok(conn);
        }
        if !force
            && let Some((until, _)) = *lock(&self.backoff)
            && std::time::Instant::now() < until
        {
            let last = lock(&self.error).clone().unwrap_or_else(|| "connecting to the server: failed".into());
            return Err(Error(last));
        }
        let dialed = match tokio::time::timeout(CONNECT_TIMEOUT, self.endpoint.connect(self.remote.clone(), ALPN)).await {
            Ok(Ok(conn)) => Ok(conn),
            Ok(Err(e)) => Err(Error::msg("connecting to the server", e)),
            Err(_) => Err(Error::msg("connecting to the server", "timed out")),
        };
        *lock(&self.error) = dialed.as_ref().err().map(ToString::to_string);
        {
            let mut backoff = lock(&self.backoff);
            *backoff = match (&dialed, *backoff) {
                (Ok(_), _) => None,
                (Err(_), Some((_, wait))) => Some((std::time::Instant::now() + wait, (wait * 2).min(BACKOFF_CAP))),
                (Err(_), None) => Some((std::time::Instant::now() + BACKOFF_START, BACKOFF_START * 2)),
            };
        }
        let conn = dialed?;
        *lock(&self.conn) = Some(conn.clone());
        Ok(conn)
    }

    fn live(&self) -> Option<Connection> {
        lock(&self.conn).clone().filter(|c| c.close_reason().is_none())
    }
}

async fn accept_loop(listener: TcpListener, shared: Arc<Shared>, cancel: CancellationToken) {
    let mut failures = 0u32;
    loop {
        let tcp = tokio::select! {
            () = cancel.cancelled() => return,
            accepted = listener.accept() => match accepted {
                Ok((tcp, _)) => {
                    failures = 0;
                    tcp
                }
                Err(e) => {
                    failures += 1;
                    if failures >= ACCEPT_FAILURES {
                        // The socket is gone for good: say so instead of spinning.
                        tracing::warn!("bridge: the loopback listener stopped: {e}");
                        *lock(&shared.error) = Some(format!("the loopback listener stopped: {e}"));
                        shared.listening.store(false, std::sync::atomic::Ordering::Relaxed);
                        return;
                    }
                    tracing::debug!("bridge: accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
        };
        // Over the limit the connection is closed at once.
        let Ok(permit) = shared.pipes.clone().try_acquire_owned() else {
            tracing::debug!("bridge: too many connections");
            continue;
        };
        let shared = shared.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let _permit = permit;
            tokio::select! {
                () = cancel.cancelled() => {}
                result = pipe(&shared, tcp) => if let Err(e) = result {
                    tracing::debug!("bridge: {e}");
                },
            }
        });
    }
}

/// Copies one TCP connection to and from a fresh bi-stream until both sides end.
async fn pipe(shared: &Shared, mut tcp: TcpStream) -> Result<(), Error> {
    let _ = tcp.set_nodelay(true);
    let (send, recv) = match shared.connection(false).await?.open_bi().await {
        Ok(streams) => streams,
        // The connection died between checks: dial once more.
        Err(_) => {
            lock(&shared.conn).take();
            shared.connection(false).await?.open_bi().await.map_err(|e| Error::msg("opening a stream", e))?
        }
    };
    let mut quic = tokio::io::join(recv, send);
    tokio::io::copy_bidirectional(&mut tcp, &mut quic).await.map_err(|e| Error::msg("forwarding", e))?;
    Ok(())
}
