//! One device of a person in a workspace: the session every program keeps, and the one entry an
//! agent, `tilo mcp` or the API server takes it by.

use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_actions::remote::host_account::Refusal;
use tilo_actions::remote::{
    ActionHost as _, ActionHostError, Caller, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_relay_client::login::Admission;
#[cfg(not(target_arch = "wasm32"))]
use tilo_relay_client::login::{Door, SignIn};
use tilo_relay_client::{CertifyError, DeviceSigner};
use tilo_relay_session::workspaces::{
    CREATE_OWN_ROOM, CreatedWorkspace, HOST_ACTIONS_PROVIDER, HOST_ROOMS_RESOURCE, KEEP_PERSONAL,
    LIST_PERSONAL, PersonalList, WorkspaceListing,
};
use tilo_user_identity::DeviceCert;
use web_time::Duration as Wait;

use crate::session::{HOST_ITSELF, PeerConfig};
use crate::{Cmd, Link, Outbox, TransportPhase};
use crate::{MemoryPeerStore, PeerStore};
use tilo_relay_protocol::WorkspaceId;
use tilo_session_core::remote::SessionActions;

/// How long joining a workspace waits for its host to admit this device.
const ADMITTING: Wait = Wait::from_secs(20);
/// How often a join looks at the session while it waits.
const POLL: Wait = Wait::from_millis(100);
/// How often an entry looks at its session while it waits for the host's answer: a page
/// waits for nothing else, so it looks often.
const ENTRY_POLL: Wait = Wait::from_millis(5);
/// How long a workspace's host has to answer an operation of its own.
const ANSWERING: u32 = 20_000;

/// A device of a person, in the workspaces its session is in.
#[derive(Clone)]
pub struct Device {
    /// The session with the workspaces' host.
    peer: Link,
    cert: DeviceCert,
    /// The host's own device: whose device answers a workspace's own actions.
    host_cert: DeviceCert,
    /// The workspace the device entered.
    workspace_id: WorkspaceId,
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
            peer: crate::session::start_session(
                config.clone(),
                signer,
                Arc::new(MemoryPeerStore::default()) as Arc<dyn PeerStore>,
            ),
            cert: config.cert,
            host_cert: config.host_cert,
            workspace_id: WorkspaceId::from_u128(0),
            label: label.to_owned(),
        }
    }

    /// The device whose session `peer` already is: a page's, which started its session itself.
    /// `cert` must be the one `peer` was started with.
    #[must_use]
    pub fn of(peer: Link, cert: DeviceCert, host_cert: DeviceCert, label: &str) -> Self {
        Self {
            peer,
            cert,
            host_cert,
            workspace_id: WorkspaceId::from_u128(0),
            label: label.to_owned(),
        }
    }

    /// A session for the device `sign_in` signed in. A computer's entry: a
    /// page writes login codes and never reads one, so it makes its device
    /// with [`Self::over`] and joins the workplaces its grants reach.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn connect(sign_in: &SignIn, signer: Arc<dyn DeviceSigner>) -> Self {
        Self::entering(sign_in, signer, None)
    }

    /// [`Self::connect`], its session entering a workspace through its door as it opens.
    #[cfg(not(target_arch = "wasm32"))]
    fn entering(
        sign_in: &SignIn,
        signer: Arc<dyn DeviceSigner>,
        entering: Option<tilo_relay_protocol::entering::Entering>,
    ) -> Self {
        Self::over(
            PeerConfig {
                host: sign_in.host.clone(),
                cert: sign_in.cert.clone(),
                host_cert: sign_in.host_cert.clone(),
                revocations: tilo_user_identity::RevocationList::new(),
                entering,
            },
            signer,
            &sign_in.label,
        )
    }

    /// Take `sign_in`'s device into the workspace its link names ([`Self::enter_workspace`]). The
    /// site's `door`, where it names one, lets the person in on the session as it opens: how a host
    /// lets in someone it does not know yet, before it makes them a workspace of their own.
    ///
    /// # Errors
    /// The workspace's id is not one, the host could not be asked, or the workspace refused.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn enter(
        door: Option<&Door>,
        sign_in: &SignIn,
        signer: Arc<dyn DeviceSigner>,
    ) -> Result<Self, CertifyError> {
        let workspace = sign_in.link.workspace_id();
        WorkspaceId::parse(&workspace)
            .ok_or_else(|| CertifyError::Request("the link names no workspace".to_string()))?;
        let entering = door.and_then(Door::workspace).map(|workspace| {
            tilo_relay_protocol::entering::Entering {
                workspace,
                name: tilo_relay_protocol::web::kept(
                    &sign_in.agent,
                    tilo_relay_protocol::web::MAX_DISPLAY_NAME_CHARS,
                ),
                request_id: rand::random(),
                through: tilo_relay_protocol::entering::Through::Door,
            }
        });
        let door_entered = entering.is_some();
        let device = Self::entering(sign_in, signer, entering);
        if door_entered {
            entered(&device.peer).await?;
        } else {
            device.connected().await?;
        }
        let entered = device
            .enter_workspace(&workspace, &sign_in.label)
            .await
            .map_err(CertifyError::from)?;
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

    /// Join `workspace` (its UUID) and wait for its host to say this device is in: its id here.
    ///
    /// # Errors
    /// It is no workspace UUID, the session is closed, or its host did not admit this device.
    pub async fn join(&self, workspace: &str) -> Result<WorkspaceId, CertifyError> {
        let id = WorkspaceId::parse(workspace)
            .ok_or_else(|| CertifyError::Request(format!("{workspace} names no workspace")))?;
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
        let started = tilo_clock::now();
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
        let (done, receive) = crate::answer();
        self.peer
            .send(Cmd::Enter(entering, done))
            .map_err(|closed| CertifyError::Request(closed.into()))?;
        let entered = receive.await.map_err(|_| {
            CertifyError::Request("the session ended before the host answered".into())
        })??;
        admission(entered).map_err(|refusal| {
            CertifyError::Denied(format!(
                "The workspace did not let this device in: {}",
                refusal.reason()
            ))
        })
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
    ) -> Result<WorkspaceId, HostRefusal> {
        let id = WorkspaceId::parse(workspace)
            .ok_or_else(|| HostRefusal::malformed(format!("{workspace} names no workspace")))?;
        if self.peer.status().workspaces.get(&id) == Some(&true) {
            return Ok(id);
        }
        let mut named = Map::new();
        named.insert("room".to_owned(), Value::String(workspace.to_owned()));
        let asked = match self.own_workspace(named, &self.label).await {
            Ok((listing, invitation)) if listing.workspace == workspace => {
                self.redeem(&invitation, shown_as).await?;
                None
            }
            Ok(_) => Some(HostRefusal::malformed(
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
    pub async fn connected(&self) -> Result<(), HostRefusal> {
        let started = tilo_clock::now();
        loop {
            let status = self.peer.status();
            match status.phase {
                TransportPhase::Connected => return Ok(()),
                TransportPhase::Failed | TransportPhase::Closed => {
                    return Err(status.failures.last().map_or_else(
                        || HostRefusal::unanswered("the session with the host ended"),
                        |failure| HostRefusal::failed(&failure.message),
                    ));
                }
                _ => {}
            }
            if started.elapsed() >= ADMITTING {
                return Err(HostRefusal::unanswered(
                    "the workspace's host did not take the session in time",
                ));
            }
            n0_future::time::sleep(POLL).await;
        }
    }

    /// The person's own workspace on the host, and the invitation into it: its `name` makes
    /// another, or the `room` it must be.
    ///
    /// # Errors
    /// The host did not answer, refused, or answered no invitation.
    pub async fn own_workspace(
        &self,
        arguments: Map<String, Value>,
        caller: &str,
    ) -> Result<(WorkspaceListing, Vec<u8>), HostRefusal> {
        let output = self.host_action(CREATE_OWN_ROOM, arguments, caller).await?;
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

    /// Have this device in the person's own workspace on the host: the host makes it the first time, or answers the one the person has (another
    /// for each `name` they give; the one called `room`, a UUID, when it says, made under that name
    /// for a person who has none yet), and this very device redeems the invitation into it, so the
    /// key this session holds is the one that opens it next. A workspace `entered` says this device is
    /// in already is not redeemed again. `shown_as` is the name its members see.
    ///
    /// # Errors
    /// The host did not answer or refused (typed, as it said), or the workspace did not admit this
    /// device.
    pub async fn enter_own_workspace(
        &self,
        name: Option<&str>,
        room: Option<&str>,
        shown_as: &str,
        entered: impl FnOnce(&str) -> bool,
    ) -> Result<WorkspaceListing, HostRefusal> {
        let mut arguments = Map::new();
        if let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) {
            arguments.insert("name".to_owned(), Value::String(name.to_owned()));
        }
        if let Some(room) = room {
            arguments.insert("room".to_owned(), Value::String(room.to_owned()));
        }
        let (listing, invitation) = self.own_workspace(arguments, &self.label).await?;
        if !entered(&listing.workspace) {
            self.redeem(&invitation, shown_as)
                .await
                .map_err(HostRefusal::from)?;
        }
        Ok(listing)
    }

    /// The person's workspace list as the host keeps it: every
    /// workspace of theirs their devices said, wherever it is hosted, with the workspaces this host
    /// keeps for them, each naming its host. A key for one workspace reads that one alone.
    ///
    /// # Errors
    /// The host did not answer, refused, or answered something else.
    pub async fn personal_list(&self) -> Result<PersonalList, HostRefusal> {
        let listed = self
            .host_action(LIST_PERSONAL, Map::new(), &self.label)
            .await?;
        serde_json::from_value(listed).map_err(|error| {
            HostRefusal::malformed(format!("The host answered no workspace list: {error}"))
        })
    }

    /// Keep `list` on the host as the person's workspace list: what their keys read back with [`Self::personal_list`]. Only a device of the person's own
    /// keeps it.
    ///
    /// # Errors
    /// The host did not answer or refused.
    pub async fn keep_personal(&self, list: &PersonalList) -> Result<(), HostRefusal> {
        let mut arguments = Map::new();
        arguments.insert(
            "list".to_owned(),
            serde_json::to_value(list)
                .map_err(|error| HostRefusal::malformed(error.to_string()))?,
        );
        self.host_action(KEEP_PERSONAL, arguments, &self.label)
            .await
            .map(drop)
    }

    /// The host's own operation `action` with `arguments`, asked on the session itself: no
    /// workspace decides it.
    ///
    /// # Errors
    /// The host did not answer, refused, or did not finish.
    pub async fn host_action(
        &self,
        action: &str,
        arguments: Map<String, Value>,
        caller: &str,
    ) -> Result<Value, HostRefusal> {
        let target = RemoteActionTarget {
            provider: HOST_ACTIONS_PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(HOST_ROOMS_RESOURCE, action),
        };
        self.ask_host(HOST_ITSELF, target, arguments, caller).await
    }

    /// What the host itself answers to `target` with `arguments`, asked in
    /// `workspace_id` ([`HOST_ITSELF`] for none): one of its own operations, or the person's
    /// account on it.
    ///
    /// # Errors
    /// The host did not answer, refused, or did not finish.
    pub async fn ask_host(
        &self,
        workspace_id: WorkspaceId,
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
    pub fn actions(&self, workspace_id: WorkspaceId) -> SessionActions {
        SessionActions::new(&self.peer, workspace_id, self.host_cert.device_public_key)
    }

    /// Close the session: it says this device left, and stops.
    pub fn close(&self) {
        self.peer.close();
    }
}

/// How long before it ends a certificate a device holds is certified anew rather than entered with.
const RENEWING_NS: u64 = 24 * 60 * 60 * 1_000_000_000;

/// A certificate a device holds from an earlier entry, and the host's own certificate as it was
/// then: what the page keeps, as `{cert, hostCert}`.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct Held {
    pub cert: DeviceCert,
    #[serde(rename = "hostCert")]
    pub host_cert: Option<DeviceCert>,
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
    /// The one the host just admitted it under (an invitation, its door, a pairing): kept from now
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
        .and_then(|admitted| admitted.host_cert.as_ref())
        .unwrap_or(host);
    let kept_elsewhere = kept
        .and_then(|kept| kept.host_cert.as_ref())
        .is_some_and(|kept| !same_host(kept, answering));
    match (kept, admitted) {
        (_, Some(_)) if kept_elsewhere => Reconnecting::AdoptHost,
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
/// Nothing held serves and no root is at hand ([`HostRefusal::NO_CERTIFICATE`]), or the root did
/// not sign.
pub async fn entering_certificate(
    held: Option<&Held>,
    host: &DeviceCert,
    device: &dyn DeviceSigner,
    root: Option<&dyn DeviceSigner>,
    label: &str,
    now_ns: u64,
) -> Result<DeviceCert, HostRefusal> {
    let serves = |held: &&Held| {
        let same_host = held
            .host_cert
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
        return Err(HostRefusal {
            code: HostRefusal::NO_CERTIFICATE.to_owned(),
            message: "This device holds no certificate of this host's for its key, and no root \
                      to certify it with."
                .to_owned(),
            refusal: None,
            retry_after_ms: None,
            door: None,
        });
    };
    tilo_relay_client::invite::guest_certificate(root, device.device_public_key(), label, now_ns)
        .await
        .map_err(|error| HostRefusal {
            code: HostRefusal::NO_CERTIFICATE.to_owned(),
            message: error.to_string(),
            refusal: None,
            retry_after_ms: None,
            door: None,
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
) -> Result<(DeviceCert, tilo_relay_protocol::entering::Entering), HostRefusal> {
    let entry =
        invitation_entering(invitation, entering.name, entering.token).map_err(|message| {
            HostRefusal {
                code: HostRefusal::NOT_ADMITTED.to_owned(),
                message,
                refusal: None,
                retry_after_ms: None,
                door: None,
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
) -> Result<tilo_relay_protocol::entering::Entering, String> {
    let workspace =
        tilo_peer_state::join_ledger::invitation_workspace(invitation, tilo_clock::unix_now_ns())
            .map_err(|error| format!("This invitation cannot be used: {error}"))?;
    Ok(tilo_relay_protocol::entering::Entering {
        workspace: workspace.to_string(),
        name: tilo_relay_protocol::web::kept(
            name,
            tilo_relay_protocol::web::MAX_DISPLAY_NAME_CHARS,
        ),
        request_id: rand::random(),
        through: tilo_relay_protocol::entering::Through::Invitation {
            invitation: invitation.to_vec(),
            token: token.map(<[u8]>::to_vec),
        },
    })
}

/// What a workspace said as it admitted a device, or why it did not.
fn admission(
    entered: tilo_relay_protocol::entering::Entered,
) -> Result<Admission, tilo_relay_protocol::entering::EnterRefusal> {
    match entered {
        tilo_relay_protocol::entering::Entered::Admitted {
            workspace,
            workspace_id,
            name,
            role,
            ..
        } => Ok(Admission {
            workspace,
            workspace_id,
            name,
            role,
        }),
        tilo_relay_protocol::entering::Entered::Refused(refusal) => Err(refusal),
    }
}

/// What `device` enters `workspace` through its door with, on the host whose certificate is
/// `host`: the certificate [`entering_certificate`] chooses (`entering.token` is not read: the
/// door hands out its own) and the entry its session asks for as it opens
/// ([`PeerConfig::entering`]). The door, the workspace's challenge and the admission all happen on
/// that session's handshake; [`entered`] waits for the host's answer.
///
/// # Errors
/// The device holds no certificate it could enter with.
pub async fn door_entry(
    host: &DeviceCert,
    workspace: &str,
    device: &dyn DeviceSigner,
    entering: Entering<'_>,
) -> Result<(DeviceCert, tilo_relay_protocol::entering::Entering), HostRefusal> {
    let cert = entering_certificate(
        entering.held,
        host,
        device,
        entering.root,
        entering.label,
        tilo_clock::unix_now_ns(),
    )
    .await?;
    let entry = tilo_relay_protocol::entering::Entering {
        workspace: workspace.to_owned(),
        name: tilo_relay_protocol::web::kept(
            entering.name,
            tilo_relay_protocol::web::MAX_DISPLAY_NAME_CHARS,
        ),
        request_id: rand::random(),
        through: tilo_relay_protocol::entering::Through::Door,
    };
    Ok((cert, entry))
}

/// Wait for the host's answer to the entry `peer`'s session asked for ([`door_entry`]).
///
/// # Errors
/// The door or the workspace refused (the door's own code: `banned`, `removed`,
/// `invitation-only`, `full` with the wait it asked for, `refused`), or no answer came in time.
pub async fn entered(peer: &Link) -> Result<Admission, HostRefusal> {
    let started = tilo_clock::now();
    let waits = ADMITTING.max(tilo_relay_protocol::entering::ENTERING_DEADLINE);
    loop {
        let status = peer.status();
        match status.entry {
            Some(crate::Entry::Admitted {
                workspace,
                workspace_id,
                name,
                role,
            }) => {
                return Ok(Admission {
                    workspace,
                    workspace_id,
                    name,
                    role,
                });
            }
            Some(crate::Entry::Refused {
                code,
                message,
                retry_after_secs,
                door,
            }) => {
                return Err(HostRefusal {
                    code: code.to_owned(),
                    message,
                    refusal: None,
                    retry_after_ms: retry_after_secs.map(|secs| secs.saturating_mul(1000)),
                    door,
                });
            }
            None => {}
        }
        if let Some(failure) = status
            .failures
            .iter()
            .find(|_| status.phase == TransportPhase::Failed)
        {
            return Err(HostRefusal::failed(&failure.message));
        }
        if started.elapsed() >= waits {
            return Err(HostRefusal::unanswered(
                "the workspace's host did not answer the entry in time",
            ));
        }
        n0_future::time::sleep(ENTRY_POLL).await;
    }
}

/// Why a workspace's host did not do what a device asked of it, as the host said it: what every
/// caller (the CLI, the API server, the page) reads and shows, never a sentence to parse.
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
    /// How long the host asks to wait before the next try, in milliseconds, when it asked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    /// The door's own refusal, when a workspace's door turned the device away: what it says
    /// about a wait list the workspace keeps, which its page offers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub door: Option<tilo_relay_protocol::directory::DoorRefused>,
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
    /// The device holds no certificate it could enter with, and no root to certify it.
    pub const NO_CERTIFICATE: &'static str = "no_certificate";
    /// The host is busy with this person's changes: nothing ran, and the same request works
    /// once [`Self::retry_after_ms`] passed.
    pub const RATE_LIMITED: &'static str = "rate_limited";

    /// The host's refusal `error`, its typed refusal read off it where it carries one. A host
    /// too busy to take the action reads as [`Self::RATE_LIMITED`], with the wait it said.
    #[must_use]
    pub fn of(error: &ActionHostError) -> Self {
        match error {
            ActionHostError::Refused { reason } => Self::said(error.code(), reason),
            ActionHostError::Unavailable { reason } => {
                tilo_relay_protocol::refusal::busy_wait(reason).map_or_else(
                    || Self::plain(error),
                    |wait| Self::rate_limited(u64::try_from(wait.as_millis()).unwrap_or(u64::MAX)),
                )
            }
            other => Self::plain(other),
        }
    }

    fn plain(error: &ActionHostError) -> Self {
        Self {
            code: error.code().to_owned(),
            message: error.to_string(),
            refusal: None,
            retry_after_ms: None,
            door: None,
        }
    }

    /// The host is busy with this person's changes; the same request works in `retry_after_ms`.
    #[must_use]
    pub fn rate_limited(retry_after_ms: u64) -> Self {
        Self {
            code: Self::RATE_LIMITED.to_owned(),
            message: "the host takes this person's changes more slowly; wait and try again"
                .to_owned(),
            refusal: None,
            retry_after_ms: Some(retry_after_ms),
            door: None,
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
            retry_after_ms: None,
            door: None,
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
            door: None,
        }
    }

    /// `reason` as the host said it, typed where it is a [`Refusal`]'s text, or the refusal of
    /// the site's door when the host turned away a person it never let in.
    fn said(code: &str, reason: &str) -> Self {
        if let Some(door) = tilo_relay_protocol::directory::DoorRefused::from_reason(reason) {
            return Self {
                code: Self::NOT_ADMITTED.to_owned(),
                message: door.error.clone(),
                refusal: None,
                retry_after_ms: None,
                door: Some(door),
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
            door: None,
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
                retry_after_ms: None,
                door: None,
            },
            CertifyError::RateLimited { retry_after_ms } => Self::rate_limited(retry_after_ms),
            other => Self::unanswered(&other.to_string()),
        }
    }
}

/// What a computer's entry reports: a refusal is the host's denial, no answer a failed request.
impl From<HostRefusal> for CertifyError {
    fn from(refusal: HostRefusal) -> Self {
        if refusal.code == HostRefusal::UNANSWERED {
            Self::Request(refusal.message)
        } else if refusal.code == HostRefusal::RATE_LIMITED {
            Self::RateLimited {
                retry_after_ms: refusal.retry_after_ms.unwrap_or(1000),
            }
        } else {
            Self::Denied(refusal.message)
        }
    }
}

/// Say `peer`'s session is one run of the agent `label` names: every request it makes from then on
/// is the agent's, for the device's person, and the person's agent door lists the run while the
/// session lasts.
///
/// # Errors
/// The session is already another agent's run, the session is closed, or the host did not answer.
pub async fn run_as_agent(peer: &Link, label: &str) -> Result<(), String> {
    let label: String = label
        .chars()
        .filter(|character| !character.is_control())
        .take(tilo_relay_protocol::transport::MAX_AGENT_LABEL_CHARS)
        .collect();
    let (done, receive) = crate::answer();
    peer.send(Cmd::AgentRun(label, done))?;
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
    fn a_busy_host_reads_as_rate_limited_with_its_wait_through_any_entry() {
        let busy = ActionHostError::Unavailable {
            reason: format!(
                "backpressure: {}",
                tilo_relay_protocol::refusal::too_many_changes(std::time::Duration::from_millis(
                    480
                ))
            ),
        };
        let refused = HostRefusal::of(&busy);
        assert_eq!(refused.code, HostRefusal::RATE_LIMITED);
        assert_eq!(refused.retry_after_ms, Some(480));
        // Through an entry's certification error and back, it stays what it is.
        let entry = CertifyError::from(refused.clone());
        assert!(
            matches!(
                entry,
                CertifyError::RateLimited {
                    retry_after_ms: 480
                }
            ),
            "{entry}"
        );
        assert_eq!(HostRefusal::from(entry), refused);
        // Any other unavailability stays as it was.
        let offline = HostRefusal::of(&ActionHostError::Unavailable {
            reason: "offline: the device left".into(),
        });
        assert_eq!(offline.code, "remote_actions_unavailable");
        assert_eq!(offline.retry_after_ms, None);
    }

    #[test]
    fn every_refusal_reads_with_the_code_and_words_it_travels_with() {
        // Each refusal as it travels, with its code and words (tilo-actions holds the file to the
        // type): what the CLI, the API server and the page read off it.
        let file: Vec<Value> = serde_json::from_str(include_str!(
            "../../../collab/actions/tests/host_refusals.json"
        ))
        .unwrap();
        for entry in file {
            let wire: ActionHostError = serde_json::from_value(entry["wire"].clone()).unwrap();
            let read = HostRefusal::of(&wire);
            assert_eq!(read.code, entry["code"], "{entry}");
            match (&wire, &read.refusal) {
                (_, Some(typed)) => assert_eq!(read.message, typed.message),
                (ActionHostError::Refused { reason }, None) => assert_eq!(&read.message, reason),
                (_, None) => assert_eq!(read.message, entry["display"], "{entry}"),
            }
        }
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
            host_cert: Some(cert(host, None)),
        }
    }

    #[test]
    fn a_device_comes_back_with_what_it_kept_from_that_host_alone() {
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

    /// A host turning away a person its site never let in says it as the site's door does: the
    /// refusal reads as not admitted, with the door's wait list for whoever shows it.
    #[test]
    fn a_host_refusing_a_person_never_let_in_answers_the_doors_refusal() {
        use tilo_relay_protocol::directory::{DoorRefusalCode, DoorRefused};
        let door = DoorRefused {
            code: DoorRefusalCode::InvitationOnly,
            error: "tilo.test takes people by invitation".into(),
            wait_list: true,
            contact: Some("hello@tilo.test".into()),
            mail: true,
        };
        let refusal = HostRefusal::of(&ActionHostError::Refused {
            reason: door.to_reason(),
        });
        assert_eq!(refusal.code, HostRefusal::NOT_ADMITTED);
        assert_eq!(refusal.message, door.error);
        assert_eq!(refusal.door.as_ref(), Some(&door));
        assert!(matches!(
            CertifyError::from(refusal),
            CertifyError::Denied(why) if why == door.error
        ));
        // A host account's typed refusal is not the door's.
        let plain = HostRefusal::of(&ActionHostError::Refused {
            reason: "no such workspace".into(),
        });
        assert_eq!(plain.door, None);
    }
}
