//! One device of a person in a workspace: the session every program keeps, and the one entry an
//! agent, `tilo mcp` or the front door takes it by.

use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_actions::remote::host_account::Refusal;
use tilo_actions::remote::{
    ActionHost as _, ActionHostError, Caller, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_relay_client::login::redeem;
#[cfg(not(target_arch = "wasm32"))]
use tilo_relay_client::login::{Door, SignIn, through_door};
use tilo_relay_client::{CertifyError, DeviceSigner, HttpClient};
use tilo_relay_session::workspaces::{
    CREATE_OWN_ROOM, CreatedWorkspace, HOST_ACTIONS_PROVIDER, HOST_ROOMS_RESOURCE, WorkspaceListing,
};
use tilo_user_identity::DeviceCert;
use web_time::{Duration as Wait, Instant};

use crate::session::{AgentRunCompletion, PeerConfig, PeerHandle, TransportPhase};
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

    /// The device whose session `peer` already is: a page's, which started its session itself.
    /// `cert` and `signer` must be the ones `peer` was started with.
    #[must_use]
    pub fn of(
        peer: PeerHandle,
        cert: DeviceCert,
        host_cert: DeviceCert,
        signer: Arc<dyn DeviceSigner>,
        label: &str,
    ) -> Self {
        Self {
            peer,
            cert,
            host_cert,
            signer,
            workspace_id: 0,
            label: label.to_owned(),
        }
    }

    /// A session for the device `sign_in` signed in. A computer's entry: a
    /// page writes login codes and never reads one, so it makes its device
    /// with [`Self::over`] and joins the workplaces its grants reach.
    #[cfg(not(target_arch = "wasm32"))]
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
    #[cfg(not(target_arch = "wasm32"))]
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
        // The session is the agent's run: the person's agent door lists it while it lasts.
        let _ = run_as_agent(&device.peer, &sign_in.agent).await;
        Ok(device)
    }

    /// The session itself.
    #[must_use]
    pub fn session(&self) -> &PeerHandle {
        &self.peer
    }

    /// This device's certificate.
    #[must_use]
    pub fn cert(&self) -> &DeviceCert {
        &self.cert
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
    ) -> Result<(WorkspaceListing, Vec<u8>), HostRefusal> {
        let output = self
            .host_action(workspace_id, CREATE_OWN_ROOM, arguments, caller)
            .await?;
        let malformed = |why: String| {
            HostRefusal::malformed(format!(
                "The host made no workspace of the person's own: {why}"
            ))
        };
        let made: CreatedWorkspace = serde_json::from_value(output)
            .map_err(|error| malformed(format!("it answered something else: {error}")))?;
        let invitation = made
            .invitation
            .ok_or_else(|| malformed("it issued no invitation".to_owned()))?;
        let invitation =
            hex::decode(invitation.invitation).map_err(|error| malformed(error.to_string()))?;
        Ok((made.workspace, invitation))
    }

    /// Have this device in the person's own workspace on the host, asked from inside
    /// `workspace_id`: the host makes it the first time, or answers the one the person has (another
    /// for each `name` they give), and this very device redeems the invitation into it, so the key
    /// this session holds is the one that opens it next. A workspace `entered` says this device is
    /// in already is not redeemed again. `shown_as` is the name its members see.
    ///
    /// # Errors
    /// The host did not answer or refused (typed, as it said), or the workspace did not admit this
    /// device.
    pub async fn enter_own_workspace(
        &self,
        client: &dyn HttpClient,
        workspace_id: u64,
        name: Option<&str>,
        shown_as: &str,
        entered: impl FnOnce(&str) -> bool,
    ) -> Result<WorkspaceListing, HostRefusal> {
        let mut arguments = Map::new();
        if let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) {
            arguments.insert("name".to_owned(), Value::String(name.to_owned()));
        }
        let (listing, invitation) = self
            .own_workspace(workspace_id, arguments, &self.label)
            .await?;
        if !entered(&listing.workspace) {
            self.redeem(client, &invitation, shown_as)
                .await
                .map_err(HostRefusal::from)?;
        }
        Ok(listing)
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
    ) -> Result<Value, HostRefusal> {
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
    ) -> Result<Value, HostRefusal> {
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
            .ok_or_else(|| HostRefusal::unanswered("the host did not answer"))?
            .map_err(|error| HostRefusal::of(&error))?;
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
            ReceiptStatus::Failed { error } => Err(HostRefusal::failed(&error.message)),
            ReceiptStatus::Rejected { error } => Err(HostRefusal::of(&error)),
            ReceiptStatus::Cancelled { .. } | ReceiptStatus::Accepted => Err(
                HostRefusal::unanswered("the host did not finish what was asked of it"),
            ),
        }
    }

    /// This device's actions in `workspace_id`, asked of the workspace's host.
    #[must_use]
    pub fn actions(&self, workspace_id: u64) -> SessionActions {
        SessionActions::new(&self.peer, workspace_id, self.host_cert.device_public_key)
    }

    /// Close the session: it says this device left, and stops.
    pub fn close(&self) {
        self.peer.close();
    }
}

/// Why a workspace's host did not do what a device asked of it, as the host said it: what every
/// caller (the CLI, the front door, the page) reads and shows, never a sentence to parse.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct HostRefusal {
    /// What kind of answer it was: an [`ActionHostError::code`] when the host refused,
    /// [`HostRefusal::FAILED`] when the operation ran and failed, [`HostRefusal::UNANSWERED`] when
    /// no answer came, [`HostRefusal::MALFORMED`] when the answer was not what the operation
    /// answers, and [`HostRefusal::NOT_ADMITTED`] when a workspace did not let the device in.
    pub code: String,
    /// What happened, in plain words for whoever asked.
    pub message: String,
    /// The host's typed refusal, when it gave one: its reason and the ways through it.
    pub refusal: Option<Refusal>,
}

impl HostRefusal {
    /// The operation ran on the host and failed.
    pub const FAILED: &'static str = "failed";
    /// The host did not answer, or did not finish.
    pub const UNANSWERED: &'static str = "unanswered";
    /// The host answered something the operation does not answer.
    pub const MALFORMED: &'static str = "malformed";
    /// The workspace did not admit the device.
    pub const NOT_ADMITTED: &'static str = "not_admitted";

    /// The host's refusal `error`, its typed refusal read off it where it carries one.
    #[must_use]
    pub fn of(error: &ActionHostError) -> Self {
        match error {
            ActionHostError::Refused { reason } => Self::said(error.code(), reason),
            other => Self {
                code: other.code().to_owned(),
                message: other.to_string(),
                refusal: None,
            },
        }
    }

    /// The operation failed, saying `message` (a typed refusal's text, where it is one).
    #[must_use]
    pub fn failed(message: &str) -> Self {
        Self::said(Self::FAILED, message)
    }

    /// No answer came: `why`.
    #[must_use]
    pub fn unanswered(why: &str) -> Self {
        Self {
            code: Self::UNANSWERED.to_owned(),
            message: why.to_owned(),
            refusal: None,
        }
    }

    /// The answer was not the operation's: `why`.
    #[must_use]
    pub fn malformed(why: String) -> Self {
        Self {
            code: Self::MALFORMED.to_owned(),
            message: why,
            refusal: None,
        }
    }

    /// `reason` as the host said it, typed where it is a [`Refusal`]'s text.
    fn said(code: &str, reason: &str) -> Self {
        let refusal = Refusal::from_reason(reason);
        Self {
            code: code.to_owned(),
            message: refusal
                .as_ref()
                .map_or_else(|| reason.to_owned(), |refusal| refusal.message.clone()),
            refusal,
        }
    }
}

impl std::fmt::Display for HostRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HostRefusal {}

/// A device's entry failed: a workspace that did not admit it was refused; anything else was not
/// answered.
impl From<CertifyError> for HostRefusal {
    fn from(error: CertifyError) -> Self {
        match error {
            CertifyError::Denied(why) => Self {
                code: Self::NOT_ADMITTED.to_owned(),
                message: why,
                refusal: None,
            },
            other => Self::unanswered(&other.to_string()),
        }
    }
}

/// What a computer's entry reports: a refusal is the host's denial, no answer a failed request.
impl From<HostRefusal> for CertifyError {
    fn from(refusal: HostRefusal) -> Self {
        if refusal.code == HostRefusal::UNANSWERED {
            Self::Request(refusal.message)
        } else {
            Self::Denied(refusal.message)
        }
    }
}

/// A refusal the workspace made says `refusal`; anything else stands.
#[cfg(not(target_arch = "wasm32"))]
fn refused(error: CertifyError, refusal: &str) -> CertifyError {
    match error {
        CertifyError::Denied(_) => CertifyError::Denied(refusal.to_owned()),
        other => other,
    }
}

/// Say `peer`'s session is one run of the agent `label` names: every request it makes from then on
/// is the agent's, for the device's person, and the person's agent door lists the run while the
/// session lasts.
///
/// # Errors
/// The session is already another agent's run, the session is closed, or the host did not answer.
pub async fn run_as_agent(peer: &PeerHandle, label: &str) -> Result<(), String> {
    let (send, receive) = tokio::sync::oneshot::channel();
    let done: AgentRunCompletion = Box::new(move |answer| {
        let _ = send.send(answer);
    });
    peer.run_as_agent_then(label, done)?;
    receive
        .await
        .map_err(|_| "the session ended before the host answered".to_owned())?
}

#[cfg(test)]
mod tests {
    use tilo_actions::remote::host_account::{Reason, Refusal};

    use super::*;

    fn too_many() -> Refusal {
        Refusal {
            reason: Reason::TooManyWorkspaces {
                limit: 1,
                existing: vec!["a".into()],
            },
            message: "You already have a workspace of your own here.".into(),
            next: Vec::new(),
        }
    }

    #[test]
    fn a_hosts_typed_refusal_reads_typed_with_its_own_words() {
        let refused = HostRefusal::of(&ActionHostError::Refused {
            reason: too_many().to_reason(),
        });
        assert_eq!(refused.code, "refused");
        assert_eq!(refused.message, too_many().message);
        assert_eq!(refused.refusal, Some(too_many()));
        // An operation that ran and failed with one reads the same way.
        let failed = HostRefusal::failed(&too_many().to_reason());
        assert_eq!(failed.code, HostRefusal::FAILED);
        assert_eq!(failed.refusal, Some(too_many()));
    }

    #[test]
    fn a_refusal_in_words_alone_keeps_them_and_every_other_answer_its_code() {
        let refused = HostRefusal::of(&ActionHostError::Refused {
            reason: "that workspace is not one of your own".into(),
        });
        assert_eq!(refused.message, "that workspace is not one of your own");
        assert_eq!(refused.refusal, None);
        let unknown = HostRefusal::of(&ActionHostError::UnknownAction);
        assert_eq!(unknown.code, "unknown_action");
        assert_eq!(unknown.message, "no such action is offered");
        assert_eq!(
            HostRefusal::of(&ActionHostError::StaleRevision).code,
            "stale_revision"
        );
    }

    #[test]
    fn a_computers_entry_reports_no_answer_as_a_request_and_a_refusal_as_a_denial() {
        assert!(matches!(
            CertifyError::from(HostRefusal::unanswered("the host did not answer")),
            CertifyError::Request(_)
        ));
        assert!(matches!(
            CertifyError::from(HostRefusal::of(&ActionHostError::NotPermitted)),
            CertifyError::Denied(_)
        ));
        let admitted = HostRefusal::from(CertifyError::Denied("not in".into()));
        assert_eq!(admitted.code, HostRefusal::NOT_ADMITTED);
    }
}
