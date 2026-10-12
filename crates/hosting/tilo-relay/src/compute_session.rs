//! Dedicated authenticated session for personal-mesh compute control.
//!
//! This protocol intentionally does not extend the frozen relay-v1 postcard
//! enum. It has its own ALPN, frame ceiling, client, and server handler while
//! reusing the same certified-device challenge/response authentication.
//!
//! The first stream of a connection is the control stream: the handshake,
//! then [`ComputeEnvelope`]s. Compute jobs and worker advertisements are the
//! relay owner's devices' alone. Every further stream is one environment
//! session, legacy-windows view or legacy launch
//! ([`crate::environment_routing`]); those are open to any device the trust
//! policy admits, and each is admitted by its workspace's membership.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use iroh::endpoint::{Connection, Endpoint, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh_base::EndpointAddr;
use thiserror::Error;
use tilo_clock::{unix_now_ms, unix_now_ns};
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::mpsc;

use crate::auth::{self, AuthenticatedPeer, Hello, HelloAck};
use crate::compute::ComputeJobSpec;
use crate::compute_transport::{
    COMPUTE_ALPN, ComputeEnvelope, ComputeJobBinding, ComputeRouteOutcome, ComputeTransportError,
    MAX_COMPUTE_FRAME_BYTES,
};
use crate::frame::{read_frame, write_frame};
use crate::handshake::{EndpointTrust, HandshakeError, LiveIdentity};
use crate::peer::WorkspaceRegistry;
use crate::policy::TrustPolicy;
use crate::room_access::{self, Decision, DenyReason, RelayTrust, RoomOperation};
use crate::workspace::SharedWorkspace;

const COMPUTE_OUTBOUND_CAPACITY: usize = 32;
static COMPUTE_QUEUE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error)]
/// Failure while authenticating or using the dedicated compute stream.
pub enum ComputeSessionError {
    /// Framing or stream I/O failed.
    #[error("compute transport I/O: {0}")]
    Io(#[from] std::io::Error),
    /// A postcard message was invalid.
    #[error("compute transport codec: {0}")]
    Codec(#[from] postcard::Error),
    /// Certified-device authentication failed.
    #[error("compute transport authentication: {0}")]
    Auth(#[from] crate::auth::AuthError),
    /// A compute route, lifecycle, or bound was rejected.
    #[error("compute transport policy: {0}")]
    Transport(#[from] ComputeTransportError),
    /// Durable workspace state failed.
    #[error("compute transport store: {0}")]
    Store(#[from] crate::store::StoreError),
    /// The peer violated the compute protocol state machine.
    #[error("compute transport protocol violation: {0}")]
    Protocol(String),
    /// The Iroh connection could not be established.
    #[error("compute transport connection: {0}")]
    Connection(String),
}

impl From<HandshakeError> for ComputeSessionError {
    fn from(error: HandshakeError) -> Self {
        match error {
            HandshakeError::Io { source, .. } => Self::Io(source),
            HandshakeError::Codec { source, .. } => Self::Codec(source),
            HandshakeError::Auth(error) => Self::Auth(error),
        }
    }
}

/// Iroh handler for the versioned personal-compute ALPN.
#[derive(Clone)]
pub(crate) struct ComputeProtocol {
    workspaces: WorkspaceRegistry,
    identity: LiveIdentity,
    trust_policy: Arc<std::sync::RwLock<TrustPolicy>>,
    agents: crate::peer::AgentRegistry,
    metering: Arc<dyn crate::environment_host::Metering>,
}

/// The relay-wide trust the compute ALPN admits by.
pub(crate) struct ComputeTrust {
    pub(crate) trust_policy: Arc<std::sync::RwLock<TrustPolicy>>,
    pub(crate) agents: crate::peer::AgentRegistry,
}

impl ComputeProtocol {
    pub(crate) fn new(
        workspaces: WorkspaceRegistry,
        identity: LiveIdentity,
        trust: ComputeTrust,
        metering: Arc<dyn crate::environment_host::Metering>,
    ) -> Self {
        Self {
            workspaces,
            identity,
            trust_policy: trust.trust_policy,
            agents: trust.agents,
            metering,
        }
    }

    fn trust(&self) -> RelayTrust<'_> {
        RelayTrust {
            revocations: &self.identity.revocations,
            policy: &self.trust_policy,
            own_user: &self.identity.user_public_key,
            agents: &self.agents,
        }
    }

    fn routing(&self) -> crate::environment_routing::RoutingContext {
        crate::environment_routing::RoutingContext {
            workspaces: self.workspaces.clone(),
            own_user_public_key: self.identity.user_public_key,
            trust_policy: Arc::clone(&self.trust_policy),
            revocations: Arc::clone(&self.identity.revocations),
            agents: Arc::clone(&self.agents),
            metering: Arc::clone(&self.metering),
        }
    }
}

impl std::fmt::Debug for ComputeProtocol {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ComputeProtocol")
    }
}

impl ProtocolHandler for ComputeProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let this = self.clone();
        let (send, receive) = connection.accept_bi().await?;
        tokio::spawn(async move {
            if let Err(error) = run_server_session(this, connection.clone(), send, receive).await {
                tracing::debug!(%error, "personal compute session ended");
                connection.close(0u32.into(), b"compute session ended");
            }
        });
        Ok(())
    }
}

async fn run_server_session(
    protocol: ComputeProtocol,
    connection: Connection,
    mut send: SendStream,
    mut receive: RecvStream,
) -> Result<(), ComputeSessionError> {
    let peer = authenticate_server(&protocol, &mut send, &mut receive).await?;
    let owner = peer.user_public_key == protocol.identity.user_public_key;
    let streams = tokio::spawn(accept_streams(
        protocol.routing(),
        peer.clone(),
        connection.clone(),
    ));
    let _abort_streams = AbortOnDrop(streams);
    let (outbound, mut outbound_rx) = mpsc::channel(COMPUTE_OUTBOUND_CAPACITY);
    let (disconnect, mut disconnect_rx) = mpsc::channel(1);
    let mut attached = Vec::new();

    let outcome = async {
        loop {
            tokio::select! {
                biased;
                _ = connection.closed() => break Ok(()),
                _ = disconnect_rx.recv() => {
                    break Err(ComputeSessionError::Protocol("outbound compute queue saturated".into()));
                }
                outgoing = outbound_rx.recv() => {
                    let Some(outgoing) = outgoing else { break Ok(()); };
                    write_compute(&mut send, &outgoing).await?;
                }
                incoming = read_compute(&mut receive) => {
                    let incoming = incoming?;
                    if !owner {
                        // Another user's device reaches environments on its
                        // own streams; the control stream is the owner's.
                        if matches!(incoming, ComputeEnvelope::Ping) {
                            write_compute(&mut send, &ComputeEnvelope::Pong).await?;
                            continue;
                        }
                        break Err(ComputeSessionError::Protocol(
                            "compute control is reserved to the relay owner's devices".into(),
                        ));
                    }
                    if let ComputeEnvelope::Attach { workspace_id } = incoming {
                        let admitted = attach_workspace(
                            &protocol,
                            &peer,
                            workspace_id,
                            (&outbound, &disconnect),
                            &mut send,
                            &connection,
                        ).await?;
                        if admitted && !attached.contains(&workspace_id) {
                            attached.push(workspace_id);
                        }
                        continue;
                    }
                    if let Some(workspace_id) = incoming.workspace_id()
                        && !attached.contains(&workspace_id)
                    {
                        break Err(ComputeSessionError::Protocol(
                            "compute message sent before workspace attach".into(),
                        ));
                    }
                    if let Some(reply) = dispatch_compute(&protocol.workspaces, &peer, incoming).await? {
                        write_compute(&mut send, &reply).await?;
                    }
                }
            }
        }
    }
    .await;

    for workspace_id in attached {
        if let Some(workspace) = find_workspace(&protocol.workspaces, workspace_id).await {
            workspace
                .lock()
                .await
                .compute
                .detach(&peer.device_public_key);
        }
    }
    outcome
}

/// Serve every further stream of an authenticated connection.
async fn accept_streams(
    routing: crate::environment_routing::RoutingContext,
    peer: AuthenticatedPeer,
    connection: Connection,
) {
    while let Ok((send, receive)) = connection.accept_bi().await {
        let routing = routing.clone();
        let peer = peer.clone();
        tokio::spawn(async move {
            if let Err(error) =
                crate::environment_routing::serve_stream(routing, peer, send, receive).await
            {
                tracing::debug!(%error, "environment stream ended");
            }
        });
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Clone a workspace handle without carrying the registry lock into work on
/// that workspace. Registry membership and workspace state have independent
/// lock domains; nesting them needlessly serializes unrelated sessions.
async fn find_workspace(
    workspaces: &WorkspaceRegistry,
    workspace_id: u64,
) -> Option<SharedWorkspace> {
    workspaces.lock().await.get(&workspace_id).cloned()
}

/// The compute handshake runs under the live policy; the control stream is
/// then the owner's alone and every further stream is admitted by its
/// workspace.
async fn authenticate_server(
    protocol: &ComputeProtocol,
    send: &mut SendStream,
    receive: &mut RecvStream,
) -> Result<AuthenticatedPeer, ComputeSessionError> {
    Ok(protocol
        .identity
        .authenticate(
            EndpointTrust::Live(&protocol.trust_policy),
            receive,
            send,
            MAX_COMPUTE_FRAME_BYTES,
        )
        .await?)
}

/// Admit `peer` to `workspace`'s compute plane, enrolling a same-owner
/// device first. Membership comes from the same reload as every routing read,
/// so a pending or failed replacement's ceiling bounds it too; the enrolment
/// is the serialized async admission, never filesystem work under the
/// routing lock.
///
/// # Errors
///
/// The store's error when membership cannot be read or the enrolment cannot
/// be persisted; `Ok(Err(reason))` when the device is refused.
pub(crate) async fn admit_compute_peer(
    workspace: &SharedWorkspace,
    trust: &RelayTrust<'_>,
    peer: &AuthenticatedPeer,
) -> Result<Result<(), DenyReason>, crate::store::StoreError> {
    let (decision, revision) = {
        let runtime = crate::workspace_io::fresh_membership(workspace).await?;
        (
            room_access::decide(&runtime, trust, peer, RoomOperation::Compute),
            runtime.membership_revision,
        )
    };
    match decision {
        Decision::Admit => Ok(Ok(())),
        Decision::Deny(reason) => Ok(Err(reason)),
        Decision::AdmitAndEnrol => {
            crate::workspace_io::admit_member(workspace, peer.device_public_key, revision).await?;
            let runtime = crate::workspace_io::fresh_membership(workspace).await?;
            Ok(
                match room_access::decide(&runtime, trust, peer, RoomOperation::Compute) {
                    Decision::Admit => Ok(()),
                    Decision::AdmitAndEnrol => Err(DenyReason::NotAMember),
                    Decision::Deny(reason) => Err(reason),
                },
            )
        }
    }
}

async fn attach_workspace(
    protocol: &ComputeProtocol,
    peer: &AuthenticatedPeer,
    workspace_id: u64,
    (outbound, disconnect): (&mpsc::Sender<ComputeEnvelope>, &mpsc::Sender<()>),
    send: &mut SendStream,
    connection: &Connection,
) -> Result<bool, ComputeSessionError> {
    let Some(workspace) = find_workspace(&protocol.workspaces, workspace_id).await else {
        write_compute(
            send,
            &ComputeEnvelope::NotAuthorised {
                workspace_id,
                reason: "workspace not bound on this relay".into(),
            },
        )
        .await?;
        return Ok(false);
    };
    let trust = protocol.trust();
    let admission = admit_compute_peer(&workspace, &trust, peer).await?;
    let pending = match admission {
        Ok(()) => {
            // The backlog is a directory scan: blocking I/O, so it runs on
            // the blocking pool. The workspace lock moves with it, so nothing
            // is queued between the scan and the attach below.
            let device = peer.device_public_key;
            let workspace = workspace.lock_owned().await;
            let (mut workspace, pending) = tokio::task::spawn_blocking(move || {
                let pending = workspace.store.load_compute_pending(&device);
                (workspace, pending)
            })
            .await
            .map_err(|_| std::io::Error::other("compute backlog scan panicked"))?;
            let pending = pending?;
            // Admission may have changed while the lane and the scan ran.
            match room_access::decide(&workspace, &trust, peer, RoomOperation::Compute) {
                Decision::Admit => {
                    workspace.compute.attach(
                        device,
                        outbound.clone(),
                        disconnect.clone(),
                        Some(connection.clone()),
                    );
                    drop(workspace);
                    Ok(pending)
                }
                Decision::AdmitAndEnrol => Err(DenyReason::NotAMember),
                Decision::Deny(reason) => Err(reason),
            }
        }
        Err(reason) => Err(reason),
    };
    let pending = match pending {
        Ok(pending) => pending,
        Err(reason) => {
            write_compute(
                send,
                &ComputeEnvelope::NotAuthorised {
                    workspace_id,
                    reason: reason.reason().into(),
                },
            )
            .await?;
            return Ok(false);
        }
    };
    let pending_count = u32::try_from(pending.len()).unwrap_or(u32::MAX);
    for entry in pending {
        if entry.bytes.len() > MAX_COMPUTE_FRAME_BYTES {
            return Err(ComputeSessionError::Protocol(
                "persisted compute frame exceeds protocol bound".into(),
            ));
        }
        let envelope: ComputeEnvelope = postcard::from_bytes(&entry.bytes)?;
        write_compute(send, &envelope).await?;
    }
    write_compute(
        send,
        &ComputeEnvelope::Ready {
            workspace_id,
            pending: pending_count,
        },
    )
    .await?;
    Ok(true)
}

async fn dispatch_compute(
    workspaces: &WorkspaceRegistry,
    peer: &AuthenticatedPeer,
    envelope: ComputeEnvelope,
) -> Result<Option<ComputeEnvelope>, ComputeSessionError> {
    if matches!(envelope, ComputeEnvelope::Ping) {
        return Ok(Some(ComputeEnvelope::Pong));
    }
    if let ComputeEnvelope::Ack {
        workspace_id,
        sequence,
    } = envelope
    {
        let Some(workspace) = find_workspace(workspaces, workspace_id).await else {
            return Ok(Some(ComputeEnvelope::NotAuthorised {
                workspace_id,
                reason: "workspace not bound on this relay".into(),
            }));
        };
        workspace
            .lock()
            .await
            .store
            .ack_compute_pending(&peer.device_public_key, sequence)?;
        return Ok(None);
    }
    if is_server_only(&envelope) {
        return Err(ComputeSessionError::Protocol(
            "client sent a server-only compute envelope".into(),
        ));
    }
    let workspace_id = envelope.workspace_id().ok_or_else(|| {
        ComputeSessionError::Protocol("compute envelope has no workspace binding".into())
    })?;
    let Some(workspace) = find_workspace(workspaces, workspace_id).await else {
        return Ok(Some(ComputeEnvelope::NotAuthorised {
            workspace_id,
            reason: "workspace not bound on this relay".into(),
        }));
    };
    let mut workspace = workspace.lock().await;
    let outcome = {
        let crate::workspace::WorkspaceRuntime { compute, store, .. } = &mut *workspace;
        match &envelope {
            ComputeEnvelope::Advertise { advertisement, .. } => {
                if !peer.cert.capabilities.can_act_as_host {
                    return Err(ComputeSessionError::Protocol(
                        "worker advertisement requires a host-capable certificate".into(),
                    ));
                }
                compute.advertise(
                    peer.device_public_key,
                    workspace_id,
                    unix_now_ms(),
                    advertisement.clone(),
                )?;
                return Ok(None);
            }
            ComputeEnvelope::Request { spec } => compute.accept_request(
                peer.device_public_key,
                workspace_id,
                unix_now_ms(),
                store,
                spec,
            )?,
            ComputeEnvelope::Cancel { binding } => {
                compute.accept_cancel(peer.device_public_key, store, binding)?
            }
            ComputeEnvelope::Status { update } => {
                if !peer.cert.capabilities.can_act_as_host {
                    return Err(ComputeSessionError::Protocol(
                        "worker status requires a host-capable certificate".into(),
                    ));
                }
                compute.accept_status(peer.device_public_key, store, update)?
            }
            ComputeEnvelope::Result { result } => {
                if !peer.cert.capabilities.can_act_as_host {
                    return Err(ComputeSessionError::Protocol(
                        "worker result requires a host-capable certificate".into(),
                    ));
                }
                compute.accept_result(peer.device_public_key, store, result)?
            }
            _ => {
                return Err(ComputeSessionError::Protocol(
                    "unexpected compute envelope".into(),
                ));
            }
        }
    };
    if let ComputeRouteOutcome::Route(target) = outcome {
        let crate::workspace::WorkspaceRuntime { compute, store, .. } = &mut *workspace;
        compute.route(target, &envelope, store, next_queue_sequence())?;
    }
    drop(workspace);
    Ok(None)
}

fn is_server_only(envelope: &ComputeEnvelope) -> bool {
    matches!(
        envelope,
        ComputeEnvelope::Ready { .. }
            | ComputeEnvelope::NotAuthorised { .. }
            | ComputeEnvelope::Pong
            | ComputeEnvelope::Attach { .. }
            | ComputeEnvelope::Delivery { .. }
    )
}

fn next_queue_sequence() -> u64 {
    let observed = unix_now_ns();
    COMPUTE_QUEUE_SEQUENCE
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |previous| {
            Some(observed.max(previous.saturating_add(1)))
        })
        .map_or(observed, |previous| {
            observed.max(previous.saturating_add(1))
        })
}

async fn read_compute(receive: &mut RecvStream) -> Result<ComputeEnvelope, ComputeSessionError> {
    let bytes = read_frame(receive, MAX_COMPUTE_FRAME_BYTES).await?;
    Ok(postcard::from_bytes(&bytes)?)
}

async fn write_compute(
    send: &mut SendStream,
    envelope: &ComputeEnvelope,
) -> Result<(), ComputeSessionError> {
    let bytes = postcard::to_allocvec(envelope)?;
    if bytes.len() > MAX_COMPUTE_FRAME_BYTES {
        return Err(ComputeSessionError::Protocol(
            "encoded compute frame exceeds protocol bound".into(),
        ));
    }
    write_frame(send, &bytes).await?;
    Ok(())
}

/// Mutable authenticated client for the dedicated personal-compute stream.
pub struct ComputeClient {
    relay_peer: AuthenticatedPeer,
    own_device: [u8; 32],
    send: SendStream,
    receive: RecvStream,
    connection: Connection,
    endpoint: Endpoint,
}

/// One received compute message and its optional durable delivery token.
pub struct ComputeDelivery {
    /// Workspace whose durable queue owns this delivery.
    pub workspace_id: Option<u64>,
    /// Sequence to acknowledge after the consumer has accepted the message.
    pub sequence: Option<u64>,
    /// Typed compute message.
    pub envelope: ComputeEnvelope,
}

impl ComputeClient {
    /// Connect and mutually authenticate to an exact same-owner relay.
    ///
    /// # Errors
    ///
    /// Returns an error when dialing, framing, or certified-device
    /// authentication fails, including a mismatched pinned relay device.
    pub async fn connect(
        endpoint: Endpoint,
        relay_address: EndpointAddr,
        expected_server_user: [u8; 32],
        expected_server_device: Option<[u8; 32]>,
        own_cert: DeviceCert,
        own_signing_key: ed25519_dalek::SigningKey,
        revocations: RevocationList,
    ) -> Result<Self, ComputeSessionError> {
        let connection = endpoint
            .connect(relay_address, COMPUTE_ALPN)
            .await
            .map_err(|error| ComputeSessionError::Connection(error.to_string()))?;
        let (mut send, mut receive) = connection
            .open_bi()
            .await
            .map_err(|error| ComputeSessionError::Connection(error.to_string()))?;
        let mut nonce = [0; 32];
        rand::Rng::fill_bytes(&mut rand::rng(), &mut nonce);
        let own_device = own_cert.device_public_key;
        let hello = Hello {
            cert: own_cert,
            client_nonce: nonce,
        };
        write_frame(&mut send, &postcard::to_allocvec(&hello)?).await?;
        let acknowledgement: HelloAck =
            postcard::from_bytes(&read_frame(&mut receive, MAX_COMPUTE_FRAME_BYTES).await?)?;
        let relay_peer = auth::verify_hello_ack(
            &acknowledgement,
            &expected_server_user,
            &revocations,
            unix_now_ns(),
            &nonce,
        )?;
        if expected_server_device.is_some_and(|expected| relay_peer.device_public_key != expected) {
            return Err(ComputeSessionError::Protocol(
                "relay device does not match the pinned enrollment".into(),
            ));
        }
        let final_ack = auth::build_hello_ack2(&own_signing_key, &acknowledgement.server_nonce);
        write_frame(&mut send, &postcard::to_allocvec(&final_ack)?).await?;
        Ok(Self {
            relay_peer,
            own_device,
            send,
            receive,
            connection,
            endpoint,
        })
    }

    #[must_use]
    /// Authenticated view of the relay certificate.
    pub fn relay_peer(&self) -> &AuthenticatedPeer {
        &self.relay_peer
    }

    /// The authenticated connection, for opening further streams (a
    /// worker's legacy launches) while this client keeps the control stream.
    #[must_use]
    pub fn connection(&self) -> Connection {
        self.connection.clone()
    }

    /// Send one bounded typed compute envelope.
    ///
    /// # Errors
    ///
    /// Returns an error when encoding or writing the frame fails.
    pub async fn send(&mut self, envelope: &ComputeEnvelope) -> Result<(), ComputeSessionError> {
        write_compute(&mut self.send, envelope).await
    }

    /// Receive the next typed compute envelope.
    ///
    /// # Errors
    ///
    /// Returns an error on EOF, an oversize frame, or invalid encoding.
    pub async fn receive(&mut self) -> Result<ComputeDelivery, ComputeSessionError> {
        let envelope = read_compute(&mut self.receive).await?;
        if let ComputeEnvelope::Delivery {
            workspace_id,
            sequence,
            message,
        } = envelope
        {
            if message.workspace_id() != Some(workspace_id)
                || matches!(
                    *message,
                    ComputeEnvelope::Delivery { .. } | ComputeEnvelope::Ack { .. }
                )
            {
                return Err(ComputeSessionError::Protocol(
                    "invalid nested compute delivery".into(),
                ));
            }
            return Ok(ComputeDelivery {
                workspace_id: Some(workspace_id),
                sequence: Some(sequence),
                envelope: *message,
            });
        }
        Ok(ComputeDelivery {
            workspace_id: envelope.workspace_id(),
            sequence: None,
            envelope,
        })
    }

    /// Acknowledge a durable delivery after the consumer accepts it.
    ///
    /// # Errors
    ///
    /// Returns an error when the acknowledgement frame cannot be written.
    pub async fn acknowledge(
        &mut self,
        delivery: &ComputeDelivery,
    ) -> Result<(), ComputeSessionError> {
        let (Some(workspace_id), Some(sequence)) = (delivery.workspace_id, delivery.sequence)
        else {
            return Ok(());
        };
        self.send(&ComputeEnvelope::Ack {
            workspace_id,
            sequence,
        })
        .await
    }

    /// Attach this certified connection to one workspace.
    ///
    /// # Errors
    ///
    /// Returns an error when the attach frame cannot be written.
    pub async fn attach(&mut self, workspace_id: u64) -> Result<(), ComputeSessionError> {
        self.send(&ComputeEnvelope::Attach { workspace_id }).await
    }

    /// Submit a job whose requester is this exact certified device.
    ///
    /// # Errors
    ///
    /// Returns an error for a spoofed requester or when the request frame
    /// cannot be written.
    pub async fn submit(&mut self, spec: &ComputeJobSpec) -> Result<(), ComputeSessionError> {
        if spec.requester != self.own_device {
            return Err(ComputeSessionError::Protocol(
                "compute request does not belong to this certified client".into(),
            ));
        }
        self.send(&ComputeEnvelope::Request { spec: spec.clone() })
            .await
    }

    /// Cancel one exact job route owned by this certified requester.
    ///
    /// # Errors
    ///
    /// Returns an error for a spoofed requester or when the cancellation
    /// frame cannot be written.
    pub async fn cancel(&mut self, binding: &ComputeJobBinding) -> Result<(), ComputeSessionError> {
        if binding.requester != self.own_device {
            return Err(ComputeSessionError::Protocol(
                "compute cancellation does not belong to this certified client".into(),
            ));
        }
        self.send(&ComputeEnvelope::Cancel {
            binding: binding.clone(),
        })
        .await
    }
}

impl ComputeClient {
    /// Leave the relay now: close the connection and wait until the relay
    /// has been told, so it withdraws this worker at once rather than when
    /// the connection times out.
    pub async fn close(self) {
        self.connection.close(0u32.into(), b"compute client closed");
        self.endpoint.close().await;
    }
}

impl Drop for ComputeClient {
    fn drop(&mut self) {
        self.connection
            .close(0u32.into(), b"compute client dropped");
        let _ = &self.endpoint;
    }
}
