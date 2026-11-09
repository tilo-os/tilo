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
        let (invitation, token) = ask_door(door, &sign_in.cert, &sign_in.agent).await?;
        let reply = ask_to_join_pre_admitted(
            client,
            &invitation,
            &sign_in.cert,
            signer.as_ref(),
            rand::random(),
            &sign_in.label,
            token.as_bytes(),
        )
        .await?;
        match reply {
            tilo_relay_protocol::workspaces::JoinReply::Admitted { .. } => {}
            tilo_relay_protocol::workspaces::JoinReply::Refused(reason) => {
                return Err(CertifyError::Denied(format!(
                    "This workspace did not let this computer in: {reason}"
                )));
            }
            _ => {
                return Err(CertifyError::Denied(
                    "This workspace did not let this computer in.".to_owned(),
                ));
            }
        }
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
    )
    .await;
    Ok(peer)
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
async fn admitted(peer: &PeerHandle, workspace_id: u64) -> bool {
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

/// Post what this device is doing in the workspace and ask who is here.
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
) -> Result<HereReply, String> {
    let request = HereRequest {
        workspace_id,
        label: text(label, MAX_HERE_LABEL_CHARS),
        window: None,
        actions,
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
