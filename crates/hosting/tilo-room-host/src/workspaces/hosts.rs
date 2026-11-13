//! The hosts of a workspace this host governs: itself first, then each host of
//! its person it asked to serve the workspace as well ([`crate::hosted`]).
//!
//! Each host signs its own address. The workspace's governance names the hosts
//! serving it beside the governor ([`tilo_peer_state::WORKSPACE_HOSTS_KEY`]), so
//! every member learns of each as it learns any change to the workspace, and the
//! workspace's join answers carry them all, governor first: a member reaches the
//! workspace wherever it is served. A hosting member is a device of the
//! governor's own person that may act as a host; attaching it admits that
//! device to the workspace as one of the owner's, which is how the workspace's
//! governance tells every member it may hold the workspace.

use std::sync::Arc;

use iroh_base::EndpointAddr;
use serde_json::{Map, Value, json};
use tilo_actions::remote::{InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostMode};
use tilo_peer_state::join::{Role, TransportHint};
use tilo_relay_session::client::RelayClient;
use tilo_relay_session::workspaces::{
    HOST_ACTIONS_PROVIDER, HOST_ROOM, HOST_ROOMS_RESOURCE, HostedWorkspace, UNHOST_ROOM, ask_host,
};
use tilo_user_identity::DeviceCert;

use super::authority::WorkspaceOp;
use super::{CollaborationError, GovernedWorkspace, Workspaces};
use crate::attach::{HostDialer, Upstream};
use crate::handshake::LiveIdentity;
use crate::host_authority::HostOperation;
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

    /// Have the host `device` of this host's own person, at `addr`, serve
    /// `workspace` as well, `caller`'s request: admit the device to the workspace as
    /// one of the owner's, hand the workspace over with [`HOST_ROOM`], and record
    /// the address it answers. Asked again, the host is handed the workspace
    /// afresh. Answers that address.
    ///
    /// # Errors
    ///
    /// The caller is not a device of this host's person, the workspace is not
    /// governed here, the host cannot be reached or is not a certified host
    /// of this person, or it refused or answered an address that is not its
    /// own for the workspace.
    pub async fn attach_host(
        &self,
        workspace: &str,
        caller: &DeviceCert,
        device: [u8; 32],
        addr: EndpointAddr,
    ) -> Result<TransportHint, String> {
        let host = &self.inner.host;
        let now = tilo_clock::unix_now_ns();
        self.authorize(
            caller,
            None,
            WorkspaceOp::Host(HostOperation::AttachHost),
            now,
        )
        .map_err(|error| error.to_string())?;
        if device == host.cert.device_public_key {
            return Err("a room's governor hosts it already".into());
        }
        self.stop_answering(workspace, device).await;
        let reached = self.reach_host(device, addr).await?;
        let member = reached.certificate.clone();
        self.enter(workspace, &member)
            .await
            .map_err(|error| error.to_string())?;
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
        arguments.insert("room".into(), Value::String(workspace.to_owned()));
        arguments.insert(
            "workspace_id".into(),
            Value::String(workspace_id.to_string()),
        );
        arguments.insert(
            "governor_hint".into(),
            Value::String(hex_postcard(&governor_hint)?),
        );
        arguments.insert("bootstrap".into(), Value::String(hex_postcard(&bootstrap)?));
        let answer = reached.ask(HOST_ROOM, arguments).await?;
        let hosted: HostedWorkspace =
            serde_json::from_value(answer).map_err(|error| error.to_string())?;
        let hint: TransportHint = crate::hosted::decode_hex(&hosted.host_hint)?;
        let address = IrohWorkspaceHostHintV1::from_transport_hint(&hint)
            .map_err(|error| error.to_string())?;
        if hint.host != member
            || address.mode != WorkspaceHostMode::CertifiedHost
            || address.workspace_id != workspace_id
        {
            return Err("the host answered an address that is not its own for the room".into());
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
        tracing::info!(%workspace, host = %hex::encode(device), "a host of this person serves the room");
        let _ = self.inner.changes.send(workspace.to_owned());
        Ok(hint)
    }

    /// Stop having the host `device` serve `workspace`, `caller`'s request: its
    /// address leaves the workspace's hosts at once, and the host is asked to stop
    /// ([`UNHOST_ROOM`]) at `addr` when given. Answers whether the host
    /// confirmed it stopped; one that could not be reached is still no
    /// longer named to members.
    ///
    /// # Errors
    ///
    /// The caller is not a device of this host's person, the workspace is not
    /// governed here, or its hosts could not be recorded.
    pub async fn detach_host(
        &self,
        workspace: &str,
        caller: &DeviceCert,
        device: [u8; 32],
        addr: Option<EndpointAddr>,
    ) -> Result<bool, String> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(
            caller,
            None,
            WorkspaceOp::Host(HostOperation::DetachHost),
            now,
        )
        .map_err(|error| error.to_string())?;
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
                    IrohWorkspaceHostHintV1::from_transport_hint(known)
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
            arguments.insert("room".into(), Value::String(workspace.to_owned()));
            reached.ask(UNHOST_ROOM, arguments).await
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
        let Some(addr) = governed
            .peer()
            .governance()
            .workspace_hosts()
            .iter()
            .find(|known| host_device(known) == device)
            .and_then(|known| IrohWorkspaceHostHintV1::from_transport_hint(known).ok())
            .map(|address| address.endpoint_addr)
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
            workspaces: host.workspaces.clone(),
            metrics: host.metrics.clone(),
            scope: Some(vec![workspace_id]),
        }
        .answer(
            Upstream::OwnHost { addr, device },
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

    /// `device`, a certified host of this host's person at `addr`, reached
    /// for its host operations: its certificate, as its handshake proves
    /// it, and the session they are asked on. The session subscribes to
    /// nothing, so it takes no session of the device's place there.
    async fn reach_host(
        &self,
        device: [u8; 32],
        addr: EndpointAddr,
    ) -> Result<PersonsHost, String> {
        let host = &self.inner.host;
        let revocations = host.revocations.read_recover().clone();
        let client = RelayClient::connect_v2_to_enrolled_home_peer(
            host.endpoint.clone(),
            addr,
            host.cert.user_public_key,
            device,
            host.cert.clone(),
            host.key.clone(),
            revocations,
        )
        .await
        .map_err(|error| error.to_string())?;
        Ok(PersonsHost {
            certificate: client.relay_peer.cert.clone(),
            client,
        })
    }
}

/// A certified host of this host's person, reached for its host operations
/// ([`Workspaces::reach_host`]).
struct PersonsHost {
    /// Its certificate, as its handshake proved it.
    certificate: DeviceCert,
    client: RelayClient,
}

impl PersonsHost {
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
            address: ActionAddress::new(HOST_ROOMS_RESOURCE.to_owned(), action),
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
        tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1 {
            version: 1,
            mode: host.mode,
            workspace_id: workspace.workspace_id(),
            endpoint_addr,
            host_user_public_key: host.cert.user_public_key,
            host_device_certificate: host.cert.clone(),
            packet_relay_url,
        }
        .transport_hint(&host.key)
        .map_err(|error| CollaborationError::new(error.to_string()))
    }
}
