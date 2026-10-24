//! The hosts of a room this host governs: itself first, then each host of
//! its person it asked to serve the room as well ([`crate::hosted`]).
//!
//! Each host signs its own address; the governor keeps the list beside the
//! room (`<room>.hosts`, its own record, not the room's governance) and the
//! room's join answers carry it, governor first, so a member reaches the room
//! wherever it is served. A hosting member is a device of the governor's own
//! person that may act as a host; attaching it admits that device to the
//! room as one of the owner's, which is how the room's governance tells every
//! member it may hold the room.

use std::path::PathBuf;

use iroh_base::EndpointAddr;
use serde_json::{Map, Value, json};
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
    RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostMode};
use tilo_peer_state::join::{MemberRole, TransportHint};
use tilo_relay_session::client::RelayClient;
use tilo_relay_session::rooms::{
    HOST_ACTIONS_PROVIDER, HOST_ROOM, HOST_ROOMS_RESOURCE, HostedRoom, UNHOST_ROOM,
};
use tilo_user_identity::DeviceCert;

use super::authority::RoomOp;
use super::{CollaborationError, GovernedRoom, Rooms};
use crate::federation::{PeerRelayDialer, Upstream};
use crate::handshake::LiveIdentity;
use crate::host_authority::HostOperation;
use crate::transport::{Envelope, REMOTE_ACTION_ENVELOPE_VERSION};
use tilo_lock::RwLockRecover as _;

/// Where the hosts serving `room` beside the governor are recorded.
pub(super) fn hosts_path(room: &std::path::Path) -> PathBuf {
    room.with_extension("hosts")
}

/// The hosts a room's governor recorded beside it, each its own signed
/// address; none when it recorded none.
pub(super) fn load_hosts(room: &std::path::Path) -> Result<Vec<TransportHint>, String> {
    let path = hosts_path(room);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let encoded: Vec<String> = serde_json::from_str(&text).map_err(|error| error.to_string())?;
    encoded
        .iter()
        .map(|hint| crate::hosted::decode_hex(hint))
        .collect()
}

fn save_hosts(room: &std::path::Path, hosts: &[TransportHint]) -> Result<(), String> {
    use std::io::Write as _;
    let encoded = hosts
        .iter()
        .map(hex_postcard)
        .collect::<Result<Vec<_>, _>>()?;
    let bytes = serde_json::to_vec_pretty(&encoded).map_err(|error| error.to_string())?;
    let path = hosts_path(room);
    let staged = path.with_extension("hosts.tmp");
    let mut file = std::fs::File::create(&staged).map_err(|error| error.to_string())?;
    file.write_all(&bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    std::fs::rename(&staged, &path).map_err(|error| error.to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::File::open(dir)
            .and_then(|dir| dir.sync_all())
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// This host's answer to the session a host serving one of its rooms opened
/// here ([`Rooms::answer_host`]).
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

impl Rooms {
    /// Every host of `room`, each its own signed address: this host first.
    ///
    /// # Errors
    ///
    /// The room is not governed here, or this host's address cannot be
    /// signed.
    pub async fn hosts(&self, room: &str) -> Result<Vec<TransportHint>, CollaborationError> {
        let governed = self.room(room).await?;
        let mut governed = governed.lock().await;
        self.refresh_hosts(&mut governed)?;
        Ok(governed.hosts.clone())
    }

    /// Bring `room`'s list of hosts up to date with this host's own address.
    pub(super) fn refresh_hosts(&self, room: &mut GovernedRoom) -> Result<(), CollaborationError> {
        let own = self.transport_hint(room)?;
        room.hosts = std::iter::once(own)
            .chain(room.member_hosts.iter().cloned())
            .collect();
        Ok(())
    }

    /// Have the host `device` of this host's own person, at `addr`, serve
    /// `room` as well, `caller`'s request: admit the device to the room as
    /// one of the owner's, hand the room over with [`HOST_ROOM`], and record
    /// the address it answers. Asked again, the host is handed the room
    /// afresh. Answers that address.
    ///
    /// # Errors
    ///
    /// The caller is not a device of this host's person, the room is not
    /// governed here, the host cannot be reached or is not a certified host
    /// of this person, or it refused or answered an address that is not its
    /// own for the room.
    pub async fn attach_host(
        &self,
        room: &str,
        caller: &DeviceCert,
        device: [u8; 32],
        addr: EndpointAddr,
    ) -> Result<TransportHint, String> {
        let host = &self.inner.host;
        let now = tilo_clock::unix_now_ns();
        self.authorize(caller, None, RoomOp::Host(HostOperation::AttachHost), now)
            .map_err(|error| error.to_string())?;
        if device == host.cert.device_public_key {
            return Err("a room's governor hosts it already".into());
        }
        self.stop_answering(room, device).await;
        let client = self.dial_host(device, addr).await?;
        let member = client.relay_peer.cert.clone();
        self.enter(room, &member)
            .await
            .map_err(|error| error.to_string())?;
        let (workspace_id, governor_hint, bootstrap) = {
            let governed = self.room(room).await.map_err(|error| error.to_string())?;
            let governed = governed.lock().await;
            let mut bootstrap =
                governed
                    .governed()
                    .bootstrap([0; 32], member.clone(), MemberRole::Editor);
            // The host follows governance alone: records reach it through the
            // room's federation, and leaving them here keeps the hand-over
            // within one action frame.
            bootstrap.records.clear();
            bootstrap.governance_messages.retain(|bytes| {
                !matches!(
                    postcard::from_bytes::<neighbour_wire::NeighbourMsg>(bytes),
                    Ok(neighbour_wire::NeighbourMsg::AppDocument { .. })
                )
            });
            let hint = self
                .transport_hint(&governed)
                .map_err(|error| error.to_string())?;
            (governed.workspace_id(), hint, bootstrap)
        };
        let mut arguments = Map::new();
        arguments.insert("room".into(), Value::String(room.to_owned()));
        arguments.insert("workspace_id".into(), Value::from(workspace_id));
        arguments.insert(
            "governor_hint".into(),
            Value::String(hex_postcard(&governor_hint)?),
        );
        arguments.insert("bootstrap".into(), Value::String(hex_postcard(&bootstrap)?));
        let answer = ask_host(&client, device, workspace_id, HOST_ROOM, arguments).await?;
        let hosted: HostedRoom =
            serde_json::from_value(answer).map_err(|error| error.to_string())?;
        let hint: TransportHint = crate::hosted::decode_hex(&hosted.host_hint)?;
        let address = IrohWorkspaceHostHintV1::from_transport_hint(&hint)
            .map_err(|error| error.to_string())?;
        if hint.host != member
            || address.mode != WorkspaceHostMode::CertifiedHost
            || address.room != room
            || address.workspace_id != workspace_id
            || address.governing_authority != host.cert
        {
            return Err("the host answered an address that is not its own for the room".into());
        }
        let governed = self.room(room).await.map_err(|error| error.to_string())?;
        let mut governed = governed.lock().await;
        let mut member_hosts = governed.member_hosts.clone();
        member_hosts.retain(|known| host_device(known) != device);
        member_hosts.push(hint.clone());
        save_hosts(&governed.path, &member_hosts)?;
        governed.member_hosts = member_hosts;
        self.refresh_hosts(&mut governed)
            .map_err(|error| error.to_string())?;
        drop(governed);
        // The host dials this device as it takes the room up, perhaps before
        // its address was recorded above: a session it opened already is
        // answered here, one it opens later when it says hello.
        if let Some(inbound) = self.peer_relay_session(workspace_id, device).await {
            self.answer_host(room, device, inbound).await;
        }
        tracing::info!(%room, host = %hex::encode(device), "a host of this person serves the room");
        let _ = self.inner.changes.send(room.to_owned());
        Ok(hint)
    }

    /// Stop having the host `device` serve `room`, `caller`'s request: its
    /// address leaves the room's hosts at once, and the host is asked to stop
    /// ([`UNHOST_ROOM`]) at `addr` when given. Answers whether the host
    /// confirmed it stopped; one that could not be reached is still no
    /// longer named to members.
    ///
    /// # Errors
    ///
    /// The caller is not a device of this host's person, the room is not
    /// governed here, or its hosts could not be recorded.
    pub async fn detach_host(
        &self,
        room: &str,
        caller: &DeviceCert,
        device: [u8; 32],
        addr: Option<EndpointAddr>,
    ) -> Result<bool, String> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(caller, None, RoomOp::Host(HostOperation::DetachHost), now)
            .map_err(|error| error.to_string())?;
        let (workspace_id, known) = {
            let governed = self.room(room).await.map_err(|error| error.to_string())?;
            let mut governed = governed.lock().await;
            let mut member_hosts = governed.member_hosts.clone();
            let known = member_hosts
                .iter()
                .find(|known| host_device(known) == device)
                .map(|known| {
                    IrohWorkspaceHostHintV1::from_transport_hint(known)
                        .map(|address| address.endpoint_addr)
                })
                .and_then(Result::ok);
            member_hosts.retain(|known| host_device(known) != device);
            save_hosts(&governed.path, &member_hosts)?;
            governed.member_hosts = member_hosts;
            governed.answered.remove(&device);
            self.refresh_hosts(&mut governed)
                .map_err(|error| error.to_string())?;
            (governed.workspace_id(), known)
        };
        let _ = self.inner.changes.send(room.to_owned());
        let Some(addr) = addr.or(known) else {
            return Ok(false);
        };
        let stopped = async {
            let client = self.dial_host(device, addr).await?;
            let mut arguments = Map::new();
            arguments.insert("room".into(), Value::String(room.to_owned()));
            ask_host(&client, device, workspace_id, UNHOST_ROOM, arguments).await
        }
        .await;
        match stopped {
            Ok(_) => Ok(true),
            Err(error) => {
                tracing::info!(%room, host = %hex::encode(device), %error, "a detached host was not reached");
                Ok(false)
            }
        }
    }

    /// Answer the host `device` serving `room` as well, over the session
    /// `inbound` it opened here, with a session back to it
    /// ([`PeerRelayDialer::answer`]): once for each session it opens.
    /// Nothing when `device` does not serve the room.
    pub(crate) async fn answer_host(
        &self,
        room: &str,
        device: [u8; 32],
        inbound: tokio::sync::mpsc::Sender<()>,
    ) {
        let Ok(governed) = self.room(room).await else {
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
            .member_hosts
            .iter()
            .find(|known| host_device(known) == device)
            .and_then(|known| IrohWorkspaceHostHintV1::from_transport_hint(known).ok())
            .map(|address| address.endpoint_addr)
        else {
            return;
        };
        let workspace_id = governed.workspace_id();
        let host = &self.inner.host;
        let session = PeerRelayDialer {
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
            format!("host {} of {room}", hex::encode(&device[..4])),
        )
        .abort_handle();
        governed
            .answered
            .insert(device, Answer { inbound, session });
    }

    /// Stop answering the host `device` for `room`: before this device opens
    /// a session of its own to it, which would take the answer's place
    /// there. The host's session here stays; the next one it opens is
    /// answered again.
    async fn stop_answering(&self, room: &str, device: [u8; 32]) {
        if let Ok(governed) = self.room(room).await {
            governed.lock().await.answered.remove(&device);
        }
    }

    /// The session the federated host `device` last opened to `workspace_id`
    /// here, while it lasts.
    async fn peer_relay_session(
        &self,
        workspace_id: u64,
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
            .peer_relay_sessions
            .get(&device)
            .filter(|session| !session.is_closed())
            .cloned()
    }

    /// A session to `device`, a certified host of this host's person.
    async fn dial_host(&self, device: [u8; 32], addr: EndpointAddr) -> Result<RelayClient, String> {
        let host = &self.inner.host;
        let revocations = host.revocations.read_recover().clone();
        RelayClient::connect_v2_to_enrolled_home_peer(
            host.endpoint.clone(),
            addr,
            host.cert.user_public_key,
            device,
            host.cert.clone(),
            host.key.clone(),
            revocations,
        )
        .await
        .map_err(|error| error.to_string())
    }
}

/// `value` as the hand-over sends it: postcard in hex.
fn hex_postcard(value: &impl serde::Serialize) -> Result<String, String> {
    postcard::to_stdvec(value)
        .map(hex::encode)
        .map_err(|error| error.to_string())
}

/// Ask `client`'s host, the device `host`, the host operation `action` with
/// `arguments`, on `workspace_id`, and answer what it produced.
async fn ask_host(
    client: &RelayClient,
    host: [u8; 32],
    workspace_id: u64,
    action: &str,
    arguments: Map<String, Value>,
) -> Result<Value, String> {
    client
        .send(Envelope::Subscribe { workspace_id })
        .map_err(|_| "the host's session closed".to_string())?;
    loop {
        match client.recv().await {
            Some(Envelope::Ready {
                workspace_id: ready,
                ..
            }) if ready == workspace_id => break,
            Some(Envelope::NotAuthorised {
                workspace_id: refused,
                reason,
            }) if refused == workspace_id => {
                return Err(format!("the host refused the room's workspace: {reason}"));
            }
            Some(_) => {}
            None => return Err("the host's session closed".into()),
        }
    }
    let invocation = InvocationId::new();
    let frame = RemoteActionFrame::new(RemoteActionMessage::Invoke(InvokeRequest {
        invocation,
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
    }))
    .encode()
    .map_err(|error| error.to_string())?;
    client
        .send(Envelope::RemoteAction {
            workspace_id,
            version: REMOTE_ACTION_ENVELOPE_VERSION,
            to_device_public_key: host,
            from_device_public_key: [0; 32],
            from_user_public_key: [0; 32],
            frame,
        })
        .map_err(|_| "the host's session closed".to_string())?;
    loop {
        match client.recv().await {
            Some(Envelope::RemoteAction { frame, .. }) => {
                let message = RemoteActionFrame::decode(&frame)
                    .map_err(|error| error.to_string())?
                    .message;
                let RemoteActionMessage::Receipt(receipt) = message else {
                    continue;
                };
                if receipt.invocation != invocation {
                    continue;
                }
                return match receipt.status {
                    ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(json!({}))),
                    other => Err(format!("the host did not do it: {other:?}")),
                };
            }
            Some(Envelope::RemoteActionUndeliverable { reason, .. }) => {
                return Err(format!("the host did not take the action: {reason}"));
            }
            Some(_) => {}
            None => return Err("the host's session closed".into()),
        }
    }
}
