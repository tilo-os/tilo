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
    Actor, ArtifactId, ArtifactKind, CapDecision, DocumentMutation, RedactedMarker, ResourceRef,
    SignedOp, ValidationError, Verb, document_artifact_id, document_attestation_id, role_permits,
    validate_admitted_inbound_op,
};
use runtime_collab::raw::{
    OpKind, action_catalogue_stream, action_input, channel_handle, collab_service, context_stream,
    invocation_stream, presence_event, presence_handle, presence_payload, presence_stream,
    scrollback_batch, stream, task_run_stream,
};
use ui::collaboration::{CollabEvent, PresencePayload};
use ui::presence::PresenceKey;

use crate::shell::collab_broker::{
    AuditEntry, BrokerChannels, CrdtRoute, FanoutState, apply_redaction_from_envelope,
};
use crate::shell::collab_outbound::PeerPublisher;
use crate::shell::state::{
    CapRole, Principal, WorkspaceTask, WorkspaceTaskStatus, WorkspaceTree, WorkspaceTreeHandle,
};

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
    /// Delegated agent principal for this channel. The certified device still
    /// proves which person launched it, while policy uses this revocable id.
    agent_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BrokerActor {
    id: String,
    is_agent: bool,
}

impl AdmittedJoinIdentity {
    fn broker_actor(&self) -> BrokerActor {
        BrokerActor {
            id: self
                .agent_id
                .clone()
                .unwrap_or_else(|| self.principal_id.clone()),
            is_agent: self.agent_id.is_some(),
        }
    }

    fn policy_actor(&self, room: &str) -> Actor {
        match &self.agent_id {
            Some(agent) => Actor::agent(agent),
            None => Actor::person(Some(self.role), self.principal_id.clone()),
        }
        .in_room(room.to_string())
    }
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
        agent_id: None,
    })
}

fn admit_agent_delegation(
    tree: &WorkspaceTree,
    route: &CrdtRoute,
    admitted: &mut AdmittedJoinIdentity,
    agent_id: &str,
) -> Result<(), String> {
    let workspace = tree
        .workspaces
        .iter()
        .find(|workspace| workspace.room.to_string() == route.cluster_id)
        .ok_or_else(|| "agent delegation names an unavailable workspace".to_string())?;
    if !workspace.may_administer(&admitted.principal_id) {
        return Err("only a workspace administrator may launch a delegated agent session".into());
    }
    let role = workspace
        .role_of_agent(agent_id)
        .filter(|role| *role != CapRole::Denied)
        .ok_or_else(|| "the requested agent is not an active workspace member".to_string())?;
    admitted.role = role;
    admitted.agent_id = Some(agent_id.to_string());
    admitted.display_handle = Some(agent_id.to_string());
    Ok(())
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
enum ContextEnvelope {
    Snapshot {
        collaborators: Vec<ui::collaboration::Collaborator>,
        permissions: Vec<String>,
    },
    FollowChanged {
        peer_id: Option<String>,
    },
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
    pub params: Vec<qos_actions::ActionParam>,
    pub contract: qos_actions::ActionContract,
    pub input: PublishedInput,
}

/// A live application's complete, level-triggered action surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedSurface {
    pub peer_id: u64,
    pub app_id: String,
    pub route: CrdtRoute,
    pub actions: Vec<PublishedAction>,
    /// False for a permission-filterable catalogue retained after the app
    /// closes. Only actions that explicitly opted into launchable/durable
    /// discovery are ever retained this way.
    pub live: bool,
}

#[derive(Clone, Debug)]
struct InvocationEnvelope {
    id: u64,
    resource: String,
    action: String,
    argument: Option<String>,
    actor: String,
    context: Option<qos_actions::ActionContext>,
}

#[derive(Clone, Debug)]
struct TaskRunEnvelope {
    task_id: String,
    title: String,
    run_id: u64,
    lease: String,
    planning: runtime_collab::TaskRunPlanning,
    action_context: qos_actions::ActionContext,
}

#[derive(Clone, Debug)]
struct TaskSubscription {
    agent_id: String,
    cluster_id: String,
    tx: mpsc::UnboundedSender<TaskRunEnvelope>,
}

#[derive(Clone, Debug)]
struct PendingTaskRun {
    peer_id: u64,
    agent_id: String,
    cluster_id: String,
    task_id: String,
    lease: String,
    action_context: qos_actions::ActionContext,
    terminal: bool,
}

/// The application's terminal answer to a directed invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationCompletion {
    pub code: String,
    pub message: String,
}

pub struct InvocationJob {
    pub id: u64,
    pub completion: std::sync::mpsc::Receiver<InvocationCompletion>,
}

struct PendingInvocation {
    peer_id: u64,
    tx: std::sync::mpsc::Sender<InvocationCompletion>,
    update: qos_actions::InvocationUpdate,
    last_sequence: u64,
    actor: String,
    cancellable: bool,
    retryable_on_disconnect: bool,
    execution: qos_actions::ExecutionMode,
    result: qos_actions::ResultSpec,
    /// Admission/progress heartbeat. A silent application must not leave a
    /// broker-owned job in `Accepted` forever.
    last_activity: std::time::Instant,
    /// Starts the shorter acknowledgement deadline for cooperative cancel.
    cancellation_requested_at: Option<std::time::Instant>,
}

struct ConfirmationGrant {
    peer_id: u64,
    resource: String,
    action: String,
    /// Binds the exact arguments without retaining secrets or other raw user
    /// input in the grant registry.
    argument_digest: [u8; 32],
    actor: String,
    expires: std::time::Instant,
}

fn argument_digest(argument: Option<&str>) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    match argument {
        Some(value) => {
            hasher.update(b"some\0");
            hasher.update(value.as_bytes());
        }
        None => {
            hasher.update(b"none\0");
        }
    }
    *hasher.finalize().as_bytes()
}

fn bounded_task_message(message: &str) -> Result<String, String> {
    let message = message.trim();
    if message.is_empty() || message.chars().count() > 280 {
        return Err("task update message must be between 1 and 280 characters".into());
    }
    Ok(message.to_string())
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

type ActionCatalogueSubscriber = (String, BrokerActor, mpsc::UnboundedSender<String>);

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
    /// Last safe launchable/durable projection per app and workspace. This is
    /// session catalogue state, never an executable capability by itself.
    archived_surfaces: Mutex<HashMap<(String, String, String), PublishedSurface>>,
    /// Directed invocation stream registered by each application connection.
    invocation_subscribers: Mutex<HashMap<u64, mpsc::UnboundedSender<InvocationEnvelope>>>,
    action_catalogue_subscribers: Mutex<HashMap<u64, ActionCatalogueSubscriber>>,
    pending_invocations: Mutex<HashMap<u64, PendingInvocation>>,
    completed_invocations: Mutex<HashMap<u64, (String, qos_actions::InvocationUpdate)>>,
    confirmation_grants: Mutex<HashMap<String, ConfirmationGrant>>,
    cancellation_requests: Mutex<std::collections::HashSet<u64>>,
    task_subscribers: Mutex<HashMap<u64, TaskSubscription>>,
    pending_task_runs: Mutex<HashMap<u64, PendingTaskRun>>,
    task_run_id: AtomicU64,
    invocation_id: AtomicU64,
}

impl CapnpBrokerState {
    const MAX_RETAINED_INVOCATIONS: usize = 256;
    const INVOCATION_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);
    const CANCELLATION_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

    fn expire_stale_invocations(&self) {
        self.expire_stale_invocations_at(
            std::time::Instant::now(),
            Self::INVOCATION_IDLE_TIMEOUT,
            Self::CANCELLATION_GRACE,
        );
    }

    fn expire_stale_invocations_at(
        &self,
        now: std::time::Instant,
        idle_timeout: std::time::Duration,
        cancellation_grace: std::time::Duration,
    ) {
        let expired = {
            let Ok(mut pending) = self.pending_invocations.lock() else {
                return;
            };
            let ids = pending
                .iter()
                .filter_map(|(id, invocation)| {
                    let cancellation_expired =
                        invocation
                            .cancellation_requested_at
                            .is_some_and(|requested| {
                                now.saturating_duration_since(requested) >= cancellation_grace
                            });
                    let idle_expired =
                        now.saturating_duration_since(invocation.last_activity) >= idle_timeout;
                    (cancellation_expired || idle_expired).then_some((*id, cancellation_expired))
                })
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|(id, cancellation_expired)| {
                    pending
                        .remove(&id)
                        .map(|invocation| (id, cancellation_expired, invocation))
                })
                .collect::<Vec<_>>()
        };
        for (id, cancellation_expired, pending) in expired {
            let (code, message) = if cancellation_expired {
                (
                    "action_cancellation_timeout",
                    "application did not acknowledge cancellation before the deadline",
                )
            } else {
                (
                    "action_timeout",
                    "application stopped reporting action progress before the deadline",
                )
            };
            let completion = InvocationCompletion {
                code: code.into(),
                message: message.into(),
            };
            let update = qos_actions::InvocationUpdate::Failed {
                invocation: qos_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id))),
                error: qos_actions::ActionFailure {
                    code: completion.code.clone(),
                    message: completion.message.clone(),
                    details: None,
                    retryable: pending.retryable_on_disconnect,
                },
            };
            self.record_completion(id, pending.actor.clone(), update);
            if let Ok(mut requests) = self.cancellation_requests.lock() {
                requests.remove(&id);
            }
            let _ = pending.tx.send(completion);
        }
    }

    fn record_completion(&self, id: u64, actor: String, update: qos_actions::InvocationUpdate) {
        if let Ok(mut completed) = self.completed_invocations.lock() {
            completed.insert(id, (actor, update));
            while completed.len() > Self::MAX_RETAINED_INVOCATIONS {
                let Some(oldest) = completed.keys().min().copied() else {
                    break;
                };
                completed.remove(&oldest);
            }
        }
    }

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
            archived_surfaces: Mutex::new(HashMap::new()),
            invocation_subscribers: Mutex::new(HashMap::new()),
            action_catalogue_subscribers: Mutex::new(HashMap::new()),
            pending_invocations: Mutex::new(HashMap::new()),
            completed_invocations: Mutex::new(HashMap::new()),
            confirmation_grants: Mutex::new(HashMap::new()),
            cancellation_requests: Mutex::new(std::collections::HashSet::new()),
            task_subscribers: Mutex::new(HashMap::new()),
            pending_task_runs: Mutex::new(HashMap::new()),
            task_run_id: AtomicU64::new(1),
            invocation_id: AtomicU64::new(1),
        })
    }

    /// Snapshot every action surface currently published by a connected app.
    pub fn published_surfaces(&self) -> Vec<PublishedSurface> {
        let live = self
            .surfaces
            .lock()
            .map(|surfaces| surfaces.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let live_keys = live
            .iter()
            .map(|surface| {
                (
                    surface.app_id.clone(),
                    surface.route.cluster_id.clone(),
                    surface.route.record_id.clone(),
                )
            })
            .collect::<std::collections::HashSet<_>>();
        let mut result = live;
        if let Ok(archived) = self.archived_surfaces.lock() {
            result.extend(
                archived
                    .iter()
                    .filter(|(key, _)| !live_keys.contains(*key))
                    .map(|(_, surface)| surface.clone()),
            );
        }
        result
    }

    #[allow(
        clippy::too_many_lines,
        reason = "catalogue projection applies one ordered authorization and serialization pass"
    )]
    #[cfg(test)]
    fn action_catalogue_json(&self, cluster_id: &str, actor: &str) -> Result<String, String> {
        self.action_catalogue_json_for(
            cluster_id,
            &BrokerActor {
                id: actor.to_string(),
                is_agent: false,
            },
        )
    }

    fn action_catalogue_json_for(
        &self,
        cluster_id: &str,
        actor: &BrokerActor,
    ) -> Result<String, String> {
        let membership = self
            .membership
            .as_ref()
            .ok_or_else(|| "the action broker has no workspace authority".to_string())?;
        let tree = membership.read();
        let workspace = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == cluster_id)
            .ok_or_else(|| "the application workspace is no longer hosted".to_string())?
            .clone();
        drop(tree);
        let caller = if actor.is_agent {
            workspace.actor_for_agent(&actor.id)
        } else {
            workspace.actor_for_person(&actor.id)
        };
        let live_apps = self
            .published_surfaces()
            .into_iter()
            .filter(|surface| surface.live && surface.route.cluster_id == cluster_id)
            .map(|surface| surface.app_id)
            .collect::<std::collections::HashSet<_>>();
        let mut surfaces = self
            .published_surfaces()
            .into_iter()
            .filter(|surface| surface.live && surface.route.cluster_id == cluster_id)
            .filter_map(|surface| {
                let actions = surface
                    .actions
                    .into_iter()
                    .filter(|action| {
                        let Some(verb) = (match action.contract.permission {
                            qos_actions::PermissionRequirement::None => None,
                            qos_actions::PermissionRequirement::Read => Some(Verb::Read),
                            qos_actions::PermissionRequirement::Comment => Some(Verb::Comment),
                            qos_actions::PermissionRequirement::Write => Some(Verb::Write),
                            qos_actions::PermissionRequirement::Invite => Some(Verb::Invite),
                            qos_actions::PermissionRequirement::Admin => Some(Verb::Admin),
                        }) else {
                            return true;
                        };
                        workspace.decide(
                            &caller,
                            &ResourceRef::Action {
                                app: surface.app_id.clone(),
                                action: action.action.clone(),
                            },
                            verb,
                        ) == CapDecision::Allow
                    })
                    .map(|action| {
                        let input = match action.input {
                            PublishedInput::None => serde_json::json!({"kind": "none"}),
                            PublishedInput::Text {
                                prompt,
                                placeholder,
                                wants_content,
                            } => serde_json::json!({
                                "kind": "text",
                                "prompt": prompt,
                                "placeholder": placeholder,
                                "wants_content": wants_content,
                            }),
                        };
                        serde_json::json!({
                            "resource": action.resource,
                            "action": action.action,
                            "resource_kind": action.resource_kind,
                            "resource_label": action.resource_label,
                            "action_label": action.action_label,
                            "description": action.description,
                            "operation_kind": action.operation_kind,
                            "params": action.params,
                            "contract": action.contract,
                            "input": input,
                        })
                    })
                    .collect::<Vec<_>>();
                (!actions.is_empty()).then(|| {
                    serde_json::json!({
                        "peer_id": surface.peer_id,
                        "app_id": surface.app_id,
                        "cluster_id": surface.route.cluster_id,
                        "record_id": surface.route.record_id,
                        "live": surface.live,
                        "actions": actions,
                    })
                })
            })
            .collect::<Vec<_>>();
        for app in crate::shell::launcher::gui_apps()
            .into_iter()
            .filter(|app| !live_apps.contains(&app.package_id))
            .filter(|app| {
                workspace.decide(
                    &caller,
                    &ResourceRef::App(app.package_id.clone()),
                    Verb::Write,
                ) == CapDecision::Allow
            })
        {
            let package_id = app.package_id;
            let display_name = app.display_name;
            surfaces.push(serde_json::json!({
                "peer_id": 0,
                "app_id": package_id,
                "cluster_id": cluster_id,
                "record_id": "",
                "live": true,
                "actions": [{
                    "resource": format!("app.{package_id}"),
                    "action": "open",
                    "resource_kind": "applications.app",
                    "resource_label": display_name,
                    "action_label": format!("Open {display_name}"),
                    "description": "Launch this application into the current workspace",
                    "operation_kind": qos_actions::OperationKind::Effect,
                    "params": [],
                    "contract": qos_actions::ActionContract::default(),
                    "input": {"kind": "none"},
                }],
            }));
        }
        serde_json::to_string(&serde_json::json!({
            "version": 1,
            "surfaces": surfaces,
        }))
        .map_err(|error| error.to_string())
    }

    fn publish_action_catalogue(&self) {
        let subscribers = self
            .action_catalogue_subscribers
            .lock()
            .map(|subscribers| {
                subscribers
                    .iter()
                    .map(|(peer, (cluster, actor, tx))| {
                        (*peer, cluster.clone(), actor.clone(), tx.clone())
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut disconnected = Vec::new();
        for (peer, cluster, actor, tx) in subscribers {
            let delivered = self
                .action_catalogue_json_for(&cluster, &actor)
                .is_ok_and(|snapshot| tx.send(snapshot).is_ok());
            if !delivered {
                disconnected.push(peer);
            }
        }
        if !disconnected.is_empty()
            && let Ok(mut subscribers) = self.action_catalogue_subscribers.lock()
        {
            for peer in disconnected {
                subscribers.remove(&peer);
            }
        }
    }

    /// Address one operation back to the exact connection that published it.
    /// The returned receiver resolves only after the application reports a
    /// structured completion; dropping the connection resolves as a refusal.
    ///
    /// # Errors
    ///
    /// Returns an error if the surface or action is unavailable or the
    /// invocation cannot be sent to its owning connection.
    pub fn invoke(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
    ) -> Result<std::sync::mpsc::Receiver<InvocationCompletion>, String> {
        self.start_invocation(peer_id, resource, action, argument, actor)
            .map(|job| job.completion)
    }

    /// Start an unconfirmed invocation.
    ///
    /// # Errors
    ///
    /// Returns an error when the action is unavailable or cannot be delivered.
    pub fn start_invocation(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
    ) -> Result<InvocationJob, String> {
        self.start_invocation_confirmed(peer_id, resource, action, argument, actor, None)
    }

    /// Prepare a one-use confirmation token bound to this exact invocation.
    ///
    /// # Errors
    ///
    /// Returns an error when the action, phrase, or confirmation registry is invalid.
    pub fn prepare_confirmation(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
        phrase: Option<&str>,
    ) -> Result<String, String> {
        let offered =
            self.surfaces
                .lock()
                .ok()
                .and_then(|surfaces| surfaces.get(&peer_id).cloned())
                .and_then(|surface| {
                    surface.actions.into_iter().find(|candidate| {
                        candidate.resource == resource && candidate.action == action
                    })
                })
                .ok_or_else(|| "the application no longer publishes that action".to_string())?;
        match &offered.contract.confirmation {
            qos_actions::ConfirmationPolicy::None => {
                return Err("the action does not require confirmation".into());
            }
            qos_actions::ConfirmationPolicy::Confirm => {}
            qos_actions::ConfirmationPolicy::Phrase { phrase: expected } => {
                if phrase != Some(expected.as_str()) {
                    return Err("the confirmation phrase did not match".into());
                }
            }
        }
        let token = uuid::Uuid::new_v4().to_string();
        let mut grants = self
            .confirmation_grants
            .lock()
            .map_err(|_| "the confirmation registry is unavailable".to_string())?;
        let now = std::time::Instant::now();
        grants.retain(|_, grant| grant.expires >= now);
        grants.insert(
            token.clone(),
            ConfirmationGrant {
                peer_id,
                resource: resource.to_string(),
                action: action.to_string(),
                argument_digest: argument_digest(argument),
                actor: actor.to_string(),
                expires: std::time::Instant::now()
                    .checked_add(std::time::Duration::from_secs(60))
                    .ok_or_else(|| "confirmation expiry overflowed".to_string())?,
            },
        );
        drop(grants);
        Ok(token)
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "invocation admission validates one complete broker protocol transaction"
    )]
    /// Start an invocation with optional broker confirmation evidence.
    ///
    /// # Errors
    ///
    /// Returns an error when admission, authorization, confirmation, or delivery fails.
    pub fn start_invocation_confirmed(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
        confirmation_token: Option<&str>,
    ) -> Result<InvocationJob, String> {
        self.start_invocation_confirmed_for(
            peer_id,
            resource,
            action,
            argument,
            &BrokerActor {
                id: actor.to_string(),
                is_agent: false,
            },
            confirmation_token,
            None,
        )
    }

    fn start_invocation_confirmed_for(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &BrokerActor,
        confirmation_token: Option<&str>,
        context: Option<qos_actions::ActionContext>,
    ) -> Result<InvocationJob, String> {
        let offered = self
            .surfaces
            .lock()
            .ok()
            .and_then(|surfaces| surfaces.get(&peer_id).cloned())
            .and_then(|surface| {
                let action = surface
                    .actions
                    .iter()
                    .find(|candidate| candidate.resource == resource && candidate.action == action)
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
        let caller = if actor.is_agent {
            workspace.actor_for_agent(&actor.id)
        } else {
            workspace.actor_for_person(&actor.id)
        };
        let target = ResourceRef::Action {
            app: offered.0.app_id.clone(),
            action: action.to_string(),
        };
        if let qos_actions::Availability::Unavailable { reason, .. } =
            &offered.1.contract.availability
        {
            return Err(format!("the application action is unavailable: {reason}"));
        }
        let verb = match offered.1.contract.permission {
            qos_actions::PermissionRequirement::None => None,
            qos_actions::PermissionRequirement::Read => Some(Verb::Read),
            qos_actions::PermissionRequirement::Comment => Some(Verb::Comment),
            qos_actions::PermissionRequirement::Write => Some(Verb::Write),
            qos_actions::PermissionRequirement::Invite => Some(Verb::Invite),
            qos_actions::PermissionRequirement::Admin => Some(Verb::Admin),
        };
        if verb.is_some_and(|verb| workspace.decide(&caller, &target, verb) != CapDecision::Allow) {
            return Err("the caller is not allowed to invoke that application action".into());
        }
        drop(tree);
        if !matches!(
            offered.1.contract.confirmation,
            qos_actions::ConfirmationPolicy::None
        ) {
            let token = confirmation_token.ok_or_else(|| {
                "action_confirmation_required: request and present a one-use confirmation token"
                    .to_string()
            })?;
            let grant = self
                .confirmation_grants
                .lock()
                .map_err(|_| "the confirmation registry is unavailable".to_string())?
                .remove(token)
                .ok_or_else(|| "the confirmation token is unknown or already used".to_string())?;
            if grant.expires < std::time::Instant::now()
                || grant.peer_id != peer_id
                || grant.resource != resource
                || grant.action != action
                || grant.argument_digest != argument_digest(argument)
                || grant.actor != actor.id
            {
                return Err(
                    "the confirmation token is expired or does not match this invocation".into(),
                );
            }
        }
        let id = self.invocation_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = std::sync::mpsc::channel();
        self.pending_invocations
            .lock()
            .map_err(|_| "the invocation registry is unavailable".to_string())?
            .insert(
                id,
                PendingInvocation {
                    peer_id,
                    tx,
                    update: qos_actions::InvocationUpdate::Accepted {
                        invocation: qos_actions::InvocationId(uuid::Uuid::from_u128(u128::from(
                            id,
                        ))),
                    },
                    last_sequence: 0,
                    actor: actor.id.clone(),
                    cancellable: matches!(
                        offered.1.contract.execution,
                        qos_actions::ExecutionMode::Background { cancellable: true }
                            | qos_actions::ExecutionMode::Streaming {
                                cancellable: true,
                                ..
                            }
                    ),
                    retryable_on_disconnect: matches!(
                        offered.1.contract.retry,
                        qos_actions::RetryPolicy::Idempotent
                            | qos_actions::RetryPolicy::RetryWithInvocationId
                    ),
                    execution: offered.1.contract.execution.clone(),
                    result: offered.1.contract.result.clone(),
                    last_activity: std::time::Instant::now(),
                    cancellation_requested_at: None,
                },
            );
        if sender
            .send(InvocationEnvelope {
                id,
                resource: resource.to_string(),
                action: action.to_string(),
                argument: argument.map(str::to_string),
                actor: actor.id.clone(),
                context,
            })
            .is_err()
        {
            let _ = self
                .pending_invocations
                .lock()
                .map(|mut pending| pending.remove(&id));
            return Err("the application invocation stream closed".into());
        }
        Ok(InvocationJob { id, completion: rx })
    }

    pub fn invocation_status(&self, id: u64) -> Option<qos_actions::InvocationUpdate> {
        self.expire_stale_invocations();
        self.pending_invocations
            .lock()
            .ok()
            .and_then(|pending| pending.get(&id).map(|job| job.update.clone()))
            .or_else(|| {
                self.completed_invocations
                    .lock()
                    .ok()
                    .and_then(|jobs| jobs.get(&id).map(|(_, update)| update.clone()))
            })
    }

    pub fn invocation_statuses(&self, actor: &str) -> Vec<(u64, qos_actions::InvocationUpdate)> {
        self.expire_stale_invocations();
        let mut jobs = self
            .pending_invocations
            .lock()
            .map(|pending| {
                pending
                    .iter()
                    .filter(|(_, job)| job.actor == actor)
                    .map(|(id, job)| (*id, job.update.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if let Ok(completed) = self.completed_invocations.lock() {
            jobs.extend(
                completed
                    .iter()
                    .filter(|(_, (owner, _))| owner == actor)
                    .map(|(id, (_, update))| (*id, update.clone())),
            );
        }
        jobs.sort_by_key(|(id, _)| *id);
        jobs
    }

    /// Request cooperative cancellation for an actor-owned invocation.
    ///
    /// # Errors
    ///
    /// Returns an error when the invocation is unknown, foreign, or non-cancellable.
    pub fn request_cancellation(&self, id: u64, actor: &str) -> Result<(), String> {
        self.expire_stale_invocations();
        let mut pending = self
            .pending_invocations
            .lock()
            .map_err(|_| "the invocation registry is unavailable".to_string())?;
        let job = pending
            .get(&id)
            .ok_or_else(|| "the invocation is not running".to_string())?;
        if job.actor != actor {
            return Err("only the invocation owner may cancel it".into());
        }
        if !job.cancellable {
            return Err("the action did not declare cooperative cancellation".into());
        }
        let job = pending
            .get_mut(&id)
            .ok_or_else(|| "the invocation is not running".to_string())?;
        job.cancellation_requested_at
            .get_or_insert_with(std::time::Instant::now);
        drop(pending);
        self.cancellation_requests
            .lock()
            .map_err(|_| "the cancellation registry is unavailable".to_string())?
            .insert(id);
        Ok(())
    }

    pub fn can_cancel(&self, id: u64, actor: &str) -> bool {
        self.expire_stale_invocations();
        self.pending_invocations
            .lock()
            .ok()
            .and_then(|pending| {
                pending
                    .get(&id)
                    .map(|job| job.cancellable && job.actor == actor)
            })
            .unwrap_or(false)
    }

    /// Whether this room currently has a subscribed, admitted agent that can
    /// receive a task lease. Presence or membership alone is not sufficient.
    pub fn agent_is_live_task_capable(&self, cluster_id: &str, agent_id: &str) -> bool {
        self.task_subscribers.lock().is_ok_and(|subscribers| {
            subscribers.values().any(|subscriber| {
                subscriber.cluster_id == cluster_id && subscriber.agent_id == agent_id
            })
        })
    }

    /// Mint and deliver one live-only task run. No durable/offline queue is
    /// kept: losing the subscribed agent means Launch remains unavailable.
    pub fn dispatch_workspace_task(&self, cluster_id: &str, task_id: &str) -> Result<u64, String> {
        let membership = self
            .membership
            .as_ref()
            .ok_or_else(|| "the task broker has no workspace authority".to_string())?;
        let (workspace_id, task) = {
            let tree = membership.read();
            let workspace = tree
                .workspaces
                .iter()
                .find(|workspace| workspace.room.to_string() == cluster_id)
                .ok_or_else(|| "the task workspace is no longer hosted".to_string())?;
            let task = workspace
                .tasks
                .iter()
                .find(|task| task.id == task_id)
                .cloned()
                .ok_or_else(|| "the task no longer exists".to_string())?;
            (workspace.id, task)
        };
        let agent_id = task
            .assigned_agent
            .clone()
            .filter(|_| task.route != crate::shell::state::WorkspaceTaskRoute::Manual)
            .ok_or_else(|| "the task has no agent route".to_string())?;
        if task.status.is_finished() {
            return Err("a finished task cannot be launched".into());
        }
        let (peer_id, tx) = self
            .task_subscribers
            .lock()
            .map_err(|_| "the task subscription registry is unavailable".to_string())?
            .iter()
            .find(|(_, subscriber)| {
                subscriber.cluster_id == cluster_id && subscriber.agent_id == agent_id
            })
            .map(|(peer_id, subscriber)| (*peer_id, subscriber.tx.clone()))
            .ok_or_else(|| "the assigned agent is not live and task-capable".to_string())?;
        let run_id = self.task_run_id.fetch_add(1, Ordering::Relaxed);
        let lease = uuid::Uuid::new_v4().to_string();
        let context = task.action_context();
        tx.send(TaskRunEnvelope {
            task_id: task.id.clone(),
            title: task.title.clone(),
            run_id,
            lease: lease.clone(),
            planning: match task.route {
                crate::shell::state::WorkspaceTaskRoute::RulesOnly => {
                    runtime_collab::TaskRunPlanning::RulesOnly
                }
                crate::shell::state::WorkspaceTaskRoute::Model => {
                    runtime_collab::TaskRunPlanning::Model
                }
                crate::shell::state::WorkspaceTaskRoute::Manual => {
                    return Err("a manual task cannot be dispatched to an agent".into());
                }
            },
            action_context: context.clone(),
        })
        .map_err(|_| "the assigned agent task stream closed".to_string())?;
        self.pending_task_runs
            .lock()
            .map_err(|_| "the task run registry is unavailable".to_string())?
            .insert(
                run_id,
                PendingTaskRun {
                    peer_id,
                    agent_id: agent_id.clone(),
                    cluster_id: cluster_id.to_string(),
                    task_id: task.id.clone(),
                    lease,
                    action_context: context,
                    terminal: false,
                },
            );
        let mut tree = membership.write();
        let actor = format!("agent:{agent_id}");
        let _ = tree.set_workspace_task_status(
            workspace_id,
            &task.id,
            WorkspaceTaskStatus::InProgress,
            &actor,
        );
        let _ = tree.append_workspace_task_audit(
            workspace_id,
            &task.id,
            &actor,
            format!("Broker dispatched live run {run_id}"),
        );
        Ok(run_id)
    }

    fn task_run_for(
        &self,
        peer_id: u64,
        agent_id: &str,
        run_id: u64,
        lease: &str,
    ) -> Result<PendingTaskRun, String> {
        self.pending_task_runs
            .lock()
            .map_err(|_| "the task run registry is unavailable".to_string())?
            .get(&run_id)
            .filter(|run| {
                run.peer_id == peer_id
                    && run.agent_id == agent_id
                    && run.lease == lease
                    && !run.terminal
            })
            .cloned()
            .ok_or_else(|| {
                "the task run lease is unknown, expired, or belongs to another agent".to_string()
            })
    }

    /// A person cancelled this task from the native Task Center. Agents may
    /// observe that fact, but only the person-facing task surface changes the
    /// durable cancellation state.
    fn task_run_cancelled(
        &self,
        peer_id: u64,
        agent_id: &str,
        run_id: u64,
        lease: &str,
    ) -> Result<bool, String> {
        let run = self.task_run_for(peer_id, agent_id, run_id, lease)?;
        let membership = self
            .membership
            .as_ref()
            .ok_or_else(|| "the task broker has no workspace authority".to_string())?;
        let tree = membership.read();
        let workspace = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == run.cluster_id)
            .ok_or_else(|| "the task workspace is no longer hosted".to_string())?;
        Ok(workspace
            .tasks
            .iter()
            .find(|task| task.id == run.task_id)
            .is_some_and(|task| task.status == WorkspaceTaskStatus::Cancelled))
    }

    fn record_task_run_update(
        &self,
        peer_id: u64,
        agent_id: &str,
        run_id: u64,
        lease: &str,
        update: runtime_collab::TaskRunUpdate,
    ) -> Result<(), String> {
        let run = self.task_run_for(peer_id, agent_id, run_id, lease)?;
        let (summary, status, terminal) = match update {
            runtime_collab::TaskRunUpdate::Accepted => {
                ("Agent accepted run".to_string(), None, false)
            }
            runtime_collab::TaskRunUpdate::Progress { message } => (
                format!("Agent progress · {}", bounded_task_message(&message)?),
                None,
                false,
            ),
            runtime_collab::TaskRunUpdate::Succeeded { message } => (
                format!("Agent completed · {}", bounded_task_message(&message)?),
                Some(WorkspaceTaskStatus::Done),
                true,
            ),
            runtime_collab::TaskRunUpdate::Failed { message } => (
                format!(
                    "Agent needs attention · {}",
                    bounded_task_message(&message)?
                ),
                Some(WorkspaceTaskStatus::Waiting),
                true,
            ),
            runtime_collab::TaskRunUpdate::Cancelled { message } => (
                format!("Agent cancelled · {}", bounded_task_message(&message)?),
                Some(WorkspaceTaskStatus::Cancelled),
                true,
            ),
        };
        if terminal {
            let mut runs = self
                .pending_task_runs
                .lock()
                .map_err(|_| "the task run registry is unavailable".to_string())?;
            let Some(current) = runs.get_mut(&run_id) else {
                return Err("the task run disappeared".into());
            };
            if current.terminal {
                return Err("the task run is already terminal".into());
            }
            current.terminal = true;
        }
        let membership = self
            .membership
            .as_ref()
            .ok_or_else(|| "the task broker has no workspace authority".to_string())?;
        let mut tree = membership.write();
        let workspace = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == run.cluster_id)
            .ok_or_else(|| "the task workspace is no longer hosted".to_string())?;
        let actor = format!("agent:{}", run.agent_id);
        if let Some(status) = status {
            let _ = tree.set_workspace_task_status(workspace.id, &run.task_id, status, &actor);
        }
        let _ = tree.append_workspace_task_audit(workspace.id, &run.task_id, &actor, summary);
        Ok(())
    }

    fn start_task_action(
        &self,
        peer_id: u64,
        agent_id: &str,
        run_id: u64,
        lease: &str,
        target_peer: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
    ) -> Result<InvocationJob, String> {
        let run = self.task_run_for(peer_id, agent_id, run_id, lease)?;
        if self.task_run_cancelled(peer_id, agent_id, run_id, lease)? {
            return Err("the task was cancelled; no further action may start".into());
        }
        let confirmation_required = self
            .surfaces
            .lock()
            .ok()
            .and_then(|surfaces| surfaces.get(&target_peer).cloned())
            .and_then(|surface| {
                (surface.route.cluster_id == run.cluster_id)
                    .then_some(surface)
                    .and_then(|surface| {
                        surface.actions.into_iter().find(|candidate| {
                            candidate.resource == resource && candidate.action == action
                        })
                    })
            })
            .ok_or_else(|| "the task action is no longer published in this workspace".to_string())?
            .contract
            .confirmation;
        if !matches!(confirmation_required, qos_actions::ConfirmationPolicy::None) {
            return Err(
                "task action requires an interactive confirmation and cannot run unattended".into(),
            );
        }
        self.start_invocation_confirmed_for(
            target_peer,
            resource,
            action,
            argument,
            &BrokerActor {
                id: run.agent_id,
                is_agent: true,
            },
            None,
            Some(run.action_context),
        )
    }

    fn remove_connection(&self, peer_id: u64) {
        if let Ok(mut surfaces) = self.surfaces.lock() {
            surfaces.remove(&peer_id);
        }
        if let Ok(mut subscribers) = self.invocation_subscribers.lock() {
            subscribers.remove(&peer_id);
        }
        if let Ok(mut subscribers) = self.action_catalogue_subscribers.lock() {
            subscribers.remove(&peer_id);
        }
        if let Ok(mut subscribers) = self.task_subscribers.lock() {
            subscribers.remove(&peer_id);
        }
        if let Ok(mut pending) = self.pending_invocations.lock() {
            let ids = pending
                .iter()
                .filter_map(|(id, invocation)| (invocation.peer_id == peer_id).then_some(*id))
                .collect::<Vec<_>>();
            for id in ids {
                if let Some(invocation) = pending.remove(&id) {
                    let completion = InvocationCompletion {
                        code: "action_failed".into(),
                        message: "application disconnected before completing the action".into(),
                    };
                    let update = qos_actions::InvocationUpdate::Failed {
                        invocation: qos_actions::InvocationId(uuid::Uuid::from_u128(u128::from(
                            id,
                        ))),
                        error: qos_actions::ActionFailure {
                            code: completion.code.clone(),
                            message: completion.message.clone(),
                            details: None,
                            retryable: invocation.retryable_on_disconnect,
                        },
                    };
                    self.record_completion(id, invocation.actor.clone(), update);
                    if let Ok(mut requests) = self.cancellation_requests.lock() {
                        requests.remove(&id);
                    }
                    let _ = invocation.tx.send(completion);
                }
            }
        }
        self.publish_action_catalogue();
    }

    /// `joinChannel(channelId, identity)` carries no announced handle,
    /// so the room lookup is the only answer for who to stamp traffic as.
    fn participant_id(admitted: &AdmittedJoinIdentity) -> String {
        admitted.agent_id.clone().unwrap_or_else(|| {
            admitted
                .display_handle
                .clone()
                .unwrap_or_else(|| admitted.principal_id.clone())
        })
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
        if let Some(agent_id) = &admitted.agent_id {
            admit_agent_delegation(&membership.read(), route, &mut current, agent_id).map_err(
                |error| {
                    capnp::Error::failed(format!(
                        "certified agent session is no longer admitted: {error}"
                    ))
                },
            )?;
        } else {
            current.display_handle.clone_from(&admitted.display_handle);
        }
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
        let actor = admitted
            .policy_actor(&route.cluster_id)
            .with_principal(Principal::App(route.app_id.clone()));
        let resource = ResourceRef::Artifact {
            app: Some(route.app_id.clone()),
            kind: ArtifactKind::Note,
            instance: expected_artifact,
        };
        let required = collab_types::caps::verb_for_opclass(envelope.op_class);
        let decision = workspace.decide(&actor, &resource, required);
        let role_allows = role_permits(admitted.role, ArtifactKind::Note, envelope.op_class);
        drop(tree);
        if decision != CapDecision::Allow || !role_allows {
            return Err(capnp::Error::failed(
                "current workspace capability denies this document action".into(),
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
            CollabEvent::FollowChanged { peer_id } => {
                Self::fanout_follow_changed(channels, except, peer_id.as_deref());
            }
            CollabEvent::ActionArrived { type_id, .. } => {
                // Unreachable today; said out loud, not silently dropped.
                tracing::warn!(
                    %type_id,
                    "capnp fanout: no action stream yet, action dropped"
                );
            }
            CollabEvent::ScrollbackBatch { .. }
            | CollabEvent::Invocation { .. }
            | CollabEvent::InvocationCancellation { .. }
            | CollabEvent::ActionCatalogue { .. }
            | CollabEvent::PublishedInvocationResult { .. } => {
                // Invocation delivery and cooperative cancellation are
                // point-to-point and never enter route fanout.
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
        let env = ContextEnvelope::Snapshot {
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

    /// Session-local view binding. It rides the session context stream but
    /// is explicitly tagged so clients do not replace their roster with an
    /// empty control frame.
    fn fanout_follow_changed<'a>(
        channels: impl Iterator<Item = &'a mut ChannelSubscribers>,
        except: u64,
        peer_id: Option<&str>,
    ) {
        let env = ContextEnvelope::FollowChanged {
            peer_id: peer_id.map(str::to_owned),
        };
        for subs in channels {
            subs.context.retain(|sub| {
                if sub.peer_id == except {
                    return true;
                }
                sub.tx.send(env.clone()).is_ok()
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
            let participant_kind = reader.get_participant_kind()?.to_str().map_err(|error| {
                capnp::Error::failed(format!("participant kind not utf8: {error}"))
            })?;
            match participant_kind {
                "" | "person" => {
                    if !participant_handle.is_empty() {
                        verified_identity.display_handle = Some(participant_handle);
                    }
                }
                "agent" => {
                    if participant_handle.is_empty() {
                        return Err(capnp::Error::failed(
                            "an agent session must name its workspace agent".into(),
                        ));
                    }
                    let membership = state.membership.as_ref().ok_or_else(|| {
                        capnp::Error::failed("agent session has no workspace authority".into())
                    })?;
                    admit_agent_delegation(
                        &membership.read(),
                        &route,
                        &mut verified_identity,
                        &participant_handle,
                    )
                    .map_err(|error| {
                        capnp::Error::failed(format!("agent collaboration join rejected: {error}"))
                    })?;
                }
                other => {
                    return Err(capnp::Error::failed(format!(
                        "unknown collaboration participant kind `{other}`"
                    )));
                }
            }
            tracing::info!(
                target: "qos_collab_probe",
                channel = %channel_id,
                peer_id,
                role = ?verified_identity.role,
                principal = %verified_identity.principal_id,
                acting_as = ?verified_identity.agent_id,
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

    #[allow(
        clippy::too_many_lines,
        reason = "the RPC validates and installs one complete published action surface"
    )]
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
                    params: {
                        let json = row.get_params_json()?.to_str()?;
                        if json.is_empty() {
                            Vec::new()
                        } else {
                            serde_json::from_str(json).map_err(|error| {
                                capnp::Error::failed(format!(
                                    "invalid exported action parameters: {error}"
                                ))
                            })?
                        }
                    },
                    contract: {
                        let json = row.get_contract_json()?.to_str()?;
                        if json.is_empty() {
                            match row.get_operation_kind()?.to_str()? {
                                "view" => qos_actions::ActionContract::view(),
                                _ => qos_actions::ActionContract::default(),
                            }
                        } else {
                            serde_json::from_str(json).map_err(|error| {
                                capnp::Error::failed(format!(
                                    "invalid exported action contract: {error}"
                                ))
                            })?
                        }
                    },
                    input,
                });
                let published = actions.last().ok_or_else(|| {
                    capnp::Error::failed("published action disappeared during validation".into())
                })?;
                if published.resource_kind.trim().is_empty()
                    || published.resource_label.trim().is_empty()
                    || published.action_label.trim().is_empty()
                {
                    return Err(capnp::Error::failed(
                        "exported actions require non-empty kind and labels".into(),
                    ));
                }
                if let Some(reason) = published.contract.validate_declaration().into_iter().next() {
                    return Err(capnp::Error::failed(format!(
                        "invalid exported action contract: {reason}"
                    )));
                }
                let mut parameter_names = std::collections::HashSet::new();
                for parameter in &published.params {
                    if !parameter_names.insert(parameter.name.as_ref()) {
                        return Err(capnp::Error::failed(
                            "exported action parameter names must be unique".into(),
                        ));
                    }
                    if let Some(reason) = parameter.validate_declaration().into_iter().next() {
                        return Err(capnp::Error::failed(format!(
                            "invalid exported action parameter `{}`: {reason}",
                            parameter.name
                        )));
                    }
                }
            }
            let surface = PublishedSurface {
                peer_id,
                app_id,
                route,
                actions,
                live: true,
            };
            let archived_actions = surface
                .actions
                .iter()
                .filter(|action| {
                    !matches!(action.contract.discovery, qos_actions::DiscoveryScope::Live)
                })
                .cloned()
                .collect::<Vec<_>>();
            let archive_key = (
                surface.app_id.clone(),
                surface.route.cluster_id.clone(),
                surface.route.record_id.clone(),
            );
            if let Ok(mut archived) = state.archived_surfaces.lock() {
                if archived_actions.is_empty() {
                    archived.remove(&archive_key);
                } else {
                    archived.insert(
                        archive_key,
                        PublishedSurface {
                            actions: archived_actions,
                            live: false,
                            ..surface.clone()
                        },
                    );
                }
            }
            state
                .surfaces
                .lock()
                .map_err(|_| capnp::Error::failed("action surface registry unavailable".into()))?
                .insert(peer_id, surface);
            state.publish_action_catalogue();
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

    fn subscribe_task_runs(
        self: Rc<Self>,
        _params: channel_handle::SubscribeTaskRunsParams,
        mut results: channel_handle::SubscribeTaskRunsResults,
    ) -> Promise<(), capnp::Error> {
        let current = match self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            Ok(current) => current,
            Err(error) => return Promise::err(error),
        };
        let Some(agent_id) = current.agent_id else {
            return Promise::err(capnp::Error::failed(
                "only an admitted delegated agent may subscribe to task runs".into(),
            ));
        };
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut subscribers) = self.state.task_subscribers.lock() {
            subscribers.insert(
                self.peer_id,
                TaskSubscription {
                    agent_id,
                    cluster_id: self.route().cluster_id,
                    tx,
                },
            );
        } else {
            return Promise::err(capnp::Error::failed(
                "task subscription registry unavailable".into(),
            ));
        }
        let client: task_run_stream::Client = capnp_rpc::new_client(TaskRunStreamServer {
            rx: Rc::new(AsyncMutex::new(rx)),
        });
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn report_task_run(
        self: Rc<Self>,
        params: channel_handle::ReportTaskRunParams,
        _results: channel_handle::ReportTaskRunResults,
    ) -> Promise<(), capnp::Error> {
        let current = match self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            Ok(current) => current,
            Err(error) => return Promise::err(error),
        };
        let Some(agent_id) = current.agent_id else {
            return Promise::err(capnp::Error::failed(
                "only an agent may update a task run".into(),
            ));
        };
        let state = self.state.clone();
        let peer_id = self.peer_id;
        Promise::from_future(async move {
            let request = params.get()?;
            let lease = request.get_lease()?.to_str()?.to_owned();
            let update = serde_json::from_str::<runtime_collab::TaskRunUpdate>(
                request.get_update_json()?.to_str()?,
            )
            .map_err(|error| capnp::Error::failed(format!("invalid task update: {error}")))?;
            state
                .record_task_run_update(peer_id, &agent_id, request.get_run_id(), &lease, update)
                .map_err(capnp::Error::failed)
        })
    }

    fn invoke_task_action(
        self: Rc<Self>,
        params: channel_handle::InvokeTaskActionParams,
        mut results: channel_handle::InvokeTaskActionResults,
    ) -> Promise<(), capnp::Error> {
        let current = match self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            Ok(current) => current,
            Err(error) => return Promise::err(error),
        };
        let Some(agent_id) = current.agent_id else {
            return Promise::err(capnp::Error::failed(
                "only an agent may invoke a task action".into(),
            ));
        };
        let state = self.state.clone();
        let peer_id = self.peer_id;
        Promise::from_future(async move {
            let request = params.get()?;
            // Interactive confirmations have no safe unattended equivalent.
            if !request.get_confirmation_json()?.to_str()?.trim().is_empty() {
                return Err(capnp::Error::failed(
                    "task action confirmation evidence is not accepted; request human confirmation outside the run".into(),
                ));
            }
            let lease = request.get_lease()?.to_str()?.to_owned();
            let resource = request.get_resource()?.to_str()?.to_owned();
            let action = request.get_action()?.to_str()?.to_owned();
            let argument = request
                .get_has_argument()
                .then(|| {
                    request
                        .get_argument()
                        .and_then(|value| value.to_str().map(str::to_owned))
                })
                .transpose()?;
            let job = state
                .start_task_action(
                    peer_id,
                    &agent_id,
                    request.get_run_id(),
                    &lease,
                    request.get_peer_id(),
                    &resource,
                    &action,
                    argument.as_deref(),
                )
                .map_err(capnp::Error::failed)?;
            let update = state.invocation_status(job.id).ok_or_else(|| {
                capnp::Error::failed("accepted task action has no broker state".into())
            })?;
            results.get().set_id(job.id);
            results.get().set_update_json(
                &serde_json::to_string(&update)
                    .map_err(|error| capnp::Error::failed(error.to_string()))?,
            );
            drop(job.completion);
            Ok(())
        })
    }

    fn task_run_cancelled(
        self: Rc<Self>,
        params: channel_handle::TaskRunCancelledParams,
        mut results: channel_handle::TaskRunCancelledResults,
    ) -> Promise<(), capnp::Error> {
        let current = match self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            Ok(current) => current,
            Err(error) => return Promise::err(error),
        };
        let Some(agent_id) = current.agent_id else {
            return Promise::err(capnp::Error::failed(
                "only an agent may observe task cancellation".into(),
            ));
        };
        let state = self.state.clone();
        let peer_id = self.peer_id;
        Promise::from_future(async move {
            let request = params.get()?;
            let lease = request.get_lease()?.to_str()?.to_owned();
            let cancelled = state
                .task_run_cancelled(peer_id, &agent_id, request.get_run_id(), &lease)
                .map_err(capnp::Error::failed)?;
            results.get().set_cancelled(cancelled);
            Ok(())
        })
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
            if !completion.code.is_empty() && completion.message.trim().is_empty() {
                return Err(capnp::Error::failed(
                    "failed invocation completion requires a message".into(),
                ));
            }
            let mut registry = state
                .pending_invocations
                .lock()
                .map_err(|_| capnp::Error::failed("invocation registry unavailable".into()))?;
            if registry.get(&id).map(|pending| pending.peer_id) != Some(peer_id) {
                return Err(capnp::Error::failed(
                    "an application may complete only its own invocation".into(),
                ));
            }
            if completion.code.is_empty() {
                registry
                    .get(&id)
                    .ok_or_else(|| capnp::Error::failed("unknown invocation id".into()))?
                    .result
                    .validate(&qos_actions::ActionOutput::default())
                    .map_err(|error| {
                        capnp::Error::failed(format!(
                            "legacy completion violated its published result contract: {error}"
                        ))
                    })?;
            }
            let pending = registry
                .remove(&id)
                .ok_or_else(|| capnp::Error::failed("unknown invocation id".into()))?;
            let invocation = qos_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id)));
            let update = if completion.code.is_empty() {
                qos_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: qos_actions::ActionOutput::default(),
                }
            } else {
                qos_actions::InvocationUpdate::Failed {
                    invocation,
                    error: qos_actions::ActionFailure {
                        code: completion.code.clone(),
                        message: completion.message.clone(),
                        details: None,
                        retryable: false,
                    },
                }
            };
            drop(registry);
            state.record_completion(id, pending.actor.clone(), update);
            if let Ok(mut requests) = state.cancellation_requests.lock() {
                requests.remove(&id);
            }
            let _ = pending.tx.send(completion);
            Ok(())
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the RPC validates the complete invocation lifecycle state machine"
    )]
    fn update_invocation(
        self: Rc<Self>,
        params: channel_handle::UpdateInvocationParams,
        _results: channel_handle::UpdateInvocationResults,
    ) -> Promise<(), capnp::Error> {
        let state = self.state.clone();
        let peer_id = self.peer_id;
        Promise::from_future(async move {
            let request = params.get()?;
            let id = request.get_id();
            let json = request.get_update_json()?.to_str()?;
            let update: qos_actions::InvocationUpdate =
                serde_json::from_str(json).map_err(|error| {
                    capnp::Error::failed(format!("invalid invocation update: {error}"))
                })?;
            let expected = qos_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id)));
            if update.invocation() != expected {
                return Err(capnp::Error::failed(
                    "invocation update identity mismatch".into(),
                ));
            }
            let mut registry = state
                .pending_invocations
                .lock()
                .map_err(|_| capnp::Error::failed("invocation registry unavailable".into()))?;
            let pending = registry
                .get_mut(&id)
                .filter(|pending| pending.peer_id == peer_id)
                .ok_or_else(|| capnp::Error::failed("unknown invocation id".into()))?;
            pending.last_activity = std::time::Instant::now();
            if matches!(&update, qos_actions::InvocationUpdate::Accepted { .. }) {
                return Err(capnp::Error::failed(
                    "invocation acceptance is owned by the broker".into(),
                ));
            }
            if let qos_actions::InvocationUpdate::Progress {
                sequence,
                phase,
                completed,
                total,
                ..
            } = &update
            {
                if matches!(pending.execution, qos_actions::ExecutionMode::Immediate) {
                    return Err(capnp::Error::failed(
                        "immediate action reported background progress".into(),
                    ));
                }
                if *sequence <= pending.last_sequence {
                    return Err(capnp::Error::failed(
                        "invocation progress must be monotonic".into(),
                    ));
                }
                if phase.trim().is_empty() {
                    return Err(capnp::Error::failed(
                        "invocation progress phase must be non-empty".into(),
                    ));
                }
                if completed
                    .zip(*total)
                    .is_some_and(|(completed, total)| completed > total)
                {
                    return Err(capnp::Error::failed(
                        "invocation progress cannot exceed its total".into(),
                    ));
                }
                pending.last_sequence = *sequence;
            }
            if let qos_actions::InvocationUpdate::Item {
                sequence, value, ..
            } = &update
            {
                let qos_actions::ExecutionMode::Streaming { item_schema, .. } = &pending.execution
                else {
                    return Err(capnp::Error::failed(
                        "non-streaming action reported a streamed item".into(),
                    ));
                };
                if *sequence <= pending.last_sequence {
                    return Err(capnp::Error::failed(
                        "invocation stream sequence must be monotonic".into(),
                    ));
                }
                qos_actions::validate_json_schema(value, item_schema).map_err(|error| {
                    capnp::Error::failed(format!(
                        "streamed item violated its published contract: {error}"
                    ))
                })?;
                pending.last_sequence = *sequence;
            }
            if let qos_actions::InvocationUpdate::Succeeded { output, .. } = &update {
                pending.result.validate(output).map_err(|error| {
                    capnp::Error::failed(format!(
                        "invocation result violated its published contract: {error}"
                    ))
                })?;
            }
            if let qos_actions::InvocationUpdate::Failed { error, .. } = &update {
                if error.code.trim().is_empty() || error.message.trim().is_empty() {
                    return Err(capnp::Error::failed(
                        "invocation failure requires a code and message".into(),
                    ));
                }
                if error.retryable && !pending.retryable_on_disconnect {
                    return Err(capnp::Error::failed(
                        "at-most-once action cannot advertise a retryable failure".into(),
                    ));
                }
            }
            if matches!(&update, qos_actions::InvocationUpdate::Cancelled { .. })
                && !pending.cancellable
            {
                return Err(capnp::Error::failed(
                    "non-cancellable action reported cancellation".into(),
                ));
            }
            pending.update = update.clone();
            if update.is_terminal() {
                let completion = match &update {
                    qos_actions::InvocationUpdate::Succeeded { .. } => InvocationCompletion {
                        code: String::new(),
                        message: String::new(),
                    },
                    qos_actions::InvocationUpdate::Failed { error, .. } => InvocationCompletion {
                        code: error.code.clone(),
                        message: error.message.clone(),
                    },
                    qos_actions::InvocationUpdate::Cancelled { message, .. } => {
                        InvocationCompletion {
                            code: "action_cancelled".into(),
                            message: message.clone().unwrap_or_default(),
                        }
                    }
                    _ => {
                        return Err(capnp::Error::failed(
                            "nonterminal invocation reached terminal handling".into(),
                        ));
                    }
                };
                let pending = registry
                    .remove(&id)
                    .ok_or_else(|| capnp::Error::failed("unknown invocation id".into()))?;
                drop(registry);
                state.record_completion(id, pending.actor.clone(), update);
                if let Ok(mut requests) = state.cancellation_requests.lock() {
                    requests.remove(&id);
                }
                let _ = pending.tx.send(completion);
            }
            Ok(())
        })
    }

    fn invocation_cancellation(
        self: Rc<Self>,
        params: channel_handle::InvocationCancellationParams,
        mut results: channel_handle::InvocationCancellationResults,
    ) -> Promise<(), capnp::Error> {
        let request = match params.get() {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let id = request.get_id();
        let owns = self
            .state
            .pending_invocations
            .lock()
            .ok()
            .and_then(|pending| pending.get(&id).map(|job| job.peer_id == self.peer_id))
            .unwrap_or(false);
        if !owns {
            return Promise::err(capnp::Error::failed("unknown invocation id".into()));
        }
        let cancelled = self
            .state
            .cancellation_requests
            .lock()
            .is_ok_and(|requests| requests.contains(&id));
        results.get().set_cancelled(cancelled);
        Promise::ok(())
    }

    fn action_catalogue(
        self: Rc<Self>,
        _params: channel_handle::ActionCatalogueParams,
        mut results: channel_handle::ActionCatalogueResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        match self.state.action_catalogue_json_for(
            &self.route().cluster_id,
            &self.verified_identity.broker_actor(),
        ) {
            Ok(snapshot) => {
                results.get().set_catalogue_json(&snapshot);
                Promise::ok(())
            }
            Err(error) => Promise::err(capnp::Error::failed(error)),
        }
    }

    fn subscribe_action_catalogue(
        self: Rc<Self>,
        _params: channel_handle::SubscribeActionCatalogueParams,
        mut results: channel_handle::SubscribeActionCatalogueResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let cluster = self.route().cluster_id;
        let actor = self.verified_identity.broker_actor();
        let (tx, rx) = mpsc::unbounded_channel();
        let initial = match self.state.action_catalogue_json_for(&cluster, &actor) {
            Ok(snapshot) => snapshot,
            Err(error) => return Promise::err(capnp::Error::failed(error)),
        };
        if tx.send(initial).is_err() {
            return Promise::err(capnp::Error::disconnected(
                "action catalogue stream closed".into(),
            ));
        }
        if let Ok(mut subscribers) = self.state.action_catalogue_subscribers.lock() {
            subscribers.insert(self.peer_id, (cluster, actor, tx));
        } else {
            return Promise::err(capnp::Error::failed(
                "action catalogue subscriber registry unavailable".into(),
            ));
        }
        let client: action_catalogue_stream::Client =
            capnp_rpc::new_client(ActionCatalogueStreamServer {
                rx: Rc::new(AsyncMutex::new(rx)),
            });
        results.get().set_stream(client);
        Promise::ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the RPC performs one complete authorized invocation admission transaction"
    )]
    fn invoke_published_action(
        self: Rc<Self>,
        params: channel_handle::InvokePublishedActionParams,
        mut results: channel_handle::InvokePublishedActionResults,
    ) -> Promise<(), capnp::Error> {
        let current = match self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            Ok(current) => current,
            Err(error) => return Promise::err(error),
        };
        let request = match params.get() {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let peer_id = request.get_peer_id();
        let resource = match request.get_resource().and_then(|s| {
            s.to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value.to_owned(),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let action = match request.get_action().and_then(|s| {
            s.to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value.to_owned(),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let argument = if request.get_has_argument() {
            match request.get_argument().and_then(|s| {
                s.to_str()
                    .map_err(|error| capnp::Error::failed(error.to_string()))
            }) {
                Ok(value) => Some(value.to_owned()),
                Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
            }
        } else {
            None
        };
        let confirmation = match request.get_confirmation_json().and_then(|s| {
            s.to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok("") => serde_json::Value::Object(serde_json::Map::new()),
            Ok(value) => match serde_json::from_str::<serde_json::Value>(value) {
                Ok(value) => value,
                Err(error) => {
                    return Promise::err(capnp::Error::failed(format!(
                        "invalid confirmation evidence: {error}"
                    )));
                }
            },
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let state = self.state.clone();
        let cluster = self.route().cluster_id;
        Promise::from_future(async move {
            let offered = state
                .surfaces
                .lock()
                .ok()
                .and_then(|surfaces| surfaces.get(&peer_id).cloned())
                .filter(|surface| surface.route.cluster_id == cluster)
                .and_then(|surface| {
                    surface.actions.into_iter().find(|candidate| {
                        candidate.resource == resource && candidate.action == action
                    })
                })
                .ok_or_else(|| capnp::Error::failed("unknown application action".into()))?;
            let actor = current.broker_actor();
            let phrase = confirmation
                .get("confirmation_phrase")
                .and_then(serde_json::Value::as_str);
            let job = match &offered.contract.confirmation {
                qos_actions::ConfirmationPolicy::None => state.start_invocation_confirmed_for(
                    peer_id,
                    &resource,
                    &action,
                    argument.as_deref(),
                    &actor,
                    None,
                    None,
                ),
                qos_actions::ConfirmationPolicy::Confirm => {
                    if confirmation
                        .get("confirm")
                        .and_then(serde_json::Value::as_bool)
                        != Some(true)
                    {
                        return Err(capnp::Error::failed(
                            "explicit confirmation is required".into(),
                        ));
                    }
                    let token = state
                        .prepare_confirmation(
                            peer_id,
                            &resource,
                            &action,
                            argument.as_deref(),
                            &actor.id,
                            None,
                        )
                        .map_err(capnp::Error::failed)?;
                    state.start_invocation_confirmed_for(
                        peer_id,
                        &resource,
                        &action,
                        argument.as_deref(),
                        &actor,
                        Some(&token),
                        None,
                    )
                }
                qos_actions::ConfirmationPolicy::Phrase { .. } => {
                    let token = state
                        .prepare_confirmation(
                            peer_id,
                            &resource,
                            &action,
                            argument.as_deref(),
                            &actor.id,
                            phrase,
                        )
                        .map_err(capnp::Error::failed)?;
                    state.start_invocation_confirmed_for(
                        peer_id,
                        &resource,
                        &action,
                        argument.as_deref(),
                        &actor,
                        Some(&token),
                        None,
                    )
                }
            }
            .map_err(capnp::Error::failed)?;
            let update = state.invocation_status(job.id).ok_or_else(|| {
                capnp::Error::failed("accepted invocation has no broker state".into())
            })?;
            let update_json = serde_json::to_string(&update)
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
            results.get().set_id(job.id);
            results.get().set_update_json(&update_json);
            drop(job.completion);
            Ok(())
        })
    }

    fn published_invocation_status(
        self: Rc<Self>,
        params: channel_handle::PublishedInvocationStatusParams,
        mut results: channel_handle::PublishedInvocationStatusResults,
    ) -> Promise<(), capnp::Error> {
        let current = match self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            Ok(current) => current,
            Err(error) => return Promise::err(error),
        };
        let id = match params.get() {
            Ok(request) => request.get_id(),
            Err(error) => return Promise::err(error),
        };
        let update = self
            .state
            .invocation_statuses(&current.broker_actor().id)
            .into_iter()
            .find_map(|(candidate, update)| (candidate == id).then_some(update));
        let mut result = results.get();
        result.set_found(update.is_some());
        if let Some(update) = update {
            match serde_json::to_string(&update) {
                Ok(json) => result.set_update_json(&json),
                Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
            }
        }
        Promise::ok(())
    }

    fn cancel_published_invocation(
        self: Rc<Self>,
        params: channel_handle::CancelPublishedInvocationParams,
        mut results: channel_handle::CancelPublishedInvocationResults,
    ) -> Promise<(), capnp::Error> {
        let current = match self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            Ok(current) => current,
            Err(error) => return Promise::err(error),
        };
        let id = match params.get() {
            Ok(request) => request.get_id(),
            Err(error) => return Promise::err(error),
        };
        if let Err(error) = self
            .state
            .request_cancellation(id, &current.broker_actor().id)
        {
            return Promise::err(capnp::Error::failed(error));
        }
        let Some(update) = self.state.invocation_status(id) else {
            return Promise::err(capnp::Error::failed("unknown invocation id".into()));
        };
        match serde_json::to_string(&update) {
            Ok(json) => {
                results.get().set_update_json(&json);
                Promise::ok(())
            }
            Err(error) => Promise::err(capnp::Error::failed(error.to_string())),
        }
    }

    fn launch_published_application(
        self: Rc<Self>,
        params: channel_handle::LaunchPublishedApplicationParams,
        mut results: channel_handle::LaunchPublishedApplicationResults,
    ) -> Promise<(), capnp::Error> {
        let current = match self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            Ok(current) => current,
            Err(error) => return Promise::err(error),
        };
        let request = match params.get() {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let app_id = match request.get_app_id() {
            Ok(value) => match value.to_str() {
                Ok(value) => value.to_owned(),
                Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
            },
            Err(error) => return Promise::err(error),
        };
        let Some(membership) = self.state.membership.as_ref() else {
            return Promise::err(capnp::Error::failed(
                "the action broker has no workspace authority".into(),
            ));
        };
        let workspace = {
            let tree = membership.read();
            let Some(workspace) = tree
                .workspaces
                .iter()
                .find(|workspace| workspace.room.to_string() == self.route().cluster_id)
            else {
                return Promise::err(capnp::Error::failed(
                    "the application workspace is no longer hosted".into(),
                ));
            };
            let workspace = workspace.clone();
            drop(tree);
            let caller = current.policy_actor(&self.route().cluster_id);
            if workspace.decide(&caller, &ResourceRef::App(app_id.clone()), Verb::Write)
                != CapDecision::Allow
            {
                return Promise::err(capnp::Error::failed(
                    "the caller is not allowed to launch that application".into(),
                ));
            }
            workspace
        };
        let id = self.state.invocation_id.fetch_add(1, Ordering::Relaxed);
        let invocation = qos_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id)));
        let update = match crate::shell::launcher::launch_in_workspace(&app_id, &workspace) {
            Ok(_) => qos_actions::InvocationUpdate::Succeeded {
                invocation,
                output: qos_actions::ActionOutput::default(),
            },
            Err(error) => qos_actions::InvocationUpdate::Failed {
                invocation,
                error: qos_actions::ActionFailure {
                    code: "launch_failed".into(),
                    message: error.to_string(),
                    details: None,
                    retryable: false,
                },
            },
        };
        self.state
            .record_completion(id, current.broker_actor().id, update.clone());
        match serde_json::to_string(&update) {
            Ok(json) => {
                results.get().set_id(id);
                results.get().set_update_json(&json);
                Promise::ok(())
            }
            Err(error) => Promise::err(capnp::Error::failed(error.to_string())),
        }
    }

    fn report_continuation(
        self: Rc<Self>,
        params: channel_handle::ReportContinuationParams,
        _results: channel_handle::ReportContinuationResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let request = match params.get() {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let app_id = match request.get_app_id().and_then(|value| {
            value
                .to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value.to_owned(),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let route = self.route();
        if route.app_id.is_empty() || app_id != route.app_id {
            return Promise::err(capnp::Error::failed(
                "reportContinuation appId must match the certified connection route".into(),
            ));
        }
        let Some(instance_id) = route
            .record_id
            .strip_prefix("continuation:")
            .and_then(|id| id.parse::<u64>().ok())
            .map(crate::shell::personal_continuity::AppInstanceId)
        else {
            return Promise::err(capnp::Error::failed(
                "reportContinuation requires a durable shell tab id".into(),
            ));
        };
        let Some(room) = crate::shell::state::RoomId::parse(&route.cluster_id) else {
            return Promise::err(capnp::Error::failed(
                "reportContinuation route has no durable workspace room".into(),
            ));
        };
        let metadata = match request.get_metadata() {
            Ok(bytes) => bytes.to_vec(),
            Err(error) => return Promise::err(error),
        };
        match crate::shell::personal_continuity_registry::global_registry()
            .lock()
            .map_err(|_| "continuation registry unavailable".to_string())
            .and_then(|mut reports| {
                reports
                    .report(
                        room,
                        app_id,
                        instance_id,
                        request.get_schema_version(),
                        metadata,
                    )
                    .map_err(|error| error.to_string())
            }) {
            Ok(_) => Promise::ok(()),
            Err(error) => Promise::err(capnp::Error::failed(error)),
        }
    }

    fn clear_continuation(
        self: Rc<Self>,
        params: channel_handle::ClearContinuationParams,
        _results: channel_handle::ClearContinuationResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let request = match params.get() {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let app_id = match request.get_app_id().and_then(|value| {
            value
                .to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value.to_owned(),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let route = self.route();
        if route.app_id.is_empty() || app_id != route.app_id {
            return Promise::err(capnp::Error::failed(
                "clearContinuation appId must match the certified connection route".into(),
            ));
        }
        let Some(instance_id) = route
            .record_id
            .strip_prefix("continuation:")
            .and_then(|id| id.parse::<u64>().ok())
            .map(crate::shell::personal_continuity::AppInstanceId)
        else {
            return Promise::err(capnp::Error::failed(
                "clearContinuation requires a durable shell tab id".into(),
            ));
        };
        let Some(room) = crate::shell::state::RoomId::parse(&route.cluster_id) else {
            return Promise::err(capnp::Error::failed(
                "clearContinuation route has no durable workspace room".into(),
            ));
        };
        match crate::shell::personal_continuity_registry::global_registry()
            .lock()
            .map_err(|_| "continuation registry unavailable".to_string())
        {
            Ok(mut reports) => {
                reports.clear(room, &app_id, instance_id);
                Promise::ok(())
            }
            Err(error) => Promise::err(capnp::Error::failed(error)),
        }
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
                if envelope.op_class == collab_types::OpClass::Compaction
                    && let Ok(cp) = collab_types::parse_compaction_payload(&envelope)
                {
                    let _ = ch_state.apply_compaction(&cp);
                }
            }
            if envelope.op_class == collab_types::OpClass::Redact {
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

    fn publish_pointer(
        self: Rc<Self>,
        params: presence_handle::PublishPointerParams,
        _results: presence_handle::PublishPointerResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self.revalidate() {
            return Promise::err(error);
        }
        Promise::from_future(async move {
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            self.publish_keyed(
                &key_bytes,
                PresencePayload::Pointer {
                    x: r.get_x(),
                    y: r.get_y(),
                },
            );
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

    fn publish_typing(
        self: Rc<Self>,
        params: presence_handle::PublishTypingParams,
        _results: presence_handle::PublishTypingResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self.revalidate() {
            return Promise::err(error);
        }
        Promise::from_future(async move {
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            let start = usize::try_from(r.get_start()).map_err(|_| {
                capnp::Error::failed("publishTyping: start overflows usize on this platform".into())
            })?;
            let end = usize::try_from(r.get_end()).map_err(|_| {
                capnp::Error::failed("publishTyping: end overflows usize on this platform".into())
            })?;
            self.publish_keyed(&key_bytes, PresencePayload::Typing { start, end });
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

struct TaskRunStreamServer {
    rx: Rc<AsyncMutex<mpsc::UnboundedReceiver<TaskRunEnvelope>>>,
}

struct ActionCatalogueStreamServer {
    rx: Rc<AsyncMutex<mpsc::UnboundedReceiver<String>>>,
}

impl action_catalogue_stream::Server for ActionCatalogueStreamServer {
    fn read(
        self: Rc<Self>,
        _params: action_catalogue_stream::ReadParams,
        mut results: action_catalogue_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            let mut guard = rx.lock().await;
            let snapshot = guard.recv().await.ok_or_else(|| {
                capnp::Error::disconnected("action catalogue stream closed".into())
            })?;
            drop(guard);
            results.get().set_catalogue_json(&snapshot);
            Ok(())
        })
    }
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
            let context = invocation
                .context
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|error| capnp::Error::failed(error.to_string()))?
                .unwrap_or_default();
            result.set_context_json(&context);
            Ok(())
        })
    }
}

impl task_run_stream::Server for TaskRunStreamServer {
    fn read(
        self: Rc<Self>,
        _params: task_run_stream::ReadParams,
        mut results: task_run_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            let mut guard = rx.lock().await;
            let run = guard
                .recv()
                .await
                .ok_or_else(|| capnp::Error::disconnected("agent task stream closed".into()))?;
            drop(guard);
            let payload = runtime_collab::TaskRunEnvelope {
                version: 1,
                task_id: run.task_id,
                title: run.title,
                run_id: run.run_id,
                lease: run.lease,
                planning: run.planning,
                action_context: serde_json::to_value(run.action_context)
                    .map_err(|error| capnp::Error::failed(error.to_string()))?,
            };
            results.get().set_task_json(
                &serde_json::to_string(&payload)
                    .map_err(|error| capnp::Error::failed(error.to_string()))?,
            );
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
                    let mut c = payload.init_pointer();
                    c.set_x(x);
                    c.set_y(y);
                }
                PresencePayload::Caret { pos } => {
                    payload.init_caret().set_pos(pos as u64);
                }
                PresencePayload::Selection { start, end } => {
                    let mut s = payload.init_selection();
                    s.set_start(start as u64);
                    s.set_end(end as u64);
                }
                PresencePayload::Typing { start, end } => {
                    let mut s = payload.init_typing();
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
            let (collaborators, permissions) = match env {
                ContextEnvelope::Snapshot {
                    collaborators,
                    permissions,
                } => (collaborators, permissions),
                ContextEnvelope::FollowChanged { peer_id } => {
                    ctx.set_follow_changed(true);
                    ctx.set_follow_target(peer_id.as_deref().unwrap_or(""));
                    return Ok(());
                }
            };
            let collab_count = u32::try_from(collaborators.len()).map_err(|_| {
                capnp::Error::failed("context: too many collaborators to encode".into())
            })?;
            {
                let mut collabs = ctx.reborrow().init_collaborators(collab_count);
                for (i, c) in collaborators.iter().enumerate() {
                    // `i < env.collaborators.len()`, already proven to fit
                    // in `u32` above; `u32::MAX` is unreachable here.
                    let idx = u32::try_from(i).unwrap_or(u32::MAX);
                    let mut slot = collabs.reborrow().get(idx);
                    slot.set_user_id(&c.id);
                    slot.set_display_name(&c.name);
                    slot.set_color(pack_color(c.color));
                    slot.set_is_agent(c.is_agent);
                    slot.set_is_self(c.is_self);
                }
            }
            let perm_count = u32::try_from(permissions.len()).map_err(|_| {
                capnp::Error::failed("context: too many permissions to encode".into())
            })?;
            {
                let mut perms = ctx.init_permissions(perm_count);
                for (i, p) in permissions.iter().enumerate() {
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
                        () = tokio::time::sleep(Duration::from_millis(100)) => {
                            // This cadence is also the broker's lifecycle
                            // watchdog. Expiration must not depend on a caller
                            // polling status: completion waiters deserve a
                            // terminal answer even after an app goes silent.
                            state.expire_stale_invocations();
                            continue;
                        },
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
            collab_types::Verb::Read,
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
    fn agent_delegation_uses_agent_caps_and_is_revoked_live() {
        let root = SigningKey::from_bytes(&[44; 32]);
        let principal = principal_id_for_root(root.verifying_key().to_bytes());
        let mut workspace = crate::shell::state::Workspace::test_default(
            crate::shell::state::WorkspaceId(44),
            "Agent room",
        );
        workspace.owner = Some(principal.clone());
        let route = CrdtRoute::cluster(workspace.room.to_string());
        let agent_membership = workspace.grant_capability(CapOverride::membership(
            Subject::agent("scribe"),
            workspace.room.to_string(),
            Verb::Comment,
        ));
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);

        let mut admitted = admit_verified_join(&tree, &route, verified_device(&root))
            .expect("the owner is admitted before delegation");
        admit_agent_delegation(&tree, &route, &mut admitted, "scribe")
            .expect("the owner may operate an admitted agent");
        assert_eq!(admitted.broker_actor().id, "scribe");
        assert!(admitted.broker_actor().is_agent);
        assert_eq!(admitted.role, CapRole::Commenter);
        assert!(
            admitted
                .policy_actor(&route.cluster_id)
                .principals
                .contains(&Principal::Agent("scribe".into()))
        );

        let membership = tree.shared();
        assert!(membership.write().workspaces[0].revoke_capability(&agent_membership));
        let fanout = Arc::new(FanoutState::new());
        let state = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(membership),
            None,
        );
        assert!(
            state.revalidate_admission(&route, &admitted).is_err(),
            "revoking agent membership invalidates an already-open agent channel"
        );
    }

    #[test]
    fn non_owner_cannot_claim_an_agent_principal() {
        let member = SigningKey::from_bytes(&[45; 32]);
        let member_id = principal_id_for_root(member.verifying_key().to_bytes());
        let mut workspace = crate::shell::state::Workspace::test_default(
            crate::shell::state::WorkspaceId(45),
            "Delegation room",
        );
        let route = CrdtRoute::cluster(workspace.room.to_string());
        workspace.grant_capability(CapOverride::membership(
            Subject::person(&member_id),
            workspace.room.to_string(),
            Verb::Write,
        ));
        workspace.grant_capability(CapOverride::membership(
            Subject::agent("operator"),
            workspace.room.to_string(),
            Verb::Write,
        ));
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let mut admitted = admit_verified_join(&tree, &route, verified_device(&member))
            .expect("the person is a member");
        assert!(admit_agent_delegation(&tree, &route, &mut admitted, "operator").is_err());
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
            collab_types::OpClass::Splice,
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
    fn record_scoped_document_fanout_reaches_only_its_targeted_subscriber() {
        let fanout = Arc::new(FanoutState::new());
        let state = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            None,
            None,
        );
        fanout.attach_capnp(Arc::clone(&state));

        let target = CrdtRoute::record("dev.qos.notes", "room-1", "note-a");
        let other_record = CrdtRoute::record("dev.qos.notes", "room-1", "note-b");
        let other_app = CrdtRoute::record("dev.qos.files", "room-1", "note-a");
        let other_room = CrdtRoute::record("dev.qos.notes", "room-2", "note-a");
        let mut target_rx = state.subscribe_ops(&target.channel_key(), 2);
        let mut other_record_rx = state.subscribe_ops(&other_record.channel_key(), 3);
        let mut other_app_rx = state.subscribe_ops(&other_app.channel_key(), 4);
        let mut other_room_rx = state.subscribe_ops(&other_room.channel_key(), 5);

        let (_signing, author) = collab_types::generate_keypair();
        let envelope = SignedOp::new_unsigned(
            DocumentMutation::Incremental(vec![7, 9])
                .encode()
                .expect("typed incremental mutation"),
            document_artifact_id(&target.app_id, &target.cluster_id, &target.record_id),
            author,
            document_attestation_id(&target.app_id, &target.cluster_id, &target.record_id),
            collab_types::OpClass::Splice,
            SystemTime::now(),
            1,
        );
        state.fanout_to_route(
            &CollabEvent::Document {
                envelope: Box::new(envelope),
                from_user_id: "peer@42".into(),
            },
            1,
            &target,
        );

        assert!(
            target_rx.try_recv().is_ok(),
            "the addressed record receives the op"
        );
        for (label, received) in [
            ("another record", other_record_rx.try_recv()),
            ("another app", other_app_rx.try_recv()),
            ("another room", other_room_rx.try_recv()),
        ] {
            assert!(
                matches!(received, Err(tokio::sync::mpsc::error::TryRecvError::Empty)),
                "{label} must not receive a record-directed document",
            );
        }
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
                actions: vec![
                    PublishedAction {
                        resource: "files.entry:/tmp/report".into(),
                        action: "reveal".into(),
                        resource_kind: "files.entry".into(),
                        resource_label: "report".into(),
                        action_label: "Reveal".into(),
                        description: None,
                        operation_kind: qos_actions::OperationKind::View,
                        params: Vec::new(),
                        contract: qos_actions::ActionContract::view(),
                        input: PublishedInput::None,
                    },
                    PublishedAction {
                        resource: "files.entry:/tmp/report".into(),
                        action: "erase".into(),
                        resource_kind: "files.entry".into(),
                        resource_label: "report".into(),
                        action_label: "Erase".into(),
                        description: None,
                        operation_kind: qos_actions::OperationKind::Effect,
                        params: Vec::new(),
                        contract: qos_actions::ActionContract {
                            risk: qos_actions::RiskLevel::Destructive,
                            confirmation: qos_actions::ConfirmationPolicy::Confirm,
                            ..qos_actions::ActionContract::default()
                        },
                        input: PublishedInput::None,
                    },
                    PublishedAction {
                        resource: "files.entry:/tmp/report".into(),
                        action: "refresh".into(),
                        resource_kind: "files.entry".into(),
                        resource_label: "report".into(),
                        action_label: "Refresh".into(),
                        description: None,
                        operation_kind: qos_actions::OperationKind::Effect,
                        params: Vec::new(),
                        contract: qos_actions::ActionContract {
                            execution: qos_actions::ExecutionMode::Background { cancellable: true },
                            result: qos_actions::ResultSpec::Job,
                            ..qos_actions::ActionContract::default()
                        },
                        input: PublishedInput::None,
                    },
                ],
                live: true,
            },
        );
        let (tx, mut invocations) = mpsc::unbounded_channel();
        state
            .invocation_subscribers
            .lock()
            .expect("subscribers")
            .insert(7, tx);

        let action_cluster = state
            .surfaces
            .lock()
            .expect("surfaces")
            .get(&7)
            .expect("surface")
            .route
            .cluster_id
            .clone();
        let owner_catalogue = state
            .action_catalogue_json(&action_cluster, "owner")
            .expect("owner catalogue");
        assert!(owner_catalogue.contains("files.entry:/tmp/report"));
        let denied_catalogue = state
            .action_catalogue_json(&action_cluster, "stranger")
            .expect("filtered catalogue");
        assert!(
            !denied_catalogue.contains("files.entry:/tmp/report"),
            "denied actions must be absent from discovery, not merely refused later"
        );

        let completion = state
            .invoke(7, "files.entry:/tmp/report", "reveal", None, "owner")
            .expect("owner can invoke current offer");
        let invocation = invocations.try_recv().expect("directed invocation");
        assert_eq!(invocation.resource, "files.entry:/tmp/report");
        assert!(
            state
                .start_invocation(7, "files.entry:/tmp/report", "erase", None, "owner")
                .is_err(),
            "a consequential action cannot bypass the confirmation protocol"
        );
        let token = state
            .prepare_confirmation(7, "files.entry:/tmp/report", "erase", None, "owner", None)
            .expect("confirmation grant");
        let erased = state
            .start_invocation_confirmed(
                7,
                "files.entry:/tmp/report",
                "erase",
                None,
                "owner",
                Some(&token),
            )
            .expect("matching one-use grant");
        assert!(
            state
                .start_invocation_confirmed(
                    7,
                    "files.entry:/tmp/report",
                    "erase",
                    None,
                    "owner",
                    Some(&token),
                )
                .is_err(),
            "confirmation grants are single use"
        );
        let erase_invocation = invocations.try_recv().expect("confirmed invocation");
        assert_eq!(erase_invocation.id, erased.id);

        let refresh = state
            .start_invocation(7, "files.entry:/tmp/report", "refresh", None, "owner")
            .expect("cancellable job");
        let refresh_invocation = invocations.try_recv().expect("job invocation");
        assert_eq!(refresh_invocation.id, refresh.id);
        assert!(state.request_cancellation(refresh.id, "stranger").is_err());
        state
            .request_cancellation(refresh.id, "owner")
            .expect("owner can request cooperative cancellation");
        assert!(
            state
                .cancellation_requests
                .lock()
                .expect("cancellation registry")
                .contains(&refresh.id)
        );
        let now = std::time::Instant::now();
        {
            let mut pending = state.pending_invocations.lock().expect("pending");
            pending
                .get_mut(&refresh.id)
                .expect("refresh remains pending")
                .cancellation_requested_at = Some(now - std::time::Duration::from_secs(2));
            pending
                .get_mut(&erased.id)
                .expect("erase remains pending")
                .last_activity = now - std::time::Duration::from_secs(2);
        }
        state.expire_stale_invocations_at(
            now,
            std::time::Duration::from_secs(1),
            std::time::Duration::from_secs(1),
        );
        assert!(matches!(
            state.invocation_status(refresh.id),
            Some(qos_actions::InvocationUpdate::Failed { error, .. })
                if error.code == "action_cancellation_timeout"
        ));
        assert_eq!(
            refresh
                .completion
                .recv()
                .expect("cancel timeout completion")
                .code,
            "action_cancellation_timeout"
        );
        assert!(matches!(
            state.invocation_status(erased.id),
            Some(qos_actions::InvocationUpdate::Failed { error, .. })
                if error.code == "action_timeout"
        ));
        assert_eq!(
            erased
                .completion
                .recv()
                .expect("idle timeout completion")
                .code,
            "action_timeout"
        );
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

        let launchable = PublishedSurface {
            peer_id: 99,
            app_id: "dev.qos.files".into(),
            route: CrdtRoute::app(
                "dev.qos.files",
                state
                    .surfaces
                    .lock()
                    .expect("surfaces")
                    .get(&7)
                    .expect("live surface")
                    .route
                    .cluster_id
                    .clone(),
            ),
            actions: vec![PublishedAction {
                resource: "files.app".into(),
                action: "new-window".into(),
                resource_kind: "files.app".into(),
                resource_label: "Files".into(),
                action_label: "New window".into(),
                description: None,
                operation_kind: qos_actions::OperationKind::View,
                params: Vec::new(),
                contract: qos_actions::ActionContract {
                    discovery: qos_actions::DiscoveryScope::Launchable,
                    ..qos_actions::ActionContract::view()
                },
                input: PublishedInput::None,
            }],
            live: false,
        };
        state.archived_surfaces.lock().expect("archive").insert(
            (
                launchable.app_id.clone(),
                launchable.route.cluster_id.clone(),
                launchable.route.record_id.clone(),
            ),
            launchable,
        );
        state.surfaces.lock().expect("surfaces").remove(&7);
        let archived = state.published_surfaces();
        assert_eq!(archived.len(), 1);
        assert!(!archived[0].live);
        assert_eq!(archived[0].actions[0].action, "new-window");
        assert!(
            !state
                .action_catalogue_json(&archived[0].route.cluster_id, "owner")
                .expect("live-only direct catalogue")
                .contains("new-window"),
            "archived launchable descriptors are not directly invokable through this protocol"
        );
    }
}
