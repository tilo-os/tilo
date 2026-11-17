//! The hosts of a workspace this host governs: itself first, then each host it
//! asked to hold the workspace as well ([`crate::hosted`]): another host of its
//! own person, or another person's host whose operator takes that work.
//!
//! Each host signs its own address. The workspace's governance names the hosts
//! serving it beside the governor ([`tilo_peer_state::WORKSPACE_HOSTS_KEY`]), so
//! every member learns of each as it learns any change to the workspace, and the
//! workspace's join answers carry them all, governor first: a member reaches the
//! workspace wherever it is served. Being named there is what admits a host
//! everywhere the workspace is held: its device holds the workspace, and its
//! person may reach the hosts that do ([`tilo_peer_state::GovernedAccess`]).
//! A host of the governor's own person is also admitted as one of the owner's
//! devices before it is handed the workspace, as before it was named.

use std::sync::Arc;
use std::time::Duration;

use iroh_base::EndpointAddr;
use serde_json::{Map, Value, json};
use tilo_actions::remote::{InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
use tilo_net::proxy::host::{Destinations, check_destination, guarded_client, system_lookup};
use tilo_peer_state::join::{Role, TransportHint};
use tilo_relay_client::{
    LinkError,
    https::{HttpClient, HttpFuture},
};
use tilo_relay_session::client::RelayClient;
use tilo_relay_session::workspaces::{
    HOST_ACTIONS_PROVIDER, HOST_WORKSPACE, HOST_WORKSPACES_RESOURCE, HostedWorkspace,
    UNHOST_WORKSPACE, ask_host,
};
use tilo_user_identity::DeviceCert;

use super::{CollaborationError, GovernedWorkspace, Workspaces};
use crate::attach::{HostDialer, Upstream};
use crate::handshake::LiveIdentity;
use tilo_lock::RwLockRecover as _;
use tilo_relay_protocol::WorkspaceId;

/// This host's answer to the session a host serving one of its workspaces opened
/// here ([`Workspaces::answer_host`]).
pub(super) struct Answer {
    /// The session answered.
    inbound: tokio::sync::mpsc::Sender<()>,
    /// The session back to the host.
    session: tokio::task::AbortHandle,
}

impl Drop for Answer {
    fn drop(&mut self) {
        self.session.abort();
    }
}

/// The device a signed host address names.
const fn host_device(hint: &TransportHint) -> [u8; 32] {
    hint.host.device_public_key
}

impl Workspaces {
    /// Every host of `workspace`, each its own signed address: this host first.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here, or this host's address cannot be
    /// signed.
    pub async fn hosts(&self, workspace: &str) -> Result<Vec<TransportHint>, CollaborationError> {
        let governed = self.workspace(workspace).await?;
        let governed = governed.lock().await;
        let own = self.transport_hint(&governed)?;
        let mut hosts = vec![own.clone()];
        hosts.extend(
            governed
                .peer()
                .governance()
                .workspace_hosts()
                .into_iter()
                .filter(|hint| hint.host.device_public_key != own.host.device_public_key),
        );
        drop(governed);
        Ok(hosts)
    }

    /// Whether `workspace`'s governance names the device `device` among its
    /// hosts.
    pub(crate) async fn names_host(&self, workspace: &str, device: [u8; 32]) -> bool {
        let Ok(governed) = self.workspace(workspace).await else {
            return false;
        };
        let governed = governed.lock().await;
        governed
            .peer()
            .governance()
            .workspace_hosts()
            .iter()
            .any(|hint| host_device(hint) == device)
    }

    /// Name this host's own signed address among the workspace's hosts, as its
    /// governor, so every member reads where the workspace is served from the
    /// replica it already holds. Naming it as it is named changes nothing.
    pub(super) async fn name_own_host(
        &self,
        governed: &mut GovernedWorkspace,
    ) -> Result<(), String> {
        let host = &self.inner.host;
        let hint = self
            .transport_hint(governed)
            .map_err(|error| error.to_string())?;
        let now = tilo_clock::wall();
        let mut candidate = governed.peer().clone();
        if candidate
            .governance_mut()
            .author_workspace_host(&hint, &host.key, now)?
        {
            governed.commit(candidate, &host.key, now, |record, _| {
                Ok(record.collaboration.clone())
            })?;
            self.settle(governed).await?;
        }
        Ok(())
    }

    /// Name this host's own signed address in every workspace it governs,
    /// after where it answers changed: what their governance names is where
    /// members and invitees dial this host.
    pub(super) async fn name_own_hosts(&self) {
        let workspaces = self
            .inner
            .workspaces
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for workspace in workspaces {
            let mut workspace = workspace.lock().await;
            if let Err(error) = self.name_own_host(&mut workspace).await {
                tracing::error!(
                    workspace = %workspace.workspace(),
                    %error,
                    "the host's address is not named in the workspace's governance"
                );
            }
        }
    }

    /// Follow the relays this host answers through: each change of home relay
    /// is named in the workspaces it governs, so an invitation always carries
    /// an address a stranger can dial. A host that has lost its relays names
    /// nothing: the address it named stays what a workspace says, and a member
    /// reaches it again through the relay once it comes back.
    pub(super) fn follow_own_address(&self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let workspaces = Arc::downgrade(&self.inner);
        let following =
            tilo_endpoint_reachability::follow(&self.inner.host.endpoint, &runtime, move |_| {
                let workspaces = workspaces.clone();
                async move {
                    let Some(inner) = workspaces.upgrade() else {
                        return;
                    };
                    Self { inner }.name_own_hosts().await;
                }
            });
        let _ = self.inner.own_address.set(following);
    }

    /// Change which hosts `workspace`'s governance names with `change`, as its
    /// governor.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked until its decision is committed and settled"
    )]
    async fn name_host(
        &self,
        workspace: &str,
        change: impl FnOnce(
            tilo_peer_state::PeerGovernanceMut<'_>,
            &ed25519_dalek::SigningKey,
            std::time::SystemTime,
        ) -> Result<bool, String>,
    ) -> Result<(), String> {
        let host = &self.inner.host;
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut governed = state.lock().await;
        let now = tilo_clock::wall();
        let mut candidate = governed.peer().clone();
        if change(candidate.governance_mut(), &host.key, now)? {
            governed.commit(candidate, &host.key, now, |record, _| {
                Ok(record.collaboration.clone())
            })?;
            self.settle(&mut governed).await?;
        }
        Ok(())
    }

    /// Have the host `device`, at `addr`, hold `workspace` as well, `caller`'s
    /// request: hand the workspace over with [`HOST_WORKSPACE`] and name the
    /// address it answers among the workspace's hosts. A host of this host's
    /// own person is admitted to the workspace as one of the owner's devices
    /// first; another person's host takes the workspace only when its
    /// operator's work setting does. Asked again, the host is handed the
    /// workspace afresh. Answers that address.
    ///
    /// # Errors
    ///
    /// The caller may not choose the workspace's hosts, the workspace is not
    /// governed here, the host cannot be reached or is not that device, or it
    /// refused or answered an address that is not its own for the workspace.
    pub async fn attach_host(
        &self,
        workspace: &str,
        caller: &DeviceCert,
        device: [u8; 32],
        addr: EndpointAddr,
    ) -> Result<TransportHint, String> {
        let host = &self.inner.host;
        self.authorize_hosts(workspace, caller).await?;
        if device == host.cert.device_public_key {
            return Err("a workspace's governor hosts it already".into());
        }
        self.stop_answering(workspace, device).await;
        let reached = self.reach_host(device, addr).await?;
        let member = reached.certificate.clone();
        let (workspace_id, arguments) = self.hand_over(workspace, &member).await?;
        let answer = reached.ask(HOST_WORKSPACE, arguments).await?;
        let hosted: HostedWorkspace =
            serde_json::from_value(answer).map_err(|error| error.to_string())?;
        let hint: TransportHint = crate::hosted::decode_hex(&hosted.host_hint)?;
        let address =
            WorkspaceDeviceHintV1::from_transport_hint(&hint).map_err(|error| error.to_string())?;
        if hint.host != member || !address.holds_keys || address.workspace_id != workspace_id {
            return Err(
                "the device answered an address that is not its own for the workspace".into(),
            );
        }
        self.name_host(workspace, |governance, key, now| {
            governance.author_workspace_host(&hint, key, now)
        })
        .await?;
        // The host dials this device as it takes the workspace up, perhaps before
        // its address was recorded above: a session it opened already is
        // answered here, one it opens later when it says hello.
        if let Some(inbound) = self.attach_session(workspace_id, device).await {
            self.answer_host(workspace, device, inbound).await;
        }
        tracing::info!(%workspace, host = %hex::encode(device), person = %hex::encode(member.user_public_key), "another host holds the workspace");
        let _ = self.inner.changes.send(workspace.to_owned());
        Ok(hint)
    }

    /// What `workspace` is handed to the device `member` with: the workspace
    /// as a joining member gets it and this device's own signed address,
    /// under the workspace's id. A device of this one's own person is
    /// admitted to the workspace as one of the owner's devices first.
    async fn hand_over(
        &self,
        workspace: &str,
        member: &DeviceCert,
    ) -> Result<(WorkspaceId, Map<String, Value>), String> {
        if member.user_public_key == self.inner.host.cert.user_public_key {
            self.enter(workspace, member)
                .await
                .map_err(|error| error.to_string())?;
        }
        let (workspace_id, governor_hint, bootstrap) = {
            let governed = self
                .workspace(workspace)
                .await
                .map_err(|error| error.to_string())?;
            let governed = governed.lock().await;
            let mut bootstrap = governed
                .governed()
                .bootstrap([0; 32], member.clone(), Role::Editor)
                .map_err(|error| error.to_string())?;
            // The host follows governance alone: records reach it over the
            // attach session, and leaving them here keeps the hand-over
            // within one action frame.
            bootstrap.records.clear();
            let hint = self
                .transport_hint(&governed)
                .map_err(|error| error.to_string())?;
            (governed.workspace_id(), hint, bootstrap)
        };
        let mut arguments = Map::new();
        arguments.insert("workspace".into(), Value::String(workspace.to_owned()));
        arguments.insert(
            "workspace_id".into(),
            Value::String(workspace_id.to_string()),
        );
        arguments.insert(
            "governor_hint".into(),
            Value::String(hex_postcard(&governor_hint)?),
        );
        arguments.insert("bootstrap".into(), Value::String(hex_postcard(&bootstrap)?));
        Ok((workspace_id, arguments))
    }

    /// Hand `workspace` to the device `device` at `addr` as its owner's
    /// choice of it does, but under the name `named`, and answer what that
    /// device said: what a test asks with whether a device that is handed a
    /// workspace takes it under any name but its own id.
    ///
    /// # Errors
    ///
    /// The device was not reached, or refused.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn hand_over_named(
        &self,
        workspace: &str,
        device: [u8; 32],
        addr: EndpointAddr,
        named: &str,
    ) -> Result<Value, String> {
        let reached = self.reach_host(device, addr).await?;
        let (_, mut arguments) = self.hand_over(workspace, &reached.certificate).await?;
        arguments.insert("workspace".into(), Value::String(named.to_owned()));
        reached.ask(HOST_WORKSPACE, arguments).await
    }

    /// Stop having the host `device` serve `workspace`, `caller`'s request: its
    /// address leaves the workspace's hosts at once, and the host is asked to stop
    /// ([`UNHOST_WORKSPACE`]) at `addr` when given. Answers whether the host
    /// confirmed it stopped; one that could not be reached is still no
    /// longer named to members.
    ///
    /// # Errors
    ///
    /// The caller may not choose the workspace's hosts, the workspace is not
    /// governed here, or its hosts could not be recorded.
    pub async fn detach_host(
        &self,
        workspace: &str,
        caller: &DeviceCert,
        device: [u8; 32],
        addr: Option<EndpointAddr>,
    ) -> Result<bool, String> {
        self.authorize_hosts(workspace, caller).await?;
        let known = {
            let governed = self
                .workspace(workspace)
                .await
                .map_err(|error| error.to_string())?;
            let mut governed = governed.lock().await;
            let known = governed
                .peer()
                .governance()
                .workspace_hosts()
                .iter()
                .find(|known| host_device(known) == device)
                .map(|known| {
                    WorkspaceDeviceHintV1::from_transport_hint(known)
                        .map(|address| address.endpoint_addr)
                })
                .and_then(Result::ok);
            governed.answered.remove(&device);
            known
        };
        self.name_host(workspace, |governance, key, now| {
            governance.author_workspace_host_removed(&device, key, now)
        })
        .await?;
        let _ = self.inner.changes.send(workspace.to_owned());
        let Some(addr) = addr.or(known) else {
            return Ok(false);
        };
        let stopped = async {
            let reached = self.reach_host(device, addr).await?;
            let mut arguments = Map::new();
            arguments.insert("workspace".into(), Value::String(workspace.to_owned()));
            reached.ask(UNHOST_WORKSPACE, arguments).await
        }
        .await;
        match stopped {
            Ok(_) => Ok(true),
            Err(error) => {
                tracing::info!(%workspace, host = %hex::encode(device), %error, "a detached host was not reached");
                Ok(false)
            }
        }
    }

    /// Answer the host `device` serving `workspace` as well, over the session
    /// `inbound` it opened here, with a session back to it
    /// ([`HostDialer::answer`]): once for each session it opens.
    /// Nothing when `device` does not serve the workspace.
    pub(crate) async fn answer_host(
        &self,
        workspace: &str,
        device: [u8; 32],
        inbound: tokio::sync::mpsc::Sender<()>,
    ) {
        let Ok(governed) = self.workspace(workspace).await else {
            return;
        };
        let mut governed = governed.lock().await;
        if inbound.is_closed()
            || governed
                .answered
                .get(&device)
                .is_some_and(|answered| answered.inbound.same_channel(&inbound))
        {
            return;
        }
        let Some((addr, person)) = governed
            .peer()
            .governance()
            .workspace_hosts()
            .iter()
            .find(|known| host_device(known) == device)
            .and_then(|known| WorkspaceDeviceHintV1::from_transport_hint(known).ok())
            .map(|address| (address.endpoint_addr, address.user_public_key))
        else {
            return;
        };
        let workspace_id = governed.workspace_id();
        let host = &self.inner.host;
        let session = HostDialer {
            endpoint: host.endpoint.clone(),
            identity: LiveIdentity {
                user_public_key: host.cert.user_public_key,
                cert: host.cert.clone(),
                signing_key: host.key.clone(),
                revocations: host.revocations.clone(),
            },
            trust_policy: host.trust_policy.clone(),
            workspaces: host.workspaces.clone(),
            metrics: host.metrics.clone(),
            scope: Some(vec![workspace_id]),
        }
        .answer(
            Upstream {
                addr,
                device,
                person,
            },
            inbound.clone(),
            format!("host {} of {workspace_id}", hex::encode(&device[..4])),
        )
        .abort_handle();
        governed
            .answered
            .insert(device, Answer { inbound, session });
    }

    /// Stop answering the host `device` for `workspace`: before this device opens
    /// a session of its own to it, which would take the answer's place
    /// there. The host's session here stays; the next one it opens is
    /// answered again.
    async fn stop_answering(&self, workspace: &str, device: [u8; 32]) {
        if let Ok(governed) = self.workspace(workspace).await {
            governed.lock().await.answered.remove(&device);
        }
    }

    /// The session the attached host `device` last opened to `workspace_id`
    /// here, while it lasts.
    async fn attach_session(
        &self,
        workspace_id: WorkspaceId,
        device: [u8; 32],
    ) -> Option<tokio::sync::mpsc::Sender<()>> {
        let workspace = self
            .inner
            .host
            .workspaces
            .lock()
            .await
            .get(&workspace_id)
            .cloned()?;
        workspace
            .lock()
            .await
            .attach_sessions
            .get(&device)
            .filter(|session| !session.is_closed())
            .cloned()
    }

    /// Whether `caller` may choose which hosts hold `workspace`: a device of
    /// this host's own person, or of an owner of the workspace (a person it is
    /// kept for), who decides where it lives; or that person's server key,
    /// which acts for them from a script or a command. Never an agent's grant.
    async fn authorize_hosts(&self, workspace: &str, caller: &DeviceCert) -> Result<(), String> {
        let governed = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let governed = governed.lock().await;
        if self.chooses_hosts(&governed, caller) {
            return Ok(());
        }
        Err("only an owner of the workspace chooses where it is held".into())
    }

    /// Whether `caller` chooses which hosts hold `governed`
    /// ([`Self::authorize_hosts`]).
    pub(super) fn chooses_hosts(&self, governed: &GovernedWorkspace, caller: &DeviceCert) -> bool {
        let person = caller.user_public_key;
        let speaks_for_person =
            caller.capabilities.agent.is_none() || caller.capabilities.server_key;
        speaks_for_person
            && (person == self.inner.host.cert.user_public_key
                || governed
                    .peer()
                    .governance()
                    .governed_policy()
                    .is_ok_and(|policy| policy.is_keeper(&hex::encode(person))))
    }

    /// The hosts holding `workspace`, as its owners list them: this host, its
    /// governor, first.
    ///
    /// # Errors
    ///
    /// The caller does not choose the workspace's hosts, or it is not governed
    /// here.
    pub async fn host_list(
        &self,
        workspace: &str,
        caller: &DeviceCert,
    ) -> Result<Vec<WorkspaceHost>, String> {
        self.authorize_hosts(workspace, caller).await?;
        let hosts = self
            .hosts(workspace)
            .await
            .map_err(|error| error.to_string())?;
        Ok(hosts
            .iter()
            .enumerate()
            .map(|(at, hint)| WorkspaceHost {
                device: hex::encode(hint.host.device_public_key),
                person: hex::encode(hint.host.user_public_key),
                governs: at == 0,
            })
            .collect())
    }

    /// Have the holder whose site is at `origin` (`https://tilo.sh`) hold
    /// `workspace` as well, `caller`'s request: the holder says who it is and
    /// where it is dialed at its origin, and is then handed the workspace
    /// ([`Self::attach_host`]), which it takes as its own rules for whom it
    /// keeps workspaces say.
    ///
    /// # Errors
    ///
    /// As [`Self::attach_host`], or the origin did not say who it is.
    pub async fn add_host(
        &self,
        workspace: &str,
        caller: &DeviceCert,
        origin: &str,
    ) -> Result<TransportHint, String> {
        self.authorize_hosts(workspace, caller).await?;
        let (device, addr) = host_at(origin, self.site_reach()).await?;
        self.attach_host(workspace, caller, device, addr).await
    }

    /// Which addresses this device asks when an owner names a holder by its
    /// site: those on the public internet, and its own network too only
    /// where its operator lets it call there, the one leave a person's model
    /// provider is called by ([`Self::call_private_model_addresses`]).
    fn site_reach(&self) -> Destinations {
        if self.inner.models.private_addresses() {
            Destinations::Any
        } else {
            Destinations::Public
        }
    }

    /// Stop having the holder `named` hold `workspace`: named by its site's
    /// origin, or by its device key (64 hex) as [`Self::host_list`] names it.
    /// Answers whether the holder confirmed it let the workspace go
    /// ([`Self::detach_host`]).
    ///
    /// # Errors
    ///
    /// The caller does not choose the workspace's holders, the holder is not one of
    /// the workspace's, or the origin did not say who it is.
    pub async fn remove_host(
        &self,
        workspace: &str,
        caller: &DeviceCert,
        named: &str,
    ) -> Result<bool, String> {
        self.authorize_hosts(workspace, caller).await?;
        let named = named.trim();
        let (device, addr) = if let Some(device) = device_key(named) {
            (device, None)
        } else {
            let (device, addr) = host_at(named, self.site_reach()).await?;
            (device, Some(addr))
        };
        if !self.names_host(workspace, device).await {
            return Err(format!("{named} does not hold the workspace"));
        }
        self.detach_host(workspace, caller, device, addr).await
    }

    /// `device`, a host at `addr`, reached for its host operations: its
    /// certificate, as its handshake proves it, and the session they are
    /// asked on. The handshake pins the device, whoever's it is. The session
    /// subscribes to nothing, so it takes no session of the device's place
    /// there.
    async fn reach_host(&self, device: [u8; 32], addr: EndpointAddr) -> Result<OtherHost, String> {
        let host = &self.inner.host;
        let revocations = host.revocations.read_recover().clone();
        let client = RelayClient::connect_with_signer(
            host.endpoint.clone(),
            addr,
            None,
            Some(device),
            tilo_relay_session::client::ClientCredentials {
                cert: host.cert.clone(),
                signer: &host.key,
                revocations,
                entering: None,
            },
        )
        .await
        .map_err(|error| match &error {
            // A host refuses a session it does not trust by closing it
            // unanswered: another person's host trusts one only when it keeps
            // workspaces for the people it lets in, or its operator takes
            // workspaces it does not own.
            tilo_relay_session::client::ClientError::NoHandshakeAnswer(_) => format!(
                "the host refused this host's session: it keeps workspaces only for the \
                 people it let in, and this one keeps none ({error})"
            ),
            other => other.to_string(),
        })?;
        Ok(OtherHost {
            certificate: client.relay_peer.cert.clone(),
            client,
        })
    }
}

/// Another host, reached for its host operations ([`Workspaces::reach_host`]).
struct OtherHost {
    /// Its certificate, as its handshake proved it.
    certificate: DeviceCert,
    client: RelayClient,
}

impl OtherHost {
    /// Ask the host the host operation `action` with `arguments`, and answer
    /// what it produced.
    async fn ask(&self, action: &str, arguments: Map<String, Value>) -> Result<Value, String> {
        let status = ask_host(&self.client, host_invocation(action, arguments)).await?;
        receipt_output(status)
    }
}

/// The invocation of the host operation `action` with `arguments`.
fn host_invocation(action: &str, arguments: Map<String, Value>) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: HOST_ACTIONS_PROVIDER.into(),
            address: ActionAddress::new(HOST_WORKSPACES_RESOURCE.to_owned(), action),
        },
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    }
}

/// What a host operation produced, from its receipt.
fn receipt_output(status: ReceiptStatus) -> Result<Value, String> {
    match status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(json!({}))),
        other => Err(format!("the host did not do it: {other:?}")),
    }
}

/// `value` as the hand-over sends it: postcard in hex.
fn hex_postcard(value: &impl serde::Serialize) -> Result<String, String> {
    postcard::to_stdvec(value)
        .map(hex::encode)
        .map_err(|error| error.to_string())
}

/// One host of a workspace, as its owners list them ([`Workspaces::host_list`]).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceHost {
    /// The host's device key, hex: what removing it may name it by.
    pub device: String,
    /// The person the host runs for, by their user public key, hex.
    pub person: String,
    /// Whether this is the host that governs the workspace: the one it was
    /// made on, which every other host follows.
    pub governs: bool,
}

/// `text` as a device key, when it is 64 hex characters; anything else names
/// a host by its site.
fn device_key(text: &str) -> Option<[u8; 32]> {
    use hex::FromHex as _;
    <[u8; 32]>::from_hex(text).ok()
}

/// How long a holder's site may take to say who it is.
const SITE_ANSWERS: Duration = Duration::from_secs(20);
/// The most read of what a site says of its holder.
const MAX_SITE_ANSWER: usize = 64 * 1024;

/// A site, asked through the function it holds.
struct Site<F>(F);

impl<F> HttpClient for Site<F>
where
    F: Fn(&str, Vec<u8>) -> HttpFuture<'static> + Send + Sync,
{
    fn post(&self, path: &str, body: Vec<u8>) -> HttpFuture<'_> {
        (self.0)(path, body)
    }
}

/// The holder whose site is at `origin`, as it says there: its device and
/// where it is dialed.
///
/// An owner names the site, and this device asks it from inside its own
/// network. So it asks as it asks a provider a person named: an https
/// address, on the public internet at the lookup the connection dials, and
/// no redirect is followed. With [`Destinations::Any`], its operator's
/// leave, an address on its own network is asked too, also over plain http.
/// What a site answers a refusal with is not handed on.
async fn host_at(
    origin: &str,
    destinations: Destinations,
) -> Result<([u8; 32], EndpointAddr), String> {
    let origin = origin.trim().trim_end_matches('/');
    let no_site = || {
        format!(
            "{origin:?} is no holder's address: name its site by its https address, like \
             https://tilo.sh"
        )
    };
    let url = tilo_net::Url::parse(origin).map_err(|_| no_site())?;
    let plain = url.scheme() == "http" && destinations == Destinations::Any;
    if !(url.scheme() == "https" || plain)
        || !url.username().is_empty()
        || url.password().is_some()
        || check_destination(&url, destinations).is_err()
    {
        return Err(no_site());
    }
    let client =
        guarded_client(destinations, system_lookup()).map_err(|error| error.to_string())?;
    let site = origin.to_owned();
    let asked = Site(move |path: &str, body: Vec<u8>| -> HttpFuture<'static> {
        let request = client
            .post(format!("{site}{path}"))
            .header("content-type", "application/octet-stream")
            .timeout(SITE_ANSWERS)
            .body(body);
        Box::pin(async move {
            let mut response = request
                .send()
                .await
                .map_err(|error| error.without_url().to_string())?;
            let status = response.status().as_u16();
            let mut answer = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| error.without_url().to_string())?
            {
                if answer.len().saturating_add(chunk.len()) > MAX_SITE_ANSWER {
                    return Err("it answered more than a holder says of itself".to_owned());
                }
                answer.extend_from_slice(&chunk);
            }
            Ok((status, answer))
        })
    });
    let info = tilo_relay_client::https::host_info(&asked)
        .await
        .map_err(|error| match error {
            // What a site answers a refusal with stays with it: an owner
            // names any site, and its words are not this device's to hand on.
            LinkError::Refused(_) => format!("{origin} refused to say which holder it is"),
            other => format!("{origin} did not say which holder it is: {other}"),
        })?;
    let address = info
        .address
        .ok_or_else(|| format!("{origin} names no address to reach its host at"))?;
    Ok((info.host_cert.device_public_key, address.endpoint_addr))
}

impl Workspaces {
    /// The signed Iroh address a joining device dials to redeem an
    /// invitation of `workspace`: the relay this host is reached through and
    /// the addresses a stranger can dial, never the ones only the network
    /// around this host knows (`tilo_delivery_protocol::workspace_join::reachable`).
    pub(super) fn transport_hint(
        &self,
        workspace: &GovernedWorkspace,
    ) -> Result<tilo_peer_state::join::TransportHint, CollaborationError> {
        let host = &self.inner.host;
        // An invitation is handed to whoever the link reaches, so what this
        // host answers at inside its own network stays out of it.
        let endpoint_addr =
            tilo_delivery_protocol::workspace_join::reachable(&host.endpoint.addr());
        let packet_relay_url = endpoint_addr
            .relay_urls()
            .next()
            .map_or_else(|| "iroh:direct".to_owned(), ToString::to_string);
        tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1 {
            version: 1,
            holds_keys: true,
            runs_work: !host.work.takes_nothing(),
            workspace_id: workspace.workspace_id(),
            endpoint_addr,
            user_public_key: host.cert.user_public_key,
            device_certificate: host.cert.clone(),
            packet_relay_url,
        }
        .transport_hint(&host.key)
        .map_err(|error| CollaborationError::new(error.to_string()))
    }
}
