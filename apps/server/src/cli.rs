//! `domovoi-server remote …` and `domovoi-server stop`: manage a server from another
//! terminal. They reach the running server the way a client does (a
//! short-lived pairing code from the shared database, exchanged over
//! loopback), call the same routes the apps use, and sign out again. With no
//! server running they act on the home directory's files instead.

use crate::auth::{Grant, session_id};
use crate::control::{CLI_LABEL, Device};
use crate::util::sha256_hex;
use crate::{Base, Fallible};
use serde::Serialize;
use serde::de::DeserializeOwned;
use domovoi_model::typed::{AuthAccessTokenResult, RemoteAccessState, RemoteAccessStatus, RequestRejection};
use std::time::Duration;

#[derive(clap::Subcommand)]
pub enum RemoteCmd {
    /// Whether remote access is on, how it is reachable and who is paired.
    Status,
    /// Turn remote access on (kept across restarts).
    On,
    /// Turn remote access off.
    Off,
    /// List paired devices.
    Devices,
    /// Sign a device out; takes an id (or its first characters) from `devices`.
    Revoke { id: String },
}

/// A signed-in loopback connection to the running server.
struct Local<'a> {
    http: reqwest::Client,
    origin: String,
    token: String,
    base: &'a Base,
}

impl Drop for Local<'_> {
    fn drop(&mut self) {
        // Sign out: the session only existed for this command.
        let _ = self.base.auth.revoke(&session_id(&sha256_hex(self.token.as_bytes())), false);
    }
}

/// Connects to the server using this home directory on `host:port`; `None`
/// when nothing answers there.
async fn open<'a>(base: &'a Base, host: &str, port: u16) -> Fallible<Option<Local<'a>>> {
    let host = match host {
        "0.0.0.0" | "::" | "[::]" => "127.0.0.1",
        other => other,
    };
    let origin = format!("http://{host}:{port}");
    let http = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
    let Ok(res) = http.get(format!("{origin}/.well-known/t3/environment")).send().await else {
        return Ok(None);
    };
    // Anything but this server's own descriptor is another server's answer.
    let environment = res.json::<domovoi_contracts::environment::EnvironmentDescriptor>().await.ok();
    if environment.map(|found| found.environment_id) != Some(base.environment.environment_id.clone()) {
        return Err(format!("the server on port {port} uses a different home directory; pass its --home-dir or --port").into());
    }
    let grant = Grant::limited(vec!["access:read".into(), "access:write".into()]);
    let code = base.auth.issue_pairing_code_with(Duration::from_secs(60), &grant)?;
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "urn:ietf:params:oauth:grant-type:token-exchange")
        .append_pair("subject_token", &code)
        .append_pair("subject_token_type", "urn:t3:params:oauth:token-type:environment-bootstrap")
        .append_pair("requested_token_type", "urn:ietf:params:oauth:token-type:access_token")
        .append_pair("client_label", CLI_LABEL)
        .append_pair("client_device_type", "bot")
        .finish();
    let res = http.post(format!("{origin}/oauth/token")).header("content-type", "application/x-www-form-urlencoded").body(form).send().await?;
    if !res.status().is_success() {
        return Err(format!("the server refused the command-line sign-in ({})", res.status()).into());
    }
    let token = res.json::<AuthAccessTokenResult>().await.map_err(|_| "the server sent no access token")?.access_token;
    Ok(Some(Local { http, origin, token, base }))
}

impl Local<'_> {
    async fn get<T: DeserializeOwned>(&self, path: &str) -> Fallible<T> {
        answer(self.http.get(format!("{}{path}", self.origin)).bearer_auth(&self.token).send().await?).await
    }

    async fn post<T: DeserializeOwned>(&self, path: &str, body: &impl Serialize) -> Fallible<T> {
        answer(self.http.post(format!("{}{path}", self.origin)).bearer_auth(&self.token).json(body).send().await?).await
    }
}

/// The body of an answer, or why the server refused the request.
async fn answer<T: DeserializeOwned>(res: reqwest::Response) -> Fallible<T> {
    let status = res.status();
    if status.is_success() {
        return Ok(res.json().await?);
    }
    let rejection = res.json::<RequestRejection>().await.ok();
    let detail = rejection.as_ref().and_then(|r| r.message().or(r.reason())).unwrap_or("request failed");
    Err(format!("{detail} ({status})").into())
}

/// A request without a body of its own.
#[derive(Serialize)]
struct NoBody {}

pub async fn remote(base: &Base, host: &str, port: u16, cmd: &RemoteCmd) -> Fallible {
    let local = open(base, host, port).await?;
    match (cmd, &local) {
        (RemoteCmd::Status, Some(local)) => print_status(&local.get("/api/domovoi/remote").await?),
        (RemoteCmd::Status, None) => {
            let settings = base.remote.settings();
            println!("Server: not running");
            println!("Remote access: {} at the next start", if settings.enabled { "on" } else { "off" });
            println!("Endpoint: {}", base.remote.endpoint_id());
            print_devices(&crate::control::devices_in(&base.db, |_| (false, None)));
        }
        (RemoteCmd::On | RemoteCmd::Off, Some(local)) => {
            let enabled = matches!(cmd, RemoteCmd::On);
            print_status(&local.post("/api/domovoi/remote/configure", &crate::http::ConfigureRequest { enabled, relays: None }).await?);
        }
        (RemoteCmd::On | RemoteCmd::Off, None) => {
            let enabled = matches!(cmd, RemoteCmd::On);
            base.remote.save_enabled(enabled)?;
            println!("Remote access: {} at the next start (no server is running)", if enabled { "on" } else { "off" });
        }
        (RemoteCmd::Devices, Some(local)) => {
            let status: RemoteAccessStatus = local.get("/api/domovoi/remote").await?;
            print_devices(&status.devices);
        }
        (RemoteCmd::Devices, None) => print_devices(&crate::control::devices_in(&base.db, |_| (false, None))),
        (RemoteCmd::Revoke { id }, running) => {
            let devices = match running {
                Some(local) => local.get::<RemoteAccessStatus>("/api/domovoi/remote").await?.devices,
                None => crate::control::devices_in(&base.db, |_| (false, None)),
            };
            let matches: Vec<&Device> = devices.iter().filter(|d| d.id.starts_with(id.as_str())).collect();
            let device = match matches.as_slice() {
                [one] => *one,
                [] => return Err(format!("no device has an id starting with {id}").into()),
                _ => return Err(format!("{} devices have an id starting with {id}; give more of it", matches.len()).into()),
            };
            match running {
                Some(local) => {
                    local.post::<serde::de::IgnoredAny>("/api/auth/clients/revoke", &crate::http::RevokeRequest { session_id: device.id.clone() }).await?;
                }
                None => {
                    if let crate::auth::Revoke::Done(hash) = base.auth.revoke(&device.id, false)? {
                        let _ = crate::push::forget_sessions(&base.db, &[hash]);
                    }
                }
            }
            println!("Signed out {}", device.label);
        }
    }
    Ok(())
}

pub async fn stop(base: &Base, host: &str, port: u16) -> Fallible {
    match open(base, host, port).await? {
        Some(local) => {
            local.post::<serde::de::IgnoredAny>("/api/domovoi/server/stop", &NoBody {}).await?;
            println!("Stopping the server on port {port}");
        }
        None => println!("No server is running on port {port}"),
    }
    Ok(())
}

fn print_status(status: &RemoteAccessStatus) {
    let state = match &status.state {
        RemoteAccessState::Online => match &status.home_relay {
            Some(relay) => format!("on (relay {relay})"),
            None => "on (no relay reached yet)".into(),
        },
        RemoteAccessState::LanOnly => "on (local network only)".into(),
        RemoteAccessState::Starting => "starting".into(),
        RemoteAccessState::Error => format!("failed to start: {}", status.error.as_deref().unwrap_or("see the server log")),
        _ => "off".into(),
    };
    println!("Server: running");
    println!("Remote access: {state}");
    println!("Endpoint: {}", status.endpoint_id);
    if !status.direct_addresses.is_empty() {
        println!("Addresses: {}", status.direct_addresses.join(", "));
    }
    println!("Push: {}", if status.push.configured { format!("on, {} device(s) registered", status.push.devices) } else { "off (no APNs key)".into() });
    print_devices(&status.devices);
}

fn print_devices(devices: &[Device]) {
    if devices.is_empty() {
        println!("Devices: none paired");
        return;
    }
    println!("Devices:");
    for d in devices {
        println!(
            "  {}  {:<24} {:<8} {:<7} {}",
            &d.id[..d.id.len().min(12)],
            d.label,
            d.kind.as_str(),
            if d.remote { "remote" } else { "local" },
            if d.connected { "connected".to_owned() } else { d.last_seen.as_ref().map(|t| format!("last seen {t}")).unwrap_or_else(|| "not connected".into()) },
        );
    }
}
