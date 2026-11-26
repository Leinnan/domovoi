//! What the app was started with: a `domovoi://` link (from the shell, a
//! browser or a later launch), or a server and pairing code to connect to at
//! once (`just windows-smoke`).

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Launch {
    /// A `domovoi://connect?…` link the system handed the app. The core asks
    /// before it connects (`Event::OpenLink`).
    pub link: Option<String>,
    /// `DOMOVOI_SERVER`: a local server's URL to connect to at once…
    pub server: Option<String>,
    /// …with `DOMOVOI_PAIRING`, a one-time code it minted (`domovoi-server pair`).
    pub pairing: Option<String>,
    /// `DOMOVOI_SMOKE=1`: exits 0 once connected, 1 when the connection fails.
    pub smoke: bool,
}

impl Launch {
    pub fn from_args(args: impl Iterator<Item = String>) -> Self {
        let link = args.into_iter().find(|a| a.starts_with("domovoi://"));
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        Launch { link, server: env("DOMOVOI_SERVER"), pairing: env("DOMOVOI_PAIRING"), smoke: env("DOMOVOI_SMOKE").is_some_and(|v| v == "1") }
    }
}

#[cfg(test)]
mod tests {
    use super::Launch;

    #[test]
    fn the_link_is_found_among_the_arguments() {
        let launch = Launch::from_args(["--flag".to_owned(), "domovoi://connect?ticket=t&code=1".to_owned()].into_iter());
        assert_eq!(launch.link.as_deref(), Some("domovoi://connect?ticket=t&code=1"));
        assert_eq!(Launch::from_args(std::iter::empty()).link, None);
    }
}
