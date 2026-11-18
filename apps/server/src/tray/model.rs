//! What the status bar menu shows for a given server status. Pure, so it is
//! tested without a menu bar.

use crate::control::ServerStatus;
use crate::remote::Phase;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IconKind {
    /// Remote access is off or still starting.
    Off,
    /// On, nobody connected.
    On,
    /// On, with a remote device connected.
    Connected,
    /// Remote access failed to start.
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuModel {
    /// The first, disabled line.
    pub status_line: String,
    /// A second disabled line with the reason remote access failed.
    pub error_line: Option<String>,
    pub remote_checked: bool,
    /// The switch is unavailable while a change is under way.
    pub remote_enabled: bool,
    /// Pairing needs remote access running.
    pub pairing_enabled: bool,
    pub devices: Vec<DeviceRow>,
    pub icon: IconKind,
    pub tooltip: String,
    /// Everything that needs the server (false until it reports in).
    pub ready: bool,
    pub launch_at_login: bool,
}

/// `status` is `None` until the server reports in. `busy` while a change the
/// user asked for is under way; `flash` is a short-lived confirmation
/// ("Pairing link copied") shown in place of the status line.
pub fn model(status: Option<&ServerStatus>, busy: bool, flash: Option<&str>, launch_at_login: bool) -> MenuModel {
    let Some(status) = status else {
        return MenuModel {
            status_line: "Domovoi Server is starting…".into(),
            error_line: None,
            remote_checked: false,
            remote_enabled: false,
            pairing_enabled: false,
            devices: Vec::new(),
            icon: IconKind::Off,
            tooltip: "Domovoi Server".into(),
            ready: false,
            launch_at_login,
        };
    };
    let running = matches!(status.phase, Phase::Online { .. });
    let remote_connected = status.devices.iter().any(|d| d.remote && d.connected);
    let summary = status.summary();
    MenuModel {
        status_line: flash.map(str::to_owned).unwrap_or_else(|| summary.clone()),
        error_line: match &status.phase {
            Phase::Failed(reason) => Some(clip(reason, 90)),
            _ => None,
        },
        // "On" while starting too: the switch shows what was asked for.
        remote_checked: status.enabled || running || matches!(status.phase, Phase::Starting),
        remote_enabled: !busy && !matches!(status.phase, Phase::Starting),
        pairing_enabled: running,
        devices: status
            .devices
            .iter()
            .filter(|d| d.remote)
            .map(|d| DeviceRow { id: d.id.clone(), label: if d.connected { format!("{} (connected)", d.label) } else { d.label.clone() } })
            .collect(),
        icon: match &status.phase {
            Phase::Failed(_) => IconKind::Error,
            Phase::Online { .. } if remote_connected => IconKind::Connected,
            Phase::Online { .. } => IconKind::On,
            Phase::Off | Phase::Starting => IconKind::Off,
        },
        tooltip: format!("Domovoi Server: {summary}"),
        ready: true,
        launch_at_login,
    }
}

fn clip(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(chars - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::Device;

    fn device(label: &str, remote: bool, connected: bool) -> Device {
        Device { id: format!("id-{label}"), label: label.into(), kind: "mobile".into(), remote, connected, last_seen: None }
    }

    fn status(phase: Phase, devices: Vec<Device>) -> ServerStatus {
        ServerStatus { enabled: !matches!(phase, Phase::Off), phase, endpoint_id: "abc".into(), devices, push_configured: false, push_devices: 0 }
    }

    #[test]
    fn before_the_server_reports_in_nothing_is_clickable() {
        let m = model(None, false, None, false);
        assert!(!m.ready && !m.remote_enabled && !m.pairing_enabled);
        assert_eq!(m.icon, IconKind::Off);
    }

    #[test]
    fn off_and_starting() {
        let off = model(Some(&status(Phase::Off, vec![])), false, None, false);
        assert_eq!(off.status_line, "Remote access off");
        assert!(!off.remote_checked && off.remote_enabled && !off.pairing_enabled);
        assert_eq!(off.icon, IconKind::Off);
        let starting = model(Some(&status(Phase::Starting, vec![])), false, None, false);
        assert!(starting.remote_checked, "the switch shows what was asked for");
        assert!(!starting.remote_enabled && !starting.pairing_enabled);
    }

    #[test]
    fn online_counts_remote_devices_only() {
        let s = status(
            Phase::Online { relay: Some("https://relay".into()), lan_only: false },
            vec![device("Browser", false, true), device("iPhone", true, true), device("iPad", true, false)],
        );
        let m = model(Some(&s), false, None, true);
        assert_eq!(m.status_line, "Remote access on · 2 devices, 1 connected");
        assert_eq!(m.tooltip, "Domovoi Server: Remote access on · 2 devices, 1 connected");
        assert_eq!(m.icon, IconKind::Connected);
        assert!(m.pairing_enabled && m.remote_checked && m.remote_enabled && m.launch_at_login);
        assert_eq!(m.devices, vec![DeviceRow { id: "id-iPhone".into(), label: "iPhone (connected)".into() }, DeviceRow { id: "id-iPad".into(), label: "iPad".into() }]);
        // A browser on this machine does not light the icon.
        let local_only = status(Phase::Online { relay: None, lan_only: true }, vec![device("Browser", false, true)]);
        let m = model(Some(&local_only), false, None, false);
        assert_eq!(m.icon, IconKind::On);
        assert_eq!(m.status_line, "Remote access on (local network) · no devices paired");
    }

    #[test]
    fn failure_busy_and_flash() {
        let failed = model(Some(&status(Phase::Failed("binding the iroh endpoint: address in use".into()), vec![])), false, None, false);
        assert_eq!(failed.icon, IconKind::Error);
        assert_eq!(failed.status_line, "Remote access failed to start");
        assert_eq!(failed.error_line.as_deref(), Some("binding the iroh endpoint: address in use"));
        assert!(failed.remote_enabled, "it can be tried again");
        let long = model(Some(&status(Phase::Failed("x".repeat(300)), vec![])), false, None, false);
        assert_eq!(long.error_line.unwrap().chars().count(), 90);
        let s = status(Phase::Online { relay: None, lan_only: false }, vec![]);
        let busy = model(Some(&s), true, Some("Pairing link copied"), false);
        assert!(!busy.remote_enabled);
        assert_eq!(busy.status_line, "Pairing link copied");
        assert!(busy.tooltip.contains("no relay yet"), "the tooltip keeps the real status");
    }
}
