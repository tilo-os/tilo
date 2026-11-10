//! Signing a computer in as one of a person's devices, and entering the
//! workspace with it.
//!
//! The person's page opened a one-time pairing invite and copied a link
//! ([`PairingLink`]). This computer makes its own device key, asks the
//! workspace's host to have the person's root certify it ([`certify`], as a
//! new browser of theirs is certified), and is then a device of theirs: a
//! member for thirty days, the host pinned, no root anywhere here. It then
//! enters the workspace the link names ([`enter`]) and says it is here.

use std::sync::Arc;

use tilo_relay_protocol::web::{
    HereAction, HereReply, HereRequest, HostInfo, MAX_HERE_FIELD_CHARS, MAX_HERE_LABEL_CHARS,
};
use tilo_user_identity::{DeviceCert, RevocationList};
use web_time::{Duration, Instant};

use crate::custody::{CertifyError, certify};
use crate::invite::ask_to_join_pre_admitted;
use crate::pairing_link::PairingLink;
use crate::session::{HereCompletion, Host, PeerConfig, PeerHandle};
use crate::{DeviceSigner, HttpClient, MemoryPeerStore, host_info};

/// How long [`enter`] waits for the workspace to admit the device.
const ADMISSION_WAIT: Duration = Duration::from_secs(20);
/// How often [`enter`] looks at the session while it waits.
const POLL: Duration = Duration::from_millis(100);

/// A workspace's public door, as the site names it (`entry.json`'s home
/// `admission`): `client` posts below the door's origin, `path` is its
/// `join` (`/peer/v1/directory/<workspace>/join`).
pub struct Door<'a> {
    /// Posts to the door's origin.
    pub client: &'a dyn HttpClient,
    /// The door's `join`, below that origin.
    pub path: String,
}

impl Door<'_> {
    /// The workspace the door lets people into, as its path names it.
    #[must_use]
    pub fn workspace(&self) -> Option<String> {
        let mut segments = self.path.split('/').filter(|segment| !segment.is_empty());
        segments.find(|segment| *segment == "directory")?;
        segments.next().map(str::to_owned)
    }
}

/// The door of the site whose `entry.json` is `entry` to the workspace it
/// keeps on the host at `host_origin` (`home.admission`): the door's origin
/// and its `join` path, when the site names one for that host. Only HTTPS,
/// but for a door on loopback.
#[must_use]
pub fn site_door(entry: &serde_json::Value, host_origin: &str) -> Option<(String, String)> {
    let home = entry.get("home")?;
    let home_host = home.pointer("/host/origin")?.as_str()?;
    if home_host.trim_end_matches('/') != host_origin.trim_end_matches('/') {
        return None;
    }
    let admission = home.get("admission")?.as_str()?;
    let (scheme, rest) = admission.split_once("://")?;
    let (authority, path) = rest.split_at(rest.find('/')?);
    let host = authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host);
    let loopback = matches!(host, "localhost" | "127.0.0.1" | "[::1]");
    if authority.is_empty() || !(scheme == "https" || (scheme == "http" && loopback)) {
        return None;
    }
    let path = if path.ends_with('/') {
        format!("{path}join")
    } else {
        format!("{path}/join")
    };
    Some((format!("{scheme}://{authority}"), path))
}

/// The workspace of the person's own on the host and the invitation into it
/// for the person, which the host makes the first time any device of theirs
/// asks and answers again after (`room.create-own`, with `arguments`: its
/// `name`, which makes another, or the `room` it must be), asked over
/// this device's session in `workspace_id`.
///
/// # Errors
///
/// The host did not answer, refused, or answered no invitation.
pub async fn own_workspace(
    peer: &PeerHandle,
    workspace_id: u64,
    host_cert: &DeviceCert,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<(tilo_relay_session::workspaces::WorkspaceListing, Vec<u8>), CertifyError> {
    use tilo_actions::remote::{
        ActionHost as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget,
    };
    use tilo_relay_session::workspaces::{
        CREATE_OWN_ROOM, CreatedWorkspace, HOST_ACTIONS_PROVIDER, HOST_ROOMS_RESOURCE,
    };
    let refused = |why: String| {
        CertifyError::Denied(format!(
            "The host made no workspace of the person's own: {why}"
        ))
    };
    let host = crate::session_actions::SessionActions::new(
        peer,
        workspace_id,
        host_cert.device_public_key,
    );
    let receipt = host
        .invoke(
            &Caller::local_person("tilo login"),
            InvokeRequest {
                on_behalf_of: None,
                invocation: tilo_actions::InvocationId::new(),
                target: RemoteActionTarget {
                    provider: HOST_ACTIONS_PROVIDER.to_owned(),
                    address: tilo_actions::ActionAddress::new(HOST_ROOMS_RESOURCE, CREATE_OWN_ROOM),
                },
                argument: InvokeArgument::Json(arguments),
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: Some(20_000),
                appearance: None,
            },
        )
        .await
        .ok_or_else(|| refused("the host did not answer".to_owned()))?
        .map_err(|error| refused(error.to_string()))?;
    let output = match receipt.status {
        ReceiptStatus::Completed { output } => output,
        ReceiptStatus::Failed { error } => return Err(refused(error.message)),
        ReceiptStatus::Rejected { error } => return Err(refused(error.to_string())),
        ReceiptStatus::Cancelled { .. } | ReceiptStatus::Accepted => {
            return Err(refused("it did not finish".to_owned()));
        }
    };
    let made: CreatedWorkspace = serde_json::from_value(output.value.unwrap_or_default())
        .map_err(|error| refused(error.to_string()))?;
    let invitation = made
        .invitation
        .ok_or_else(|| refused("it issued no invitation".to_owned()))?;
    let invitation =
        hex::decode(invitation.invitation).map_err(|error| refused(error.to_string()))?;
    Ok((made.workspace, invitation))
}

/// A computer that is now one of the person's devices.
pub struct SignIn {
    /// The link it was signed in with.
    pub link: PairingLink,
    /// The agent that runs here, as the person's page names it.
    pub agent: String,
    /// This device's label, as the person's devices list it.
    pub label: String,
    /// Where the workspace's host is reached.
    pub host: Host,
    /// This device's certificate: a member of the person's, for thirty days.
    pub cert: DeviceCert,
    /// The host's certificate, pinned.
    pub host_cert: DeviceCert,
}

/// Ask `link`'s host to have the person's root certify `signer`'s device as
/// the agent `agent` on the computer labeled `label`. `on_pending` receives
/// the pairing code to show while the person's other device answers.
///
/// # Errors
///
/// The person declined or nobody answered in time ([`CertifyError::Expired`]),
/// the host was not reached or refused ([`CertifyError::Transport`],
/// [`CertifyError::Denied`]), or what it issued is not a certificate for this
/// device ([`CertifyError::Denied`]).
pub async fn sign_in(
    client: &dyn HttpClient,
    link: &PairingLink,
    agent: &str,
    label: &str,
    signer: &dyn DeviceSigner,
    on_pending: impl FnOnce(&str),
) -> Result<SignIn, CertifyError> {
    let host = host_of(host_info(client).await?)?;
    let invite = link.invite.unwrap_or([0; 16]);
    let (cert, host_cert) = certify(
        &host,
        link.user_public_key,
        label,
        invite,
        signer,
        on_pending,
    )
    .await?;
    Ok(SignIn {
        link: link.clone(),
        agent: agent.to_owned(),
        label: label.to_owned(),
        host,
        cert,
        host_cert,
    })
}

/// Where the host published it is reachable, as a session dials it.
fn host_of(info: HostInfo) -> Result<Host, CertifyError> {
    let address = info.address.ok_or_else(|| {
        CertifyError::Denied(
            "This workspace's host takes no device connections, so no computer can sign in to it."
                .to_owned(),
        )
    })?;
    let endpoint = match address
        .relay_url
        .as_deref()
        .and_then(|url| url.parse::<iroh_base::RelayUrl>().ok())
    {
        Some(relay) => address.endpoint_addr.with_relay_url(relay),
        None => address.endpoint_addr,
    };
    Ok(Host::at(endpoint))
}

/// Enter the workspace the link names, as the certified device, then say the
/// agent is here.
///
/// Where the site names the workspace's public door, the device goes
/// through it first, as the page does; then the session subscribes.
///
/// # Errors
///
/// The workspace's id is not one, the host could not be asked, or the
/// workspace did not let this device in: its door refused, or it admitted the
/// device to no session.
pub async fn enter(
    client: &dyn HttpClient,
    door: Option<&Door<'_>>,
    sign_in: &SignIn,
    signer: Arc<dyn DeviceSigner>,
) -> Result<PeerHandle, CertifyError> {
    let workspace = sign_in.link.workspace_id();
    let workspace_id = tilo_relay_protocol::room_workspace_id(&workspace)
        .map_err(|error| CertifyError::Request(error.to_string()))?;
    let peer = PeerHandle::start(
        PeerConfig {
            host: sign_in.host.clone(),
            cert: sign_in.cert.clone(),
            host_cert: sign_in.host_cert.clone(),
            revocations: RevocationList::new(),
        },
        signer.clone(),
        Arc::new(MemoryPeerStore::default()),
    );
    // A door the site names lets the person in on this device first, as the
    // page does it; without one, the host must already admit this device of
    // a member of the workspace.
    if let Some(door) = door {
        through_door(
            client,
            door,
            &sign_in.cert,
            signer.as_ref(),
            &sign_in.agent,
            &sign_in.label,
        )
        .await?;
    }
    // The door lets the person into the site's home workspace. The person's
    // own workspace (where the agent is set up) admits each of their devices
    // by an invitation of its own: from inside the home workspace this
    // device asks the host for the person's workspace, as the page does,
    // and redeems the invitation it answers with.
    let lobby = door.and_then(Door::workspace);
    if let Some(lobby) = lobby.as_deref().filter(|lobby| *lobby != workspace) {
        let lobby_id = tilo_relay_protocol::room_workspace_id(lobby)
            .map_err(|error| CertifyError::Request(error.to_string()))?;
        peer.join(lobby_id).map_err(CertifyError::Request)?;
        if !admitted(&peer, lobby_id).await {
            return Err(CertifyError::Denied(
                "The site's workspace let this computer join, but its session is not admitted to it."
                    .to_owned(),
            ));
        }
        let (_, invitation) =
            own_workspace(&peer, lobby_id, &sign_in.host_cert, serde_json::Map::new()).await?;
        redeem(
            client,
            &invitation,
            &sign_in.cert,
            signer.as_ref(),
            &sign_in.label,
        )
        .await?;
    }
    peer.join(workspace_id).map_err(CertifyError::Request)?;
    if !admitted(&peer, workspace_id).await {
        return Err(CertifyError::Denied(if door.is_some() {
            "This workspace let this computer join, but its session is not admitted to it."
                .to_owned()
        } else {
            "This workspace did not let this computer in, and its site names no door to ask."
                .to_owned()
        }));
    }
    let _ = say_here(
        &peer,
        workspace_id,
        &sign_in.agent,
        vec![action("Signed in", &sign_in.label, "Tilo CLI")],
        true,
    )
    .await;
    Ok(peer)
}

/// Go through `door` on the device `cert` certifies, as the page does: the
/// door lets its person in (`name` is how it greets them) and the device,
/// labeled `label`, redeems what it answers on the host `client` posts to.
///
/// # Errors
///
/// The door or the host was not reached, or either refused.
pub async fn through_door(
    client: &dyn HttpClient,
    door: &Door<'_>,
    cert: &DeviceCert,
    signer: &dyn DeviceSigner,
    name: &str,
    label: &str,
) -> Result<(), CertifyError> {
    let (invitation, token) = ask_door(door, cert, name).await?;
    let reply = ask_to_join_pre_admitted(
        client,
        &invitation,
        cert,
        signer,
        rand::random(),
        label,
        token.as_bytes(),
    )
    .await?;
    match reply {
        tilo_relay_protocol::workspaces::JoinReply::Admitted { .. } => Ok(()),
        tilo_relay_protocol::workspaces::JoinReply::Refused(reason) => Err(CertifyError::Denied(
            format!("This workspace did not let this computer in: {reason}"),
        )),
        _ => Err(CertifyError::Denied(
            "This workspace did not let this computer in.".to_owned(),
        )),
    }
}

/// Redeem `invitation` on the host `client` posts to, as the device `cert`
/// certifies, labeled `label`.
///
/// # Errors
///
/// The host was not reached, or did not admit the device.
pub async fn redeem(
    client: &dyn HttpClient,
    invitation: &[u8],
    cert: &DeviceCert,
    signer: &dyn DeviceSigner,
    label: &str,
) -> Result<(), CertifyError> {
    let reply =
        crate::invite::ask_to_join(client, invitation, cert, signer, rand::random(), label).await?;
    match reply {
        tilo_relay_protocol::workspaces::JoinReply::Admitted { .. } => Ok(()),
        tilo_relay_protocol::workspaces::JoinReply::Refused(reason) => Err(CertifyError::Denied(
            format!("The person's workspace did not let this computer in: {reason}"),
        )),
        _ => Err(CertifyError::Denied(
            "The person's workspace did not let this computer in.".to_owned(),
        )),
    }
}

/// Ask `door` to let the person in on this device, as the page asks it: the
/// invitation bound to them and the one-time token that admits them.
async fn ask_door(
    door: &Door<'_>,
    cert: &DeviceCert,
    name: &str,
) -> Result<(Vec<u8>, String), CertifyError> {
    use base64::Engine as _;
    let join = tilo_relay_protocol::directory::DoorJoin {
        user_public_key: hex::encode(cert.user_public_key),
        display_name: text(name, MAX_HERE_LABEL_CHARS),
    };
    let body =
        serde_json::to_vec(&join).map_err(|error| CertifyError::Request(error.to_string()))?;
    let (status, answer) = door.client.post(&door.path, body).await.map_err(|error| {
        CertifyError::Transport(crate::LinkError::Unavailable(format!(
            "The workspace's door was not reached: {error}"
        )))
    })?;
    if status != 200 {
        let said = String::from_utf8_lossy(&answer);
        return Err(CertifyError::Denied(format!(
            "This workspace's door did not let this computer in (HTTP {status}): {}",
            text(&said, 200)
        )));
    }
    let answer: tilo_relay_protocol::directory::DoorAnswer = serde_json::from_slice(&answer)
        .map_err(|error| {
            CertifyError::Denied(format!("The door answered something else: {error}"))
        })?;
    let invitation = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(answer.invitation.trim_end_matches('='))
        .map_err(|error| {
            CertifyError::Denied(format!("The door's invitation is damaged: {error}"))
        })?;
    Ok((invitation, answer.admission_token))
}

/// Whether the session is in `workspace_id`, waiting for its host to say so.
pub async fn admitted(peer: &PeerHandle, workspace_id: u64) -> bool {
    let started = Instant::now();
    loop {
        let status = peer.status();
        if status.workspaces.get(&workspace_id) == Some(&true) {
            return true;
        }
        if status
            .failures
            .iter()
            .any(|failure| failure.code == "not-admitted")
        {
            return false;
        }
        if started.elapsed() >= ADMISSION_WAIT {
            return false;
        }
        n0_future::time::sleep(POLL).await;
    }
}

/// Post what this device is doing in the workspace, and ask who is here:
/// `agent` when it is an agent the person runs on it ([`HereRequest::agent`]),
/// `label` the agent's name.
///
/// # Errors
///
/// The request is past what a host keeps, the session is closed, or the host
/// did not answer.
pub async fn say_here(
    peer: &PeerHandle,
    workspace_id: u64,
    label: &str,
    actions: Vec<HereAction>,
    agent: bool,
) -> Result<HereReply, String> {
    let request = HereRequest {
        workspace_id,
        label: text(label, MAX_HERE_LABEL_CHARS),
        window: None,
        actions,
        agent,
    };
    request.validate().map_err(str::to_owned)?;
    let (send, receive) = tokio::sync::oneshot::channel();
    let done: HereCompletion = Box::new(move |reply| {
        let _ = send.send(reply);
    });
    peer.here_then(request, done)?;
    receive
        .await
        .map_err(|_| "the session ended before the host answered".to_owned())?
}

/// One thing the agent did, as the workspace shows it: its title, what it
/// touched and where.
#[must_use]
pub fn action(title: &str, resource: &str, app: &str) -> HereAction {
    HereAction {
        id: rand::random(),
        action: text(title, MAX_HERE_FIELD_CHARS),
        resource: text(resource, MAX_HERE_FIELD_CHARS),
        app: text(app, MAX_HERE_FIELD_CHARS),
        at_ms: tilo_clock::unix_now_ms(),
    }
}

/// `text` without control characters, cut to what a host keeps.
fn text(text: &str, max: usize) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .take(max)
        .collect()
}
