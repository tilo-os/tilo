//! Entering the workspace a login names, as this computer's device: what `tilo login` does once
//! it is certified, and what the daemon of this computer does for every agent and command of the
//! login. Both dial the server at the address the login keeps, so the sentences a failure says
//! are the same too.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use ed25519_dalek::SigningKey;
use tilo_device_identity::login::{CertifyAnswer, Device, LOGIN_DAMAGED, Reached};
use tilo_server_client::login::{PairingLink, SignIn};
use tilo_session_client::device::Device as Entered;

use crate::server::ServerAddress;

/// What a login no server accepts any more says.
pub const LOGIN_ENDED: &str = "This computer's Tilo login has ended: ask the person to run `tilo device add` \
     on one of their devices and log in with the ticket it prints.";

/// What a tilo that speaks another version than the server says.
pub const TOO_OLD: &str = "This tilo is too old for your workspace: install it again with \
     `curl -fsSL https://tilo.sh | sh`.";

/// What a server that answers nothing usable says.
pub const UNREACHED: &str = "Tilo can't be reached from here right now; try again in a minute.";

/// What a computer with no login says.
pub const NOT_LOGGED_IN: &str = "This computer is not logged in to Tilo. Ask the person to run `tilo device add` on one of their devices and give you the ticket it prints; then run tilo login with it, like: tilo login tilo1…";

/// What a device joined over iroh keeps where a web origin would be: the server's endpoint id
/// after it, since such a server has no origin.
pub const IROH_ONLY: &str = "iroh:";

/// Why something failed, for whoever runs the logs: `RUST_LOG` set says
/// this computer keeps them, and the person gets the plain sentence instead.
pub fn debug_why(what: &str, why: &dyn std::fmt::Display) {
    if std::env::var_os("RUST_LOG").is_some() {
        eprintln!("tilo: {what}: {why}");
    }
}

/// What a failure to enter the workspace says, from the error's own chain.
#[must_use]
pub fn enter_said(error: &anyhow::Error) -> String {
    let chain = format!("{error:#}");
    let says = |needle: &str| chain.contains(needle);
    if says("Tilo login has ended") || says("device cert has expired") || says("revoked") {
        LOGIN_ENDED.to_owned()
    } else if says("Tilo login is damaged")
        || says("not hex")
        || says("names no person")
        || says("postcard")
    {
        LOGIN_DAMAGED.to_owned()
    } else if says("peer protocol")
        || says("handshake frame does not decode")
        || says("malformed answer")
        || says("answers in a way this device does not read")
    {
        TOO_OLD.to_owned()
    } else if let Some((_, reason)) = chain.split_once("The workspace did not let this device in") {
        // The workspace answered and said why: that reason is what the person can act on, and
        // calling it unreachable would send them to check a connection that works.
        format!(
            "The workspace did not let this device in{}.",
            reason.trim_end_matches('.')
        )
    } else if says("did not let this key in") || says("did not let this computer in") {
        "Tilo didn't let this computer into the workspace. Ask the person to allow it in their \
         Tilo tab, then try again."
            .to_owned()
    } else {
        UNREACHED.to_owned()
    }
}

/// Enter the workspace `device` names, as that device, dialing its server at the address the
/// login keeps; its session runs on what the device's own service `lent` it where it runs one.
///
/// # Errors
/// The login names no server address, or the server is not reached or does not let this device
/// in.
pub async fn connect(
    key: &SigningKey,
    device: &Device,
    lent: Option<tilo_session_client::Lent>,
) -> Result<Entered> {
    let address = device
        .server_address
        .as_ref()
        .ok_or_else(|| anyhow!(LOGIN_ENDED))?;
    let signed = signed_over_iroh(device, address)?;
    Ok(Entered::enter_lent(None, &signed, Arc::new(key.clone()), lent).await?)
}

/// Where the server `device` reaches is dialed: at the address its login names.
///
/// # Errors
/// The login names no server address.
#[expect(
    clippy::unused_async,
    reason = "the session's callers await it; it asks nothing of the network since a login names its server's address"
)]
pub async fn server_of(device: &Device) -> Result<tilo_server_client::ServerAddress> {
    device
        .server_address
        .as_ref()
        .map(tilo_server_client::ServerAddress::published)
        .ok_or_else(|| anyhow!(LOGIN_ENDED))
}

/// `device` signed in at the server dialed over iroh at `address`.
///
/// # Errors
/// The login names no workspace a server reads.
pub fn signed_over_iroh(device: &Device, address: &ServerAddress) -> Result<SignIn> {
    Ok(SignIn {
        link: PairingLink {
            site: device.site.clone(),
            host_origin: device.host_origin.clone(),
            workspace: uuid::Uuid::parse_str(&device.workspace)
                .map_err(|_| anyhow!(LOGIN_ENDED))?,
            user_public_key: device.cert.user_public_key,
            invite: None,
        },
        agent: device.agent.clone(),
        label: device.label().to_owned(),
        server: tilo_server_client::ServerAddress::published(address),
        cert: device.cert.clone(),
        server_cert: device.server_cert.clone(),
    })
}

/// How a person names the server at `origin`: its host name, or "its server" for one reached
/// over iroh alone.
#[must_use]
pub fn host_of(origin: &str) -> &str {
    // A server joined over iroh has no web name: it is the workspace's own.
    if origin.starts_with(IROH_ONLY) {
        return "its server";
    }
    // A server on this computer is plain http, which a link names by its
    // address alone, as it names an https server by its name.
    origin
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
}

/// The iroh address `reached` names.
///
/// # Errors
/// The endpoint id is no key.
pub fn address_of(reached: &Reached) -> Result<iroh::EndpointAddr> {
    let id = iroh::EndpointId::from_bytes(&reached.endpoint)
        .map_err(|_| anyhow!("this ticket names no device: ask for a new one"))?;
    let mut addr = iroh::EndpointAddr::new(id);
    if let Some(relay) = reached
        .relay
        .as_deref()
        .and_then(|url| url.parse::<iroh::RelayUrl>().ok())
    {
        addr = addr.with_relay_url(relay);
    }
    for direct in &reached.direct {
        addr = addr.with_ip_addr(*direct);
    }
    Ok(addr)
}

/// Where `addr` is reached, as a ticket and its answer carry it.
#[must_use]
pub fn reached_of(addr: &iroh::EndpointAddr) -> Reached {
    Reached {
        endpoint: *addr.id.as_bytes(),
        relay: addr.relay_urls().next().map(ToString::to_string),
        direct: addr.ip_addrs().copied().collect(),
    }
}

/// The login `answer` makes of this computer, for `agent`.
#[must_use]
pub fn device_of(answer: CertifyAnswer, agent: &str) -> Device {
    let enter = answer.enter;
    let address = address_of(&enter.server)
        .ok()
        .map(|endpoint_addr| ServerAddress {
            relay_url: enter.server.relay.clone(),
            endpoint_addr,
            web: None,
        });
    Device {
        site: enter.site,
        host_origin: enter.host_origin,
        workspace: enter.workspace,
        workspace_name: enter.workspace_name,
        agent: agent.to_owned(),
        cert: answer.cert,
        server_cert: enter.server_cert,
        server_address: address,
    }
}
