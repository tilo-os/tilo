// `Promise::from_future(async move {..})` refines capnp-rpc's declared
// `Promise<.., capnp::Error>` return type — the intended pattern, but it
// trips `refining_impl_trait`.
#![allow(refining_impl_trait)]

//! Native Cap'n Proto server for the qOS collaboration broker.
//!
//! Answers the `runtime-collab` schema on the shell's collaboration socket.
//!
//! `join_channel` uses the caller's `channel_id` verbatim as the routing
//! key. Document traffic has one path: `send` with a certified signed op.
//! `fetchScrollbackFor` answers from the in-memory op log; an
//! exhausted-but-empty batch triggers no fallback push, callers repeat the
//! fetch instead.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use capnp::capability::Promise;
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use collab_types::{
    Actor, ArtifactId, ArtifactKind, CapDecision, DocumentMutation, OpClass, RedactedMarker,
    ResourceRef, SignedOp, ValidationError, Verb, document_artifact_id, document_attestation_id,
    validate_admitted_inbound_op,
};
use runtime_collab::raw::{
    OpKind, action_input, channel_handle, collab_service, context_stream, invocation_stream,
    presence_event, presence_handle, presence_payload, presence_stream, scrollback_batch, stream,
};
use ui::collaboration::{CollabEvent, PresencePayload};
use ui::presence::PresenceKey;

use crate::shell::collab_broker::{
    AuditEntry, BrokerChannels, CrdtRoute, FanoutState, apply_redaction_from_envelope,
};
use crate::shell::collab_outbound::PeerPublisher;
use crate::shell::state::{CapRole, Principal, WorkspaceTree, WorkspaceTreeHandle};

#[derive(Clone, Debug, PartialEq, Eq)]
struct VerifiedJoinIdentity {
    user_public_key: [u8; 32],
    device_public_key: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AdmittedJoinIdentity {
    verified: VerifiedJoinIdentity,
    principal_id: String,
    /// Informational display label announced by this authenticated session.
    /// Authorization always uses `principal_id`, never this handle.
    display_handle: Option<String>,
    /// Workspace admission role from the live owner/capability roster.
    /// Certified document operations use this directly instead of requiring
    /// a second, channel-local role attestation for the same person.
    role: CapRole,
}

fn principal_id_for_root(user_public_key: [u8; 32]) -> String {
    match Principal::person_from_key(collab_types::Ed25519PublicKey::from_bytes(user_public_key)) {
        Principal::Person(id) => id,
        _ => hex::encode(user_public_key),
    }
}

fn admit_verified_join(
    tree: &WorkspaceTree,
    route: &CrdtRoute,
    verified: VerifiedJoinIdentity,
) -> Result<AdmittedJoinIdentity, String> {
    let workspace = tree
        .workspaces
        .iter()
        .find(|workspace| workspace.room.to_string() == route.cluster_id)
        .ok_or_else(|| "certified join names a workspace this session does not host".to_string())?;
    let principal_id = principal_id_for_root(verified.user_public_key);
    let role = workspace
        .role_of(&principal_id)
        .ok_or_else(|| "certified collaboration identity is not a workspace member".to_string())?;
    if role == CapRole::Denied {
        return Err("certified collaboration identity is blocked in this workspace".into());
    }
    Ok(AdmittedJoinIdentity {
        verified,
        principal_id,
        role,
        display_handle: None,
    })
}

fn verify_session_author(
    admitted: &AdmittedJoinIdentity,
    author: collab_types::Ed25519PublicKey,
) -> Result<(), String> {
    if author.as_bytes() != &admitted.verified.device_public_key {
        return Err("signed op author does not match the certified device for this session".into());
    }
    Ok(())
}

fn verify_join_presentation(
    route: &CrdtRoute,
    presentation: &runtime_collab::CertifiedJoinIdentity,
    expected_user_public_key: &[u8; 32],
    now_ns: u64,
    revoked: bool,
    enrolled: bool,
) -> Result<VerifiedJoinIdentity, String> {
    if presentation.workspace_id != route.cluster_id {
        return Err("certified join is scoped to a different workspace".into());
    }
    let cert: qos_user_identity::DeviceCert =
        serde_json::from_slice(&presentation.device_certificate)
            .map_err(|error| format!("device certificate is not valid JSON: {error}"))?;
    qos_user_identity::verify_device_cert_at(&cert, now_ns)
        .map_err(|error| format!("device certificate was rejected: {error}"))?;
    if &cert.user_public_key != expected_user_public_key {
        return Err("device certificate belongs to another workspace identity".into());
    }
    if revoked {
        return Err("certified collaboration device has been revoked".into());
    }
    if !enrolled {
        return Err("certified collaboration device is not enrolled".into());
    }
    let proof = <[u8; 64]>::try_from(presentation.proof.as_slice())
        .map_err(|_| "collaboration join proof must be 64 bytes".to_string())?;
    VerifyingKey::from_bytes(&cert.device_public_key)
        .map_err(|_| "device certificate contains an invalid device key".to_string())?
        .verify(
            &runtime_identity::collaboration_join_signing_bytes(&presentation.workspace_id),
            &Signature::from_bytes(&proof),
        )
        .map_err(|_| "collaboration join proof was signed by an unknown device".to_string())?;
    Ok(VerifiedJoinIdentity {
        user_public_key: cert.user_public_key,
        device_public_key: cert.device_public_key,
    })
}

fn verify_join_identity(
    route: &CrdtRoute,
    identity_bytes: &[u8],
    membership: Option<&WorkspaceTreeHandle>,
    trusted_devices: Option<&[[u8; 32]]>,
) -> Result<AdmittedJoinIdentity, String> {
    let Some(presentation) = runtime_collab::CertifiedJoinIdentity::decode(identity_bytes)
        .map_err(|error| format!("certified join presentation is malformed: {error}"))?
    else {
        return Err("collaboration join requires a certified device presentation".into());
    };
    let cert: qos_user_identity::DeviceCert =
        serde_json::from_slice(&presentation.device_certificate)
            .map_err(|error| format!("device certificate is not valid JSON: {error}"))?;
    let (expected_root, revoked, enrolled) = if let Some(devices) = trusted_devices {
        (
            cert.user_public_key,
            false,
            devices.contains(&cert.device_public_key),
        )
    } else {
        let profile = crate::identity_registry::selected_identity_profile(&route.cluster_id)
            .map_err(|error| format!("workspace identity lookup failed: {error}"))?
            .ok_or_else(|| "no identity is selected for this workspace".to_string())?;
        let fleet = crate::device_fleet::DeviceFleetStore::from_env()
            .and_then(|store| store.load(&profile.id))
            .map_err(|error| format!("device fleet lookup failed: {error}"))?;
        let revoked = fleet.is_revoked(&cert.device_public_key);
        let enrolled = fleet
            .certificates
            .iter()
            .any(|known| known.device_public_key == cert.device_public_key);
        (profile.user_public_key, revoked, enrolled)
    };
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            duration
                .as_secs()
                .saturating_mul(1_000_000_000)
                .saturating_add(u64::from(duration.subsec_nanos()))
        });
    let verified = verify_join_presentation(
        route,
        &presentation,
        &expected_root,
        now_ns,
        revoked,
        enrolled,
    )?;
    let membership = membership
        .ok_or_else(|| "certified join has no workspace membership authority".to_string())?;
    admit_verified_join(&membership.read(), route, verified)
}

/// Envelope pushed onto an [`OpStreamServer`]'s queue. `kind` is what
/// makes the queue readable: signed, delta and whole-document share
/// this type and are indistinguishable from the bytes alone.
#[derive(Clone, Debug)]
struct OpEnvelope {
    op_bytes: Vec<u8>,
    from_user_id: String,
    kind: OpKind,
}

/// Envelope pushed onto a [`PresenceStreamServer`]'s queue. Carries
/// every presence variant; the cursor stream filters to pointers.
#[derive(Clone, Debug)]
struct PresenceEnvelope {
    user_id: String,
    key_bytes: Vec<u8>,
    payload: PresencePayload,
}

/// Envelope pushed onto a [`ContextStreamServer`]'s queue.
#[derive(Clone, Debug)]
struct ContextEnvelope {
    collaborators: Vec<ui::collaboration::Collaborator>,
    permissions: Vec<String>,
}

/// Input contract published for one live application action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PublishedInput {
    None,
    Text {
        prompt: String,
        placeholder: String,
        wants_content: bool,
    },
}

/// One inert operation published by a live application connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedAction {
    pub resource: String,
    pub action: String,
    pub resource_kind: String,
    pub resource_label: String,
    pub action_label: String,
    pub description: Option<String>,
    pub operation_kind: qos_actions::OperationKind,
    pub input: PublishedInput,
}

/// A live application's complete, level-triggered action surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedSurface {
    pub peer_id: u64,
    pub app_id: String,
    pub route: CrdtRoute,
    pub actions: Vec<PublishedAction>,
}

#[derive(Clone, Debug)]
struct InvocationEnvelope {
    id: u64,
    resource: String,
    action: String,
    argument: Option<String>,
    actor: String,
}

/// The application's terminal answer to a directed invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationCompletion {
    pub code: String,
    pub message: String,
}

struct PendingInvocation {
    peer_id: u64,
    tx: std::sync::mpsc::Sender<InvocationCompletion>,
}

/// One subscription onto the broker fanout. Slot-indexed inside a
/// [`ChannelSubscribers`] table so it can be dropped en masse when
/// the connection tears down.
struct OpSub {
    peer_id: u64,
    tx: mpsc::UnboundedSender<OpEnvelope>,
}
struct PresenceSub {
    peer_id: u64,
    tx: mpsc::UnboundedSender<PresenceEnvelope>,
}
struct ContextSub {
    peer_id: u64,
    tx: mpsc::UnboundedSender<ContextEnvelope>,
}

/// Per-channel subscriber registry, keyed by `join_channel(id)`. A
/// `channelId` carries the whole address ([`ui::collaboration::channel_key`]
/// encodes `(app, cluster, record)`), decoded via
/// [`CrdtRoute::from_channel_key`]; delivery is [`CrdtRoute::delivers_to`].
#[derive(Default)]
struct ChannelSubscribers {
    op: Vec<OpSub>,
    presence: Vec<PresenceSub>,
    context: Vec<ContextSub>,
}

/// Shared typed broker state, attached to the transport-neutral fanout core.
pub struct CapnpBrokerState {
    /// Keyed by `join_channel`'s `channel_id`; lazily inserted on first join.
    channels: Mutex<HashMap<String, ChannelSubscribers>>,
    /// `Weak` so broker teardown isn't blocked by an outliving connection.
    fanout: Weak<FanoutState>,
    /// Durable validation, audit, redaction, and scrollback state.
    broker_channels: Arc<Mutex<BrokerChannels>>,
    /// A publisher with no network behind it ([`PeerPublisher::disabled`]) is legitimate
    /// (unit tests, a shell started without peer sync).
    publisher: Arc<PeerPublisher>,
    /// The shell's one replicated workspace owner/capability authority.
    /// `None` exists only for transitional test/diagnostic brokers; a
    /// certified join fails closed when they have no authority to consult.
    membership: Option<WorkspaceTreeHandle>,
    trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
    /// Latest complete action surface per live connection.
    surfaces: Mutex<HashMap<u64, PublishedSurface>>,
    /// Directed invocation stream registered by each application connection.
    invocation_subscribers: Mutex<HashMap<u64, mpsc::UnboundedSender<InvocationEnvelope>>>,
    pending_invocations: Mutex<HashMap<u64, PendingInvocation>>,
    invocation_id: AtomicU64,
}

impl CapnpBrokerState {
    /// So tests can register channel state before pushing signed
    /// envelopes through the capnp path.
    pub fn broker_channels(&self) -> Arc<Mutex<BrokerChannels>> {
        Arc::clone(&self.broker_channels)
    }

    fn new(
        fanout: &Arc<FanoutState>,
        broker_channels: Arc<Mutex<BrokerChannels>>,
        publisher: Arc<PeerPublisher>,
        membership: Option<WorkspaceTreeHandle>,
        trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            channels: Mutex::new(HashMap::new()),
            fanout: Arc::downgrade(fanout),
            broker_channels,
            publisher,
            membership,
            trusted_devices,
            surfaces: Mutex::new(HashMap::new()),
            invocation_subscribers: Mutex::new(HashMap::new()),
            pending_invocations: Mutex::new(HashMap::new()),
            invocation_id: AtomicU64::new(1),
        })
    }

    /// Snapshot every action surface currently published by a connected app.
    pub fn published_surfaces(&self) -> Vec<PublishedSurface> {
        self.surfaces
            .lock()
            .map(|surfaces| surfaces.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Address one operation back to the exact connection that published it.
    /// The returned receiver resolves only after the application reports a
    /// structured completion; dropping the connection resolves as a refusal.
    pub fn invoke(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
    ) -> Result<std::sync::mpsc::Receiver<InvocationCompletion>, String> {
        let offered = self
            .surfaces
            .lock()
            .ok()
            .and_then(|surfaces| surfaces.get(&peer_id).cloned())
            .and_then(|surface| {
                let action = surface
                    .actions
                    .iter()
                    .find(|candidate| {
                        candidate.resource == resource && candidate.action == action
                    })
                    .cloned()?;
                Some((surface, action))
            })
            .ok_or_else(|| "the application no longer publishes that action".to_string())?;
        let sender = self
            .invocation_subscribers
            .lock()
            .ok()
            .and_then(|subscribers| subscribers.get(&peer_id).cloned())
            .ok_or_else(|| "the application has no invocation stream".to_string())?;
        if offered.0.route.cluster_id.is_empty() {
            return Err("the application is not attached to a workspace".into());
        }
        let membership = self
            .membership
            .as_ref()
            .ok_or_else(|| "the action broker has no workspace authority".to_string())?;
        let tree = membership.read();
        let workspace = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == offered.0.route.cluster_id)
            .ok_or_else(|| "the application workspace is no longer hosted".to_string())?;
        let caller = workspace.actor_for_person(&actor.to_string());
        let target = ResourceRef::Action {
            app: offered.0.app_id.clone(),
            action: action.to_string(),
        };
        let verb = if offered.1.operation_kind == qos_actions::OperationKind::View {
            Verb::Read
        } else {
            Verb::Write
        };
        if workspace.decide(&caller, &target, verb) != CapDecision::Allow {
            return Err("the caller is not allowed to invoke that application action".into());
        }
        drop(tree);
        let id = self.invocation_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = std::sync::mpsc::channel();
        self.pending_invocations
            .lock()
            .map_err(|_| "the invocation registry is unavailable".to_string())?
            .insert(id, PendingInvocation { peer_id, tx });
        if sender
            .send(InvocationEnvelope {
                id,
                resource: resource.to_string(),
                action: action.to_string(),
                argument: argument.map(str::to_string),
                actor: actor.to_string(),
            })
            .is_err()
        {
            let _ = self
                .pending_invocations
                .lock()
                .map(|mut pending| pending.remove(&id));
            return Err("the application invocation stream closed".into());
        }
        Ok(rx)
    }

    fn remove_connection(&self, peer_id: u64) {
        if let Ok(mut surfaces) = self.surfaces.lock() {
            surfaces.remove(&peer_id);
        }
        if let Ok(mut subscribers) = self.invocation_subscribers.lock() {
            subscribers.remove(&peer_id);
        }
        if let Ok(mut pending) = self.pending_invocations.lock() {
            let ids = pending
                .iter()
                .filter_map(|(id, invocation)| (invocation.peer_id == peer_id).then_some(*id))
                .collect::<Vec<_>>();
            for id in ids {
                if let Some(invocation) = pending.remove(&id) {
                    let _ = invocation.tx.send(InvocationCompletion {
                        code: "action_failed".into(),
                        message: "application disconnected before completing the action".into(),
                    });
                }
            }
        }
    }

    /// `joinChannel(channelId, identity)` carries no announced handle,
    /// so the room lookup is the only answer for who to stamp traffic as.
    fn participant_id(admitted: &AdmittedJoinIdentity) -> String {
        admitted
            .display_handle
            .clone()
            .unwrap_or_else(|| admitted.principal_id.clone())
    }

    fn revalidate_admission(
        &self,
        route: &CrdtRoute,
        admitted: &AdmittedJoinIdentity,
    ) -> Result<AdmittedJoinIdentity, capnp::Error> {
        let membership = self.membership.as_ref().ok_or_else(|| {
            capnp::Error::failed("certified session lost its membership authority".into())
        })?;
        let mut current = admit_verified_join(&membership.read(), route, admitted.verified.clone())
            .map_err(|error| {
                capnp::Error::failed(format!(
                    "certified collaboration session is no longer admitted: {error}"
                ))
            })?;
        current.display_handle.clone_from(&admitted.display_handle);
        Ok(current)
    }

    /// Bind a certified operation to the route, typed document protocol and
    /// current workspace capability state. The identity service performs the
    /// same checks before signing; repeating them at receipt prevents a stale
    /// role or capability from remaining effective after launch.
    fn authorize_certified_document(
        &self,
        route: &CrdtRoute,
        admitted: &AdmittedJoinIdentity,
        envelope: &SignedOp,
    ) -> Result<DocumentMutation, capnp::Error> {
        let expected_artifact =
            document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id);
        let expected_attestation =
            document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id);
        if route.app_id.is_empty()
            || envelope.artifact != expected_artifact
            || envelope.attestation_id != expected_attestation
            || envelope.op_class != OpClass::Splice
            || envelope.encryption.is_some()
        {
            return Err(capnp::Error::failed(
                "certified document operation does not match its joined route".into(),
            ));
        }
        let mutation = DocumentMutation::decode(&envelope.payload).map_err(|error| {
            capnp::Error::failed(format!(
                "certified document operation has an invalid typed payload: {error}"
            ))
        })?;
        let membership = self.membership.as_ref().ok_or_else(|| {
            capnp::Error::failed("certified session lost its membership authority".into())
        })?;
        let tree = membership.read();
        let workspace = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == route.cluster_id)
            .ok_or_else(|| {
                capnp::Error::failed("certified document workspace is no longer available".into())
            })?;
        let actor = Actor::person(Some(admitted.role), admitted.principal_id.clone())
            .with_principal(Principal::App(route.app_id.clone()))
            .in_room(route.cluster_id.clone());
        let resource = ResourceRef::Artifact {
            app: Some(route.app_id.clone()),
            kind: ArtifactKind::Note,
            instance: expected_artifact,
        };
        let decision = workspace.decide(&actor, &resource, Verb::Write);
        drop(tree);
        if decision != CapDecision::Allow {
            return Err(capnp::Error::failed(
                "current workspace capability denies document editing".into(),
            ));
        }
        Ok(mutation)
    }

    fn publisher(&self) -> &Arc<PeerPublisher> {
        &self.publisher
    }

    /// Converts once into the transport-neutral peer presence envelope.
    /// Peer replication does not embed either local RPC dialect.
    fn publish_presence(&self, route: &CrdtRoute, key: &PresenceKey, payload: &PresencePayload) {
        let Some(presence) = crate::shell::collab_presence::to_peer(key, payload) else {
            tracing::warn!(
                channel = %route.channel_key(),
                "capnp broker: presence would not encode; peers will \
                 not see this person move",
            );
            return;
        };
        self.publisher().presence(route, presence);
    }

    /// Delegates to the shared fanout registry for typed connection ids.
    fn next_peer_id(&self) -> u64 {
        // Peer table torn down: racing with shutdown. Hand out a
        // placeholder id; the caller drops it on the next tokio yield.
        self.fanout.upgrade().map_or(u64::MAX, |t| t.next_peer_id())
    }

    /// Drive an event onto the subscribed queues `route` addresses,
    /// skipping `except`. Called from [`FanoutState::fanout_to_route`]
    /// Delegates to
    /// [`CrdtRoute::delivers_to`] rather than reimplementing the filter,
    /// so RPC and in-process delivery cannot drift.
    pub fn fanout_to_route(&self, event: &CollabEvent, except: u64, route: &CrdtRoute) {
        if route.cluster_id.is_empty() {
            return;
        }
        let Ok(mut all_channels) = self.channels.lock() else {
            return;
        };
        let channels = all_channels
            .iter_mut()
            .filter(|(k, _)| route.delivers_to(&CrdtRoute::from_channel_key(k)))
            .map(|(_, subs)| subs);
        match event {
            CollabEvent::Document {
                envelope,
                from_user_id,
            } => Self::fanout_signed_incremental(channels, except, envelope, from_user_id),
            CollabEvent::RemoteCursorMoved { user_id, x, y } => {
                Self::fanout_cursor_moved(channels, except, user_id, *x, *y);
            }
            CollabEvent::RemotePresenceUpdate {
                user_id,
                key,
                payload,
            } => Self::fanout_presence_update(channels, except, user_id, key, payload),
            CollabEvent::Context {
                collaborators,
                permissions,
            } => Self::fanout_context(channels, except, collaborators, permissions),
            CollabEvent::ActionArrived { type_id, .. } => {
                // Unreachable today; said out loud, not silently dropped.
                tracing::warn!(
                    %type_id,
                    "capnp fanout: no action stream yet, action dropped"
                );
            }
            CollabEvent::ScrollbackBatch { .. } => {
                // Per-request in the capnp schema, not a stream; apps
                // wanting late history repeat `fetchScrollbackFor`.
            }
            CollabEvent::Invocation { .. } => {
                // Invocations are point-to-point and never enter fanout.
            }
        }
    }

    /// One arm of [`Self::fanout_to_route`]: a validated signed op.
    fn fanout_signed_incremental<'a>(
        channels: impl Iterator<Item = &'a mut ChannelSubscribers>,
        except: u64,
        envelope: &SignedOp,
        from_user_id: &str,
    ) {
        let op_bytes = match postcard::to_stdvec(envelope) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "capnp fanout: postcard(SignedOp) failed");
                return;
            }
        };
        let env = OpEnvelope {
            op_bytes,
            from_user_id: from_user_id.to_owned(),
            kind: OpKind::Signed,
        };
        for subs in channels {
            subs.op.retain(|s| {
                if s.peer_id == except {
                    return true;
                }
                s.tx.send(env.clone()).is_ok()
            });
        }
    }

    /// One arm of [`Self::fanout_to_route`]: a keyless cursor move, translated
    /// onto the sole general presence stream.
    fn fanout_cursor_moved<'a>(
        channels: impl Iterator<Item = &'a mut ChannelSubscribers>,
        except: u64,
        user_id: &str,
        x: f32,
        y: f32,
    ) {
        let key_bytes = default_pointer_key_bytes();
        let presence_env = PresenceEnvelope {
            user_id: user_id.to_owned(),
            key_bytes,
            payload: PresencePayload::Pointer { x, y },
        };
        for subs in channels {
            subs.presence.retain(|s| {
                if s.peer_id == except {
                    return true;
                }
                s.tx.send(presence_env.clone()).is_ok()
            });
        }
    }

    /// One arm of [`Self::fanout_to_route`]: a keyed presence update
    /// (caret, selection, arrived, left, ...).
    fn fanout_presence_update<'a>(
        channels: impl Iterator<Item = &'a mut ChannelSubscribers>,
        except: u64,
        user_id: &str,
        key: &PresenceKey,
        payload: &PresencePayload,
    ) {
        let key_bytes = match postcard::to_stdvec(key) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "capnp fanout: postcard(PresenceKey) failed");
                return;
            }
        };
        let env = PresenceEnvelope {
            user_id: user_id.to_owned(),
            key_bytes,
            payload: payload.clone(),
        };
        for subs in channels {
            subs.presence.retain(|s| {
                if s.peer_id == except {
                    return true;
                }
                s.tx.send(env.clone()).is_ok()
            });
        }
    }

    /// One arm of [`Self::fanout_to_route`]: the room's collaborator +
    /// permission context.
    fn fanout_context<'a>(
        channels: impl Iterator<Item = &'a mut ChannelSubscribers>,
        except: u64,
        collaborators: &[ui::collaboration::Collaborator],
        permissions: &[String],
    ) {
        let env = ContextEnvelope {
            collaborators: collaborators.to_vec(),
            permissions: permissions.to_vec(),
        };
        for subs in channels {
            subs.context.retain(|s| {
                if s.peer_id == except {
                    return true;
                }
                s.tx.send(env.clone()).is_ok()
            });
        }
    }

    fn ensure_channel(&self, channel_id: &str) {
        if let Ok(mut m) = self.channels.lock() {
            m.entry(channel_id.to_owned())
                .or_insert_with(ChannelSubscribers::default);
        }
    }

    fn subscribe_ops(&self, channel_id: &str, peer_id: u64) -> mpsc::UnboundedReceiver<OpEnvelope> {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut m) = self.channels.lock() {
            m.entry(channel_id.to_owned())
                .or_insert_with(ChannelSubscribers::default)
                .op
                .push(OpSub { peer_id, tx });
        }
        rx
    }

    fn subscribe_presence(
        &self,
        channel_id: &str,
        peer_id: u64,
    ) -> mpsc::UnboundedReceiver<PresenceEnvelope> {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut m) = self.channels.lock() {
            m.entry(channel_id.to_owned())
                .or_insert_with(ChannelSubscribers::default)
                .presence
                .push(PresenceSub { peer_id, tx });
        }
        rx
    }

    fn subscribe_context(
        &self,
        channel_id: &str,
        peer_id: u64,
    ) -> mpsc::UnboundedReceiver<ContextEnvelope> {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut m) = self.channels.lock() {
            m.entry(channel_id.to_owned())
                .or_insert_with(ChannelSubscribers::default)
                .context
                .push(ContextSub { peer_id, tx });
        }
        rx
    }
}

/// Postcard-encode [`PresenceKey::pointer_default`]; cheap enough not
/// to cache.
fn default_pointer_key_bytes() -> Vec<u8> {
    postcard::to_stdvec(&PresenceKey::pointer_default()).unwrap_or_default()
}

// Server implementations

/// Root capability server. One instance per connected RPC session.
struct CapnpCollabService {
    state: Arc<CapnpBrokerState>,
    /// Peer id assigned at accept time. Fanout uses it to skip the
    /// author when replaying its own frames back at itself.
    peer_id: u64,
}

impl collab_service::Server for CapnpCollabService {
    fn join_channel(
        self: Rc<Self>,
        params: collab_service::JoinChannelParams,
        mut results: collab_service::JoinChannelResults,
    ) -> Promise<(), capnp::Error> {
        let state = self.state.clone();
        let peer_id = self.peer_id;
        Promise::from_future(async move {
            let reader = params.get()?;
            let channel_id = reader
                .get_channel_id()?
                .to_str()
                .map_err(|e| capnp::Error::failed(format!("channelId not utf8: {e}")))?
                .to_owned();
            let identity = reader.get_identity()?;
            let route = CrdtRoute::from_channel_key(&channel_id);
            let mut verified_identity = verify_join_identity(
                &route,
                identity,
                state.membership.as_ref(),
                state
                    .trusted_devices
                    .as_ref()
                    .map(|devices| devices.as_slice()),
            )
            .map_err(|error| {
                capnp::Error::failed(format!("collaboration join identity rejected: {error}"))
            })?;
            let participant_handle = reader
                .get_participant_handle()?
                .to_str()
                .map_err(|error| {
                    capnp::Error::failed(format!("participant handle not utf8: {error}"))
                })?
                .trim()
                .chars()
                .take(64)
                .collect::<String>();
            if !participant_handle.is_empty() {
                verified_identity.display_handle = Some(participant_handle);
            }
            tracing::info!(
                target: "qos_collab_probe",
                channel = %channel_id,
                peer_id,
                role = ?verified_identity.role,
                principal = %verified_identity.principal_id,
                device = ?{
                    collab_types::Ed25519PublicKey::from_bytes(
                        verified_identity.verified.device_public_key,
                    )
                },
                "PROBE join_channel",
            );
            state.ensure_channel(&channel_id);

            let handle = CapnpChannelHandle {
                state: state.clone(),
                channel_id,
                peer_id,
                verified_identity,
            };
            let client: channel_handle::Client = capnp_rpc::new_client(handle);
            results.get().set_handle(client);
            Ok(())
        })
    }
}

/// Per-channel handle established by `join_channel`; every subsequent
/// method call inherits its (`channel_id`, `peer_id`, identity) context.
struct CapnpChannelHandle {
    state: Arc<CapnpBrokerState>,
    channel_id: String,
    peer_id: u64,
    verified_identity: AdmittedJoinIdentity,
}

impl Drop for CapnpChannelHandle {
    fn drop(&mut self) {
        self.state.remove_connection(self.peer_id);
    }
}

impl CapnpChannelHandle {
    /// A record-scoped connection names its record inside the channel id
    /// (the SDK joins `<cluster>#record:<id>`), parsed back out here.
    fn route(&self) -> CrdtRoute {
        CrdtRoute::from_channel_key(&self.channel_id)
    }
}

impl channel_handle::Server for CapnpChannelHandle {
    fn send(
        self: Rc<Self>,
        params: channel_handle::SendParams,
        _results: channel_handle::SendResults,
    ) -> Promise<(), capnp::Error> {
        let state = self.state.clone();
        let peer_id = self.peer_id;
        let route = self.route();
        let verified_identity = self.verified_identity.clone();
        Promise::from_future(async move {
            let op_bytes = params.get()?.get_op()?.to_vec();
            let envelope: SignedOp = postcard::from_bytes(&op_bytes).map_err(|e| {
                capnp::Error::failed(format!(
                    "capnp send: op is not a postcard-encoded SignedOp: {e}"
                ))
            })?;
            let current_identity = state.revalidate_admission(&route, &verified_identity)?;
            verify_session_author(&current_identity, envelope.author)
                .map_err(|error| capnp::Error::failed(format!("capnp send: {error}")))?;
            state.authorize_certified_document(&route, &current_identity, &envelope)?;
            handle_signed_op_send(&state, peer_id, &route, &current_identity, envelope);
            Ok(())
        })
    }

    fn subscribe(
        self: Rc<Self>,
        _params: channel_handle::SubscribeParams,
        mut results: channel_handle::SubscribeResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let rx = self.state.subscribe_ops(&self.channel_id, self.peer_id);
        let server = OpStreamServer {
            rx: Rc::new(AsyncMutex::new(rx)),
        };
        let client: stream::Client = capnp_rpc::new_client(server);
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn presence(
        self: Rc<Self>,
        _params: channel_handle::PresenceParams,
        mut results: channel_handle::PresenceResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let handle = CapnpPresenceHandle {
            state: self.state.clone(),
            channel_id: self.channel_id.clone(),
            peer_id: self.peer_id,
            verified_identity: self.verified_identity.clone(),
        };
        let client: presence_handle::Client = capnp_rpc::new_client(handle);
        results.get().set_handle(client);
        Promise::ok(())
    }

    fn retired_unsigned_snapshot(
        self: Rc<Self>,
        _params: channel_handle::RetiredUnsignedSnapshotParams,
        _results: channel_handle::RetiredUnsignedSnapshotResults,
    ) -> Promise<(), capnp::Error> {
        Promise::ok(())
    }

    fn retired_unsigned_incremental(
        self: Rc<Self>,
        _params: channel_handle::RetiredUnsignedIncrementalParams,
        _results: channel_handle::RetiredUnsignedIncrementalResults,
    ) -> Promise<(), capnp::Error> {
        Promise::ok(())
    }

    fn subscribe_context(
        self: Rc<Self>,
        _params: channel_handle::SubscribeContextParams,
        mut results: channel_handle::SubscribeContextResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let rx = self.state.subscribe_context(&self.channel_id, self.peer_id);
        let server = ContextStreamServer {
            rx: Rc::new(AsyncMutex::new(rx)),
        };
        let client: context_stream::Client = capnp_rpc::new_client(server);
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn fetch_scrollback_for(
        self: Rc<Self>,
        params: channel_handle::FetchScrollbackForParams,
        mut results: channel_handle::FetchScrollbackForResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let broker_channels = self.state.broker_channels.clone();
        Promise::from_future(async move {
            let p = params.get()?;
            let artifact_id_bytes = p.get_artifact_id()?.to_vec();
            let before_seq = p.get_before_seq();
            let max = p.get_max();
            let uuid = uuid::Uuid::from_slice(&artifact_id_bytes).map_err(|e| {
                capnp::Error::failed(format!(
                    "capnp fetch_scrollback_for: artifactId must be 16 bytes: {e}"
                ))
            })?;
            let artifact = ArtifactId(uuid);
            let (ops, exhausted) = read_scrollback(&broker_channels, artifact, before_seq, max);
            let batch = results.get().init_batch();
            fill_scrollback_batch(batch, &ops, exhausted)?;
            Ok(())
        })
    }

    fn publish_actions(
        self: Rc<Self>,
        params: channel_handle::PublishActionsParams,
        _results: channel_handle::PublishActionsResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let state = self.state.clone();
        let peer_id = self.peer_id;
        let route = self.route();
        Promise::from_future(async move {
            let request = params.get()?;
            let app_id = request.get_app_id()?.to_str()?.to_owned();
            if route.app_id.is_empty() || app_id != route.app_id {
                return Err(capnp::Error::failed(
                    "publishActions appId must match the certified connection route".into(),
                ));
            }
            let rows = request.get_actions()?;
            let mut actions = Vec::with_capacity(rows.len() as usize);
            for index in 0..rows.len() {
                let row = rows.get(index);
                let resource = row.get_resource()?.to_str()?.to_owned();
                let action = row.get_action()?.to_str()?.to_owned();
                if resource.is_empty()
                    || action.is_empty()
                    || resource.contains(qos_actions::KEY_SEP)
                    || action.contains(qos_actions::KEY_SEP)
                    || actions.iter().any(|candidate: &PublishedAction| {
                        candidate.resource == resource && candidate.action == action
                    })
                {
                    return Err(capnp::Error::failed(
                        "exported action addresses must be non-empty, unique, and contain no separator"
                            .into(),
                    ));
                }
                let description = row.get_description()?.to_str()?.to_owned();
                let input = match row.get_input()?.which()? {
                    action_input::Which::None(()) => PublishedInput::None,
                    action_input::Which::Text(text) => PublishedInput::Text {
                        prompt: text.get_prompt()?.to_str()?.to_owned(),
                        placeholder: text.get_placeholder()?.to_str()?.to_owned(),
                        wants_content: text.get_wants_content(),
                    },
                };
                actions.push(PublishedAction {
                    resource,
                    action,
                    resource_kind: row.get_resource_kind()?.to_str()?.to_owned(),
                    resource_label: row.get_resource_label()?.to_str()?.to_owned(),
                    action_label: row.get_action_label()?.to_str()?.to_owned(),
                    description: (!description.is_empty()).then_some(description),
                    operation_kind: match row.get_operation_kind()?.to_str()? {
                        "" | "document" => qos_actions::OperationKind::Document,
                        "effect" => qos_actions::OperationKind::Effect,
                        "view" => qos_actions::OperationKind::View,
                        other => {
                            return Err(capnp::Error::failed(format!(
                                "unknown exported operation kind `{other}`"
                            )));
                        }
                    },
                    input,
                });
            }
            state
                .surfaces
                .lock()
                .map_err(|_| capnp::Error::failed("action surface registry unavailable".into()))?
                .insert(
                    peer_id,
                    PublishedSurface {
                        peer_id,
                        app_id,
                        route,
                        actions,
                    },
                );
            Ok(())
        })
    }

    fn subscribe_invocations(
        self: Rc<Self>,
        _params: channel_handle::SubscribeInvocationsParams,
        mut results: channel_handle::SubscribeInvocationsResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut subscribers) = self.state.invocation_subscribers.lock() {
            subscribers.insert(self.peer_id, tx);
        } else {
            return Promise::err(capnp::Error::failed(
                "invocation subscriber registry unavailable".into(),
            ));
        }
        let client: invocation_stream::Client = capnp_rpc::new_client(InvocationStreamServer {
            rx: Rc::new(AsyncMutex::new(rx)),
        });
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn complete_invocation(
        self: Rc<Self>,
        params: channel_handle::CompleteInvocationParams,
        _results: channel_handle::CompleteInvocationResults,
    ) -> Promise<(), capnp::Error> {
        let state = self.state.clone();
        let peer_id = self.peer_id;
        Promise::from_future(async move {
            let request = params.get()?;
            let id = request.get_id();
            let completion = InvocationCompletion {
                code: request.get_code()?.to_str()?.to_owned(),
                message: request.get_message()?.to_str()?.to_owned(),
            };
            let mut registry = state
                .pending_invocations
                .lock()
                .map_err(|_| capnp::Error::failed("invocation registry unavailable".into()))?;
            if registry.get(&id).map(|pending| pending.peer_id) != Some(peer_id) {
                return Err(capnp::Error::failed(
                    "an application may complete only its own invocation".into(),
                ));
            }
            let pending = registry
                .remove(&id)
                .ok_or_else(|| capnp::Error::failed("unknown invocation id".into()))?;
            let _ = pending.tx.send(completion);
            Ok(())
        })
    }
}

/// Apply a signed op arriving via capnp `send`: same validation /
/// audit / redaction / compaction pipeline, then
/// fans out via the shared [`FanoutState`].
fn handle_signed_op_send(
    state: &Arc<CapnpBrokerState>,
    peer_id: u64,
    route: &CrdtRoute,
    verified_identity: &AdmittedJoinIdentity,
    envelope: SignedOp,
) {
    let artifact = envelope.artifact;
    let validation = {
        let Ok(mut ch) = state.broker_channels.lock() else {
            return;
        };
        let ch_state = ch.state_for(artifact);
        ch_state.artifacts.insert(artifact, ArtifactKind::Note);
        ch_state
            .artifact_apps
            .insert(artifact, route.app_id.clone());
        validate_admitted_inbound_op(
            &envelope,
            verified_identity.role,
            ch_state,
            std::time::SystemTime::now(),
            Duration::from_secs(300),
        )
    };
    match validation {
        Ok(()) => {
            if let Ok(mut ch) = state.broker_channels.lock() {
                let ch_state = ch.state_for(artifact);
                ch_state.record_validated_op(envelope.clone());
                if envelope.op_class == OpClass::Compaction
                    && let Ok(cp) = collab_types::parse_compaction_payload(&envelope)
                {
                    let _ = ch_state.apply_compaction(&cp);
                }
            }
            if envelope.op_class == OpClass::Redact {
                apply_redaction_from_envelope(&envelope, artifact, &state.broker_channels);
            }
            // Republishing is not vouching.
            state.publisher().signed_document(route, envelope.clone());
            let from_user_id = CapnpBrokerState::participant_id(verified_identity);
            let event = CollabEvent::Document {
                envelope: Box::new(envelope),
                from_user_id,
            };
            if let Some(t) = state.fanout.upgrade() {
                t.fanout_to_route(&event, peer_id, route);
            }
        }
        Err(ValidationError::OpAlreadyRedacted { target }) => {
            tracing::warn!(
                artifact = ?artifact,
                seq = envelope.seq,
                target = ?target,
                "capnp send: late-arriving op was already redacted; storing marker",
            );
            if let Ok(mut ch) = state.broker_channels.lock() {
                let ch_state = ch.state_for(artifact);
                let mut marker_envelope = envelope.clone();
                if let Ok(marker_bytes) = RedactedMarker::v1().canonical() {
                    marker_envelope.payload = marker_bytes;
                }
                ch_state.record_validated_op(marker_envelope);
                ch.audit.record(
                    artifact,
                    AuditEntry {
                        envelope: Box::new(envelope),
                        rejection: Some(ValidationError::OpAlreadyRedacted { target }),
                        at: std::time::SystemTime::now(),
                    },
                );
            }
        }
        Err(reason) => {
            tracing::warn!(
                artifact = ?artifact,
                author = ?envelope.author,
                seq = envelope.seq,
                "capnp send: dropping invalid signed op: {reason}",
            );
            if let Ok(mut ch) = state.broker_channels.lock() {
                ch.audit.record(
                    artifact,
                    AuditEntry {
                        envelope: Box::new(envelope),
                        rejection: Some(reason),
                        at: std::time::SystemTime::now(),
                    },
                );
            }
        }
    }
}

/// Read a page of scrollback ops for `artifact`, encoded as postcard
/// bytes since the capnp `ScrollbackBatch.ops` is `List(Data)`.
fn read_scrollback(
    broker_channels: &Arc<Mutex<BrokerChannels>>,
    artifact: ArtifactId,
    before_seq: u64,
    max: u32,
) -> (Vec<Vec<u8>>, bool) {
    let Ok(mut ch) = broker_channels.lock() else {
        return (Vec::new(), true);
    };
    let ch_state = ch.state_for(artifact);
    // `max` is a peer-supplied page size; clamp rather than wrap so an
    // oversized request degrades to "as many as the u16 cap allows"
    // instead of silently truncating to an unrelated smaller number.
    let capped_max = u16::try_from(max).unwrap_or(u16::MAX);
    let (ops, exhausted) = ch_state.scrollback_for(artifact, before_seq, capped_max);
    let encoded: Vec<Vec<u8>> = ops
        .into_iter()
        .filter_map(|op| postcard::to_stdvec(&op).ok())
        .collect();
    (encoded, exhausted)
}

/// Encode a scrollback page into the capnp response.
///
/// # Errors
///
/// Returns an error if `ops` holds more entries than fit in a capnp
/// `List` count (`u32`) — unreachable in practice since `ops` is always
/// capped by the `u16` `max` accepted in [`read_scrollback`], but checked
/// rather than assumed since this is peer-request-shaped data.
fn fill_scrollback_batch(
    mut b: scrollback_batch::Builder<'_>,
    ops: &[Vec<u8>],
    exhausted: bool,
) -> Result<(), capnp::Error> {
    let n = u32::try_from(ops.len())
        .map_err(|_| capnp::Error::failed("scrollback batch too large to encode".into()))?;
    let mut list = b.reborrow().init_ops(n);
    for (i, op) in ops.iter().enumerate() {
        let idx = u32::try_from(i)
            .map_err(|_| capnp::Error::failed("scrollback batch index overflow".into()))?;
        list.set(idx, op);
    }
    // Bracket seq isn't tracked; consumers needing bounds decode the ops.
    b.set_earliest_seq(0);
    b.set_latest_seq(0);
    b.set_done(exhausted);
    Ok(())
}

// Presence handle + streams

struct CapnpPresenceHandle {
    state: Arc<CapnpBrokerState>,
    channel_id: String,
    peer_id: u64,
    verified_identity: AdmittedJoinIdentity,
}

impl CapnpPresenceHandle {
    /// Widened to room scope — presence is room-wide, only document
    /// payloads are confined to a record.
    fn route(&self) -> CrdtRoute {
        self.full_route().to_cluster_scope()
    }

    /// Unwidened, so peer replication carries the full app/cluster route.
    fn full_route(&self) -> CrdtRoute {
        CrdtRoute::from_channel_key(&self.channel_id)
    }

    fn revalidate(&self) -> Result<(), capnp::Error> {
        self.state
            .revalidate_admission(&self.route(), &self.verified_identity)
            .map(|_| ())
    }

    /// Publish one typed keyed-presence update. A malformed key is isolated
    /// to this update rather than taking down the broker.
    fn publish_keyed(&self, key_bytes: &[u8], payload: PresencePayload) {
        let Ok(key) = postcard::from_bytes::<PresenceKey>(key_bytes) else {
            tracing::debug!(
                len = key_bytes.len(),
                "capnp presence publish: key bytes not decodable — dropping",
            );
            return;
        };
        let route = self.route();
        self.state
            .publish_presence(&self.full_route(), &key, &payload);
        let event = CollabEvent::RemotePresenceUpdate {
            user_id: CapnpBrokerState::participant_id(&self.verified_identity),
            key,
            payload,
        };
        if let Some(fanout) = self.state.fanout.upgrade() {
            fanout.fanout_to_route(&event, self.peer_id, &route);
        }
    }
}

impl presence_handle::Server for CapnpPresenceHandle {
    fn publish_cursor(
        self: Rc<Self>,
        params: presence_handle::PublishCursorParams,
        _results: presence_handle::PublishCursorResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self.revalidate() {
            return Promise::err(error);
        }
        let state = self.state.clone();
        let peer_id = self.peer_id;
        let route = self.route();
        let full_route = self.full_route();
        let verified_identity = self.verified_identity.clone();
        Promise::from_future(async move {
            let r = params.get()?;
            let (x, y) = (r.get_x(), r.get_y());
            // Cursor stream is keyless; full presence uses pointer_default().
            let event = CollabEvent::RemoteCursorMoved {
                user_id: CapnpBrokerState::participant_id(&verified_identity),
                x,
                y,
            };
            state.publish_presence(
                &full_route,
                &PresenceKey::pointer_default(),
                &PresencePayload::Pointer { x, y },
            );
            if let Some(t) = state.fanout.upgrade() {
                t.fanout_to_route(&event, peer_id, &route);
            }
            Ok(())
        })
    }

    fn publish_caret(
        self: Rc<Self>,
        params: presence_handle::PublishCaretParams,
        _results: presence_handle::PublishCaretResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self.revalidate() {
            return Promise::err(error);
        }
        Promise::from_future(async move {
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            // `pos` is a peer-supplied offset; reject rather than wrap it
            // onto an unrelated position on a 32-bit target.
            let pos = usize::try_from(r.get_pos()).map_err(|_| {
                capnp::Error::failed("publishCaret: pos overflows usize on this platform".into())
            })?;
            self.publish_keyed(&key_bytes, PresencePayload::Caret { pos });
            Ok(())
        })
    }

    fn publish_selection(
        self: Rc<Self>,
        params: presence_handle::PublishSelectionParams,
        _results: presence_handle::PublishSelectionResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self.revalidate() {
            return Promise::err(error);
        }
        Promise::from_future(async move {
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            // Both are peer-supplied offsets; reject rather than wrap
            // them onto an unrelated range on a 32-bit target.
            let start = usize::try_from(r.get_start()).map_err(|_| {
                capnp::Error::failed(
                    "publishSelection: start overflows usize on this platform".into(),
                )
            })?;
            let end = usize::try_from(r.get_end()).map_err(|_| {
                capnp::Error::failed(
                    "publishSelection: end overflows usize on this platform".into(),
                )
            })?;
            self.publish_keyed(&key_bytes, PresencePayload::Selection { start, end });
            Ok(())
        })
    }

    fn publish_arrived(
        self: Rc<Self>,
        params: presence_handle::PublishArrivedParams,
        _results: presence_handle::PublishArrivedResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self.revalidate() {
            return Promise::err(error);
        }
        Promise::from_future(async move {
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            self.publish_keyed(&key_bytes, PresencePayload::Arrived);
            Ok(())
        })
    }

    fn publish_left(
        self: Rc<Self>,
        params: presence_handle::PublishLeftParams,
        _results: presence_handle::PublishLeftResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self.revalidate() {
            return Promise::err(error);
        }
        Promise::from_future(async move {
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            self.publish_keyed(&key_bytes, PresencePayload::Left);
            Ok(())
        })
    }

    fn subscribe_all(
        self: Rc<Self>,
        _params: presence_handle::SubscribeAllParams,
        mut results: presence_handle::SubscribeAllResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self.revalidate() {
            return Promise::err(error);
        }
        let rx = self
            .state
            .subscribe_presence(&self.channel_id, self.peer_id);
        let server = PresenceStreamServer {
            rx: Rc::new(AsyncMutex::new(rx)),
        };
        let client: presence_stream::Client = capnp_rpc::new_client(server);
        results.get().set_stream(client);
        Promise::ok(())
    }
}

// Stream servers

// The four stream servers below share a shape: an `Rc<AsyncMutex<mpsc::UnboundedReceiver<_>>>`
// rather than an `Rc<RefCell<_>>`. capnp-rpc will happily dispatch a second
// pipelined `read()` on the same stream before the first one's `recv().await`
// resolves; a `RefCell` would then panic on the second `borrow_mut()` (and
// this crate is `panic = "abort"`, so that panic takes the whole desktop
// session down). The async mutex instead makes the second reader simply wait
// its turn — the two calls serialize instead of racing.

struct OpStreamServer {
    rx: Rc<AsyncMutex<mpsc::UnboundedReceiver<OpEnvelope>>>,
}

impl stream::Server for OpStreamServer {
    fn read(
        self: Rc<Self>,
        _params: stream::ReadParams,
        mut results: stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            let mut guard = rx.lock().await;
            let Some(env) = guard.recv().await else {
                return Err(capnp::Error::disconnected("op stream fanout closed".into()));
            };
            drop(guard);
            let mut r = results.get();
            r.set_op(&env.op_bytes);
            r.set_from_user_id(&env.from_user_id);
            r.set_kind(env.kind);
            Ok(())
        })
    }
}

struct PresenceStreamServer {
    rx: Rc<AsyncMutex<mpsc::UnboundedReceiver<PresenceEnvelope>>>,
}

struct InvocationStreamServer {
    rx: Rc<AsyncMutex<mpsc::UnboundedReceiver<InvocationEnvelope>>>,
}

impl invocation_stream::Server for InvocationStreamServer {
    fn read(
        self: Rc<Self>,
        _params: invocation_stream::ReadParams,
        mut results: invocation_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            let mut guard = rx.lock().await;
            let invocation = guard.recv().await.ok_or_else(|| {
                capnp::Error::disconnected("application invocation stream closed".into())
            })?;
            drop(guard);
            let mut result = results.get();
            result.set_id(invocation.id);
            result.set_resource(&invocation.resource);
            result.set_action(&invocation.action);
            result.set_argument(invocation.argument.as_deref().unwrap_or(""));
            result.set_has_argument(invocation.argument.is_some());
            result.set_actor(&invocation.actor);
            Ok(())
        })
    }
}

impl presence_stream::Server for PresenceStreamServer {
    fn read(
        self: Rc<Self>,
        _params: presence_stream::ReadParams,
        mut results: presence_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            let mut guard = rx.lock().await;
            let Some(env) = guard.recv().await else {
                return Err(capnp::Error::disconnected(
                    "presence stream fanout closed".into(),
                ));
            };
            drop(guard);
            let mut ev: presence_event::Builder = results.get().init_event();
            ev.set_user_id(&env.user_id);
            ev.set_key_bytes(&env.key_bytes);
            let mut payload: presence_payload::Builder = ev.init_payload();
            match env.payload {
                PresencePayload::Pointer { x, y } => {
                    let mut c = payload.init_cursor();
                    c.set_x(x);
                    c.set_y(y);
                }
                PresencePayload::Caret { pos } => {
                    payload.init_caret().set_pos(pos as u64);
                }
                // No dedicated "typing" case in the capnp union yet; it
                // shares the selection wire shape.
                PresencePayload::Selection { start, end }
                | PresencePayload::Typing { start, end } => {
                    let mut s = payload.init_selection();
                    s.set_start(start as u64);
                    s.set_end(end as u64);
                }
                PresencePayload::Arrived => payload.set_arrived(()),
                PresencePayload::Left => payload.set_left(()),
            }
            Ok(())
        })
    }
}

struct ContextStreamServer {
    rx: Rc<AsyncMutex<mpsc::UnboundedReceiver<ContextEnvelope>>>,
}

impl context_stream::Server for ContextStreamServer {
    fn read(
        self: Rc<Self>,
        _params: context_stream::ReadParams,
        mut results: context_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            let mut guard = rx.lock().await;
            let Some(env) = guard.recv().await else {
                return Err(capnp::Error::disconnected(
                    "context stream fanout closed".into(),
                ));
            };
            drop(guard);
            let mut ctx = results.get().init_ctx();
            let collab_count = u32::try_from(env.collaborators.len()).map_err(|_| {
                capnp::Error::failed("context: too many collaborators to encode".into())
            })?;
            {
                let mut collabs = ctx.reborrow().init_collaborators(collab_count);
                for (i, c) in env.collaborators.iter().enumerate() {
                    // `i < env.collaborators.len()`, already proven to fit
                    // in `u32` above; `u32::MAX` is unreachable here.
                    let idx = u32::try_from(i).unwrap_or(u32::MAX);
                    let mut slot = collabs.reborrow().get(idx);
                    slot.set_user_id(&c.id);
                    slot.set_display_name(&c.name);
                    slot.set_color(pack_color(c.color));
                }
            }
            let perm_count = u32::try_from(env.permissions.len()).map_err(|_| {
                capnp::Error::failed("context: too many permissions to encode".into())
            })?;
            {
                let mut perms = ctx.init_permissions(perm_count);
                for (i, p) in env.permissions.iter().enumerate() {
                    let idx = u32::try_from(i).unwrap_or(u32::MAX);
                    perms.set(idx, p.as_str());
                }
            }
            Ok(())
        })
    }
}

/// Convert `[f32; 3]` RGB in 0..=1 to the wire's `0x00RRGGBB`.
fn pack_color(rgb: [f32; 3]) -> u32 {
    let clamp = |c: f32| -> u32 {
        let scaled = (c.clamp(0.0, 1.0) * 255.0).round();
        // `scaled` is clamped to 0.0..=255.0 immediately above, so this
        // cast is always exact and never negative.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "scaled is clamped to 0.0..=255.0 on the line above, so the u32 cast is always exact and non-negative"
        )]
        let result = scaled as u32;
        result
    };
    let r = clamp(rgb[0]);
    let g = clamp(rgb[1]);
    let b = clamp(rgb[2]);
    (r << 16) | (g << 8) | b
}

// Listener wiring

/// Bind `socket_path` and spawn the capnp accept loop. Attaches the
/// created [`CapnpBrokerState`] to `fanout` so every event flowing
/// through the fanout core lands on typed subscribers.
///
/// `publisher` is the other direction — attaching to the peer table lets
/// a capnp app *receive*, `publisher` lets it *send*, same instance
/// [`crate::shell::collab_broker`] uses for peer replication.
///
/// The accept thread runs a `current_thread` tokio runtime + `LocalSet`
/// (capnp-rpc futures are `!Send`), polling `quit` on a 100 ms cadence.
///
/// # Errors
///
/// Returns an error if `socket_path` cannot be bound as a Unix listener
/// (e.g. a stale socket file blocks it, permissions are wrong, or the
/// parent directory does not exist) or if spawning the accept thread
/// fails.
pub fn spawn(
    socket_path: PathBuf,
    fanout: &Arc<FanoutState>,
    broker_channels: Arc<Mutex<BrokerChannels>>,
    publisher: Arc<PeerPublisher>,
    membership: Option<WorkspaceTreeHandle>,
    trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
    quit: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    let _ = std::fs::remove_file(&socket_path);
    let wired = publisher.is_wired();
    let state = CapnpBrokerState::new(
        fanout,
        broker_channels,
        publisher,
        membership,
        trusted_devices,
    );
    fanout.attach_capnp(Arc::clone(&state));

    // Bind on the caller's thread: from the worker thread, a bind
    // failure surfaces only via a log line.
    let std_listener = std::os::unix::net::UnixListener::bind(&socket_path)?;
    std_listener.set_nonblocking(true)?; // tokio's reactor requires this for `from_std`.
    tracing::info!(
        peer_sync = wired,
        "capnp broker: bound {} for native SDK access ({})",
        socket_path.display(),
        if wired {
            "edits from apps on this socket will reach the room"
        } else {
            "LOCAL ONLY — no peer network, so edits from apps on this \
             socket stay on this machine"
        },
    );

    let thread = std::thread::Builder::new()
        .name("qos-collab-broker-capnp".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!("capnp broker: tokio runtime init failed: {e}");
                    return;
                }
            };
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let listener = match tokio::net::UnixListener::from_std(std_listener) {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::warn!(
                            "capnp broker: adopting {} into the reactor failed: {e}",
                            socket_path.display()
                        );
                        return;
                    }
                };
                loop {
                    if quit.load(Ordering::Relaxed) {
                        break;
                    }
                    let stream = tokio::select! {
                        biased;
                        () = tokio::time::sleep(Duration::from_millis(100)) => continue,
                        conn = listener.accept() => conn,
                    };
                    let (stream, _) = match stream {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!("capnp broker: accept failed: {e}; retrying");
                            continue;
                        }
                    };
                    let state = Arc::clone(&state);
                    let peer_id = state.next_peer_id();
                    tokio::task::spawn_local(async move {
                        if let Err(e) = drive_connection(stream, state, peer_id).await {
                            tracing::warn!("capnp broker: peer-{peer_id} disconnected: {e}");
                        }
                    });
                }
            });
        })?;

    Ok(thread)
}

/// Dial the capnp socket at `path` for tests. Production apps go through
/// `runtime_collab::try_connect` (launcher-passed fd). Public so
/// out-of-tree integration tests can reach it too.
///
/// # Errors
///
/// Returns an error if the socket at `path` does not appear and accept a
/// connection within about 2 seconds — the accept loop in [`spawn`] may
/// not have bound yet, or the broker never started.
pub async fn dial_capnp_for_test(
    path: PathBuf,
) -> Result<
    (
        runtime_collab::raw::collab_service::Client,
        tokio::task::JoinHandle<()>,
    ),
    std::io::Error,
> {
    // Retry-with-backoff ~2s: the accept loop may not have bound yet.
    let stream = {
        let mut last_err: Option<std::io::Error> = None;
        let mut stream = None;
        for _ in 0..50 {
            match tokio::net::UnixStream::connect(&path).await {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
        match stream {
            Some(s) => s,
            None => {
                return Err(last_err.unwrap_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "capnp socket did not appear within 2s",
                    )
                }));
            }
        }
    };
    let (r, w) = stream.into_split();
    let network = Box::new(twoparty::VatNetwork::new(
        futures::io::BufReader::new(r.compat()),
        futures::io::BufWriter::new(w.compat_write()),
        rpc_twoparty_capnp::Side::Client,
        capnp::message::ReaderOptions::default(),
    ));
    let mut rpc = RpcSystem::new(network, None);
    let client: runtime_collab::raw::collab_service::Client =
        rpc.bootstrap(rpc_twoparty_capnp::Side::Server);
    let driver = tokio::task::spawn_local(async move {
        let _ = rpc.await;
    });
    Ok((client, driver))
}

// Every `*::Server` impl in this file takes `self: Rc<Self>` (capnp-rpc's
// dispatch convention), so the client/capability chain built here and the
// `RpcSystem` future it drives are inherently `!Send`. That is fine: this
// future is only ever polled via `tokio::task::spawn_local` inside the
// single-threaded `LocalSet` set up in `spawn`, never via `tokio::spawn`,
// so nothing here needs to cross threads.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc's Rc-based server dispatch (self: Rc<Self> throughout this file) makes this future !Send by construction; it is only ever driven via spawn_local on the single-threaded LocalSet in `spawn`, never tokio::spawn"
)]
async fn drive_connection(
    stream: tokio::net::UnixStream,
    state: Arc<CapnpBrokerState>,
    peer_id: u64,
) -> Result<(), capnp::Error> {
    let (reader, writer) = stream.into_split();
    let reader = reader.compat();
    let writer = writer.compat_write();
    let network = Box::new(twoparty::VatNetwork::new(
        futures::io::BufReader::new(reader),
        futures::io::BufWriter::new(writer),
        rpc_twoparty_capnp::Side::Server,
        capnp::message::ReaderOptions::default(),
    ));
    let service = CapnpCollabService { state, peer_id };
    let client: collab_service::Client = capnp_rpc::new_client(service);
    let rpc = RpcSystem::new(network, Some(client.client));
    rpc.await
}

#[cfg(test)]
// Test helpers throughout this module pair `_sk`/`_pk` (secret/public
// key) and `owner`/`editor` role names — the clearest naming for this
// domain, and renaming would obscure the keypair relationship rather
// than clarify it.
#[allow(
    clippy::similar_names,
    reason = "sk/pk and owner/editor pairings are the conventional, intentional names for a keypair and a two-role fixture"
)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    use crate::shell::collab_outbound::recording::{RecordingSink, Sent};
    use collab_types::{CapOverride, Ed25519PublicKey, Role, Subject, sign_op};
    use ed25519_dalek::Signer as _;
    use ed25519_dalek::SigningKey;

    fn certified_join(
        workspace: &str,
        root: &SigningKey,
        device: &SigningKey,
    ) -> runtime_collab::CertifiedJoinIdentity {
        let mut cert = qos_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Test device".into(),
            capabilities: qos_user_identity::DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        qos_user_identity::sign_device_cert(&mut cert, root).expect("root signs device cert");
        runtime_collab::CertifiedJoinIdentity {
            workspace_id: workspace.into(),
            device_certificate: serde_json::to_vec(&cert).expect("certificate JSON"),
            proof: device
                .sign(&runtime_identity::collaboration_join_signing_bytes(
                    workspace,
                ))
                .to_bytes()
                .to_vec(),
        }
    }

    #[test]
    fn certified_join_verifies_root_device_and_workspace_chain() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let device = SigningKey::from_bytes(&[2; 32]);
        let route = CrdtRoute::cluster("workspace-a");
        let presentation = certified_join("workspace-a", &root, &device);
        let verified = verify_join_presentation(
            &route,
            &presentation,
            &root.verifying_key().to_bytes(),
            2,
            false,
            true,
        )
        .expect("valid certified join");
        assert_eq!(verified.user_public_key, root.verifying_key().to_bytes());
        assert_eq!(
            verified.device_public_key,
            device.verifying_key().to_bytes()
        );
    }

    #[test]
    fn certified_join_rejects_unknown_proof_signer_and_tampered_cert() {
        let root = SigningKey::from_bytes(&[3; 32]);
        let device = SigningKey::from_bytes(&[4; 32]);
        let stranger = SigningKey::from_bytes(&[5; 32]);
        let route = CrdtRoute::cluster("workspace-a");
        let mut presentation = certified_join("workspace-a", &root, &device);
        presentation.proof = stranger
            .sign(&runtime_identity::collaboration_join_signing_bytes(
                "workspace-a",
            ))
            .to_bytes()
            .to_vec();
        assert!(
            verify_join_presentation(
                &route,
                &presentation,
                &root.verifying_key().to_bytes(),
                2,
                false,
                true,
            )
            .is_err()
        );

        let mut presentation = certified_join("workspace-a", &root, &device);
        let mut cert: qos_user_identity::DeviceCert =
            serde_json::from_slice(&presentation.device_certificate).expect("certificate");
        cert.device_label.push_str(" (tampered)");
        presentation.device_certificate = serde_json::to_vec(&cert).expect("certificate JSON");
        assert!(
            verify_join_presentation(
                &route,
                &presentation,
                &root.verifying_key().to_bytes(),
                2,
                false,
                true,
            )
            .is_err()
        );
    }

    #[test]
    fn certified_join_rejects_other_workspace_revoked_and_unenrolled_device() {
        let root = SigningKey::from_bytes(&[6; 32]);
        let device = SigningKey::from_bytes(&[7; 32]);
        let presentation = certified_join("workspace-b", &root, &device);
        let expected_root = root.verifying_key().to_bytes();
        assert!(
            verify_join_presentation(
                &CrdtRoute::cluster("workspace-a"),
                &presentation,
                &expected_root,
                2,
                false,
                true,
            )
            .is_err()
        );
        let route = CrdtRoute::cluster("workspace-b");
        let other_root = SigningKey::from_bytes(&[8; 32]);
        assert!(
            verify_join_presentation(
                &route,
                &presentation,
                &other_root.verifying_key().to_bytes(),
                2,
                false,
                true,
            )
            .is_err(),
            "a certificate selected for another workspace identity is rejected"
        );
        assert!(
            verify_join_presentation(&route, &presentation, &expected_root, 2, true, true).is_err()
        );
        assert!(
            verify_join_presentation(&route, &presentation, &expected_root, 2, false, false)
                .is_err()
        );
    }

    fn verified_device(root: &SigningKey) -> VerifiedJoinIdentity {
        VerifiedJoinIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: [42; 32],
        }
    }

    fn downgrade_member_to_viewer(membership: &WorkspaceTreeHandle, principal: &str, room: &str) {
        let mut tree = membership.write();
        let workspace = &mut tree.workspaces[0];
        workspace.owner = None;
        workspace.capabilities.clear();
        workspace.capabilities.push(CapOverride::membership(
            Subject::person(principal),
            room,
            Verb::Read,
        ));
        drop(tree);
    }

    #[test]
    fn certified_join_admission_uses_workspace_membership_and_role() {
        let root = SigningKey::from_bytes(&[20; 32]);
        let principal = principal_id_for_root(root.verifying_key().to_bytes());
        let mut workspace = crate::shell::state::Workspace::test_default(
            crate::shell::state::WorkspaceId(1),
            "Member room",
        );
        let route = CrdtRoute::cluster(workspace.room.to_string());
        workspace.grant_capability(crate::shell::state::CapOverride::membership(
            crate::shell::state::Subject::person(&principal),
            workspace.room.to_string(),
            crate::shell::state::Verb::Write,
        ));
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);

        let admitted = admit_verified_join(&tree, &route, verified_device(&root))
            .expect("a certified editor is admitted");
        assert_eq!(admitted.principal_id, principal);
        assert_eq!(admitted.role, CapRole::Editor);
        assert!(
            verify_session_author(
                &admitted,
                collab_types::Ed25519PublicKey::from_bytes([42; 32]),
            )
            .is_ok(),
            "signed ops are device-authored under the certificate's device key"
        );
        assert!(
            verify_session_author(
                &admitted,
                collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
            )
            .is_err(),
            "the user root certifies the device but does not author this device's ops"
        );
        assert!(
            verify_session_author(
                &admitted,
                collab_types::Ed25519PublicKey::from_bytes([43; 32]),
            )
            .is_err(),
            "a member cannot submit another device's envelope through this session"
        );
    }

    #[test]
    fn certified_join_admission_rejects_wrong_or_removed_root() {
        let member = SigningKey::from_bytes(&[21; 32]);
        let stranger = SigningKey::from_bytes(&[22; 32]);
        let member_id = principal_id_for_root(member.verifying_key().to_bytes());
        let mut workspace = crate::shell::state::Workspace::test_default(
            crate::shell::state::WorkspaceId(2),
            "Revocation room",
        );
        let route = CrdtRoute::cluster(workspace.room.to_string());
        let membership = workspace.grant_capability(crate::shell::state::CapOverride::membership(
            crate::shell::state::Subject::person(&member_id),
            workspace.room.to_string(),
            crate::shell::state::Verb::Read,
        ));
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);

        assert!(admit_verified_join(&tree, &route, verified_device(&stranger)).is_err());
        let admitted = admit_verified_join(&tree, &route, verified_device(&member))
            .expect("member starts admitted");
        let membership_tree = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let state = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(Arc::clone(&membership_tree)),
            None,
        );
        assert!(membership_tree.write().workspaces[0].revoke_capability(&membership));
        assert!(
            state.revalidate_admission(&route, &admitted).is_err(),
            "removing membership rejects the certified session's next operation"
        );
    }

    #[test]
    fn certified_join_admission_rejects_cross_workspace_replay() {
        let root = SigningKey::from_bytes(&[23; 32]);
        let principal = principal_id_for_root(root.verifying_key().to_bytes());
        let mut member_workspace = crate::shell::state::Workspace::test_default(
            crate::shell::state::WorkspaceId(3),
            "Member room",
        );
        member_workspace.set_owner(&principal);
        let other_workspace = crate::shell::state::Workspace::test_default(
            crate::shell::state::WorkspaceId(4),
            "Other room",
        );
        let replay_route = CrdtRoute::cluster(other_workspace.room.to_string());
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(member_workspace);
        tree.workspaces.push(other_workspace);

        assert!(admit_verified_join(&tree, &replay_route, verified_device(&root)).is_err());
    }

    fn assert_replay_and_forgery_are_audited(
        state: &Arc<CapnpBrokerState>,
        broker_channels: &Arc<Mutex<BrokerChannels>>,
        route: &CrdtRoute,
        admitted: &AdmittedJoinIdentity,
        op: &SignedOp,
    ) {
        handle_signed_op_send(state, 1, route, admitted, op.clone());
        let mut forged = op.clone();
        forged.seq = 2;
        forged.payload = DocumentMutation::Incremental(b"tampered".to_vec())
            .encode()
            .expect("typed incremental");
        handle_signed_op_send(state, 1, route, admitted, forged);

        let artifact = op.artifact;
        let audit = state
            .broker_channels
            .lock()
            .expect("broker channels")
            .audit
            .for_artifact(artifact);
        assert_eq!(audit.len(), 2, "replay and forged op are both audited");
        assert!(
            audit
                .iter()
                .any(|entry| matches!(entry.rejection, Some(ValidationError::ReplaySeq { .. })))
        );
        assert!(
            audit
                .iter()
                .any(|entry| matches!(entry.rejection, Some(ValidationError::BadOpSignature(_))))
        );
        assert_eq!(
            broker_channels
                .lock()
                .expect("broker channels")
                .state_for(artifact)
                .scrollback_for(artifact, u64::MAX, 10)
                .0,
            vec![op.clone()],
            "only the valid operation enters scrollback",
        );
    }

    fn assert_published_snapshot(sink: &RecordingSink, route: &CrdtRoute) {
        let published = sink.snapshot();
        let [
            Sent::Signed {
                app_id,
                cluster_id,
                record_id,
                payload,
            },
        ] = published.as_slice()
        else {
            panic!("the certified document was not published to the peer network");
        };
        assert_eq!(app_id, "dev.qos.notes");
        assert_eq!(cluster_id, &route.cluster_id);
        assert_eq!(record_id, "meeting-notes");
        assert_eq!(
            DocumentMutation::decode(payload).expect("typed network payload"),
            DocumentMutation::Snapshot(b"automerge snapshot".to_vec()),
        );
    }

    #[test]
    fn certified_document_send_uses_live_route_role_and_typed_snapshot() {
        let root = SigningKey::from_bytes(&[24; 32]);
        let device = SigningKey::from_bytes(&[25; 32]);
        let principal = principal_id_for_root(root.verifying_key().to_bytes());
        let mut workspace = crate::shell::state::Workspace::test_default(
            crate::shell::state::WorkspaceId(5),
            "Typed document room",
        );
        workspace.set_owner(&principal);
        let route = CrdtRoute::record("dev.qos.notes", workspace.room.to_string(), "meeting-notes");
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let membership = tree.shared();
        let admitted = admit_verified_join(
            &membership.read(),
            &route,
            VerifiedJoinIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: device.verifying_key().to_bytes(),
            },
        )
        .expect("owner is admitted");

        let fanout = Arc::new(FanoutState::new());
        let broker_channels = Arc::new(Mutex::new(BrokerChannels::default()));
        let sink = Arc::new(RecordingSink::default());
        let publisher = PeerPublisher::new(
            sink.clone() as Arc<dyn crate::shell::collab_outbound::PeerPublishSink>
        );
        let state = CapnpBrokerState::new(
            &fanout,
            Arc::clone(&broker_channels),
            publisher,
            Some(Arc::clone(&membership)),
            None,
        );
        fanout.attach_capnp(Arc::clone(&state));
        state.ensure_channel(&route.channel_key());
        let mut received = state.subscribe_ops(&route.channel_key(), 2);

        let artifact = document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id);
        let mut op = SignedOp::new_unsigned(
            DocumentMutation::Snapshot(b"automerge snapshot".to_vec())
                .encode()
                .expect("typed snapshot"),
            artifact,
            Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes()),
            document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id),
            OpClass::Splice,
            SystemTime::now(),
            1,
        );
        sign_op(&mut op, &device).expect("device signs document op");
        state
            .authorize_certified_document(&route, &admitted, &op)
            .expect("live owner may edit the joined document");
        handle_signed_op_send(&state, 1, &route, &admitted, op.clone());

        let delivered = received.try_recv().expect("snapshot fanout");
        assert_eq!(delivered.kind, OpKind::Signed);
        assert_eq!(
            postcard::from_bytes::<SignedOp>(&delivered.op_bytes).expect("signed envelope"),
            op
        );
        assert_eq!(
            broker_channels
                .lock()
                .expect("broker channels")
                .state_for(artifact)
                .op_log
                .len(),
            1,
            "the admitted validator records the certified op without a role attestation"
        );
        assert_published_snapshot(&sink, &route);

        assert_replay_and_forgery_are_audited(&state, &broker_channels, &route, &admitted, &op);

        downgrade_member_to_viewer(&membership, &principal, &route.cluster_id);
        let viewer = state
            .revalidate_admission(&route, &admitted)
            .expect("member still admitted");
        assert_eq!(viewer.role, Role::Viewer);
        assert!(
            state
                .authorize_certified_document(&route, &viewer, &op)
                .is_err(),
            "a live downgrade must take effect before the next send"
        );
        let other_record =
            CrdtRoute::record(route.app_id.clone(), route.cluster_id, "other-record");
        assert!(
            state
                .authorize_certified_document(&other_record, &admitted, &op)
                .is_err(),
            "a signed envelope cannot be replayed through another record route"
        );
    }

    #[test]
    fn live_action_invocation_is_fresh_directed_and_permission_checked() {
        let mut workspace = crate::shell::state::Workspace::test_default(
            crate::shell::state::WorkspaceId(31),
            "Action room",
        );
        workspace.set_owner("owner");
        let route = CrdtRoute::app("dev.qos.files", workspace.room.to_string());
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let membership = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let state = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(membership),
            None,
        );
        state.surfaces.lock().expect("surfaces").insert(
            7,
            PublishedSurface {
                peer_id: 7,
                app_id: "dev.qos.files".into(),
                route,
                actions: vec![PublishedAction {
                    resource: "files.entry:/tmp/report".into(),
                    action: "reveal".into(),
                    resource_kind: "files.entry".into(),
                    resource_label: "report".into(),
                    action_label: "Reveal".into(),
                    description: None,
                    operation_kind: qos_actions::OperationKind::View,
                    input: PublishedInput::None,
                }],
            },
        );
        let (tx, mut invocations) = mpsc::unbounded_channel();
        state
            .invocation_subscribers
            .lock()
            .expect("subscribers")
            .insert(7, tx);

        let completion = state
            .invoke(7, "files.entry:/tmp/report", "reveal", None, "owner")
            .expect("owner can invoke current offer");
        let invocation = invocations.try_recv().expect("directed invocation");
        assert_eq!(invocation.resource, "files.entry:/tmp/report");
        assert!(
            state
                .invoke(7, "files.entry:/tmp/report", "gone", None, "owner")
                .is_err(),
            "an address absent from the current level-triggered surface is stale"
        );
        assert!(
            state
                .invoke(7, "files.entry:/tmp/report", "reveal", None, "stranger")
                .is_err(),
            "broker authorization is checked again at delivery time"
        );
        let pending = state
            .pending_invocations
            .lock()
            .expect("pending")
            .remove(&invocation.id)
            .expect("completion slot");
        pending
            .tx
            .send(InvocationCompletion {
                code: String::new(),
                message: String::new(),
            })
            .expect("caller still waiting");
        assert_eq!(
            completion.recv().expect("completion"),
            InvocationCompletion {
                code: String::new(),
                message: String::new(),
            }
        );
    }
}
