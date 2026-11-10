//! One device of a person in a workspace: the session every program keeps, and the one entry an
//! agent, `tilo mcp` or the front door takes it by.

use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_actions::remote::{
    ActionHost as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget,
};
use tilo_relay_client::login::{Door, SignIn, redeem, through_door};
use tilo_relay_client::{CertifyError, DeviceSigner, HttpClient};
use tilo_relay_protocol::web::{
    HereAction, HereReply, HereRequest, MAX_HERE_FIELD_CHARS, MAX_HERE_LABEL_CHARS, kept,
};
use tilo_relay_session::workspaces::{
    CREATE_OWN_ROOM, CreatedWorkspace, HOST_ACTIONS_PROVIDER, HOST_ROOMS_RESOURCE, WorkspaceListing,
};
use tilo_user_identity::DeviceCert;
use web_time::{Duration as Wait, Instant};

use crate::session::{HereCompletion, PeerConfig, PeerHandle, TransportPhase};
use crate::session_actions::SessionActions;
use crate::{MemoryPeerStore, PeerStore};

/// How long joining a workspace waits for its host to admit this device.
const ADMITTING: Wait = Wait::from_secs(20);
/// How often a join looks at the session while it waits.
const POLL: Wait = Wait::from_millis(100);
/// How long a workspace's host has to answer an operation of its own.
const ANSWERING: u32 = 20_000;

/// A device of a person, in the workspaces its session is in.
#[derive(Clone)]
pub struct Device {
    /// The session with the workspaces' host.
    peer: PeerHandle,
    cert: DeviceCert,
    /// The host's own device: whose device answers a workspace's own actions.
    host_cert: DeviceCert,
    signer: Arc<dyn DeviceSigner>,
    /// The workspace the device entered.
    workspace_id: u64,
    /// What this device calls itself, as its host's journals name it.
    label: String,
}

impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Device")
            .field("workspace_id", &self.workspace_id)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl Device {
    /// A session for the device `config` certifies, in no workspace yet.
    #[must_use]
    pub fn over(config: PeerConfig, signer: Arc<dyn DeviceSigner>, label: &str) -> Self {
        Self {
            peer: PeerHandle::start(
                config.clone(),
                signer.clone(),
                Arc::new(MemoryPeerStore::default()) as Arc<dyn PeerStore>,
            ),
            cert: config.cert,
            host_cert: config.host_cert,
            signer,
            workspace_id: 0,
            label: label.to_owned(),
        }
    }

    /// A session for the device `sign_in` signed in.
    #[must_use]
    pub fn connect(sign_in: &SignIn, signer: Arc<dyn DeviceSigner>) -> Self {
        Self::over(
            PeerConfig {
                host: sign_in.host.clone(),
                cert: sign_in.cert.clone(),
                host_cert: sign_in.host_cert.clone(),
                revocations: tilo_user_identity::RevocationList::new(),
            },
            signer,
            &sign_in.label,
        )
    }

    /// Take `sign_in`'s device into the workspace its link names, through the site's door where it
    /// names one and the person's own workspace where the door leads elsewhere.
    ///
    /// # Errors
    /// The workspace's id is not one, the host could not be asked, or the workspace refused.
    pub async fn enter(
        client: &dyn HttpClient,
        door: Option<&Door<'_>>,
        sign_in: &SignIn,
        signer: Arc<dyn DeviceSigner>,
    ) -> Result<Self, CertifyError> {
        let workspace = sign_in.link.workspace_id();
        tilo_relay_protocol::room_workspace_id(&workspace)
            .map_err(|error| CertifyError::Request(error.to_string()))?;
        let device = Self::connect(sign_in, signer);
        // The site's door lets the person in on this device; without one, the host must already
        // admit this device.
        if let Some(door) = door {
            through_door(
                client,
                door,
                &sign_in.cert,
                device.signer.as_ref(),
                &sign_in.agent,
                &sign_in.label,
            )
            .await?;
        }
        // The door lets the person into the site's home workspace, which admits the person's own
        // by an invitation this device asks for and redeems.
        let lobby = door.and_then(Door::workspace);
        if let Some(lobby) = lobby.as_deref().filter(|lobby| *lobby != workspace) {
            let lobby_id = device.join(lobby).await.map_err(|error| {
                refused(
                    error,
                    "The site's workspace let this computer join, but its session is not admitted \
                     to it.",
                )
            })?;
            let (_, invitation) = device
                .own_workspace(lobby_id, Map::new(), "tilo login")
                .await?;
            device.redeem(client, &invitation, &sign_in.label).await?;
        }
        let entered = device.join(&workspace).await.map_err(|error| {
            refused(
                error,
                if door.is_some() {
                    "This workspace let this computer join, but its session is not admitted to it."
                } else {
                    "This workspace did not let this computer in, and its site names no door to \
                     ask."
                },
            )
        })?;
        let device = Self {
            workspace_id: entered,
            ..device
        };
        // The person's tab shows the computer where it just came in.
        let _ = say_here(
            &device.peer,
            entered,
            &sign_in.agent,
            vec![action("Signed in", &sign_in.label, "Tilo CLI")],
            true,
        )
        .await;
        Ok(device)
    }

    /// The session itself.
    #[must_use]
    pub fn session(&self) -> &PeerHandle {
        &self.peer
    }

    /// The workspace this device entered.
    #[must_use]
    pub fn workspace_id(&self) -> u64 {
        self.workspace_id
    }

    /// What this device calls itself.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Whether the session still serves.
    #[must_use]
    pub fn is_live(&self) -> bool {
        !matches!(
            self.peer.status().phase,
            TransportPhase::Closed | TransportPhase::Failed
        )
    }

    /// Join `workspace` (its UUID) and wait for its host to say this device is in: its id here.
    ///
    /// # Errors
    /// It is no workspace UUID, the session is closed, or its host did not admit this device.
    pub async fn join(&self, workspace: &str) -> Result<u64, CertifyError> {
        let id = tilo_relay_protocol::room_workspace_id(workspace).map_err(|error| {
            CertifyError::Request(format!("{workspace} names no workspace: {error}"))
        })?;
        self.peer.join(id).map_err(CertifyError::Request)?;
        if !self.admitted(id).await {
            return Err(CertifyError::Denied(
                "The workspace did not admit this device.".to_owned(),
            ));
        }
        Ok(id)
    }

    /// Whether the session is in `workspace_id`, waiting for its host to say so.
    pub async fn admitted(&self, workspace_id: u64) -> bool {
        let started = Instant::now();
        loop {
            let status = self.peer.status();
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
            if started.elapsed() >= ADMITTING {
                return false;
            }
            n0_future::time::sleep(POLL).await;
        }
    }

    /// Redeem `invitation`, which admits this device to a workspace.
    ///
    /// # Errors
    /// The invitation was not one of the person's, or the workspace refused the entry.
    pub async fn redeem(
        &self,
        client: &dyn HttpClient,
        invitation: &[u8],
        label: &str,
    ) -> Result<(), CertifyError> {
        redeem(client, invitation, &self.cert, self.signer.as_ref(), label).await
    }

    /// The person's own workspace on the host, asked from inside `workspace_id`, and the invitation
    /// into it: its `name` makes another, or the `room` it must be.
    ///
    /// # Errors
    /// The host did not answer, refused, or answered no invitation.
    pub async fn own_workspace(
        &self,
        workspace_id: u64,
        arguments: Map<String, Value>,
        caller: &str,
    ) -> Result<(WorkspaceListing, Vec<u8>), CertifyError> {
        let refused = |why: String| {
            CertifyError::Denied(format!(
                "The host made no workspace of the person's own: {why}"
            ))
        };
        let output = self
            .host_action(workspace_id, CREATE_OWN_ROOM, arguments, caller)
            .await
            .map_err(|error| CertifyError::Denied(error.to_string()))?;
        let made: CreatedWorkspace = serde_json::from_value(output)
            .map_err(|error| refused(format!("it answered something else: {error}")))?;
        let invitation = made
            .invitation
            .ok_or_else(|| refused("it issued no invitation".to_owned()))?;
        let invitation =
            hex::decode(invitation.invitation).map_err(|error| refused(error.to_string()))?;
        Ok((made.workspace, invitation))
    }

    /// The host's own operation `action` with `arguments`, asked in `workspace_id`.
    ///
    /// # Errors
    /// The host did not answer, refused, or did not finish.
    pub async fn host_action(
        &self,
        workspace_id: u64,
        action: &str,
        arguments: Map<String, Value>,
        caller: &str,
    ) -> Result<Value, CertifyError> {
        let target = RemoteActionTarget {
            provider: HOST_ACTIONS_PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(HOST_ROOMS_RESOURCE, action),
        };
        self.ask_host(workspace_id, target, arguments, caller).await
    }

    /// What the host itself answers to `target` with `arguments`, asked in
    /// `workspace_id`: one of its own operations, or the person's account
    /// on it.
    ///
    /// # Errors
    /// The host did not answer, refused, or did not finish.
    pub async fn ask_host(
        &self,
        workspace_id: u64,
        target: RemoteActionTarget,
        arguments: Map<String, Value>,
        caller: &str,
    ) -> Result<Value, CertifyError> {
        let unreachable = |why: String| CertifyError::Request(why);
        let receipt = self
            .actions(workspace_id)
            .invoke(
                &Caller::local_person(caller),
                InvokeRequest {
                    on_behalf_of: None,
                    invocation: tilo_actions::InvocationId::new(),
                    target,
                    argument: InvokeArgument::Json(arguments),
                    context: None,
                    confirmation: None,
                    revision: None,
                    wait_ms: Some(ANSWERING),
                    appearance: None,
                },
            )
            .await
            .ok_or_else(|| unreachable("the host did not answer".to_owned()))?
            .map_err(|error| CertifyError::Denied(error.to_string()))?;
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
            ReceiptStatus::Failed { error } => Err(CertifyError::Denied(error.message)),
            ReceiptStatus::Rejected { error } => Err(CertifyError::Denied(error.to_string())),
            ReceiptStatus::Cancelled { .. } | ReceiptStatus::Accepted => Err(unreachable(
                "the host did not finish what was asked of it".to_owned(),
            )),
        }
    }

    /// This device's actions in `workspace_id`, asked of the workspace's host.
    #[must_use]
    pub fn actions(&self, workspace_id: u64) -> SessionActions {
        SessionActions::new(&self.peer, workspace_id, self.host_cert.device_public_key)
    }

    /// Say this device is here in `workspace_id`, doing `actions`.
    ///
    /// # Errors
    /// The request is past what a host keeps, the session is closed, or the host did not answer.
    pub async fn say_here(
        &self,
        workspace_id: u64,
        label: &str,
        actions: Vec<HereAction>,
        agent: bool,
    ) -> Result<HereReply, String> {
        say_here(&self.peer, workspace_id, label, actions, agent).await
    }

    /// Close the session: it says this device left, and stops.
    pub fn close(&self) {
        self.peer.close();
    }
}

/// A refusal the workspace made says `refusal`; anything else stands.
fn refused(error: CertifyError, refusal: &str) -> CertifyError {
    match error {
        CertifyError::Denied(_) => CertifyError::Denied(refusal.to_owned()),
        other => other,
    }
}

/// Post what this device is doing in the workspace, and ask who is here.
///
/// # Errors
/// The request is past what a host keeps, the session is closed, or the host did not answer.
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

/// One thing this device did, as the workspace shows it.
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
