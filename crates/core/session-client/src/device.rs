//! One device of a person in a workspace: the session every program keeps, and the one entry an
//! agent, `tilo mcp` or the API server takes it by.

use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::server_account::Refusal;
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget,
};
use tilo_lock::MutexRecover as _;
use tilo_server_client::login::Admission;
#[cfg(not(target_arch = "wasm32"))]
use tilo_server_client::login::{Entry, SignIn};
use tilo_server_client::{CertifyError, DeviceSigner, LinkError};
use tilo_server_protocol::workspaces::{
    CREATE_OWN_WORKSPACE, CreatedWorkspace, HOST_ACTIONS_PROVIDER, HOST_WORKSPACES_RESOURCE,
    LIST_PERSONAL, PersonalList, WorkspaceLabels, WorkspaceListing,
};
use tilo_user_identity::DeviceCert;
use web_time::Duration as Wait;

use crate::session::PeerConfig;
use crate::{Cmd, Link, Outbox, TransportPhase};
use crate::{MemoryPeerStore, PeerStore};
use tilo_server_protocol::WorkspaceId;
use tilo_session_actions::SessionActions;

/// How long joining a workspace waits for its host to admit this device.
const ADMITTING: Wait = Wait::from_secs(20);
/// How long a workspace's host has to answer an operation of its own.
const ANSWERING: u32 = 20_000;
/// How long an operation the server answered [`ActionCallError::Unanswered`] for waits
/// before it is asked about again, under the same invocation.
const UNANSWERED_PAUSE: Wait = Wait::from_millis(250);
/// How long an operation of the host's own that was still running past
/// [`ANSWERING`] is followed before it is given up on.
const FINISHING: Wait = Wait::from_secs(120);
/// How many reads of the person's list a device makes while the record is still arriving on the
/// host, each held until it moves.
const PERSON_LIST_READS: u32 = 4;
/// How long one held read of the person's list waits for the record to arrive, ms.
const PERSON_LIST_WAIT_MS: u32 = 10_000;

/// A device of a person, in the workspaces its session is in.
#[derive(Clone)]
pub struct Device {
    /// The session with the workspaces' host.
    peer: Link,
    cert: DeviceCert,
    /// The host's own device: whose device answers a workspace's own actions.
    server_cert: DeviceCert,
    /// The workspace the device entered.
    workspace_id: WorkspaceId,
    /// What this device calls itself, as its host's journals name it.
    label: String,
    /// Each workspace's host's signed address, as the host answered this device's entry into it
    /// ([`Admission::host`]): what this device's workspace service holds its member copy by.
    hosts: Arc<
        std::sync::Mutex<
            std::collections::BTreeMap<WorkspaceId, tilo_peer_state::join::TransportHint>,
        >,
    >,
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
        Self::kept_in(
            config,
            signer,
            label,
            Arc::new(MemoryPeerStore::for_session()),
        )
    }

    /// [`Self::over`] keeping the device's state in `store`: a session started again over the
    /// same store takes up the place in each workspace's key group the last one held, where one
    /// over a store of its own asks with a new key and takes the device's place from the session
    /// before it.
    #[must_use]
    pub fn kept_in(
        config: PeerConfig,
        signer: Arc<dyn DeviceSigner>,
        label: &str,
        store: Arc<dyn PeerStore>,
    ) -> Self {
        Self::of(
            crate::session::start_session(config.clone(), signer, store),
            config.cert,
            config.server_cert,
            label,
        )
    }

    /// [`Self::over`] for a device whose own session lends this one what it runs
    /// ([`crate::Lent`]).
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn over_lent(
        config: PeerConfig,
        signer: Arc<dyn DeviceSigner>,
        label: &str,
        lent: crate::Lent,
    ) -> Self {
        let store = Arc::new(MemoryPeerStore::for_session()) as Arc<dyn PeerStore>;
        Self::of(
            crate::session::start_session_lent(config.clone(), signer, store, lent),
            config.cert,
            config.server_cert,
            label,
        )
    }

    /// The device whose session `peer` already is: a page's, which started its session itself.
    /// `cert` must be the one `peer` was started with.
    #[must_use]
    pub fn of(peer: Link, cert: DeviceCert, server_cert: DeviceCert, label: &str) -> Self {
        Self {
            peer,
            cert,
            server_cert,
            workspace_id: WorkspaceId::from_u128(0),
            label: label.to_owned(),
            hosts: Arc::default(),
        }
    }

    /// A session for the device `sign_in` signed in. A computer's entry: a
    /// page writes login codes and never reads one, so it makes its device
    /// with [`Self::over`] and joins the workplaces its grants reach.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn connect(sign_in: &SignIn, signer: Arc<dyn DeviceSigner>) -> Self {
        Self::entering(sign_in, signer, None, None)
    }

    /// [`Self::connect`], its session entering a workspace through its entry as it opens.
    #[cfg(not(target_arch = "wasm32"))]
    fn entering(
        sign_in: &SignIn,
        signer: Arc<dyn DeviceSigner>,
        entering: Option<tilo_server_protocol::entering::Entering>,
        lent: Option<crate::Lent>,
    ) -> Self {
        let config = PeerConfig {
            host: sign_in.server.clone(),
            cert: sign_in.cert.clone(),
            server_cert: sign_in.server_cert.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
            entering,
        };
        match lent {
            Some(lent) => Self::over_lent(config, signer, &sign_in.label, lent),
            None => Self::over(config, signer, &sign_in.label),
        }
    }

    /// Take `sign_in`'s device into the workspace its link names ([`Self::enter_workspace`]). The
    /// site's `entry`, where it names one, lets the person in on the session as it opens: how a host
    /// lets in someone it does not know yet, before it makes them a workspace of their own.
    ///
    /// # Errors
    /// The workspace's id is not one, the host could not be asked, or the workspace refused.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn enter(
        entry: Option<&Entry>,
        sign_in: &SignIn,
        signer: Arc<dyn DeviceSigner>,
    ) -> Result<Self, CertifyError> {
        Self::enter_lent(entry, sign_in, signer, None).await
    }

    /// [`Self::enter`], its session running on what the device's own service `lent` it: the
    /// endpoint the device runs under its key and the key service its groups are settled by.
    ///
    /// # Errors
    /// As [`Self::enter`].
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn enter_lent(
        entry: Option<&Entry>,
        sign_in: &SignIn,
        signer: Arc<dyn DeviceSigner>,
        lent: Option<crate::Lent>,
    ) -> Result<Self, CertifyError> {
        let workspace = sign_in.link.workspace_id();
        WorkspaceId::parse(&workspace)
            .ok_or_else(|| CertifyError::Request("the link names no workspace".to_string()))?;
        let entering = entry.and_then(Entry::workspace).map(|workspace| {
            tilo_server_protocol::entering::Entering {
                workspace,
                // Whom this device carries is an agent (`sign_in.agent`), and the name a person
                // goes by is theirs, kept where they joined. A device never arrives claiming to
                // be their name: a person this workspace has not named yet is told so honestly.
                name: String::new(),
                request_id: rand::random(),
                through: tilo_server_protocol::entering::Through::Entry,
            }
        });
        let through_entry = entering.is_some();
        let device = Self::entering(sign_in, signer, entering, lent);
        // An entry that fails, or that its caller gives up on, ends the session it started: the
        // device's next try opens another under the same key, and a server keeps one connection
        // per key, so the two would cut each other off for as long as both dial.
        let unentered = Unentered(Some(device.peer.clone()));
        if through_entry {
            device.keep_host(&entered(&device.peer).await?);
        } else {
            device.connected().await?;
        }
        let entered = device
            // The person's name is theirs to give; the device's label never stands in for it.
            .enter_workspace(&workspace, "")
            .await
            .map_err(CertifyError::from)?;
        unentered.entered();
        let device = Self {
            workspace_id: entered,
            ..device
        };
        // The session is the agent's run: the person's MCP server lists it while it lasts.
        let _ = session_as_agent(&device.peer, &sign_in.agent).await;
        Ok(device)
    }

    /// The session itself.
    #[must_use]
    pub fn session(&self) -> &Link {
        &self.peer
    }

    /// This device's certificate.
    #[must_use]
    pub fn cert(&self) -> &DeviceCert {
        &self.cert
    }

    /// The workspace this device entered.
    #[must_use]
    pub fn workspace_id(&self) -> WorkspaceId {
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

    /// Whether the session still holds `workspace_id`: it was let in and has not been told since
    /// that it may be there no more (taken out, or a key its person revoked there).
    #[must_use]
    pub fn holds(&self, workspace_id: WorkspaceId) -> bool {
        self.peer.status().workspaces.contains_key(&workspace_id)
    }

    /// Join `workspace` (its UUID) and wait for its host to say this device is in: its id here.
    ///
    /// The join is the host's own entry, asked on this device's link: it admits the device and
    /// answers the membership it holds and the address its host holds the workspace at, which
    /// [`Self::hosts`] hands this device's workspace service so the workspace's records reach
    /// it. Only then does the session take part in the workspace's link, whose host admits the
    /// device the entry just let in.
    ///
    /// # Errors
    /// It is no workspace UUID, the session is closed, its host did not enter this device, or
    /// the device was not admitted to the workspace's link.
    pub async fn join(&self, workspace: &str) -> Result<WorkspaceId, CertifyError> {
        let id = WorkspaceId::parse(workspace)
            .ok_or_else(|| CertifyError::Request(format!("{workspace} names no workspace")))?;
        // A workspace this session is in already was entered, and its host's answer kept with
        // it: the link it takes part in is all that is left to join.
        if self.peer.status().workspaces.get(&id) != Some(&true) {
            let entering = tilo_server_protocol::entering::Entering {
                workspace: workspace.to_owned(),
                name: String::new(),
                request_id: rand::random(),
                through: tilo_server_protocol::entering::Through::Entry,
            };
            let entered = self.enter_once(entering).await?;
            let admission = admission(entered).map_err(|refusal| {
                CertifyError::Denied(format!(
                    "The workspace did not let this device in: {}",
                    refusal.reason()
                ))
            })?;
            self.keep_host(&admission);
        }
        self.peer
            .send(Cmd::Join(id))
            .map_err(|closed| CertifyError::Request(closed.to_string()))?;
        if !self.admitted(id).await {
            return Err(CertifyError::Denied(
                "The workspace did not admit this device.".to_owned(),
            ));
        }
        Ok(id)
    }

    /// Whether the session is in `workspace_id`, waiting for its host to say so.
    pub async fn admitted(&self, workspace_id: WorkspaceId) -> bool {
        self.peer
            .until(ADMITTING, |status| {
                if status.workspaces.get(&workspace_id) == Some(&true) {
                    return Some(true);
                }
                status
                    .failures
                    .iter()
                    .any(|failure| failure.code == "not-admitted")
                    .then_some(false)
            })
            .await
            .unwrap_or(false)
    }

    /// Redeem `invitation`, which admits this very device to a workspace, shown to its people as
    /// `name`: an entry on its own stream of this device's connection to the host
    /// ([`Cmd::Enter`]), so the session it holds stays as it is.
    ///
    /// # Errors
    /// The invitation names no workspace, the session is not connected, or the workspace refused
    /// the entry.
    pub async fn redeem(&self, invitation: &[u8], name: &str) -> Result<Admission, CertifyError> {
        let entering =
            invitation_entering(invitation, name, None).map_err(CertifyError::Request)?;
        // An answer the connection lost is asked for again under the same request, which the
        // host answers with what became of it: the invitation it spent on the first ask lets this
        // device in, rather than saying it was spent.
        let mut pause = REDEEM_RETRY_PAUSE;
        let mut tries = 1;
        let entered = loop {
            match self.enter_once(entering.clone()).await {
                Err(CertifyError::Transport(LinkError::Unavailable(why)))
                    if tries < REDEEM_TRIES =>
                {
                    tracing::debug!(%why, "the answer to a redeemed invitation was lost; asking again");
                    tries = tries.saturating_add(1);
                    n0_future::time::sleep(pause).await;
                    pause = pause.saturating_mul(2);
                }
                answered => break answered?,
            }
        };
        let admission = admission(entered).map_err(|refusal| {
            CertifyError::Denied(format!(
                "The workspace did not let this device in: {}",
                refusal.reason()
            ))
        })?;
        self.keep_host(&admission);
        Ok(admission)
    }

    /// Keep where `admission`'s host said its workspace is held, for [`Self::hosts`]. An address
    /// that does not read is the host's fault, and the workspace is not held on that word.
    fn keep_host(&self, admission: &Admission) {
        let Some(host) = &admission.host else {
            return;
        };
        match postcard::from_bytes::<tilo_peer_state::join::TransportHint>(host) {
            Ok(hint) => {
                self.hosts
                    .lock_recover()
                    .insert(admission.workspace_id, hint);
            }
            Err(error) => {
                tracing::warn!(workspace = %admission.workspace_id, %error, "the host's address for a workspace does not read; this device's service does not hold it");
            }
        }
    }

    /// Each workspace this device entered whose host said where it holds it, with that host's
    /// signed address: what this device's workspace service takes each in by, as a member.
    #[must_use]
    pub fn hosts(&self) -> Vec<(WorkspaceId, tilo_peer_state::join::TransportHint)> {
        self.hosts
            .lock_recover()
            .iter()
            .map(|(workspace, host)| (*workspace, host.clone()))
            .collect()
    }

    /// One ask of `entering` on this device's session, once it is connected: the host's answer.
    async fn enter_once(
        &self,
        entering: tilo_server_protocol::entering::Entering,
    ) -> Result<tilo_server_protocol::entering::Entered, CertifyError> {
        self.connected().await.map_err(CertifyError::from)?;
        let (done, receive) = crate::answer();
        self.peer
            .send(Cmd::Enter(entering, done))
            .map_err(|closed| CertifyError::Request(closed.into()))?;
        Ok(receive.await.map_err(|_| {
            CertifyError::Request("the session ended before the host answered".into())
        })??)
    }

    /// Have this device in `workspace` (its UUID) on its host, the one way every program enters
    /// one: a workspace of its person's own, or one they were invited into, which the host invites
    /// this device into and it redeems; or one the host admits it to already. `shown_as` is the
    /// name its members see. Its id on the session.
    ///
    /// # Errors
    /// The host refused (as it said) or did not answer, or the workspace did not admit this device.
    pub async fn enter_workspace(
        &self,
        workspace: &str,
        shown_as: &str,
    ) -> Result<WorkspaceId, DeviceRefusal> {
        let id = WorkspaceId::parse(workspace)
            .ok_or_else(|| DeviceRefusal::malformed(format!("{workspace} names no workspace")))?;
        if self.peer.status().workspaces.get(&id) == Some(&true) {
            return Ok(id);
        }
        let mut named = Map::new();
        named.insert("workspace".to_owned(), Value::String(workspace.to_owned()));
        let asked = match self
            .own_workspace(named, tilo_actions::InvocationId::new())
            .await
        {
            Ok((listing, invitation)) if listing.workspace == workspace => {
                self.redeem(&invitation, shown_as).await?;
                None
            }
            Ok(_) => Some(DeviceRefusal::malformed(
                "The host answered another workspace than the one asked for.".to_owned(),
            )),
            Err(refusal) => Some(refusal),
        };
        // A workspace the host invites no one into on its own may admit this device already.
        match self.join(workspace).await {
            Ok(id) => Ok(id),
            Err(error) => Err(asked.unwrap_or_else(|| error.into())),
        }
    }

    /// Wait until the session is connected: what asking its host anything takes.
    ///
    /// # Errors
    /// The host refused the session, or it did not connect in time.
    pub async fn connected(&self) -> Result<(), DeviceRefusal> {
        self.peer
            .until(ADMITTING, |status| match status.phase {
                TransportPhase::Connected => Some(Ok(())),
                TransportPhase::Failed | TransportPhase::Closed => {
                    Some(Err(Box::new(status.failures.last().map_or_else(
                        || DeviceRefusal::unanswered("the session with the host ended"),
                        |failure| DeviceRefusal::failed(&failure.message),
                    ))))
                }
                _ => None,
            })
            .await
            .unwrap_or_else(|| {
                Err(Box::new(DeviceRefusal::unanswered(
                    "the workspace's host did not take the session in time",
                )))
            })
            .map_err(|refusal| *refusal)
    }

    /// The person's own workspace on the host, and the invitation into it: its `name` makes
    /// another, or the `workspace` it must be. Asked under `invocation`: the same one answers the
    /// workspace an earlier ask made, so asking again after no answer makes no second workspace.
    ///
    /// # Errors
    /// The host did not answer, refused, or answered no invitation.
    pub async fn own_workspace(
        &self,
        arguments: Map<String, Value>,
        invocation: tilo_actions::InvocationId,
    ) -> Result<(WorkspaceListing, Vec<u8>), DeviceRefusal> {
        let output = self
            .server_action_as(CREATE_OWN_WORKSPACE, arguments, invocation)
            .await?;
        let malformed = |why: String| {
            DeviceRefusal::malformed(format!(
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

    /// Have this device in the person's own workspace on the host: the host makes it the first
    /// time, under an id it chooses, and answers the same one to every later ask (another for each
    /// `name` they give). This very device redeems the invitation into it, so the key this session
    /// holds is the one that opens it next. One the person did not name is named in `language`, the
    /// one they read. A workspace `entered` says this device is in already is not redeemed again.
    /// `shown_as` is the name its members see. `invocation` is the caller's own id for this make,
    /// kept across its asks (a page's dialog, asked again), or a fresh one.
    ///
    /// # Errors
    /// The host did not answer or refused (typed, as it said), or the workspace did not admit this
    /// device.
    pub async fn enter_own_workspace(
        &self,
        language: &str,
        name: Option<&str>,
        shown_as: &str,
        entered: impl FnOnce(&str) -> bool,
        invocation: Option<tilo_actions::InvocationId>,
    ) -> Result<WorkspaceListing, DeviceRefusal> {
        let mut arguments = Map::new();
        if let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) {
            arguments.insert("name".to_owned(), Value::String(name.to_owned()));
        }
        // The host names a workspace the person did not, so it is told the language they read.
        arguments.insert("language".to_owned(), Value::String(language.to_owned()));
        let (listing, invitation) = self
            .own_workspace(arguments, invocation.unwrap_or_default())
            .await?;
        if !entered(&listing.workspace) {
            self.redeem(&invitation, shown_as)
                .await
                .map_err(DeviceRefusal::from)?;
        }
        Ok(listing)
    }

    /// The person's list of their workspaces, read from the record of their own workspace through
    /// the record API ([`IDENTITY_LIST`]) on the device holding it, decided there as every read
    /// is: each workspace by its id and the name they last saw it by, in their order. A device
    /// reads it from the moment it entered their workspace with its ticket, while its own copy
    /// still fills in behind; a record still arriving there is waited for.
    ///
    /// # Errors
    /// The host did not answer or refused, or the list did not arrive on it in time.
    ///
    /// [`IDENTITY_LIST`]: tilo_server_protocol::workspaces::IDENTITY_LIST
    pub async fn person_workspaces(
        &self,
    ) -> Result<tilo_server_protocol::workspaces::PersonWorkspaces, DeviceRefusal> {
        use tilo_server_protocol::workspaces::{IDENTITY_LIST, PersonWorkspaces};
        let person = self.cert.user_public_key;
        let actions = self.actions(WorkspaceId::from(collab_types::person_workspace(&person)));
        let mut after: Option<String> = None;
        for _ in 0..PERSON_LIST_READS {
            let read = tilo_actions::remote::ResourceReadRequest {
                workspace: tilo_server_protocol::servers::person_workspace_id(&person),
                app: IDENTITY_LIST.0.to_owned(),
                record: IDENTITY_LIST.1.to_owned(),
                joined: false,
                wait_ms: after.as_ref().map(|_| PERSON_LIST_WAIT_MS),
                document: true,
                listing: false,
                after: after.clone(),
            };
            let answered = actions
                .read_record(read)
                .await
                .unwrap_or(Err(ActionCallError::Unanswered))
                .map_err(|error| DeviceRefusal::of(&error))?;
            let readings: Value = serde_json::from_str(&answered)
                .map_err(|error| DeviceRefusal::malformed(error.to_string()))?;
            if let Some(list) =
                PersonWorkspaces::of_readings(&readings).map_err(DeviceRefusal::malformed)?
            {
                return Ok(list);
            }
            after = PersonWorkspaces::revision_of(&readings);
        }
        Err(DeviceRefusal::unanswered(
            "your list of workspaces has not reached the host yet",
        ))
    }

    /// The person's workspace list as the host keeps it: every
    /// workspace of theirs their devices said, wherever it is hosted, with the workspaces this host
    /// keeps for them, each naming its host. A key for one workspace reads that one alone. With
    /// `labels`, only the workspaces labelled with every one of them, and with `workspace` only
    /// that one: the host reads no other.
    ///
    /// # Errors
    /// The host did not answer, refused, or answered something else.
    pub async fn personal_list(
        &self,
        labels: &WorkspaceLabels,
        workspace: Option<&str>,
    ) -> Result<PersonalList, DeviceRefusal> {
        self.personal_list_in(self.workspace_id, labels, workspace)
            .await
    }

    /// [`Self::personal_list`], asked in `entered`, a workspace this device is in on the host
    /// ([`crate::session::HOST_ITSELF`] for none): how a device that took a workspace in on this
    /// session asks it there.
    ///
    /// # Errors
    /// The host did not answer, refused, or answered something else.
    pub async fn personal_list_in(
        &self,
        entered: WorkspaceId,
        labels: &WorkspaceLabels,
        workspace: Option<&str>,
    ) -> Result<PersonalList, DeviceRefusal> {
        let mut arguments = Map::new();
        if let Some(workspace) = workspace {
            arguments.insert("workspace".to_owned(), serde_json::Value::from(workspace));
        }
        if !labels.is_empty() {
            arguments.insert(
                "labels".to_owned(),
                serde_json::to_value(labels)
                    .map_err(|error| DeviceRefusal::malformed(error.to_string()))?,
            );
        }
        let target = RemoteActionTarget {
            provider: HOST_ACTIONS_PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(HOST_WORKSPACES_RESOURCE, LIST_PERSONAL),
        };
        let listed = self.ask_host(entered, target, arguments).await?;
        serde_json::from_value(listed).map_err(|error| {
            DeviceRefusal::malformed(format!("The host answered no workspace list: {error}"))
        })
    }

    /// The server's own operation `action` with `arguments`, asked in the workspace this
    /// device entered ([`crate::session::HOST_ITSELF`] before it entered one): no
    /// workspace decides it, but an agent the session was handed to asks as its grants
    /// there let it.
    ///
    /// # Errors
    /// The server did not answer, refused, or did not finish.
    pub async fn host_action(
        &self,
        action: &str,
        arguments: Map<String, Value>,
    ) -> Result<Value, DeviceRefusal> {
        self.server_action_as(action, arguments, tilo_actions::InvocationId::new())
            .await
    }

    /// [`Self::host_action`], asked under `invocation` when the caller has one of its own: the
    /// same invocation answers what the server did for it before, so a caller that asks again
    /// after no answer (a person pressing the same button once more) is answered the workspace
    /// the first ask made, never a second one.
    ///
    /// # Errors
    /// The server did not answer, refused, or did not finish.
    pub async fn server_action_as(
        &self,
        action: &str,
        arguments: Map<String, Value>,
        invocation: tilo_actions::InvocationId,
    ) -> Result<Value, DeviceRefusal> {
        let target = RemoteActionTarget {
            provider: HOST_ACTIONS_PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(HOST_WORKSPACES_RESOURCE, action),
        };
        self.ask_server_as(self.workspace_id, target, arguments, invocation)
            .await
    }

    /// [`Self::ask_server_as`] under a new invocation of this device's own.
    ///
    /// # Errors
    /// The server did not answer, refused, or did not finish.
    pub async fn ask_host(
        &self,
        workspace_id: WorkspaceId,
        target: RemoteActionTarget,
        arguments: Map<String, Value>,
    ) -> Result<Value, DeviceRefusal> {
        self.ask_server_as(
            workspace_id,
            target,
            arguments,
            tilo_actions::InvocationId::new(),
        )
        .await
    }

    /// What the server itself answers to `target` with `arguments`, asked in
    /// `workspace_id` ([`crate::session::HOST_ITSELF`] for none): one of its own operations,
    /// or the person's account on it, asked as the person whose device this is.
    ///
    /// # Errors
    /// The server did not answer, refused, or did not finish.
    pub async fn ask_server_as(
        &self,
        workspace_id: WorkspaceId,
        target: RemoteActionTarget,
        arguments: Map<String, Value>,
        invocation: tilo_actions::InvocationId,
    ) -> Result<Value, DeviceRefusal> {
        let actions = self.actions(workspace_id);
        let caller = self.caller();
        let receipt = receipt_until_answered(
            &actions,
            &caller,
            InvokeRequest {
                on_behalf_of: None,
                invocation,
                target,
                argument: InvokeArgument::Json(arguments),
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: Some(ANSWERING),
                appearance: None,
                signed: None,
            },
        )
        .await?;
        let unfinished =
            || DeviceRefusal::unanswered("the server did not finish what was asked of it");
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
            ReceiptStatus::Failed { error } => Err(DeviceRefusal::failed(&error.message)),
            ReceiptStatus::Rejected { error } => Err(DeviceRefusal::of(&error)),
            ReceiptStatus::Cancelled { .. } => Err(unfinished()),
            // Still going when the wait for its answer ran out (a server
            // making hundreds of workspaces at once): followed until it ends.
            ReceiptStatus::Accepted => {
                let now = tilo_clock::now();
                let until = now.checked_add(FINISHING).unwrap_or(now);
                let settled = tilo_actions::remote::settle_with(
                    &actions,
                    &caller,
                    receipt.invocation,
                    None,
                    until,
                )
                .await
                .flatten()
                .ok_or_else(unfinished)?;
                match settled.update {
                    InvocationUpdate::Succeeded { output, .. } => {
                        Ok(output.value.unwrap_or_default())
                    }
                    InvocationUpdate::Failed { error, .. } => {
                        Err(DeviceRefusal::failed(&error.message))
                    }
                    _ => Err(unfinished()),
                }
            }
        }
    }

    /// Who this session asks as: the person whose device it is.
    #[must_use]
    pub fn caller(&self) -> Caller {
        Caller::local_person(tilo_actions::policy::PersonId::from_root(
            self.cert.user_public_key,
        ))
    }

    /// This device's actions in `workspace_id`, asked of the workspace's host.
    #[must_use]
    pub fn actions(&self, workspace_id: WorkspaceId) -> SessionActions {
        SessionActions::new(&self.peer, workspace_id, self.server_cert.device_public_key)
    }

    /// Close the session: it says this device left, and stops.
    pub fn close(&self) {
        self.peer.close();
    }
}

/// The server's receipt for `request`, asked again under the same invocation while it answers
/// [`ActionCallError::Unanswered`]: that answer does not say nothing ran, and the same invocation
/// is answered by what it did (or does it once if it never arrived). So an operation the server
/// was still busy with when one answer's wait ran out is waited for, up to the span a running one
/// is followed ([`FINISHING`]), rather than reported as failed while it carries on.
async fn receipt_until_answered(
    actions: &SessionActions,
    caller: &Caller,
    request: InvokeRequest,
) -> Result<tilo_actions::remote::InvocationReceipt, DeviceRefusal> {
    let now = tilo_clock::now();
    let until = now.checked_add(FINISHING).unwrap_or(now);
    loop {
        let answered = actions
            .invoke(caller, request.clone())
            .await
            .ok_or_else(|| DeviceRefusal::unanswered("the server did not answer"))?;
        match answered {
            Ok(receipt) => return Ok(receipt),
            Err(ActionCallError::Unanswered) if tilo_clock::now() < until => {
                n0_future::time::sleep(UNANSWERED_PAUSE).await;
            }
            Err(error) => return Err(DeviceRefusal::of(&error)),
        }
    }
}

/// The session of an entry still under way, closed when the entry ends without entering.
#[cfg(not(target_arch = "wasm32"))]
struct Unentered(Option<Link>);

#[cfg(not(target_arch = "wasm32"))]
impl Unentered {
    /// The entry entered: the session is the device's from here on.
    fn entered(mut self) {
        self.0 = None;
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for Unentered {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            session.close();
        }
    }
}

/// How long before it ends a certificate a device holds is certified anew rather than entered with.
const RENEWING_NS: u64 = 24 * 60 * 60 * 1_000_000_000;

/// A certificate a device holds from an earlier entry, and the host's own certificate as it was
/// then: what the page keeps, as `{cert, hostCert}`.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct Held {
    pub cert: DeviceCert,
    #[serde(rename = "hostCert")]
    pub server_cert: Option<DeviceCert>,
}

/// Whether `kept` and `current` are one host's certificates: the same person's same device.
#[must_use]
pub fn same_host(kept: &DeviceCert, current: &DeviceCert) -> bool {
    kept.user_public_key == current.user_public_key
        && kept.device_public_key == current.device_public_key
}

/// Whether `cert` lasts past the next day from `now_ns`, so it is not certified anew yet.
fn lasting(cert: &DeviceCert, now_ns: u64) -> bool {
    cert.expires_at_ns
        .is_none_or(|end| end > now_ns.saturating_add(RENEWING_NS))
}

/// Which certificate a device's session starts with, as it comes back to a host it was in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reconnecting {
    /// The one the host just admitted it under (an invitation, its entry, a pairing): kept from now
    /// on.
    Admitted,
    /// As [`Self::Admitted`], after moving what the device kept for the host before it (set up
    /// again since) aside, never handing it to this one.
    AdoptHost,
    /// The one it kept.
    Kept,
    /// One certified now: by the person's root where the device holds it, else by pairing.
    Certify,
    /// None: another host answers where the kept one did, and nothing admitted this device to it.
    /// What was kept stays for the host it belongs to.
    HostChanged,
}

/// How a device comes back to the host whose own certificate is `host`: with what it `kept`, what
/// an entry just `admitted` it under, or one certified now. A certificate a root renews (`with_root`)
/// is certified anew within a day of its end; one certified by pairing serves until the host
/// refuses it.
#[must_use]
pub fn reconnecting(
    kept: Option<&Held>,
    admitted: Option<&Held>,
    host: &DeviceCert,
    with_root: bool,
    now_ns: u64,
) -> Reconnecting {
    let answering = admitted
        .and_then(|admitted| admitted.server_cert.as_ref())
        .unwrap_or(host);
    let kept_elsewhere = kept
        .and_then(|kept| kept.server_cert.as_ref())
        .is_some_and(|kept| !same_host(kept, answering));
    let kept_other_device = match (kept, admitted) {
        (Some(kept), Some(admitted)) => !same_host(&kept.cert, &admitted.cert),
        _ => false,
    };
    match (kept, admitted) {
        (_, Some(_)) if kept_elsewhere || kept_other_device => Reconnecting::AdoptHost,
        (_, Some(_)) => Reconnecting::Admitted,
        (Some(_), None) if kept_elsewhere => Reconnecting::HostChanged,
        (Some(kept), None) if !with_root || lasting(&kept.cert, now_ns) => Reconnecting::Kept,
        _ => Reconnecting::Certify,
    }
}

/// The certificate `device` enters a workspace of the host `host` with: the one it `held` from that
/// same host where it still serves (it certifies this very key, as `root`'s person when a root is
/// at hand, and does not end within a day), else one `root` certifies now, labeled `label`. A
/// workspace keeps the certificate of every device it admitted, also once its person left, and
/// admits that device again only under the same one: so a device that holds one enters with it,
/// and never with another device's.
///
/// # Errors
/// Nothing held serves and no root is at hand ([`DeviceRefusal::NO_CERTIFICATE`]), or the root did
/// not sign.
pub async fn entering_certificate(
    held: Option<&Held>,
    host: &DeviceCert,
    device: &dyn DeviceSigner,
    root: Option<&dyn DeviceSigner>,
    label: &str,
    now_ns: u64,
) -> Result<DeviceCert, DeviceRefusal> {
    let serves = |held: &&Held| {
        let same_host = held
            .server_cert
            .as_ref()
            .is_some_and(|kept| same_host(kept, host));
        let person = root.is_none_or(|root| held.cert.user_public_key == root.device_public_key());
        held.cert.device_public_key == device.device_public_key()
            && same_host
            && person
            && lasting(&held.cert, now_ns)
    };
    if let Some(held) = held.filter(serves) {
        return Ok(held.cert.clone());
    }
    let Some(root) = root else {
        return Err(DeviceRefusal {
            code: DeviceRefusal::NO_CERTIFICATE.to_owned(),
            message: "This device holds no certificate of this host's for its key, and no root \
                      to certify it with."
                .to_owned(),
            refusal: None,
            retry_after_ms: None,
            entry: None,
        });
    };
    tilo_server_client::invite::guest_certificate(root, device.device_public_key(), label, now_ns)
        .await
        .map_err(|error| DeviceRefusal {
            code: DeviceRefusal::NO_CERTIFICATE.to_owned(),
            message: error.to_string(),
            refusal: None,
            retry_after_ms: None,
            entry: None,
        })
}

/// A device entering a workspace with an invitation: `held` and `root` choose the certificate
/// ([`entering_certificate`]), `label` names a certificate made now, `name` is how the
/// workspace's people see the person, and `token` is the one-time token of the pre-admission the
/// invitation was issued under, if any.
pub struct Entering<'a> {
    pub held: Option<&'a Held>,
    pub root: Option<&'a dyn DeviceSigner>,
    pub label: &'a str,
    pub name: &'a str,
    pub token: Option<&'a [u8]>,
}

/// What `device` enters the workspace `invitation` admits into with, on the host whose
/// certificate is `host`: the certificate [`entering_certificate`] chooses and the entry its
/// session asks for as it opens ([`PeerConfig::entering`]), carrying the invitation and
/// `entering.token`. The workspace admits the device on that session's handshake, at once or not
/// at all (an invitation is the approval of whoever made it); [`entered`] waits for the host's
/// answer.
///
/// # Errors
/// The invitation names no workspace, or the device holds no certificate it could enter with.
pub async fn invitation_entry(
    host: &DeviceCert,
    invitation: &[u8],
    device: &dyn DeviceSigner,
    entering: Entering<'_>,
) -> Result<(DeviceCert, tilo_server_protocol::entering::Entering), DeviceRefusal> {
    let entry =
        invitation_entering(invitation, entering.name, entering.token).map_err(|message| {
            DeviceRefusal {
                code: DeviceRefusal::NOT_ADMITTED.to_owned(),
                message,
                refusal: None,
                retry_after_ms: None,
                entry: None,
            }
        })?;
    let cert = entering_certificate(
        entering.held,
        host,
        device,
        entering.root,
        entering.label,
        tilo_clock::unix_now_ns(),
    )
    .await?;
    Ok((cert, entry))
}

/// The entry an invitation makes, shown to the workspace's people as `name`, with the
/// pre-admission's `token` where it was issued under one; or why the invitation names no
/// workspace.
fn invitation_entering(
    invitation: &[u8],
    name: &str,
    token: Option<&[u8]>,
) -> Result<tilo_server_protocol::entering::Entering, String> {
    let workspace =
        tilo_peer_state::join_log::invitation_workspace(invitation, tilo_clock::unix_now_ns())
            .map_err(|error| format!("This invitation cannot be used: {error}"))?;
    Ok(tilo_server_protocol::entering::Entering {
        workspace: workspace.to_string(),
        name: tilo_server_protocol::web::kept(
            name,
            tilo_server_protocol::web::MAX_DISPLAY_NAME_CHARS,
        ),
        request_id: rand::random(),
        through: tilo_server_protocol::entering::Through::Invitation {
            invitation: invitation.to_vec(),
            token: token.map(<[u8]>::to_vec),
        },
    })
}

/// How often an invitation is asked for again when the connection lost the host's answer, and
/// the first pause before asking: a session that reconnects does so within seconds.
const REDEEM_TRIES: u32 = 4;
const REDEEM_RETRY_PAUSE: std::time::Duration = std::time::Duration::from_secs(1);

/// What a workspace said as it admitted a device, or why it did not.
fn admission(
    entered: tilo_server_protocol::entering::Entered,
) -> Result<Admission, tilo_server_protocol::entering::EnterRefusal> {
    match entered {
        tilo_server_protocol::entering::Entered::Admitted {
            workspace,
            workspace_id,
            name,
            role,
            host,
        } => Ok(Admission {
            workspace,
            workspace_id,
            name,
            role,
            host,
        }),
        tilo_server_protocol::entering::Entered::Refused(refusal) => Err(refusal),
    }
}

/// What `device` enters `workspace` through its entry with, on the host whose certificate is
/// `host`: the certificate [`entering_certificate`] chooses (`entering.token` is not read: the
/// entry hands out its own) and the entry its session asks for as it opens
/// ([`PeerConfig::entering`]). The entry, the workspace's challenge and the admission all happen on
/// that session's handshake; [`entered`] waits for the host's answer.
///
/// # Errors
/// The device holds no certificate it could enter with.
pub async fn entry_request(
    host: &DeviceCert,
    workspace: &str,
    device: &dyn DeviceSigner,
    entering: Entering<'_>,
) -> Result<(DeviceCert, tilo_server_protocol::entering::Entering), DeviceRefusal> {
    let cert = entering_certificate(
        entering.held,
        host,
        device,
        entering.root,
        entering.label,
        tilo_clock::unix_now_ns(),
    )
    .await?;
    let entry = tilo_server_protocol::entering::Entering {
        workspace: workspace.to_owned(),
        name: tilo_server_protocol::web::kept(
            entering.name,
            tilo_server_protocol::web::MAX_DISPLAY_NAME_CHARS,
        ),
        request_id: rand::random(),
        through: tilo_server_protocol::entering::Through::Entry,
    };
    Ok((cert, entry))
}

/// Wait for the host's answer to the entry `peer`'s session asked for ([`entry_request`]).
///
/// # Errors
/// The entry or the workspace refused (the entry's own code: `banned`, `removed`,
/// `invitation-only`, `full` with the wait it asked for, `refused`), or no answer came in time.
pub async fn entered(peer: &Link) -> Result<Admission, DeviceRefusal> {
    let waits = ADMITTING.max(tilo_server_protocol::entering::ENTERING_DEADLINE);
    peer.until(waits, |status| {
        match &status.entry {
            Some(crate::Entry::Admitted {
                workspace,
                workspace_id,
                name,
                role,
                host,
            }) => {
                return Some(Ok(Admission {
                    workspace: workspace.clone(),
                    workspace_id: *workspace_id,
                    name: name.clone(),
                    role: *role,
                    host: host.clone(),
                }));
            }
            Some(crate::Entry::Refused {
                code,
                message,
                retry_after_secs,
                entry,
            }) => {
                return Some(Err(Box::new(DeviceRefusal {
                    code: (*code).to_owned(),
                    message: message.clone(),
                    refusal: None,
                    retry_after_ms: retry_after_secs.map(|secs| secs.saturating_mul(1000)),
                    entry: entry.clone(),
                })));
            }
            None => {}
        }
        status
            .failures
            .iter()
            .find(|_| status.phase == TransportPhase::Failed)
            .map(|failure| Err(Box::new(DeviceRefusal::failed(&failure.message))))
    })
    .await
    .unwrap_or_else(|| {
        Err(Box::new(DeviceRefusal::unanswered(
            "the workspace's host did not answer the entry in time",
        )))
    })
    .map_err(|refusal| *refusal)
}

/// Why a workspace's host did not do what a device asked of it, as the host said it: what every
/// caller (the CLI, the API server, the page) reads and shows, never a sentence to parse.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct DeviceRefusal {
    /// What kind of answer it was: an [`ActionCallError::code`] when the host refused,
    /// [`DeviceRefusal::FAILED`] when the operation ran and failed, [`DeviceRefusal::UNANSWERED`] when
    /// no answer came, [`DeviceRefusal::MALFORMED`] when the answer was not what the operation
    /// answers, and [`DeviceRefusal::NOT_ADMITTED`] when a workspace did not let the device in.
    pub code: String,
    /// What happened, in plain words for whoever asked.
    pub message: String,
    /// The host's typed refusal, when it gave one: its reason and the ways through it.
    pub refusal: Option<Refusal>,
    /// How long the host asks to wait before the next try, in milliseconds, when it asked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    /// The entry's own refusal, when a workspace's entry turned the device away: what it says
    /// about a wait list the workspace keeps, which its page offers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<tilo_server_protocol::directory::EntryRefused>,
}

impl DeviceRefusal {
    /// The operation ran on the host and failed.
    pub const FAILED: &'static str = "failed";
    /// The host did not answer, or did not finish.
    pub const UNANSWERED: &'static str = "unanswered";
    /// The host answered something the operation does not answer.
    pub const MALFORMED: &'static str = "malformed";
    /// The workspace did not admit the device.
    pub const NOT_ADMITTED: &'static str = "not_admitted";
    /// The device holds no certificate it could enter with, and no root to certify it.
    pub const NO_CERTIFICATE: &'static str = "no_certificate";
    /// The operation waits for the person to approve it on a device that holds their identity,
    /// and goes on once they allow it.
    pub const AWAITING_APPROVAL: &'static str = tilo_actions::remote::AWAITING_APPROVAL;
    /// The server does not know the person yet: its operator declares them an owner, or an
    /// invitation brings them in.
    pub const NOT_ON_THIS_SERVER: &'static str = tilo_actions::remote::NOT_ON_THIS_SERVER;

    /// The host's refusal `error`, its typed refusal read off it where it carries one.
    #[must_use]
    pub fn of(error: &ActionCallError) -> Self {
        match error {
            // The server put it to the person, who has not answered yet: it goes on once they
            // allow it.
            ActionCallError::Refused { reason }
                if reason.starts_with(tilo_actions::remote::AWAITING_APPROVAL) =>
            {
                Self::awaiting_approval()
            }
            ActionCallError::Refused { reason } => {
                match reason
                    .strip_prefix(tilo_actions::remote::NOT_ON_THIS_SERVER)
                    .and_then(|said| said.strip_prefix(": "))
                {
                    Some(said) => Self {
                        code: Self::NOT_ON_THIS_SERVER.to_owned(),
                        message: said.to_owned(),
                        refusal: None,
                        retry_after_ms: None,
                        entry: None,
                    },
                    None => Self::said(error.code(), reason),
                }
            }
            other => Self::plain(other),
        }
    }

    fn plain(error: &ActionCallError) -> Self {
        Self {
            code: error.code().to_owned(),
            message: error.to_string(),
            refusal: None,
            retry_after_ms: None,
            entry: None,
        }
    }

    /// The operation failed, saying `message` (a typed refusal's text, where it is one).
    #[must_use]
    pub fn failed(message: &str) -> Self {
        Self::said(Self::FAILED, message)
    }

    /// The operation waits for its person's approval ([`Self::AWAITING_APPROVAL`]).
    #[must_use]
    pub fn awaiting_approval() -> Self {
        Self {
            code: Self::AWAITING_APPROVAL.to_owned(),
            message: "Waiting for your approval: Tilo asks you on the devices that hold your \
                      identity (your Tilo tab). It is done once you allow it there."
                .to_owned(),
            refusal: None,
            retry_after_ms: None,
            entry: None,
        }
    }

    /// No answer came: `why`.
    #[must_use]
    pub fn unanswered(why: &str) -> Self {
        Self {
            code: Self::UNANSWERED.to_owned(),
            message: why.to_owned(),
            refusal: None,
            retry_after_ms: None,
            entry: None,
        }
    }

    /// The answer was not the operation's: `why`.
    #[must_use]
    pub fn malformed(why: String) -> Self {
        Self {
            code: Self::MALFORMED.to_owned(),
            message: why,
            refusal: None,
            retry_after_ms: None,
            entry: None,
        }
    }

    /// `reason` as the host said it, typed where it is a [`Refusal`]'s text, or the refusal of
    /// the site's entry when the host turned away a person it never let in.
    fn said(code: &str, reason: &str) -> Self {
        if let Some(entry) = tilo_server_protocol::directory::EntryRefused::from_reason(reason) {
            return Self {
                code: Self::NOT_ADMITTED.to_owned(),
                message: entry.error.clone(),
                refusal: None,
                retry_after_ms: None,
                entry: Some(entry),
            };
        }
        let refusal = Refusal::from_reason(reason);
        Self {
            code: code.to_owned(),
            message: refusal
                .as_ref()
                .map_or_else(|| reason.to_owned(), |refusal| refusal.message.clone()),
            refusal,
            retry_after_ms: None,
            entry: None,
        }
    }
}

impl std::fmt::Display for DeviceRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DeviceRefusal {}

/// A device's entry failed: a workspace that did not admit it was refused; anything else was not
/// answered.
impl From<CertifyError> for DeviceRefusal {
    fn from(error: CertifyError) -> Self {
        match error {
            CertifyError::Denied(why) => Self {
                code: Self::NOT_ADMITTED.to_owned(),
                message: why,
                refusal: None,
                retry_after_ms: None,
                entry: None,
            },
            other => Self::unanswered(&other.to_string()),
        }
    }
}

/// What a computer's entry reports: a refusal is the host's denial, no answer a failed request.
impl From<DeviceRefusal> for CertifyError {
    fn from(refusal: DeviceRefusal) -> Self {
        if refusal.code == DeviceRefusal::UNANSWERED {
            Self::Request(refusal.message)
        } else {
            Self::Denied(refusal.message)
        }
    }
}

/// Say `peer`'s session is one run of the agent `label` names: every request it makes from then on
/// is the agent's, for the device's person, and the person's MCP server lists the run while the
/// session lasts.
///
/// # Errors
/// The session is already another agent's run, the session is closed, or the host did not answer.
pub async fn session_as_agent(peer: &Link, label: &str) -> Result<(), String> {
    let label: String = label
        .chars()
        .filter(|character| !character.is_control())
        .take(tilo_server_protocol::transport::MAX_AGENT_LABEL_CHARS)
        .collect();
    let (done, receive) = crate::answer();
    peer.send(Cmd::AgentSession(label, done))?;
    receive
        .await
        .map_err(|_| "the session ended before the host answered".to_owned())?
}

#[cfg(test)]
mod tests {
    use tilo_actions::remote::server_account::{Reason, Refusal};

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

    tilo_test_env::scenario! {
        fn a_hosts_refusal_reads_with_its_code_and_words;

        phase a_hosts_typed_refusal_reads_typed_with_its_own_words;
        {
            let refused = DeviceRefusal::of(&ActionCallError::Refused {
                reason: too_many().to_reason(),
            });
            assert_eq!(refused.code, "refused");
            assert_eq!(refused.message, too_many().message);
            assert_eq!(refused.refusal, Some(too_many()));
            // An operation that ran and failed with one reads the same way.
            let failed = DeviceRefusal::failed(&too_many().to_reason());
            assert_eq!(failed.code, DeviceRefusal::FAILED);
            assert_eq!(failed.refusal, Some(too_many()));
        }

        phase any_other_unavailability_stays_as_it_was;
        {
            let offline = DeviceRefusal::of(&ActionCallError::Unavailable {
                reason: "offline: the device left".into(),
            });
            assert_eq!(offline.code, "remote_actions_unavailable");
            assert_eq!(offline.retry_after_ms, None);
        }

        phase every_refusal_reads_with_the_code_and_words_it_travels_with;
        {
            // Each refusal as it travels, with its code and words (tilo-actions holds the file to the
            // type): what the CLI, the API server and the page read off it.
            let file: Vec<Value> = serde_json::from_str(include_str!(
                "../../actions/tests/provider_refusals.json"
            ))
            .unwrap();
            for entry in file {
                let wire: ActionCallError = serde_json::from_value(entry["wire"].clone()).unwrap();
                let read = DeviceRefusal::of(&wire);
                assert_eq!(read.code, entry["code"], "{entry}");
                match (&wire, &read.refusal) {
                    (_, Some(typed)) => assert_eq!(read.message, typed.message),
                    (ActionCallError::Refused { reason }, None) => assert_eq!(&read.message, reason),
                    (_, None) => assert_eq!(read.message, entry["display"], "{entry}"),
                }
            }
        }

        phase a_refusal_in_words_alone_keeps_them_and_every_other_answer_its_code;
        {
            let refused = DeviceRefusal::of(&ActionCallError::Refused {
                reason: "that workspace is not one of your own".into(),
            });
            assert_eq!(refused.message, "that workspace is not one of your own");
            assert_eq!(refused.refusal, None);
            let unknown = DeviceRefusal::of(&ActionCallError::UnknownAction);
            assert_eq!(unknown.code, "unknown_action");
            assert_eq!(unknown.message, "no such action is offered");
            assert_eq!(
                DeviceRefusal::of(&ActionCallError::StaleRevision).code,
                "stale_revision"
            );
        }

        phase a_computers_entry_reports_no_answer_as_a_request_and_a_refusal_as_a_denial;
        {
            assert!(matches!(
                CertifyError::from(DeviceRefusal::unanswered("the host did not answer")),
                CertifyError::Request(_)
            ));
            assert!(matches!(
                CertifyError::from(DeviceRefusal::of(&ActionCallError::NotPermitted)),
                CertifyError::Denied(_)
            ));
            let admitted = DeviceRefusal::from(CertifyError::Denied("not in".into()));
            assert_eq!(admitted.code, DeviceRefusal::NOT_ADMITTED);
        }

        phase a_host_refusing_a_person_never_let_in_answers_the_entry_refusal;
        // A host turning away a person its site never let in says it as the site's entry does: the
        // refusal reads as not admitted, with the entry's wait list for whoever shows it.
        {
            use tilo_server_protocol::directory::{EntryRefusalCode, EntryRefused};
            let entry = EntryRefused {
                code: EntryRefusalCode::InvitationOnly,
                error: "tilo.test takes people by invitation".into(),
                wait_list: true,
                contact: Some("hello@tilo.test".into()),
                mail: true,
            };
            let refusal = DeviceRefusal::of(&ActionCallError::Refused {
                reason: entry.to_reason(),
            });
            assert_eq!(refusal.code, DeviceRefusal::NOT_ADMITTED);
            assert_eq!(refusal.message, entry.error);
            assert_eq!(refusal.entry.as_ref(), Some(&entry));
            assert!(matches!(
                CertifyError::from(refusal),
                CertifyError::Denied(why) if why == entry.error
            ));
            // A host account's typed refusal is not the entry's.
            let plain = DeviceRefusal::of(&ActionCallError::Refused {
                reason: "no such workspace".into(),
            });
            assert_eq!(plain.entry, None);
        }
    }

    fn cert(device: u8, expires_at_ns: Option<u64>) -> DeviceCert {
        DeviceCert {
            user_public_key: [1; 32],
            device_public_key: [device; 32],
            device_label: "device".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns,
            signature: [0; 64],
            issuer: None,
        }
    }

    fn held(device: u8, host: u8, expires_at_ns: Option<u64>) -> Held {
        Held {
            cert: cert(device, expires_at_ns),
            server_cert: Some(cert(host, None)),
        }
    }

    tilo_test_env::scenario! {
        fn a_returning_device_keeps_or_renews_its_certificate;

        phase a_device_comes_back_with_what_it_kept_from_that_host_alone;
        {
            let host = cert(9, None);
            let day = RENEWING_NS;
            let now = 10 * day;
            let kept = held(2, 9, None);
            assert_eq!(
                reconnecting(Some(&kept), None, &host, true, now),
                Reconnecting::Kept
            );
            assert_eq!(
                reconnecting(None, None, &host, true, now),
                Reconnecting::Certify
            );
            // Another host answers where the kept one did: nothing kept is handed to it.
            let elsewhere = held(2, 8, None);
            assert_eq!(
                reconnecting(Some(&elsewhere), None, &host, true, now),
                Reconnecting::HostChanged
            );
            // Unless that host just admitted this device: what was kept moves aside.
            let admitted = held(2, 9, None);
            assert_eq!(
                reconnecting(Some(&elsewhere), Some(&admitted), &host, true, now),
                Reconnecting::AdoptHost
            );
            assert_eq!(
                reconnecting(Some(&kept), Some(&admitted), &host, true, now),
                Reconnecting::Admitted
            );
            // Same server, another device identity: move the previous device's state aside.
            let other_device = held(3, 9, None);
            assert_eq!(
                reconnecting(Some(&kept), Some(&other_device), &host, true, now),
                Reconnecting::AdoptHost
            );
            // A root renews what ends within a day; a paired device goes on until refused.
            let ending = held(2, 9, Some(now + day / 2));
            assert_eq!(
                reconnecting(Some(&ending), None, &host, true, now),
                Reconnecting::Certify
            );
            assert_eq!(
                reconnecting(Some(&ending), None, &host, false, now),
                Reconnecting::Kept
            );
            assert_eq!(
                reconnecting(
                    Some(&held(2, 9, Some(now + 3 * day))),
                    None,
                    &host,
                    true,
                    now
                ),
                Reconnecting::Kept
            );
        }

        phase a_root_certifies_anew_from_exactly_a_day_before_the_kept_certificate_ends;
        // The day before a kept certificate ends is the boundary itself, to the nanosecond, and the
        // clock's far end does not wrap a certificate back into lasting. Each failure names its row.
        {
            use Reconnecting::{Certify, Kept};
            let host = cert(9, None);
            let day = RENEWING_NS;
            let now = 10 * day;
            let rows = [
                (
                    "exactly a day left, root",
                    Some(now + day),
                    true,
                    now,
                    Certify,
                ),
                (
                    "a day and a nanosecond left, root",
                    Some(now + day + 1),
                    true,
                    now,
                    Kept,
                ),
                (
                    "a nanosecond short of a day, root",
                    Some(now + day - 1),
                    true,
                    now,
                    Certify,
                ),
                ("ended already, root", Some(now - 1), true, now, Certify),
                (
                    "exactly a day left, paired",
                    Some(now + day),
                    false,
                    now,
                    Kept,
                ),
                ("ended already, paired", Some(now - 1), false, now, Kept),
                ("never ending, root", None, true, now, Kept),
                (
                    "the clock at its end, root",
                    Some(u64::MAX),
                    true,
                    u64::MAX,
                    Certify,
                ),
                (
                    "the clock at its end, never ending",
                    None,
                    true,
                    u64::MAX,
                    Kept,
                ),
            ];
            for (row, expires_at_ns, with_root, at, expected) in rows {
                let kept = held(2, 9, expires_at_ns);
                assert_eq!(
                    reconnecting(Some(&kept), None, &host, with_root, at),
                    expected,
                    "{row}"
                );
            }
        }

        phase the_host_that_answers_decides_before_a_kept_certificates_last_day_does;
        // The host that answers decides before the certificate's day does: nothing kept for another
        // host is handed over or renewed there, and an admission wins over a renewal. Each failure
        // names its row.
        {
            use Reconnecting::{Admitted, AdoptHost, Certify, HostChanged};
            let host = cert(9, None);
            let day = RENEWING_NS;
            let now = 10 * day;
            let ending = Some(now + day / 2);
            let here = held(2, 9, None);
            let there = held(2, 8, None);
            let rows = [
                (
                    "ending, kept elsewhere, root",
                    Some(held(2, 8, ending)),
                    None,
                    true,
                    HostChanged,
                ),
                (
                    "ending, kept elsewhere, paired",
                    Some(held(2, 8, ending)),
                    None,
                    false,
                    HostChanged,
                ),
                (
                    "kept elsewhere, paired",
                    Some(held(2, 8, None)),
                    None,
                    false,
                    HostChanged,
                ),
                ("nothing kept, admitted", None, Some(&here), true, Admitted),
                (
                    "ending, admitted here",
                    Some(held(2, 9, ending)),
                    Some(&here),
                    true,
                    Admitted,
                ),
                (
                    "kept here, admitted by another host",
                    Some(held(2, 9, None)),
                    Some(&there),
                    true,
                    AdoptHost,
                ),
                (
                    "ending, kept elsewhere, admitted here",
                    Some(held(2, 8, ending)),
                    Some(&here),
                    true,
                    AdoptHost,
                ),
                ("nothing kept, paired", None, None, false, Certify),
            ];
            for (row, kept, under, with_root, expected) in rows {
                assert_eq!(
                    reconnecting(kept.as_ref(), under, &host, with_root, now),
                    expected,
                    "{row}"
                );
            }
        }
    }
}
