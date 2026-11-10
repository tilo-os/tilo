//! Entering the workspace a signed-in device names, and saying it is here.
//!
//! [`tilo_relay_client::login`] signs a computer in as one of the person's
//! devices. This module takes that device into the workspace its link names
//! ([`enter`]) and reports what the agent does there ([`say_here`]), over the
//! session it starts here.

use std::sync::Arc;

use tilo_relay_client::CertifyError;
pub use tilo_relay_client::login::{
    Door, PairingLink, SignIn, redeem, sign_in, site_door, through_door,
};
use tilo_relay_protocol::web::{
    HereAction, HereReply, HereRequest, MAX_HERE_FIELD_CHARS, MAX_HERE_LABEL_CHARS, kept,
};
use tilo_user_identity::{DeviceCert, RevocationList};
use web_time::{Duration, Instant};

use crate::session::{HereCompletion, PeerConfig, PeerHandle};
use crate::{DeviceSigner, HttpClient, MemoryPeerStore};

/// How long [`enter`] waits for the workspace to admit the device.
const ADMISSION_WAIT: Duration = Duration::from_secs(20);
/// How often [`enter`] looks at the session while it waits.
const POLL: Duration = Duration::from_millis(100);

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
        label: kept(label, MAX_HERE_LABEL_CHARS),
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
        action: kept(title, MAX_HERE_FIELD_CHARS),
        resource: kept(resource, MAX_HERE_FIELD_CHARS),
        app: kept(app, MAX_HERE_FIELD_CHARS),
        at_ms: tilo_clock::unix_now_ms(),
    }
}
