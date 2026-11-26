//! What a desktop client learns from the machine on each tick: that it slept,
//! and whether (and through which network) it can reach the internet. The
//! client turns these into the core's events.

use std::net::IpAddr;

/// Longer than this between two ticks and the machine slept: sockets died
/// with it, and the core should drop and reopen its own.
pub const SLEEP_GAP_MS: i64 = 60_000;

/// The local address a packet to the internet would leave from, without
/// sending one: a UDP `connect` only looks up the route. `None` when there is
/// no route (no network).
pub async fn probe_route() -> Option<IpAddr> {
    async fn from(bind: &str, to: &str) -> Option<IpAddr> {
        let socket = tokio::net::UdpSocket::bind(bind).await.ok()?;
        socket.connect(to).await.ok()?;
        socket.local_addr().ok().map(|address| address.ip())
    }
    match from("0.0.0.0:0", "1.1.1.1:53").await {
        Some(ip) => Some(ip),
        None => from("[::]:0", "[2606:4700:4700::1111]:53").await,
    }
}

/// Whether `url` names this machine.
pub fn is_loopback_url(url: &str) -> bool {
    let host = url.split_once("://").map_or(url, |(_, rest)| rest).split(['/', '?']).next().unwrap_or_default();
    let host = host.strip_prefix('[').and_then(|h| h.split_once(']')).map_or_else(|| host.rsplit_once(':').map_or(host, |(h, _)| h), |(h, _)| h);
    host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// What a probe showed that the core should hear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkSignal {
    /// There is a way to the server now, or none.
    Status { online: bool },
    /// Another network than before (Wi-Fi to Ethernet, a VPN).
    Changed,
}

/// Follows the route from probe to probe.
#[derive(Debug, Default)]
pub struct NetworkWatch {
    online: Option<bool>,
    route: Option<IpAddr>,
}

impl NetworkWatch {
    /// Takes a probe's answer. `server_is_local`: the server is on this
    /// machine and not reached over a tunnel, so it is there with no network at all.
    pub fn probed(&mut self, route: Option<IpAddr>, server_is_local: bool) -> Option<NetworkSignal> {
        let online = route.is_some() || server_is_local;
        let signal = if self.online != Some(online) {
            self.online = Some(online);
            Some(NetworkSignal::Status { online })
        } else if route.is_some() && self.route.is_some() && route != self.route {
            Some(NetworkSignal::Changed)
        } else {
            None
        };
        self.route = route;
        signal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_on_this_machine_is_told_from_one_elsewhere() {
        for url in ["http://127.0.0.1:13773", "http://localhost:13773/", "http://[::1]:13773", "LOCALHOST"] {
            assert!(is_loopback_url(url), "{url}");
        }
        for url in ["http://192.168.1.20:13773", "https://example.com", ""] {
            assert!(!is_loopback_url(url), "{url}");
        }
    }

    #[test]
    fn the_core_hears_of_a_network_coming_going_and_changing() {
        let (wifi, cable): (IpAddr, IpAddr) = ("192.168.1.5".parse().expect("an address"), "10.0.0.7".parse().expect("an address"));
        let mut watch = NetworkWatch::default();
        assert_eq!(watch.probed(Some(wifi), false), Some(NetworkSignal::Status { online: true }));
        assert_eq!(watch.probed(Some(wifi), false), None);
        assert_eq!(watch.probed(Some(cable), false), Some(NetworkSignal::Changed));
        assert_eq!(watch.probed(None, false), Some(NetworkSignal::Status { online: false }));
        assert_eq!(watch.probed(None, false), None);
        // A server on this machine is there without a network.
        assert_eq!(watch.probed(None, true), Some(NetworkSignal::Status { online: true }));
        assert_eq!(watch.probed(Some(wifi), true), None);
    }
}
