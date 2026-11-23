//! Entering the workspace a login names, as this computer's device: what `tilo login` does once
//! it is certified, and what the daemon of this computer does for every agent and command of the
//! login. Both dial the server at the address the login keeps, so the sentences a failure says
//! are the same too.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use ed25519_dalek::SigningKey;
use tilo_device_identity::login::{CertifyAnswer, Device, Entry, LOGIN_DAMAGED, Reached};
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
    } else if says("certificate is damaged")
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
    let entered = Entered::enter_lent(None, &signed, Arc::new(key.clone()), lent).await?;
    enter_own_workspace(&entered);
    Ok(entered)
}

/// Have `entered` take the person's own workspace in by its id, at the device the login enters
/// through: the device the ticket named, which keeps it when the person's root issued the ticket
/// there, or the server its login enters through. Its person is that workspace's one member, so it
/// admits this device as theirs, and their list of workspaces, kept there, reaches this device. A
/// device that keeps no copy of it refuses, and this device goes on in the workspace it entered.
fn enter_own_workspace(entered: &Entered) {
    let own = tilo_server_protocol::servers::person_workspace_id(&entered.cert().user_public_key);
    if own == entered.workspace_id().to_string() {
        return;
    }
    let entered = entered.clone();
    tokio::spawn(async move {
        if let Err(error) = entered.join(&own).await {
            tracing::warn!(%error, workspace = %own, "the person's own workspace was not entered at the device the login enters through");
        }
    });
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

/// The site a computer is shown when the device that adds it enters no server's workspace but
/// its own: the same one an invitation over iroh names.
const FOUNDED_SITE: &str = "https://tilo.sh/";

/// Where a computer that a device logged in as `login` adds enters: the workspace and server that
/// login names.
///
/// # Errors
/// The login names no server any more ([`LOGIN_ENDED`]).
pub fn login_entry(login: &Device) -> Result<Entry, String> {
    let published = login
        .server_address
        .as_ref()
        .ok_or_else(|| LOGIN_ENDED.to_owned())?;
    let mut server = reached_of(&published.endpoint_addr);
    if server.relay.is_none() {
        server.relay.clone_from(&published.relay_url);
    }
    Ok(Entry {
        workspace: login.workspace.clone(),
        workspace_name: login.workspace_name.clone(),
        server_cert: login.server_cert.clone(),
        server,
        site: login.site.clone(),
        host_origin: login.host_origin.clone(),
    })
}

/// Where a computer that a device with no login adds enters: `workspace`, named `name`, which the
/// device holds, through its `endpoint` and the certificate `server_cert` its workspace service
/// proves.
#[must_use]
pub fn founded_entry(
    endpoint: &iroh::Endpoint,
    (workspace, name): (String, String),
    server_cert: tilo_user_identity::DeviceCert,
) -> Entry {
    Entry {
        workspace,
        workspace_name: Some(name),
        server_cert,
        server: reached_of(&endpoint.addr()),
        site: FOUNDED_SITE.to_owned(),
        host_origin: format!("{IROH_ONLY}{}", endpoint.id()),
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

/// What `devices#logIn` answers: the device this computer became, and for how many days its
/// certificate holds.
#[must_use]
pub fn logged_in(device: &Device) -> serde_json::Value {
    let days = device.cert.expires_at_ns.map_or(0, |end| {
        end.saturating_sub(device.cert.issued_at_ns) / (24 * 3600 * 1_000_000_000)
    });
    serde_json::json!({
        "label": device.label(),
        "days": days,
        "workspace": device.workspace,
        "workspace_name": device.workspace_name,
        "host_origin": device.host_origin,
    })
}

/// How many times the call asking the device to certify this computer is sent.
const CERTIFY_TRIES: u32 = 3;

/// Log this computer in with `ticket`, which another of the person's devices issued: that device
/// has the person's root certify this computer's key `key`, for `agent`, listed as `label`, asked
/// over `endpoint`. The certificate it answers is filed among the device's certificates in
/// `place` and made this account's identity ([`adopt`]); the device this computer became, and the
/// server it enters through, are answered to the daemon, which keeps them in memory alone.
///
/// The one way a computer becomes one of the person's devices: `tilo login <ticket>` through the
/// daemon's `devices#logIn`, and a ticket handed to the daemon at its first boot.
///
/// # Errors
/// The text is no ticket, the issuing device is not reached, the ticket is spent, wrong or ran
/// out, the person declined, or the certificate cannot be kept.
pub async fn log_in(
    key: &SigningKey,
    endpoint: &iroh::Endpoint,
    place: &tilo_device_identity::login::Place,
    ticket: &str,
    agent: &str,
    label: Option<&str>,
) -> Result<Device> {
    let ticket = tilo_device_identity::login::Ticket::parse(ticket)
        .map_err(|_| anyhow!("This is not a Tilo ticket: ask for a new one."))?;
    if ticket.direction != tilo_device_identity::login::Direction::Login {
        return Err(anyhow!(
            "That ticket signs a browser in: run `tilo device add <ticket>` with it on this \
             computer."
        ));
    }
    let answer = certified_with(key, endpoint, &ticket, agent, label).await?;
    let device = device_of(answer, agent);
    tilo_device_identity::login::file_certificate(place, &device.cert)
        .map_err(|error| anyhow!("keeping this computer's certificate: {error:#}"))?;
    adopt(place, &device.cert)?;
    Ok(device)
}

/// Make `cert`, which the person's root signed for this computer's device key, this account's
/// identity: the identity service signs for apps here as its person from then on, as on a device
/// enrolled any other way. A computer that holds the person's root is theirs already, and one
/// enrolled for another person (the root it made for itself before it logged in, or an earlier
/// login's) becomes this person's: they just chose it.
///
/// # Errors
/// The identity cannot be written.
pub fn adopt(
    place: &tilo_device_identity::login::Place,
    cert: &tilo_user_identity::DeviceCert,
) -> Result<()> {
    if cert.capabilities.can_admit_devices {
        return Ok(());
    }
    let dir = tilo_identity::store::identity_dir_from_env()
        .map_err(|error| anyhow!("this account's identity: {error}"))?;
    tilo_identity::provisioning::adopt_login_certificate(
        &dir,
        place.keys(),
        cert,
        tilo_clock::unix_now_ns(),
    )
    .map_err(|error| anyhow!("making this computer's certificate its identity: {error}"))
}

/// [`log_in`] from this process, on an endpoint of its own for the time the login takes: how a
/// computer no person's certificate names yet logs in, whose daemon serves the device key alone
/// and runs no endpoint of its own until then.
///
/// # Errors
/// As [`log_in`], and when this computer's endpoint does not bind.
pub async fn log_in_here(
    key: &SigningKey,
    place: &tilo_device_identity::login::Place,
    ticket: &str,
    agent: &str,
    label: Option<&str>,
) -> Result<Device> {
    let parsed = tilo_device_identity::login::Ticket::parse(ticket.trim())
        .map_err(|_| anyhow!("This is not a Tilo ticket: ask for a new one."))?;
    let endpoint = tilo_server_client::ServerAddress::at(address_of(&parsed.issuer)?)
        .bind(Some(key.to_bytes()))
        .await
        .map_err(|error| anyhow!("this computer's endpoint: {error}"))?;
    let logged_in = log_in(key, &endpoint, place, ticket.trim(), agent, label).await;
    endpoint.close().await;
    logged_in
}

/// Ask the device that issued `ticket` to have the person's root certify `key`, for `agent`,
/// listed as `label`: its answer, once the person approved. A call lost on its way is asked again
/// with the same invocation, which the issuer answers where the call is.
async fn certified_with(
    key: &SigningKey,
    endpoint: &iroh::Endpoint,
    ticket: &tilo_device_identity::login::Ticket,
    agent: &str,
    label: Option<&str>,
) -> Result<CertifyAnswer> {
    use tilo_actions::remote::{
        ActionCallError, InvokeArgument, InvokeRequest, RemoteActionTarget,
    };
    use tilo_device_identity::login::{
        CERTIFY, CertifyInput, PROVIDER, RESOURCE, SCOPE, TICKET_LIFETIME_NS,
    };
    let issuer = address_of(&ticket.issuer)?;
    let dialed = tilo_server_client::ServerAddress::at(issuer);
    let input = CertifyInput {
        secret: ticket.secret_hex(),
        device_public_key: hex::encode(key.verifying_key().to_bytes()),
        agent: agent.to_owned(),
        label: label.map(str::to_owned),
    };
    let serde_json::Value::Object(argument) = serde_json::to_value(&input)? else {
        return Err(anyhow!("the certify input is an object"));
    };
    // Asked as this computer's own key, the one the certificate is for: a daemon's shared action
    // service signs as its workspace service instead.
    let device = Arc::new(
        tilo_peer_state::actions::ActionService::new(
            key.clone(),
            Arc::new(tilo_peer_state::actions::NoActions),
        )
        .remote(endpoint, &dialed.addr(), SCOPE),
    );
    let request = InvokeRequest {
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(RESOURCE, CERTIFY),
        },
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let now = std::time::Instant::now();
    let deadline = now
        .checked_add(std::time::Duration::from_nanos(TICKET_LIFETIME_NS))
        .unwrap_or(now);
    let mut sent = 1;
    let settled = loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let answer = Arc::clone(&device)
            .invoke_until_settled(request.clone(), left)
            .await;
        match answer {
            Some(Err(error))
                if sent < CERTIFY_TRIES && matches!(error, ActionCallError::Unavailable { .. }) =>
            {
                sent = sent.saturating_add(1);
            }
            answer => break answer,
        }
    };
    match settled {
        Some(Ok(tilo_actions::InvocationUpdate::Succeeded { output, .. })) => output
            .value
            .and_then(|value| serde_json::from_value(value).ok())
            .ok_or_else(|| anyhow!(TOO_OLD)),
        Some(Ok(tilo_actions::InvocationUpdate::Failed { error, .. })) => Err(anyhow!(
            "Your device didn't add this computer: {}.",
            error.message.trim_end_matches('.')
        )),
        Some(Ok(_)) | None => Err(anyhow!(UNREACHED)),
        Some(Err(error)) => Err(anyhow!(certify_refused(&error))),
    }
}

/// What a call the device that made a ticket did not take says.
#[must_use]
pub fn certify_refused(error: &tilo_actions::remote::ActionCallError) -> String {
    use tilo_actions::remote::ActionCallError;
    match error {
        ActionCallError::Refused { reason } => {
            format!(
                "Your device didn't take this ticket: {}.",
                reason.trim_end_matches('.')
            )
        }
        ActionCallError::UnknownAction => TOO_OLD.to_owned(),
        ActionCallError::Unavailable { reason } => format!(
            "The device that made this ticket did not take it: {}. If its `tilo device add` \
             ended, ask for a new ticket.",
            reason.trim_end_matches('.')
        ),
        _ => "The device that made this ticket can't be reached from here: it may be asleep, or \
              its `tilo device add` ended. Ask for a new ticket."
            .to_owned(),
    }
}

/// Log this computer in at the server `host` names (the line it keeps in `web-host.json`), as
/// the device whose key is `key` and whose certificate `cert` the person's root signed here: a
/// computer that holds the root needs no ticket, since nobody else is asked to certify it. The
/// server's own list of the person's workspaces names the one the login enters first (`asked`,
/// else [`choose_workspace`]'s), and the server puts this device in that workspace's roster; the
/// daemon serving the login then takes every other one of that list in as well, and answers the
/// server's asks for the content it holds of them.
///
/// # Errors
/// `host` names no address, the server is not reached or refuses this device, or it keeps no
/// workspace of the person's (none called `asked`).
pub async fn log_in_at(
    key: &SigningKey,
    cert: &tilo_user_identity::DeviceCert,
    host: tilo_server_protocol::web::PageHost,
    asked: Option<&str>,
) -> Result<Device> {
    let address: ServerAddress = host
        .address
        .ok_or_else(|| anyhow!("that server names no address to reach it at"))?
        .into();
    let host_origin = host
        .origin
        .clone()
        .unwrap_or_else(|| format!("{IROH_ONLY}{}", address.endpoint_addr.id));
    let device = Device {
        site: host.origin.map_or_else(
            || "https://tilo.sh/".to_owned(),
            |origin| format!("{origin}/"),
        ),
        host_origin,
        // Where the session starts, until the server's list names the workspace it enters.
        workspace: tilo_server_protocol::servers::person_workspace_id(&cert.user_public_key),
        workspace_name: None,
        // The person's own device: no agent's run.
        agent: String::new(),
        cert: cert.clone(),
        server_cert: host.server_cert,
        server_address: Some(address.clone()),
    };
    let signed = signed_over_iroh(&device, &address)?;
    let session = Entered::connect(&signed, Arc::new(key.clone()));
    let chosen = async {
        session.connected().await.map_err(|refusal| {
            debug_why("dialing the server", &refusal);
            anyhow!("{refusal}")
        })?;
        let listed = session
            .personal_list(
                &tilo_server_session::workspaces::WorkspaceLabels::new(),
                None,
            )
            .await
            .map_err(|refusal| anyhow!("{refusal}"))?;
        let (workspace, name) = choose_workspace(&listed.workspaces, asked)?;
        // Entered here, the server puts this device in the workspace's roster, as a ticket's
        // issuer does for the device it certifies: the server's sync with this device's own copy
        // of the workspace checks the roster before it reads a frame of it.
        session
            .enter_workspace(&workspace, "")
            .await
            .map_err(|refusal| anyhow!("{refusal}"))?;
        anyhow::Ok((workspace, name))
    }
    .await;
    session.close();
    let (workspace, name) = chosen?;
    Ok(Device {
        workspace,
        workspace_name: Some(name).filter(|name| !name.trim().is_empty()),
        ..device
    })
}

/// The workspace a held login enters: the one `asked` names (its id, or its name in any case),
/// else the first the person made of their own, else the first they were let in to. A login names
/// no workspace to have every one of the person's: the peer it starts gives each of the others a
/// folder as well, so the one entered first is only where it starts.
///
/// # Errors
/// `asked` names none of them, or there are none.
pub fn choose_workspace(
    listed: &[tilo_server_session::workspaces::PersonalWorkspace],
    asked: Option<&str>,
) -> Result<(String, String)> {
    let pick = |found: &tilo_server_session::workspaces::PersonalWorkspace| {
        (found.workspace.clone(), found.name.clone())
    };
    if let Some(asked) = asked.map(str::trim).filter(|asked| !asked.is_empty()) {
        return listed
            .iter()
            .find(|found| {
                found.workspace.eq_ignore_ascii_case(asked)
                    || found.name.eq_ignore_ascii_case(asked)
            })
            .map(pick)
            .ok_or_else(|| {
                anyhow!(
                    "You have no workspace called \"{asked}\" there; {}",
                    names_of(listed)
                )
            });
    }
    let first = |own: bool| {
        listed
            .iter()
            .filter(|found| !own || found.role.as_deref() == Some("owner"))
            .min_by_key(|found| {
                (
                    found.made_at_ms.unwrap_or(i64::MAX),
                    found.workspace.clone(),
                )
            })
    };
    first(true)
        .or_else(|| first(false))
        .map(pick)
        .ok_or_else(|| anyhow!("You have no workspace there yet: make one in Tilo first."))
}

fn names_of(listed: &[tilo_server_session::workspaces::PersonalWorkspace]) -> String {
    let names: Vec<_> = listed
        .iter()
        .map(|found| format!("\"{}\"", found.name))
        .collect();
    format!("yours are {}.", names.join(", "))
}
