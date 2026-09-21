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

#[path = "invocation_journal.rs"]
mod invocation_journal;
pub(crate) use invocation_journal::DurableInvocation;
use invocation_journal::{InvocationJournal, InvocationKey, InvocationOwner};

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use capnp::capability::Promise;
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::{mpsc, watch};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use collab_types::{
    Actor, ArtifactId, ArtifactKind, CapDecision, RedactedMarker, ResourceRef, SignedOp,
    ValidationError, Verb, role_permits, validate_admitted_inbound_op,
};
#[cfg(test)]
use collab_types::{DocumentMutation, document_artifact_id, document_attestation_id};
use runtime_collab::raw::{
    OpKind, action_catalogue_stream, action_directory, action_input, channel_handle,
    collab_service, context_stream, invocation_stream, presence_event, presence_handle,
    presence_payload, presence_stream, realtime_stream, scrollback_batch, stream, task_run_stream,
};
use ui::collaboration::{CollabEvent, PresencePayload};
use ui::presence::PresenceKey;

use crate::collab_broker::{
    AuditEntry, BrokerChannels, CrdtRoute, FanoutState, apply_redaction_from_envelope,
};
use crate::collab_outbound::PeerPublisher;
use crate::policy::WorkspacePolicy as _;
use crate::services::{HostServices, WorkspaceAuthorityHandle as WorkspaceTreeHandle};
use collab_types::{Principal, Role as CapRole};
use qos_desktop::{WorkspaceTaskStatus, WorkspaceTree};

#[derive(Clone, Debug, PartialEq, Eq)]
struct AuthorizedDocumentRoute {
    artifact: ArtifactId,
    channel: collab_types::ChannelId,
    payload: collab_types::VersionedRoutedDocumentPayload,
}

/// Launcher preambles are deliberately tiny: they only bind a socket to the
/// shell's already-created process record. The cert presented by the
/// app still decides workspace membership and delegated-agent authority.
const MAX_COLLAB_LAUNCH_HANDOFF_BYTES: usize = 1024;
const LAUNCH_RECORD_RACE_RETRIES: u8 = 30;
const LAUNCH_RECORD_RACE_DELAY: Duration = Duration::from_millis(10);

fn invocation_target_peer(requested: u64, current_connection: u64) -> u64 {
    if requested == ui::collaboration::CURRENT_CONNECTION_PEER_ID {
        current_connection
    } else {
        requested
    }
}

#[cfg(test)]
mod realtime_service_boundary_tests {
    use super::*;
    use crate::services::{RealtimeRequestContext, RealtimeServices};

    #[derive(Default)]
    struct RecordingRealtime {
        calls: Mutex<Vec<(RealtimeRequestContext, Vec<u8>)>>,
    }

    impl RealtimeServices for RecordingRealtime {
        fn command(
            &self,
            context: &RealtimeRequestContext,
            command_json: &[u8],
        ) -> Result<Vec<u8>, String> {
            self.calls
                .lock()
                .map_err(|_| "recording service unavailable".to_string())?
                .push((context.clone(), command_json.to_vec()));
            Ok(br#"{"accepted":true}"#.to_vec())
        }

        fn subscribe(
            &self,
            _context: &RealtimeRequestContext,
        ) -> Result<mpsc::UnboundedReceiver<Vec<u8>>, String> {
            let (_tx, rx) = mpsc::unbounded_channel();
            Ok(rx)
        }

        fn disconnect(&self, _peer_id: u64) {}
    }

    fn context() -> RealtimeRequestContext {
        RealtimeRequestContext::authenticated(41, "dev.qos.video", "workspace-7")
    }

    #[test]
    fn realtime_delegation_uses_only_broker_authenticated_identity() {
        let service = Arc::new(RecordingRealtime::default());
        let services = HostServices {
            realtime: Some(service.clone()),
            ..Default::default()
        };
        let authenticated = context();
        let reply = delegate_realtime_command(
            &services,
            &authenticated,
            "dev.qos.video",
            br#"{"register":{}}"#,
        )
        .expect("matching certified route delegates");
        assert_eq!(reply, br#"{"accepted":true}"#);
        assert_eq!(
            service.calls.lock().expect("calls").as_slice(),
            &[(authenticated, br#"{"register":{}}"#.to_vec())]
        );
    }

    #[test]
    fn realtime_claim_mismatch_and_absent_service_fail_without_delegation() {
        let service = Arc::new(RecordingRealtime::default());
        let services = HostServices {
            realtime: Some(service.clone()),
            ..Default::default()
        };
        let mismatch = delegate_realtime_command(&services, &context(), "dev.qos.impostor", b"{}")
            .expect_err("caller claim cannot override certified route");
        assert!(mismatch.to_string().contains("certified connection route"));
        assert!(service.calls.lock().expect("calls").is_empty());

        let unsupported =
            delegate_realtime_command(&HostServices::default(), &context(), "dev.qos.video", b"{}")
                .expect_err("host without native service fails explicitly");
        assert!(unsupported.to_string().contains("unsupported by this host"));
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CollabLaunchHandoff {
    protocol: u16,
    app_id: String,
    workspace_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct VerifiedJoinIdentity {
    user_public_key: [u8; 32],
    device_public_key: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AdmittedJoinIdentity {
    certificate: Option<tilo_user_identity::DeviceCert>,
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

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct BrokerActor {
    id: String,
    is_agent: bool,
}

impl BrokerActor {
    #[must_use]
    pub fn person(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            is_agent: false,
        }
    }

    #[must_use]
    pub fn agent(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            is_agent: true,
        }
    }
}

impl AdmittedJoinIdentity {
    fn broker_actor(&self) -> BrokerActor {
        BrokerActor {
            id: self.agent_id.as_ref().map_or_else(
                || self.principal_id.clone(),
                |agent| format!("agent:{agent}"),
            ),
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
        certificate: None,
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
    let cert: tilo_user_identity::DeviceCert =
        serde_json::from_slice(&presentation.device_certificate)
            .map_err(|error| format!("device certificate is not valid JSON: {error}"))?;
    tilo_user_identity::verify_device_cert_at(&cert, now_ns)
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
    authority: Option<&dyn super::peer_actions::CertificateAuthority>,
) -> Result<AdmittedJoinIdentity, String> {
    let Some(presentation) = runtime_collab::CertifiedJoinIdentity::decode(identity_bytes)
        .map_err(|error| format!("certified join presentation is malformed: {error}"))?
    else {
        return Err("collaboration join requires a certified device presentation".into());
    };
    let cert: tilo_user_identity::DeviceCert =
        serde_json::from_slice(&presentation.device_certificate)
            .map_err(|error| format!("device certificate is not valid JSON: {error}"))?;
    let (expected_root, revoked, enrolled) = if let Some(authority) = authority {
        let room = uuid::Uuid::parse_str(&route.cluster_id).map_err(|_| "invalid exact room")?;
        authority.authorize_join(room, &cert)?;
        (cert.user_public_key, false, true)
    } else if let Some(devices) = trusted_devices {
        (
            cert.user_public_key,
            false,
            devices.contains(&cert.device_public_key),
        )
    } else {
        return Err("no current certificate authority is attached".into());
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
    let mut admitted = admit_verified_join(&membership.snapshot()?, route, verified)?;
    admitted.certificate = Some(cert);
    Ok(admitted)
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
    SnapshotRequested,
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
    pub operation_kind: tilo_actions::OperationKind,
    pub params: Vec<tilo_actions::ActionParam>,
    pub contract: tilo_actions::ActionContract,
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
    invocation: tilo_actions::InvocationId,
    resource: String,
    action: String,
    argument: Option<String>,
    actor: String,
    context: Option<tilo_actions::ActionContext>,
}

#[derive(Clone, Debug)]
struct TaskRunEnvelope {
    task_id: String,
    title: String,
    run_id: u64,
    lease: String,
    planning: runtime_collab::TaskRunPlanning,
    action_context: tilo_actions::ActionContext,
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
    action_context: tilo_actions::ActionContext,
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

#[derive(Clone)]
struct PendingInvocation {
    network_evidence: Option<tilo_relay::client::CertifiedAction>,
    journal_key: Option<InvocationKey>,
    peer_id: u64,
    tx: std::sync::mpsc::Sender<InvocationCompletion>,
    update: tilo_actions::InvocationUpdate,
    last_sequence: u64,
    actor: String,
    cancellable: bool,
    retryable_on_disconnect: bool,
    execution: tilo_actions::ExecutionMode,
    result: tilo_actions::ResultSpec,
    operation_kind: tilo_actions::OperationKind,
    cluster_id: String,
    context: tilo_actions::ActionContext,
    /// Admission/progress heartbeat. A silent application must not leave a
    /// broker-owned job in `Accepted` forever.
    last_activity: std::time::Instant,
    /// Starts the shorter acknowledgement deadline for cooperative cancel.
    cancellation_requested_at: Option<std::time::Instant>,
}

/// An opaque certified person admission. Its root/device are never accepted
/// from action payload strings; the existing certificate/proof verifier owns
/// construction and every operation rechecks current certificate/membership.
#[derive(Clone)]
pub(crate) struct VerifiedActionCaller {
    room: String,
    presentation: Vec<u8>,
    network: Option<tilo_relay::client::CertifiedAction>,
    admitted: AdmittedJoinIdentity,
}

impl VerifiedActionCaller {
    fn owner(&self) -> InvocationOwner {
        InvocationOwner::Certified {
            root: self.admitted.verified.user_public_key,
            device: self.admitted.verified.device_public_key,
        }
    }
}

/// Inert transport-neutral request. The production peer entrypoint validates
/// its wire host against the durable broker authority before constructing this
/// request. Tests retain that host to exercise the equivalent direct-admission
/// boundary. Task4c must also bind this to the certified recipient device
/// before entering this API.
#[derive(Clone, Debug)]
pub(crate) struct CallerInvocation {
    pub invocation: tilo_actions::InvocationId,
    #[cfg(test)]
    pub host: uuid::Uuid,
    pub peer_id: u64,
    pub app_id: String,
    pub record_id: String,
    pub resource: String,
    pub action: String,
    pub argument: Option<String>,
    pub revision: Option<String>,
    pub recipient: Option<[u8; 32]>,
}

impl CallerInvocation {
    fn intent(&self) -> serde_json::Value {
        let mut intent = serde_json::json!({ "peer_id": self.peer_id, "app_id": self.app_id,
            "record_id": self.record_id, "resource": self.resource,
            "action": self.action, "argument": self.argument });
        if let Some(revision) = &self.revision {
            intent["revision"] = serde_json::json!(revision);
        }
        if let Some(recipient) = &self.recipient {
            intent["recipient"] = serde_json::json!(recipient);
        }
        intent
    }
}

impl PendingInvocation {
    fn completion_owner(&self) -> String {
        match self.journal_key.as_ref().map(|key| &key.owner) {
            Some(InvocationOwner::Certified { device, .. }) => {
                format!("certified-device:{}", hex::encode(device))
            }
            _ => self.actor.clone(),
        }
    }
    fn is_certified(&self) -> bool {
        self.journal_key
            .as_ref()
            .is_some_and(|key| matches!(key.owner, InvocationOwner::Certified { .. }))
    }
}

#[path = "desktop_commands.rs"]
mod desktop_commands;
#[path = "peer_action_broker.rs"]
mod peer_action_broker;

impl CapnpBrokerState {
    pub(crate) fn invocation_host(&self) -> Result<uuid::Uuid, String> {
        if self.durable_owner_required {
            self.owner_health()?;
        }
        self.journal
            .lock()
            .map_err(|_| "invocation journal unavailable")?
            .as_ref()
            .map_err(Clone::clone)?
            .as_ref()
            .ok_or_else(|| "durable admission unavailable".to_string())
            .and_then(|journal| {
                journal.check()?;
                Ok(journal.host())
            })
    }

    pub(crate) fn verify_action_caller(
        &self,
        room: &str,
        presentation: &[u8],
    ) -> Result<VerifiedActionCaller, String> {
        // The local certified presentation format also binds the exact room.
        // No caller-controlled `is_agent` or principal string enters this door.
        if uuid::Uuid::parse_str(room).is_err() || presentation.len() > 16 * 1024 {
            return Err("invalid certified invocation caller".into());
        }
        let admitted = verify_join_identity(
            &CrdtRoute::cluster(room),
            presentation,
            self.membership.as_ref(),
            self.trusted_devices.as_deref().map(Vec::as_slice),
            self.certificate_authority.get().map(Arc::as_ref),
        )?;
        Ok(VerifiedActionCaller {
            room: room.into(),
            presentation: presentation.into(),
            network: None,
            admitted,
        })
    }

    fn current_action_caller(
        &self,
        caller: &VerifiedActionCaller,
    ) -> Result<AdmittedJoinIdentity, String> {
        let current = if let Some(evidence) = &caller.network {
            self.verify_network_action_caller(evidence)?
        } else {
            self.verify_action_caller(&caller.room, &caller.presentation)?
        };
        if current.owner() != caller.owner() {
            return Err("certified caller changed".into());
        }
        Ok(current.admitted)
    }

    fn caller_key(
        &self,
        caller: &VerifiedActionCaller,
        invocation: tilo_actions::InvocationId,
    ) -> Result<InvocationKey, String> {
        Ok(InvocationKey {
            room: caller.room.clone(),
            host: self.invocation_host()?,
            owner: caller.owner(),
            invocation,
        })
    }

    fn authorize_durable_result(
        &self,
        caller: &VerifiedActionCaller,
        record: &DurableInvocation,
    ) -> Result<(), String> {
        let current = self.current_action_caller(caller)?;
        let contract: tilo_actions::ActionContract =
            serde_json::from_value(record.intent["contract"].clone())
                .map_err(|_| "journal action contract invalid")?;
        let app = record.intent["app_id"]
            .as_str()
            .ok_or("journal app invalid")?;
        let action = record.intent["action"]
            .as_str()
            .ok_or("journal action invalid")?;
        let membership = self
            .membership
            .as_ref()
            .ok_or("workspace authority unavailable")?;
        let tree = membership.snapshot()?;
        let workspace = tree
            .workspaces
            .iter()
            .find(|w| w.room.to_string() == caller.room)
            .ok_or("workspace no longer hosted")?;
        let actor = workspace.actor_for_person(&current.principal_id);
        let verb = match contract.permission {
            tilo_actions::PermissionRequirement::None => None,
            tilo_actions::PermissionRequirement::Read => Some(Verb::Read),
            tilo_actions::PermissionRequirement::Comment => Some(Verb::Comment),
            tilo_actions::PermissionRequirement::Write => Some(Verb::Write),
            tilo_actions::PermissionRequirement::Invite => Some(Verb::Invite),
            tilo_actions::PermissionRequirement::Admin => Some(Verb::Admin),
        };
        if verb.is_some_and(|verb| {
            workspace.decide(
                &actor,
                &ResourceRef::Action {
                    app: app.into(),
                    action: action.into(),
                },
                verb,
            ) != CapDecision::Allow
        }) {
            return Err("caller no longer authorized for invocation".into());
        }
        Ok(())
    }

    pub(crate) fn verified_invocation_status(
        &self,
        caller: &VerifiedActionCaller,
        invocation: tilo_actions::InvocationId,
    ) -> Result<Option<DurableInvocation>, String> {
        self.current_action_caller(caller)?;
        self.expire_stale_invocations();
        let key = self.caller_key(caller, invocation)?;
        let record = self
            .journal
            .lock()
            .map_err(|_| "invocation journal unavailable")?
            .as_ref()
            .map_err(Clone::clone)?
            .as_ref()
            .ok_or("durable admission unavailable")?
            .lookup(&key)?;
        if let Some(record) = &record {
            self.authorize_durable_result(caller, record)?;
        }
        Ok(record)
    }

    #[cfg(test)]
    pub(crate) fn start_verified_invocation(
        &self,
        caller: &VerifiedActionCaller,
        request: &CallerInvocation,
        confirmation: Option<&str>,
    ) -> Result<DurableInvocation, String> {
        let _admission = self
            .admission_lock
            .lock()
            .map_err(|_| "invocation admission unavailable")?;
        let admitted = self.current_action_caller(caller)?;
        if request.host != self.invocation_host()?
            || request.invocation.0.is_nil()
            || request.peer_id == ui::collaboration::CURRENT_CONNECTION_PEER_ID
            || [
                &request.app_id,
                &request.record_id,
                &request.resource,
                &request.action,
            ]
            .iter()
            .any(|s| s.len() > 512)
            || request
                .argument
                .as_ref()
                .is_some_and(|s| s.len() > 256 * 1024)
        {
            return Err("invalid invocation target or bounds".into());
        }
        if let Some(record) = self.verified_invocation_status(caller, request.invocation)? {
            if record.intent["request"] != request.intent() {
                return Err("invocation identity reused with different intent".into());
            }
            return Ok(record);
        }
        let offered = self
            .surfaces
            .lock()
            .map_err(|_| "action catalogue unavailable")?
            .get(&request.peer_id)
            .cloned()
            .filter(|surface| {
                surface.live
                    && surface.route.cluster_id == caller.room
                    && surface.app_id == request.app_id
                    && surface.route.record_id == request.record_id
            })
            .and_then(|surface| {
                let action = surface
                    .actions
                    .iter()
                    .find(|a| a.resource == request.resource && a.action == request.action)?
                    .clone();
                Some((surface, action))
            })
            .ok_or("action no longer offered in this exact room/app/record")?;
        self.dispatch_invocation_from_offered(
            request.peer_id,
            offered,
            request.argument.as_deref(),
            &admitted.broker_actor(),
            confirmation,
            None,
            Some((caller, request)),
            None,
        )?;
        self.verified_invocation_status(caller, request.invocation)?
            .ok_or_else(|| "admitted invocation missing".into())
    }

    pub(crate) fn cancel_verified_invocation(
        &self,
        caller: &VerifiedActionCaller,
        invocation: tilo_actions::InvocationId,
    ) -> Result<DurableInvocation, String> {
        let record = self
            .verified_invocation_status(caller, invocation)?
            .ok_or("unknown invocation")?;
        let mut registry = self
            .pending_invocations
            .lock()
            .map_err(|_| "invocation registry unavailable")?;
        let pending = registry
            .get_mut(&record.wire_id)
            .filter(|p| p.journal_key.as_ref() == Some(&record.key))
            .ok_or("invocation is not running")?;
        if !pending.cancellable {
            return Err("action did not declare cancellation".into());
        }
        self.persist_cancellation(pending)?;
        pending
            .cancellation_requested_at
            .get_or_insert_with(std::time::Instant::now);
        self.cancellation_requests
            .lock()
            .map_err(|_| "cancellation registry unavailable")?
            .insert(record.wire_id);
        drop(registry);
        self.verified_invocation_status(caller, invocation)?
            .ok_or_else(|| "invocation missing".into())
    }

    fn persist_cancellation(&self, pending: &PendingInvocation) -> Result<(), String> {
        if let Some(key) = &pending.journal_key {
            self.journal
                .lock()
                .map_err(|_| "invocation journal unavailable")?
                .as_mut()
                .map_err(|e| e.clone())?
                .as_mut()
                .ok_or("durable admission unavailable")?
                .cancel(key)?;
        }
        Ok(())
    }

    fn persist_invocation(
        &self,
        pending: &PendingInvocation,
        update: tilo_actions::InvocationUpdate,
        uncertain: bool,
    ) -> Result<(), String> {
        if let Some(key) = &pending.journal_key {
            self.journal
                .lock()
                .map_err(|_| "invocation journal unavailable")?
                .as_mut()
                .map_err(|e| e.clone())?
                .as_mut()
                .ok_or("durable admission unavailable")?
                .update(key, update.clone(), uncertain)?;
            if let Some(evidence) = &pending.network_evidence {
                self.peer_action_queue.publish(evidence.clone(), update);
            }
        }
        Ok(())
    }
}

/// A live application's pull-only directory.  Keeping the certified route
/// beside the capability makes every page/resolve check independent of a
/// stale eager `publishActions` snapshot.
#[derive(Clone)]
struct RegisteredActionDirectory {
    app_id: String,
    route: CrdtRoute,
    directory: action_directory::Client,
}

/// Inert, enumerable route metadata for one connected pull directory.  It
/// contains no capability and disappears on disconnect, so shell search can
/// select a source without reaching into the broker `LocalSet`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionDirectoryRoot {
    pub peer_id: u64,
    pub app_id: String,
    pub cluster_id: String,
    pub record_id: String,
}

/// A deterministic, inert page over the broker-retained directory root
/// projection.  Rebuilding the projection is a connect/disconnect operation;
/// palette keystrokes only clone the requested small page.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ActionDirectoryRootPage {
    pub roots: Vec<ActionDirectoryRoot>,
    pub next_after_peer: Option<u64>,
    pub generation: u64,
}

#[derive(Default)]
struct ActionDirectoryRootSnapshot {
    roots: Vec<ActionDirectoryRoot>,
    generation: u64,
}

/// LocalSet-only ownership for Rc-backed application capabilities.  The
/// shared broker state deliberately stores neither this map nor its receiver:
/// shell/chrome worker threads must remain Send.
struct LocalActionDirectoryRegistry {
    directories: RefCell<HashMap<u64, RegisteredActionDirectory>>,
    bridge_rx: RefCell<std::sync::mpsc::Receiver<ActionDirectoryBridgeRequest>>,
}

impl LocalActionDirectoryRegistry {
    fn new(bridge_rx: std::sync::mpsc::Receiver<ActionDirectoryBridgeRequest>) -> Self {
        Self {
            directories: RefCell::new(HashMap::new()),
            bridge_rx: RefCell::new(bridge_rx),
        }
    }

    fn registered(
        &self,
        peer_id: u64,
        cluster_id: &str,
    ) -> Result<RegisteredActionDirectory, capnp::Error> {
        self.directories
            .borrow()
            .get(&peer_id)
            .filter(|entry| entry.route.cluster_id == cluster_id)
            .cloned()
            .ok_or_else(|| capnp::Error::failed("unknown live application directory".into()))
    }

    fn remove(&self, peer_id: u64) {
        // Dropping the removed Cap'n Proto client can synchronously release a
        // connection lease and re-enter this idempotent cleanup. Release the
        // registry borrow before capability drop so the nested removal sees
        // an ordinary absent entry instead of panicking on RefCell reborrow.
        let removed = self.directories.borrow_mut().remove(&peer_id);
        drop(removed);
    }
}

/// Inert result returned from the broker `LocalSet` to shell chrome.  It never
/// carries a Cap'n Proto capability or widget reference across threads.
#[derive(Debug)]
pub enum ActionDirectoryBridgeResponse {
    Page(runtime_collab::ActionDirectoryPage),
    Resolution(runtime_collab::ActionDirectoryResolution),
    StartedDirectoryAction(StartedDirectoryAction),
}

/// Explicit consent supplied with an atomic directory invocation.  It is
/// carried to the broker `LocalSet`, checked against the freshly resolved
/// contract, and never retained in a shell search-result cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryActionConfirmation {
    Confirmed,
    Phrase(String),
}

/// Inert admission receipt for a directory action.  Progress remains
/// available through the existing invocation-status subscription.
#[derive(Debug, Clone, PartialEq)]
pub struct StartedDirectoryAction {
    pub invocation_id: u64,
    pub initial_update: tilo_actions::InvocationUpdate,
}

/// A bounded bridge failure.  `Busy` is deliberate backpressure: palette
/// code must retain its last cached results rather than blocking a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionDirectoryBridgeError {
    Busy,
    InvalidRequest,
    Unavailable,
    TimedOut,
    StaleRevision,
    NotFound,
    ConfirmationRequired,
}

pub type ActionDirectoryBridgeReply =
    Result<ActionDirectoryBridgeResponse, ActionDirectoryBridgeError>;

enum ActionDirectoryBridgeRequest {
    Query {
        actor: BrokerActor,
        cluster_id: String,
        query: runtime_collab::ActionDirectoryQuery,
        reply: tokio::sync::oneshot::Sender<ActionDirectoryBridgeReply>,
    },
    Resolve {
        actor: BrokerActor,
        cluster_id: String,
        peer_id: u64,
        revision: String,
        resource: String,
        action: String,
        reply: tokio::sync::oneshot::Sender<ActionDirectoryBridgeReply>,
    },
    Start {
        actor: BrokerActor,
        cluster_id: String,
        peer_id: u64,
        revision: String,
        resource: String,
        action: String,
        argument: Option<String>,
        confirmation: Option<DirectoryActionConfirmation>,
        reply: tokio::sync::oneshot::Sender<ActionDirectoryBridgeReply>,
    },
}

/// Thread-safe sender for palette/search code.  The receiver is drained only
/// by the broker's `LocalSet`, where its Rc-backed capability clients remain
/// valid.  Callers receive a oneshot they may poll from any executor.
#[derive(Clone)]
pub struct ActionDirectoryRequestQueue {
    sender: std::sync::mpsc::SyncSender<ActionDirectoryBridgeRequest>,
}

impl ActionDirectoryRequestQueue {
    const CAPACITY: usize = 64;

    pub(crate) fn unavailable() -> Self {
        let (sender, receiver) = std::sync::mpsc::sync_channel(0);
        drop(receiver);
        Self { sender }
    }

    /// # Errors
    ///
    /// Returns an error when the query is invalid, the bounded broker queue is
    /// full, or the broker is unavailable.
    pub fn try_query(
        &self,
        actor: BrokerActor,
        cluster_id: impl Into<String>,
        query: runtime_collab::ActionDirectoryQuery,
    ) -> Result<
        tokio::sync::oneshot::Receiver<ActionDirectoryBridgeReply>,
        ActionDirectoryBridgeError,
    > {
        if validate_directory_query_input(&query).is_err() {
            return Err(ActionDirectoryBridgeError::InvalidRequest);
        }
        let (reply, response) = tokio::sync::oneshot::channel();
        match self.sender.try_send(ActionDirectoryBridgeRequest::Query {
            actor,
            cluster_id: cluster_id.into(),
            query,
            reply,
        }) {
            Ok(()) => Ok(response),
            Err(std::sync::mpsc::TrySendError::Full(_)) => Err(ActionDirectoryBridgeError::Busy),
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                Err(ActionDirectoryBridgeError::Unavailable)
            }
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Resolution carries the complete authenticated route and bounded invocation inputs to the broker."
    )]
    /// # Errors
    ///
    /// Returns an error when the target is invalid, the bounded broker queue
    /// is full, or the broker is unavailable.
    pub fn try_resolve(
        &self,
        actor: BrokerActor,
        cluster_id: impl Into<String>,
        peer_id: u64,
        revision: impl Into<String>,
        resource: impl Into<String>,
        action: impl Into<String>,
    ) -> Result<
        tokio::sync::oneshot::Receiver<ActionDirectoryBridgeReply>,
        ActionDirectoryBridgeError,
    > {
        let cluster_id = cluster_id.into();
        let revision = revision.into();
        let resource = resource.into();
        let action = action.into();
        if validate_directory_target_input(&cluster_id, &revision, &resource, &action, None, None)
            .is_err()
        {
            return Err(ActionDirectoryBridgeError::InvalidRequest);
        }
        let (reply, response) = tokio::sync::oneshot::channel();
        match self.sender.try_send(ActionDirectoryBridgeRequest::Resolve {
            actor,
            cluster_id,
            peer_id,
            revision,
            resource,
            action,
            reply,
        }) {
            Ok(()) => Ok(response),
            Err(std::sync::mpsc::TrySendError::Full(_)) => Err(ActionDirectoryBridgeError::Busy),
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                Err(ActionDirectoryBridgeError::Unavailable)
            }
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Confirmed invocation preserves the resolved target, caller context and confirmation token together."
    )]
    /// # Errors
    ///
    /// Returns an error when invocation inputs are invalid, the bounded broker
    /// queue is full, or the broker is unavailable.
    pub fn try_start_resolved(
        &self,
        actor: BrokerActor,
        cluster_id: impl Into<String>,
        peer_id: u64,
        revision: impl Into<String>,
        resource: impl Into<String>,
        action: impl Into<String>,
        argument: Option<String>,
        confirmation: Option<DirectoryActionConfirmation>,
    ) -> Result<
        tokio::sync::oneshot::Receiver<ActionDirectoryBridgeReply>,
        ActionDirectoryBridgeError,
    > {
        let cluster_id = cluster_id.into();
        let revision = revision.into();
        let resource = resource.into();
        let action = action.into();
        if validate_directory_target_input(
            &cluster_id,
            &revision,
            &resource,
            &action,
            argument.as_deref(),
            confirmation.as_ref(),
        )
        .is_err()
        {
            return Err(ActionDirectoryBridgeError::InvalidRequest);
        }
        let (reply, response) = tokio::sync::oneshot::channel();
        match self.sender.try_send(ActionDirectoryBridgeRequest::Start {
            actor,
            cluster_id,
            peer_id,
            revision,
            resource,
            action,
            argument,
            confirmation,
            reply,
        }) {
            Ok(()) => Ok(response),
            Err(std::sync::mpsc::TrySendError::Full(_)) => Err(ActionDirectoryBridgeError::Busy),
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                Err(ActionDirectoryBridgeError::Unavailable)
            }
        }
    }
}

const DIRECTORY_MAX_IDENTIFIER_BYTES: usize = 512;
const DIRECTORY_MAX_KIND_BYTES: usize = 256;
const DIRECTORY_MAX_LABEL_BYTES: usize = 4 * 1024;
const DIRECTORY_MAX_DESCRIPTION_BYTES: usize = 8 * 1024;
const DIRECTORY_MAX_CONTRACT_BYTES: usize = 32 * 1024;
const DIRECTORY_MAX_PARAMS_BYTES: usize = 32 * 1024;
const DIRECTORY_MAX_QUERY_REVISION_BYTES: usize = 512;
const DIRECTORY_MAX_QUERY_NEEDLE_BYTES: usize = 4 * 1024;
const DIRECTORY_MAX_CURSOR_BYTES: usize = 4 * 1024;
const DIRECTORY_MAX_CONFIRMATION_BYTES: usize = 4 * 1024;

fn validate_directory_text(
    value: &str,
    field: &str,
    maximum: usize,
    required: bool,
) -> Result<(), capnp::Error> {
    if (required && value.is_empty()) || value.len() > maximum || value.contains('\0') {
        return Err(capnp::Error::failed(format!(
            "invalid action directory {field}"
        )));
    }
    Ok(())
}

fn validate_directory_identifier(
    value: &str,
    field: &str,
    maximum: usize,
    required: bool,
) -> Result<(), capnp::Error> {
    validate_directory_text(value, field, maximum, required)?;
    if value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(capnp::Error::failed(format!(
            "invalid action directory {field}"
        )));
    }
    Ok(())
}

fn validate_directory_query_input(
    query: &runtime_collab::ActionDirectoryQuery,
) -> Result<(), capnp::Error> {
    validate_directory_text(
        &query.revision,
        "query revision",
        DIRECTORY_MAX_QUERY_REVISION_BYTES,
        false,
    )?;
    validate_directory_text(
        &query.needle,
        "query needle",
        DIRECTORY_MAX_QUERY_NEEDLE_BYTES,
        false,
    )?;
    validate_directory_identifier(
        &query.parent_resource,
        "query parent resource",
        DIRECTORY_MAX_IDENTIFIER_BYTES,
        false,
    )?;
    if query.cursor.len() > DIRECTORY_MAX_CURSOR_BYTES {
        return Err(capnp::Error::failed(
            "invalid action directory query cursor".into(),
        ));
    }
    Ok(())
}

fn validate_directory_target_input(
    cluster_id: &str,
    revision: &str,
    resource: &str,
    action: &str,
    argument: Option<&str>,
    confirmation: Option<&DirectoryActionConfirmation>,
) -> Result<(), capnp::Error> {
    validate_directory_identifier(
        cluster_id,
        "directory cluster",
        DIRECTORY_MAX_IDENTIFIER_BYTES,
        true,
    )?;
    validate_directory_text(
        revision,
        "directory revision",
        DIRECTORY_MAX_QUERY_REVISION_BYTES,
        false,
    )?;
    validate_directory_identifier(
        resource,
        "directory resource",
        DIRECTORY_MAX_IDENTIFIER_BYTES,
        true,
    )?;
    validate_directory_identifier(
        action,
        "directory action",
        DIRECTORY_MAX_IDENTIFIER_BYTES,
        true,
    )?;
    if let Some(argument) = argument {
        validate_directory_text(
            argument,
            "directory argument",
            DIRECTORY_MAX_PARAMS_BYTES,
            false,
        )?;
    }
    if let Some(DirectoryActionConfirmation::Phrase(phrase)) = confirmation {
        validate_directory_text(
            phrase,
            "directory confirmation",
            DIRECTORY_MAX_CONFIRMATION_BYTES,
            true,
        )?;
    }
    Ok(())
}

fn write_directory_descriptor(
    mut out: runtime_collab::collab_capnp::exported_action::Builder<'_>,
    descriptor: &runtime_collab::ExportedAction,
) {
    out.set_resource(&descriptor.resource);
    out.set_action(&descriptor.action);
    out.set_resource_kind(&descriptor.resource_kind);
    out.set_resource_label(&descriptor.resource_label);
    out.set_action_label(&descriptor.action_label);
    out.set_description(descriptor.description.as_deref().unwrap_or(""));
    out.set_operation_kind(&descriptor.operation_kind);
    out.set_params_json(&descriptor.params_json);
    out.set_contract_json(&descriptor.contract_json);
    let mut input = out.init_input();
    match &descriptor.input {
        runtime_collab::ExportedInput::None => input.set_none(()),
        runtime_collab::ExportedInput::Text {
            prompt,
            placeholder,
            wants_content,
        } => {
            let mut text = input.init_text();
            text.set_prompt(prompt);
            text.set_placeholder(placeholder);
            text.set_wants_content(*wants_content);
        }
    }
}

fn write_directory_row(
    mut out: runtime_collab::collab_capnp::action_directory_row::Builder<'_>,
    row: &runtime_collab::ActionDirectoryRow,
) {
    out.set_parent_resource(&row.parent_resource);
    out.set_resource(&row.resource);
    out.set_resource_kind(&row.resource_kind);
    out.set_resource_label(&row.resource_label);
    out.set_action(row.action.as_deref().unwrap_or(""));
    out.set_has_children(row.has_children);
    out.set_visibility_action(row.visibility_action.as_deref().unwrap_or(""));
    out.set_visibility_contract_json(&row.visibility_contract_json);
    if let Some(descriptor) = &row.descriptor {
        write_directory_descriptor(out.reborrow().init_descriptor(), descriptor);
    }
}

fn write_directory_page(
    mut out: runtime_collab::collab_capnp::action_directory_page::Builder<'_>,
    page: &runtime_collab::ActionDirectoryPage,
) -> Result<(), capnp::Error> {
    out.set_revision(&page.revision);
    out.set_next_cursor(&page.next_cursor);
    out.set_exhausted(page.exhausted);
    out.set_stale_cursor(page.stale_cursor);
    out.set_budget_exhausted(page.budget_exhausted);
    let len = u32::try_from(page.rows.len())
        .map_err(|_| capnp::Error::failed("too many action directory rows".into()))?;
    let mut rows = out.reborrow().init_rows(len);
    for (index, row) in page.rows.iter().enumerate() {
        write_directory_row(
            rows.reborrow()
                .get(u32::try_from(index).unwrap_or(u32::MAX)),
            row,
        );
    }
    Ok(())
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

type ActionCatalogueSnapshot = Option<Arc<str>>;
type ActionCatalogueSubscriber = (String, BrokerActor, watch::Sender<ActionCatalogueSnapshot>);

/// Host-owned admission for one private SDK descriptor. Never serialized on the wire.
#[derive(Clone)]
struct WorkerBinding {
    route: CrdtRoute,
    root: [u8; 32],
    device: [u8; 32],
}
struct PendingWorker {
    stream: std::os::unix::net::UnixStream,
    binding: WorkerBinding,
}
/// Shared typed broker state, attached to the transport-neutral fanout core.
pub struct CapnpBrokerState {
    /// Serializes the cloneable shell facade against final shutdown. Broker
    /// LocalSet tasks are dropped before shutdown takes the write side.
    lifecycle: RwLock<()>,
    accepting: AtomicBool,
    stopped: AtomicBool,
    /// Keyed by `join_channel`'s `channel_id`; lazily inserted on first join.
    channels: Mutex<HashMap<String, ChannelSubscribers>>,
    /// `Weak` so broker teardown isn't blocked by an outliving connection.
    fanout: Weak<FanoutState>,
    /// Durable validation, audit, redaction, and scrollback state.
    broker_channels: Arc<Mutex<BrokerChannels>>,
    /// A publisher with no network behind it ([`PeerPublisher::disabled`]) is legitimate
    /// (unit tests, a shell started without peer sync).
    publisher: Arc<PeerPublisher>,
    durable_owner_required: bool,
    owner_commit_failure: Mutex<Option<String>>,
    owner_commit_order: AsyncMutex<()>,
    /// The shell's one replicated workspace owner/capability authority.
    /// `None` exists only for transitional test/diagnostic brokers; a
    /// certified join fails closed when they have no authority to consult.
    membership: Option<WorkspaceTreeHandle>,
    trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
    certificate_authority: std::sync::OnceLock<Arc<dyn super::peer_actions::CertificateAuthority>>,
    /// Latest complete action surface per live connection.
    surfaces: Mutex<HashMap<u64, PublishedSurface>>,
    /// Monotonic generation for shell-side retained action projections.
    surface_generation: AtomicU64,
    /// Last safe launchable/durable projection per app and workspace. This is
    /// session catalogue state, never an executable capability by itself.
    archived_surfaces: Mutex<HashMap<(String, String, String), PublishedSurface>>,
    /// Pull-only, revisioned directories owned by currently connected apps.
    /// This intentionally has no durable/archive counterpart: retaining a
    /// capability after disconnect would make a stale live-only row look
    /// executable.
    action_directory_bridge: ActionDirectoryRequestQueue,
    peer_action_queue: super::peer_actions::PeerActionQueue,
    peer_action_rx: Mutex<Option<tokio::sync::mpsc::Receiver<super::peer_actions::Request>>>,
    action_directory_roots: Mutex<HashMap<u64, ActionDirectoryRoot>>,
    action_directory_root_snapshot: Mutex<ActionDirectoryRootSnapshot>,
    /// Directed invocation stream registered by each application connection.
    invocation_subscribers: Mutex<HashMap<u64, mpsc::UnboundedSender<InvocationEnvelope>>>,
    action_catalogue_subscribers: Mutex<HashMap<u64, ActionCatalogueSubscriber>>,
    pending_invocations: Mutex<HashMap<u64, PendingInvocation>>,
    /// Admission serialization covers policy, one-use confirmation, the durable
    /// barrier and the single application delivery. Never held across awaits.
    admission_lock: Mutex<()>,
    journal: Mutex<Result<Option<InvocationJournal>, String>>,
    stable_invocation_subscribers: Mutex<std::collections::HashSet<u64>>,
    completed_invocations: Mutex<HashMap<u64, (String, tilo_actions::InvocationUpdate)>>,
    confirmation_grants: Mutex<HashMap<String, ConfirmationGrant>>,
    cancellation_requests: Mutex<std::collections::HashSet<u64>>,
    task_subscribers: Mutex<HashMap<u64, TaskSubscription>>,
    pending_task_runs: Mutex<HashMap<u64, PendingTaskRun>>,
    task_run_id: AtomicU64,
    invocation_id: AtomicU64,
    services: HostServices,
    workers: Mutex<Vec<PendingWorker>>,
    #[cfg(test)]
    thread_callbacks: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
}

impl CapnpBrokerState {
    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    pub(crate) fn is_accepting(&self) -> bool {
        self.accepting.load(Ordering::Acquire) && !self.is_stopped()
    }

    pub(crate) fn request_stop(&self) {
        self.accepting.store(false, Ordering::Release);
    }

    /// Hold the broker lifecycle read side for one complete shell-facade
    /// operation. Once shutdown obtains the write side, no stale facade can
    /// admit or publish new work.
    pub(crate) fn while_live<T>(
        &self,
        operation: impl FnOnce(&Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let _live = self
            .lifecycle
            .read()
            .map_err(|_| "broker unavailable".to_string())?;
        if !self.is_accepting() {
            return Err("broker unavailable".into());
        }
        let _call =
            crate::collab_broker::BrokerFacadeCallGuard::enter(std::ptr::from_ref(self).addr());
        operation(self)
    }

    /// Like `while_live`, but discard a newly cloned handle if a callback
    /// requested this exact broker's shutdown before the handle escaped.
    pub(crate) fn while_live_fenced<T>(
        &self,
        operation: impl FnOnce(&Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let result = self.while_live(operation);
        if !self.is_accepting() {
            return Err("broker unavailable".into());
        }
        result
    }

    /// Finalize the shared state after the LocalSet and all of its RPC tasks
    /// have been dropped. Surviving Arc-backed facades become inert, and the
    /// journal lock is released before the socket path can be reclaimed.
    pub(crate) fn stop(&self) {
        let _exclusive = match self.lifecycle.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        self.request_stop();
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }

        macro_rules! clear {
            ($field:expr) => {
                match $field.lock() {
                    Ok(mut value) => value.clear(),
                    Err(poisoned) => poisoned.into_inner().clear(),
                }
            };
        }
        clear!(self.channels);
        clear!(self.surfaces);
        clear!(self.action_directory_roots);
        clear!(self.invocation_subscribers);
        clear!(self.action_catalogue_subscribers);
        clear!(self.pending_invocations);
        clear!(self.stable_invocation_subscribers);
        clear!(self.confirmation_grants);
        clear!(self.cancellation_requests);
        clear!(self.task_subscribers);
        clear!(self.pending_task_runs);
        clear!(self.workers);
        #[cfg(test)]
        clear!(self.thread_callbacks);
        match self.peer_action_rx.lock() {
            Ok(mut receiver) => {
                receiver.take();
            }
            Err(poisoned) => {
                poisoned.into_inner().take();
            }
        }
        self.refresh_action_directory_root_snapshot();

        let journal = {
            let mut slot = match self.journal.lock() {
                Ok(slot) => slot,
                Err(poisoned) => poisoned.into_inner(),
            };
            std::mem::replace(&mut *slot, Ok(None))
        };
        drop(journal);
    }

    #[cfg(test)]
    fn enqueue_thread_callback(&self, callback: Box<dyn FnOnce() + Send>) {
        match self.thread_callbacks.lock() {
            Ok(mut callbacks) => callbacks.push(callback),
            Err(poisoned) => poisoned.into_inner().push(callback),
        }
    }

    /// Allocate a private connected descriptor for an actual host-mounted worker.
    /// The peer must still prove the exact current certificate on every channel join.
    pub fn connect_local_worker(
        &self,
        route: CrdtRoute,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<std::os::unix::net::UnixStream, String> {
        self.owner_health()?;
        if route.app_id.is_empty() || !route.is_publishable() {
            return Err("worker needs an exact app route".into());
        }
        let authority = self
            .certificate_authority
            .get()
            .ok_or("current certificate authority unavailable")?;
        let room = uuid::Uuid::parse_str(&route.cluster_id).map_err(|_| "invalid worker room")?;
        authority.authorize_join(room, certificate)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "clock unavailable")?
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64;
        tilo_user_identity::verify_device_cert_at(certificate, now)
            .map_err(|_| "worker certificate rejected")?;
        if certificate.capabilities.can_act_as_host {
            return Err("worker requires device authority".into());
        }
        let tree = self
            .membership
            .as_ref()
            .ok_or("current workspace authority unavailable")?
            .snapshot()?;
        admit_verified_join(
            &tree,
            &route,
            VerifiedJoinIdentity {
                user_public_key: certificate.user_public_key,
                device_public_key: certificate.device_public_key,
            },
        )?;
        let mut workers = self
            .workers
            .lock()
            .map_err(|_| "worker admission unavailable")?;
        if workers.len() >= 32 {
            return Err("worker admission queue full".into());
        }
        let (server, client) = std::os::unix::net::UnixStream::pair().map_err(|e| e.to_string())?;
        server.set_nonblocking(true).map_err(|e| e.to_string())?;
        workers.push(PendingWorker {
            stream: server,
            binding: WorkerBinding {
                route,
                root: certificate.user_public_key,
                device: certificate.device_public_key,
            },
        });
        Ok(client)
    }

    const MAX_RETAINED_INVOCATIONS: usize = 256;
    const INVOCATION_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);
    const CANCELLATION_GRACE: std::time::Duration = std::time::Duration::from_secs(30);
    /// Broker ceilings are deliberately lower than Cap'n Proto list limits:
    /// applications must not turn a palette keystroke into an unbounded RPC.
    const DIRECTORY_MAX_RESULTS: u32 = 256;
    const DIRECTORY_MAX_BYTES: u32 = 256 * 1024;
    const DIRECTORY_MAX_WORK: u32 = 4_096;
    const DIRECTORY_MAX_DEPTH: u16 = 32;
    const DIRECTORY_BRIDGE_TIMEOUT: Duration = Duration::from_secs(2);

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
            #[allow(
                clippy::needless_collect,
                reason = "expired ids must be snapshotted before removing from the same pending-invocation map"
            )]
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
            let update = tilo_actions::InvocationUpdate::Failed {
                invocation: pending.update.invocation(),
                error: tilo_actions::ActionFailure {
                    code: completion.code.clone(),
                    message: completion.message.clone(),
                    details: None,
                    retryable: pending.retryable_on_disconnect,
                },
            };
            let (update, completion) = if pending.journal_key.is_some() {
                let update = invocation_journal::unknown(pending.update.invocation());
                let completion = InvocationCompletion {
                    code: "action_outcome_unknown".into(),
                    message: "application outcome unknown after silence".into(),
                };
                (update, completion)
            } else {
                (update, completion)
            };
            if self
                .persist_invocation(&pending, update.clone(), pending.journal_key.is_some())
                .is_err()
            {
                continue;
            }
            self.record_completion(id, pending.completion_owner(), update.clone());
            if let Ok(mut requests) = self.cancellation_requests.lock() {
                requests.remove(&id);
            }
            let _ = pending.tx.send(completion);
            self.record_effect_receipt(&pending, &update);
        }
    }

    fn record_completion(&self, id: u64, actor: String, update: tilo_actions::InvocationUpdate) {
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

    fn owner_health(&self) -> Result<(), String> {
        if self.durable_owner_required {
            self.membership
                .as_ref()
                .ok_or("current workspace authority unavailable")?
                .snapshot()?;
        }
        if let Some(reason) = self
            .owner_commit_failure
            .lock()
            .map_err(|_| "owner health unavailable")?
            .as_ref()
        {
            return Err(reason.clone());
        }
        self.publisher.owner_commit_health()
    }

    async fn commit_owner_request(
        &self,
        request: qos_peer_state::owner_commit::OwnerCommitRequest,
    ) -> Result<(), capnp::Error> {
        self.owner_health().map_err(capnp::Error::failed)?;
        // Dropping the waiting RPC cannot convert an admitted effect into a
        // refusal or leave the standalone capability apparently healthy.
        struct Waiting<'a> {
            failure: &'a Mutex<Option<String>>,
            complete: bool,
        }
        impl Drop for Waiting<'_> {
            fn drop(&mut self) {
                if !self.complete
                    && let Ok(mut failure) = self.failure.lock()
                {
                    failure
                        .get_or_insert_with(|| "owner commit interrupted; outcome unknown".into());
                }
            }
        }
        let mut waiting = Waiting {
            failure: &self.owner_commit_failure,
            complete: false,
        };
        self.publisher
            .commit_owner(request)
            .await
            .map_err(capnp::Error::failed)?;
        waiting.complete = true;
        Ok(())
    }

    async fn commit_effect_before_terminal(
        &self,
        pending: &PendingInvocation,
        update: &tilo_actions::InvocationUpdate,
    ) -> Result<(), capnp::Error> {
        if !self.durable_owner_required || !update.is_terminal() {
            return Ok(());
        }
        self.owner_health().map_err(capnp::Error::failed)?;
        if pending.operation_kind != tilo_actions::OperationKind::Effect {
            return Ok(());
        }
        let status = tilo_actions::EffectReceiptStatus::from_terminal(update)
            .ok_or_else(|| capnp::Error::failed("nonterminal effect receipt".into()))?;
        let receipt = crate::policy::redacted_effect_receipt(
            update.invocation(),
            &pending.actor,
            pending.context.clone(),
            status,
        );
        self.commit_owner_request(qos_peer_state::owner_commit::OwnerCommitRequest {
            room: pending.cluster_id.clone(),
            operation: qos_peer_state::owner_commit::CommitOperation::EffectReceipt {
                invocation: update.invocation().0,
                redacted: serde_json::to_vec(&receipt)
                    .map_err(|e| capnp::Error::failed(e.to_string()))?,
            },
        })
        .await
    }

    fn record_effect_receipt(
        &self,
        pending: &PendingInvocation,
        update: &tilo_actions::InvocationUpdate,
    ) {
        if self.durable_owner_required {
            return;
        }
        if pending.operation_kind != tilo_actions::OperationKind::Effect {
            return;
        }
        let Some(status) = tilo_actions::EffectReceiptStatus::from_terminal(update) else {
            return;
        };
        if let Some(projection) = &self.services.legacy_workspace {
            if let Err(error) = projection.record_effect(
                &pending.cluster_id,
                update.invocation(),
                &pending.actor,
                pending.context.clone(),
                status,
            ) {
                tracing::warn!(%error, "native effect projection unavailable");
            }
        }
    }

    /// So tests can register channel state before pushing signed
    /// envelopes through the capnp path.
    pub fn broker_channels(&self) -> Arc<Mutex<BrokerChannels>> {
        Arc::clone(&self.broker_channels)
    }

    #[cfg(any(test, feature = "test-support"))]
    fn new(
        fanout: &Arc<FanoutState>,
        broker_channels: Arc<Mutex<BrokerChannels>>,
        publisher: Arc<PeerPublisher>,
        membership: Option<crate::test_support::WorkspaceTreeHandle>,
        trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
    ) -> (
        Arc<Self>,
        std::sync::mpsc::Receiver<ActionDirectoryBridgeRequest>,
    ) {
        let services = membership
            .as_ref()
            .map(|tree| tree.services())
            .unwrap_or_default();
        Self::new_configured(
            fanout,
            broker_channels,
            publisher,
            membership.map(|tree| tree.authority()),
            trusted_devices,
            None,
            services,
        )
    }

    fn new_configured(
        fanout: &Arc<FanoutState>,
        broker_channels: Arc<Mutex<BrokerChannels>>,
        publisher: Arc<PeerPublisher>,
        membership: Option<WorkspaceTreeHandle>,
        trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
        config: Option<&super::peer_actions::NativePeerBrokerConfig>,
        services: HostServices,
    ) -> (
        Arc<Self>,
        std::sync::mpsc::Receiver<ActionDirectoryBridgeRequest>,
    ) {
        let (bridge_tx, bridge_rx) =
            std::sync::mpsc::sync_channel(ActionDirectoryRequestQueue::CAPACITY);
        let (peer_action_queue, peer_action_rx) = super::peer_actions::PeerActionQueue::channel();
        let journal_root = config
            .map(|config| config.journal_root.clone())
            .or_else(|| {
                membership
                    .as_ref()
                    .and_then(|authority| authority.legacy_journal_root())
            });
        let journal = journal_root
            .map(|path| InvocationJournal::open(&path))
            .transpose();
        let next_invocation = journal
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .map_or(1, InvocationJournal::next_wire_id);
        (
            Arc::new(Self {
                channels: Mutex::new(HashMap::new()),
                lifecycle: RwLock::new(()),
                accepting: AtomicBool::new(true),
                stopped: AtomicBool::new(false),
                fanout: Arc::downgrade(fanout),
                broker_channels,
                publisher,
                durable_owner_required: config.is_some(),
                owner_commit_failure: Mutex::new(None),
                owner_commit_order: AsyncMutex::new(()),
                membership,
                trusted_devices,
                certificate_authority: config
                    .map(|config| config.certificate_authority.clone())
                    .or_else(|| services.native_certificates.clone())
                    .map_or_else(std::sync::OnceLock::new, std::sync::OnceLock::from),
                surfaces: Mutex::new(HashMap::new()),
                surface_generation: AtomicU64::new(0),
                archived_surfaces: Mutex::new(HashMap::new()),
                action_directory_bridge: ActionDirectoryRequestQueue { sender: bridge_tx },
                peer_action_queue,
                peer_action_rx: Mutex::new(Some(peer_action_rx)),
                action_directory_roots: Mutex::new(HashMap::new()),
                action_directory_root_snapshot: Mutex::new(ActionDirectoryRootSnapshot::default()),
                invocation_subscribers: Mutex::new(HashMap::new()),
                action_catalogue_subscribers: Mutex::new(HashMap::new()),
                pending_invocations: Mutex::new(HashMap::new()),
                admission_lock: Mutex::new(()),
                journal: Mutex::new(journal),
                stable_invocation_subscribers: Mutex::new(std::collections::HashSet::new()),
                completed_invocations: Mutex::new(HashMap::new()),
                confirmation_grants: Mutex::new(HashMap::new()),
                cancellation_requests: Mutex::new(std::collections::HashSet::new()),
                task_subscribers: Mutex::new(HashMap::new()),
                pending_task_runs: Mutex::new(HashMap::new()),
                task_run_id: AtomicU64::new(1),
                invocation_id: AtomicU64::new(next_invocation),
                services,
                workers: Mutex::new(Vec::new()),
                #[cfg(test)]
                thread_callbacks: Mutex::new(Vec::new()),
            }),
            bridge_rx,
        )
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test_broker_with_idle_directory_queue() -> crate::collab_broker::AppActionBroker {
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = Self::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            None,
            None,
        );
        crate::collab_broker::AppActionBroker::from_state_for_test(state)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn replace_action_directory_roots_for_test(&self, roots: Vec<ActionDirectoryRoot>) {
        if let Ok(mut registered) = self.action_directory_roots.lock() {
            registered.clear();
            registered.extend(roots.into_iter().map(|root| (root.peer_id, root)));
        }
        self.refresh_action_directory_root_snapshot();
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn replace_published_surfaces_for_test(&self, surfaces: Vec<PublishedSurface>) {
        if let Ok(mut published) = self.surfaces.lock() {
            published.clear();
            published.extend(
                surfaces
                    .into_iter()
                    .map(|surface| (surface.peer_id, surface)),
            );
            self.surface_generation.fetch_add(1, Ordering::Relaxed);
        }
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

    /// Check one live action route without cloning the published catalogue.
    pub fn has_live_published_action(
        &self,
        app_id: &str,
        cluster_id: &str,
        record_id: &str,
        resource: &str,
        action: &str,
    ) -> bool {
        self.surfaces
            .lock()
            .map(|surfaces| {
                surfaces.values().any(|surface| {
                    surface.live
                        && surface.app_id == app_id
                        && surface.route.cluster_id == cluster_id
                        && surface.route.record_id == record_id
                        && surface.actions.iter().any(|descriptor| {
                            descriptor.resource == resource && descriptor.action == action
                        })
                })
            })
            .unwrap_or(false)
    }

    fn published_surfaces_for_scope(
        &self,
        scope: Option<&tilo_relay::peer_actions::ExactActionRoute>,
    ) -> Vec<PublishedSurface> {
        let Some(scope) = scope else {
            return self.published_surfaces();
        };
        self.surfaces
            .lock()
            .map(|surfaces| {
                surfaces
                    .values()
                    .filter_map(|surface| {
                        (surface.app_id == scope.app_id
                            && surface.route.record_id == scope.record_id)
                            .then(|| PublishedSurface {
                                peer_id: surface.peer_id,
                                app_id: surface.app_id.clone(),
                                route: surface.route.clone(),
                                actions: surface
                                    .actions
                                    .iter()
                                    .filter(|action| {
                                        action.resource == scope.resource
                                            && action.action == scope.action
                                    })
                                    .cloned()
                                    .collect(),
                                live: surface.live,
                            })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Generation of the eager action-surface source. Reading this never
    /// clones the potentially large application action inventory.
    #[must_use]
    pub fn published_surfaces_generation(&self) -> u64 {
        self.surface_generation.load(Ordering::Relaxed)
    }

    /// Clone the nonblocking bridge used by shell search.  Sending only
    /// enqueues inert values; the broker `LocalSet` rechecks authority when
    /// it later services the request.
    #[must_use]
    pub fn action_directory_request_queue(&self) -> ActionDirectoryRequestQueue {
        self.action_directory_bridge.clone()
    }

    fn refresh_action_directory_root_snapshot(&self) {
        let mut roots = self
            .action_directory_roots
            .lock()
            .map(|roots| roots.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        roots.sort_by_key(|root| root.peer_id);
        if let Ok(mut snapshot) = self.action_directory_root_snapshot.lock() {
            snapshot.roots = roots;
            snapshot.generation = snapshot.generation.wrapping_add(1);
        }
    }

    /// Return one bounded page from the retained directory-root projection.
    /// This never clones or sorts the live registry on a palette query.
    #[must_use]
    pub fn action_directory_root_page(
        &self,
        after_peer: Option<u64>,
        max_results: usize,
    ) -> ActionDirectoryRootPage {
        let limit = max_results.min(Self::DIRECTORY_MAX_RESULTS as usize);
        let Ok(snapshot) = self.action_directory_root_snapshot.lock() else {
            return ActionDirectoryRootPage::default();
        };
        let start = after_peer.map_or(0, |peer| {
            snapshot.roots.partition_point(|root| root.peer_id <= peer)
        });
        let end = start.saturating_add(limit).min(snapshot.roots.len());
        let roots = snapshot.roots.get(start..end).unwrap_or_default().to_vec();
        let next_after_peer = (end < snapshot.roots.len())
            .then(|| {
                snapshot
                    .roots
                    .get(end.saturating_sub(1))
                    .map(|root| root.peer_id)
            })
            .flatten();
        ActionDirectoryRootPage {
            roots,
            next_after_peer,
            generation: snapshot.generation,
        }
    }

    /// Compatibility convenience for consumers that need just the first
    /// bounded page. New consumers should retain `next_after_peer` when
    /// presenting deeper root navigation.
    #[must_use]
    pub fn action_directory_roots(&self) -> Vec<ActionDirectoryRoot> {
        self.action_directory_root_page(None, Self::DIRECTORY_MAX_RESULTS as usize)
            .roots
    }

    /// Drain a bounded batch on the broker `LocalSet`.  This function must only
    /// be called by the accept-loop `LocalSet`: it starts the Rc-backed RPCs
    /// there, while the request sender is safe for compositor threads.
    #[allow(
        clippy::too_many_lines,
        reason = "the bounded bridge deliberately keeps request validation, LocalSet dispatch, and reply delivery in one ordered drain turn"
    )]
    fn drain_action_directory_bridge(
        self: &Arc<Self>,
        directories: &Rc<LocalActionDirectoryRegistry>,
    ) {
        const MAX_DRAIN_PER_TICK: usize = ActionDirectoryRequestQueue::CAPACITY;
        for _ in 0..MAX_DRAIN_PER_TICK {
            let request = match directories.bridge_rx.borrow().try_recv() {
                Ok(request) => request,
                Err(
                    std::sync::mpsc::TryRecvError::Empty
                    | std::sync::mpsc::TryRecvError::Disconnected,
                ) => break,
            };
            let state = Arc::clone(self);
            let directories = Rc::clone(directories);
            tokio::task::spawn_local(async move {
                match request {
                    ActionDirectoryBridgeRequest::Query {
                        actor,
                        cluster_id,
                        query,
                        reply,
                    } => {
                        if reply.is_closed() {
                            return;
                        }
                        let response = tokio::time::timeout(
                            Self::DIRECTORY_BRIDGE_TIMEOUT,
                            state.query_action_directory_for_actor(
                                &directories,
                                &actor,
                                &cluster_id,
                                &query,
                            ),
                        )
                        .await
                        .map_or_else(
                            |_| Err(ActionDirectoryBridgeError::TimedOut),
                            |result| {
                                result
                                    .map(ActionDirectoryBridgeResponse::Page)
                                    .map_err(|_| ActionDirectoryBridgeError::Unavailable)
                            },
                        );
                        let _ = reply.send(response);
                    }
                    ActionDirectoryBridgeRequest::Resolve {
                        actor,
                        cluster_id,
                        peer_id,
                        revision,
                        resource,
                        action,
                        reply,
                    } => {
                        if reply.is_closed() {
                            return;
                        }
                        let response = tokio::time::timeout(
                            Self::DIRECTORY_BRIDGE_TIMEOUT,
                            state.resolve_action_directory_for_actor(
                                &directories,
                                &actor,
                                &cluster_id,
                                peer_id,
                                &revision,
                                &resource,
                                &action,
                            ),
                        )
                        .await
                        .map_or_else(
                            |_| Err(ActionDirectoryBridgeError::TimedOut),
                            |result| {
                                result
                                    .map(ActionDirectoryBridgeResponse::Resolution)
                                    .map_err(|_| ActionDirectoryBridgeError::Unavailable)
                            },
                        );
                        let _ = reply.send(response);
                    }
                    ActionDirectoryBridgeRequest::Start {
                        actor,
                        cluster_id,
                        peer_id,
                        revision,
                        resource,
                        action,
                        argument,
                        confirmation,
                        reply,
                    } => {
                        if reply.is_closed() {
                            return;
                        }
                        let response = tokio::time::timeout(
                            Self::DIRECTORY_BRIDGE_TIMEOUT,
                            state.start_action_directory_for_actor(
                                &directories,
                                &actor,
                                &cluster_id,
                                peer_id,
                                &revision,
                                &resource,
                                &action,
                                argument.as_deref(),
                                confirmation,
                            ),
                        )
                        .await
                        .map_or_else(
                            |_| Err(ActionDirectoryBridgeError::TimedOut),
                            |result| {
                                result.map(ActionDirectoryBridgeResponse::StartedDirectoryAction)
                            },
                        );
                        let _ = reply.send(response);
                    }
                }
            });
        }
    }

    /// Shell-local counterpart to the wire `queryActionDirectory` method.
    /// It forwards one selected live directory only, preserving the same
    /// broker budgets and current permission checks without synthesising an
    /// eager cross-app catalogue for every shell keystroke.
    #[allow(
        clippy::future_not_send,
        reason = "the registered Cap'n Proto capability is Rc-backed and is awaited on the shell LocalSet"
    )]
    async fn query_action_directory_for_actor(
        &self,
        directories: &LocalActionDirectoryRegistry,
        actor: &BrokerActor,
        cluster_id: &str,
        query: &runtime_collab::ActionDirectoryQuery,
    ) -> Result<runtime_collab::ActionDirectoryPage, String> {
        if query.peer_id == 0
            || query.max_results == 0
            || query.max_bytes == 0
            || query.max_work == 0
        {
            return Err("directory query requires one peer and non-zero budgets".into());
        }
        validate_directory_query_input(query).map_err(|error| error.to_string())?;
        let registered = directories
            .registered(query.peer_id, cluster_id)
            .map_err(|error| error.to_string())?;
        let max_results = query.max_results.min(Self::DIRECTORY_MAX_RESULTS);
        let max_bytes = query.max_bytes.min(Self::DIRECTORY_MAX_BYTES);
        let max_work = query.max_work.min(Self::DIRECTORY_MAX_WORK);
        let max_depth = query.max_depth.min(Self::DIRECTORY_MAX_DEPTH);
        let mut request = registered.directory.query_request();
        {
            let mut wire = request.get().init_query();
            wire.set_peer_id(query.peer_id);
            wire.set_revision(&query.revision);
            wire.set_cursor(&query.cursor);
            wire.set_needle(&query.needle);
            wire.set_parent_resource(&query.parent_resource);
            wire.set_max_results(max_results);
            wire.set_max_bytes(max_bytes);
            wire.set_max_work(max_work);
            wire.set_max_depth(max_depth);
        }
        let reply = request
            .send()
            .promise
            .await
            .map_err(|error| error.to_string())?;
        let page = reply
            .get()
            .and_then(
                runtime_collab::collab_capnp::action_directory::query_results::Reader::get_page,
            )
            .map_err(|error| error.to_string())?;
        if page
            .total_size()
            .map_err(|error| error.to_string())?
            .word_count
            .saturating_mul(8)
            > u64::from(max_bytes)
        {
            return Err("application exceeded directory byte budget".into());
        }
        let page = runtime_collab::decode_action_directory_page(page)
            .map_err(|error| error.to_string())?;
        filter_directory_page(
            self,
            cluster_id,
            actor,
            &registered.app_id,
            page,
            max_results,
        )
        .map_err(|error| error.to_string())
    }

    /// Shell-local exact re-resolution. A `Found` row is policy-checked at
    /// this instant; callers must never execute from an earlier page cache.
    #[allow(
        clippy::future_not_send,
        reason = "the registered Cap'n Proto capability is Rc-backed and is awaited on the shell LocalSet"
    )]
    #[allow(
        clippy::too_many_arguments,
        reason = "the exact directory identity tuple is kept explicit so resolve cannot use a cached or partially authenticated route"
    )]
    async fn resolve_action_directory_for_actor(
        &self,
        directories: &LocalActionDirectoryRegistry,
        actor: &BrokerActor,
        cluster_id: &str,
        peer_id: u64,
        revision: &str,
        resource: &str,
        action: &str,
    ) -> Result<runtime_collab::ActionDirectoryResolution, String> {
        validate_directory_target_input(cluster_id, revision, resource, action, None, None)
            .map_err(|error| error.to_string())?;
        let registered = directories
            .registered(peer_id, cluster_id)
            .map_err(|error| error.to_string())?;
        let mut request = registered.directory.resolve_request();
        {
            let mut wire = request.get();
            wire.set_revision(revision);
            wire.set_resource(resource);
            wire.set_action(action);
        }
        let reply = request
            .send()
            .promise
            .await
            .map_err(|error| error.to_string())?;
        let resolution = reply
            .get()
            .and_then(runtime_collab::collab_capnp::action_directory::resolve_results::Reader::get_resolution)
            .map_err(|error| error.to_string())?;
        let decoded = runtime_collab::decode_action_directory_resolution(resolution)
            .map_err(|error| error.to_string())?;
        match decoded {
            runtime_collab::ActionDirectoryResolution::Found(row)
                if row.resource == resource
                    && row.action.as_deref() == Some(action)
                    && permits_directory_row(self, cluster_id, actor, &registered.app_id, &row)
                        .map_err(|error| error.to_string())? =>
            {
                Ok(runtime_collab::ActionDirectoryResolution::Found(row))
            }
            runtime_collab::ActionDirectoryResolution::Found(_) => {
                Ok(runtime_collab::ActionDirectoryResolution::NotFound)
            }
            other => Ok(other),
        }
    }

    /// Atomically resolve and start a directory action on the broker
    /// `LocalSet`.  No cached descriptor, eager surface entry, or compositor
    /// thread participates in admission.
    #[allow(
        clippy::too_many_arguments,
        clippy::future_not_send,
        reason = "Admission needs the complete authenticated route and runs on the broker LocalSet with non-Send Capnp state."
    )]
    async fn start_action_directory_for_actor(
        &self,
        directories: &LocalActionDirectoryRegistry,
        actor: &BrokerActor,
        cluster_id: &str,
        peer_id: u64,
        revision: &str,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        confirmation: Option<DirectoryActionConfirmation>,
    ) -> Result<StartedDirectoryAction, ActionDirectoryBridgeError> {
        validate_directory_target_input(
            cluster_id,
            revision,
            resource,
            action,
            argument,
            confirmation.as_ref(),
        )
        .map_err(|_| ActionDirectoryBridgeError::InvalidRequest)?;
        let registered = directories
            .registered(peer_id, cluster_id)
            .map_err(|_| ActionDirectoryBridgeError::Unavailable)?;
        let resolved = self
            .resolve_action_directory_for_actor(
                directories,
                actor,
                cluster_id,
                peer_id,
                revision,
                resource,
                action,
            )
            .await
            .map_err(|_| ActionDirectoryBridgeError::Unavailable)?;
        let runtime_collab::ActionDirectoryResolution::Found(row) = resolved else {
            return Err(match resolved {
                runtime_collab::ActionDirectoryResolution::StaleRevision => {
                    ActionDirectoryBridgeError::StaleRevision
                }
                _ => ActionDirectoryBridgeError::NotFound,
            });
        };
        let published = published_directory_action(&row)
            .map_err(|_| ActionDirectoryBridgeError::Unavailable)?;
        // Resolve rechecked the current capability and actor.  Re-run the
        // validation immediately before constructing the invocation surface
        // so a future local caller cannot bypass the same boundary.
        if !permits_directory_row(self, cluster_id, actor, &registered.app_id, &row)
            .map_err(|_| ActionDirectoryBridgeError::Unavailable)?
        {
            return Err(ActionDirectoryBridgeError::NotFound);
        }
        let confirmation_token = self.directory_confirmation_token(
            peer_id,
            resource,
            action,
            argument,
            actor,
            &published.contract,
            confirmation,
        )?;
        let surface = PublishedSurface {
            peer_id,
            app_id: registered.app_id,
            route: registered.route,
            actions: vec![published.clone()],
            live: true,
        };
        let job = self
            .start_invocation_from_offered(
                peer_id,
                (surface, published),
                argument,
                actor,
                confirmation_token.as_deref(),
                None,
                Some(revision),
            )
            .map_err(|_| ActionDirectoryBridgeError::Unavailable)?;
        let initial_update = self
            .invocation_status(job.id)
            .ok_or(ActionDirectoryBridgeError::Unavailable)?;
        Ok(StartedDirectoryAction {
            invocation_id: job.id,
            initial_update,
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "The token binds all confirmation evidence to the exact caller, target and invocation."
    )]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "the owned confirmation is intentionally matched without retaining caller-owned approval material"
    )]
    fn directory_confirmation_token(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &BrokerActor,
        contract: &tilo_actions::ActionContract,
        confirmation: Option<DirectoryActionConfirmation>,
    ) -> Result<Option<String>, ActionDirectoryBridgeError> {
        match &contract.confirmation {
            tilo_actions::ConfirmationPolicy::None => return Ok(None),
            tilo_actions::ConfirmationPolicy::Confirm
                if matches!(confirmation, Some(DirectoryActionConfirmation::Confirmed)) => {}
            tilo_actions::ConfirmationPolicy::Phrase { phrase } if matches!(&confirmation, Some(DirectoryActionConfirmation::Phrase(value)) if value == phrase) =>
                {}
            _ => return Err(ActionDirectoryBridgeError::ConfirmationRequired),
        }
        let token = uuid::Uuid::new_v4().to_string();
        let mut grants = self
            .confirmation_grants
            .lock()
            .map_err(|_| ActionDirectoryBridgeError::Unavailable)?;
        let now = std::time::Instant::now();
        grants.retain(|_, grant| grant.expires >= now);
        grants.insert(
            token.clone(),
            ConfirmationGrant {
                peer_id,
                resource: resource.to_string(),
                action: action.to_string(),
                argument_digest: argument_digest(argument),
                actor: actor.id.clone(),
                expires: now
                    .checked_add(std::time::Duration::from_secs(60))
                    .ok_or(ActionDirectoryBridgeError::Unavailable)?,
            },
        );
        // Token ownership is now in the registry; do not retain its mutex
        // while the caller begins the invocation it authorizes.
        drop(grants);
        Ok(Some(token))
    }

    /// Check the descriptor returned by an untrusted app capability against
    /// the current workspace authority, not the authority that existed when a
    /// cursor was issued. Resource-only rows are rejected by callers because
    /// they carry no contract from which policy can be evaluated.
    fn permits_directory_action(
        &self,
        cluster_id: &str,
        actor: &BrokerActor,
        app_id: &str,
        action: &str,
        contract_json: &str,
    ) -> Result<bool, capnp::Error> {
        if contract_json.is_empty() || contract_json.len() > DIRECTORY_MAX_CONTRACT_BYTES {
            return Err(capnp::Error::failed(
                "action directory contract must be non-empty and bounded".into(),
            ));
        }
        let contract = serde_json::from_str::<tilo_actions::ActionContract>(contract_json)
            .map_err(|error| {
                capnp::Error::failed(format!("invalid action directory contract: {error}"))
            })?;
        let Some(verb) = (match contract.permission {
            tilo_actions::PermissionRequirement::None => None,
            tilo_actions::PermissionRequirement::Read => Some(Verb::Read),
            tilo_actions::PermissionRequirement::Comment => Some(Verb::Comment),
            tilo_actions::PermissionRequirement::Write => Some(Verb::Write),
            tilo_actions::PermissionRequirement::Invite => Some(Verb::Invite),
            tilo_actions::PermissionRequirement::Admin => Some(Verb::Admin),
        }) else {
            return Ok(true);
        };
        let membership = self.membership.as_ref().ok_or_else(|| {
            capnp::Error::failed("the action broker has no workspace authority".into())
        })?;
        let tree = membership.snapshot().map_err(capnp::Error::failed)?;
        let workspace = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == cluster_id)
            .ok_or_else(|| {
                capnp::Error::failed("the application workspace is no longer hosted".into())
            })?;
        let caller = if actor.is_agent {
            workspace.actor_for_agent(actor.id.strip_prefix("agent:").unwrap_or(&actor.id))
        } else {
            workspace.actor_for_person(&actor.id)
        };
        let allowed = workspace.decide(
            &caller,
            &ResourceRef::Action {
                app: app_id.to_string(),
                action: action.to_string(),
            },
            verb,
        ) == CapDecision::Allow;
        // The authorization result is independent of the membership guard;
        // release it before serializing the directory reply.
        drop(tree);
        Ok(allowed)
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
            None,
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "catalogue serialization and policy filtering remain adjacent so a published action cannot bypass the actor check"
    )]
    fn action_catalogue_json_for(
        &self,
        cluster_id: &str,
        actor: &BrokerActor,
        scope: Option<&tilo_relay::peer_actions::ExactActionRoute>,
    ) -> Result<String, String> {
        let membership = self
            .membership
            .as_ref()
            .ok_or_else(|| "the action broker has no workspace authority".to_string())?;
        let tree = membership.snapshot()?;
        let workspace = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == cluster_id)
            .ok_or_else(|| "the application workspace is no longer hosted".to_string())?
            .clone();
        drop(tree);
        let caller = if actor.is_agent {
            workspace.actor_for_agent(actor.id.strip_prefix("agent:").unwrap_or(&actor.id))
        } else {
            workspace.actor_for_person(&actor.id)
        };
        let published = self.published_surfaces_for_scope(scope);
        let live_apps = self
            .surfaces
            .lock()
            .map(|surfaces| {
                surfaces
                    .values()
                    .filter(|surface| surface.live && surface.route.cluster_id == cluster_id)
                    .map(|surface| surface.app_id.clone())
                    .collect::<std::collections::HashSet<_>>()
            })
            .unwrap_or_default();
        let mut surfaces = published
            .into_iter()
            .filter(|surface| surface.live && surface.route.cluster_id == cluster_id)
            .filter_map(|surface| {
                let actions = surface
                    .actions
                    .into_iter()
                    .filter(|action| {
                        scope.is_none_or(|scope| {
                            action.resource == scope.resource && action.action == scope.action
                        })
                    })
                    .filter(|action| {
                        let Some(verb) = (match action.contract.permission {
                            tilo_actions::PermissionRequirement::None => None,
                            tilo_actions::PermissionRequirement::Read => Some(Verb::Read),
                            tilo_actions::PermissionRequirement::Comment => Some(Verb::Comment),
                            tilo_actions::PermissionRequirement::Write => Some(Verb::Write),
                            tilo_actions::PermissionRequirement::Invite => Some(Verb::Invite),
                            tilo_actions::PermissionRequirement::Admin => Some(Verb::Admin),
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
        for app in self
            .services
            .launcher
            .as_ref()
            .map(|service| service.applications())
            .unwrap_or_default()
            .into_iter()
            .filter(|app| {
                scope.is_none_or(|scope| {
                    app.package_id == scope.app_id
                        && scope.record_id.is_empty()
                        && scope.resource == format!("app.{}", app.package_id)
                        && scope.action == "open"
                })
            })
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
                    "operation_kind": tilo_actions::OperationKind::Effect,
                    "params": [],
                    "contract": tilo_actions::ActionContract::default(),
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
        let mut audiences: HashMap<(String, BrokerActor), ActionCatalogueSnapshot> = HashMap::new();
        let mut disconnected = Vec::new();
        for (peer, cluster, actor, tx) in subscribers {
            let snapshot = audiences
                .entry((cluster.clone(), actor.clone()))
                .or_insert_with(|| {
                    self.action_catalogue_json_for(&cluster, &actor, None)
                        .ok()
                        .map(Arc::<str>::from)
                })
                .clone();
            if let Some(snapshot) = snapshot {
                if tx.send(Some(snapshot)).is_err() {
                    disconnected.push(peer);
                }
            } else {
                // Invalidate an unread prior snapshot before removing this
                // audience. A failed current authorization must never let
                // a slow reader consume stale discovery state.
                tx.send_replace(None);
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
            tilo_actions::ConfirmationPolicy::None => {
                return Err("the action does not require confirmation".into());
            }
            tilo_actions::ConfirmationPolicy::Confirm => {}
            tilo_actions::ConfirmationPolicy::Phrase { phrase: expected } => {
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

    #[allow(
        clippy::too_many_arguments,
        reason = "the complete invocation identity and confirmation context are explicit at the admission boundary"
    )]
    fn start_invocation_confirmed_for(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &BrokerActor,
        confirmation_token: Option<&str>,
        context: Option<tilo_actions::ActionContext>,
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
        self.start_invocation_from_offered(
            peer_id,
            offered,
            argument,
            actor,
            confirmation_token,
            context,
            None,
        )
    }

    /// Admit an invocation from a just-validated live action.  Directory
    /// actions reach this point without ever being copied into the eager
    /// published surface; the caller must have resolved the row on this
    /// `LocalSet` turn and supplied its certified route.
    #[allow(
        clippy::too_many_lines,
        clippy::needless_pass_by_value,
        reason = "a freshly offered live action is consumed into one invocation job while all route and confirmation checks remain atomic"
    )]
    fn start_invocation_from_offered(
        &self,
        peer_id: u64,
        offered: (PublishedSurface, PublishedAction),
        argument: Option<&str>,
        actor: &BrokerActor,
        confirmation_token: Option<&str>,
        context: Option<tilo_actions::ActionContext>,
        revision: Option<&str>,
    ) -> Result<InvocationJob, String> {
        let _admission = self
            .admission_lock
            .lock()
            .map_err(|_| "invocation admission unavailable")?;
        self.dispatch_invocation_from_offered(
            peer_id,
            offered,
            argument,
            actor,
            confirmation_token,
            context,
            None,
            revision,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Dispatch retains the authenticated peer and complete offered action contract without reconstructing authority."
    )]
    fn dispatch_invocation_from_offered(
        &self,
        peer_id: u64,
        offered: (PublishedSurface, PublishedAction),
        argument: Option<&str>,
        actor: &BrokerActor,
        confirmation_token: Option<&str>,
        context: Option<tilo_actions::ActionContext>,
        durable: Option<(&VerifiedActionCaller, &CallerInvocation)>,
        revision: Option<&str>,
    ) -> Result<InvocationJob, String> {
        if offered.0.app_id == qos_peer_state::desktop_command::APP_ID
            && self
                .pending_invocations
                .lock()
                .map_err(|_| "invocation registry unavailable")?
                .values()
                .filter(|pending| pending.peer_id == peer_id)
                .count()
                >= 32
        {
            return Err("desktop invocation capacity exhausted".into());
        }
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
        let tree = membership.snapshot()?;
        let workspace = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == offered.0.route.cluster_id)
            .ok_or_else(|| "the application workspace is no longer hosted".to_string())?;
        let caller = if actor.is_agent {
            workspace.actor_for_agent(actor.id.strip_prefix("agent:").unwrap_or(&actor.id))
        } else {
            workspace.actor_for_person(&actor.id)
        };
        let target = ResourceRef::Action {
            app: offered.0.app_id.clone(),
            action: offered.1.action.clone(),
        };
        if let tilo_actions::Availability::Unavailable { reason, .. } =
            &offered.1.contract.availability
        {
            return Err(format!("the application action is unavailable: {reason}"));
        }
        let verb = match offered.1.contract.permission {
            tilo_actions::PermissionRequirement::None => None,
            tilo_actions::PermissionRequirement::Read => Some(Verb::Read),
            tilo_actions::PermissionRequirement::Comment => Some(Verb::Comment),
            tilo_actions::PermissionRequirement::Write => Some(Verb::Write),
            tilo_actions::PermissionRequirement::Invite => Some(Verb::Invite),
            tilo_actions::PermissionRequirement::Admin => Some(Verb::Admin),
        };
        if verb.is_some_and(|verb| workspace.decide(&caller, &target, verb) != CapDecision::Allow) {
            return Err("the caller is not allowed to invoke that application action".into());
        }
        drop(tree);
        if durable.is_some()
            && !self
                .stable_invocation_subscribers
                .lock()
                .map_err(|_| "application capability unavailable")?
                .contains(&peer_id)
        {
            return Err("application_stable_identity_unavailable".into());
        }
        // A configured but unreadable/corrupt journal disables every new admission.
        let mut journal_guard = self
            .journal
            .lock()
            .map_err(|_| "invocation journal unavailable")?;
        let journal = journal_guard.as_mut().map_err(|error| error.clone())?;
        if durable.is_some() && journal.is_none() {
            return Err("durable admission unavailable".into());
        }
        if !matches!(
            offered.1.contract.confirmation,
            tilo_actions::ConfirmationPolicy::None
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
                || grant.resource != offered.1.resource
                || grant.action != offered.1.action
                || grant.argument_digest != argument_digest(argument)
                || grant.actor != actor.id
            {
                return Err(
                    "the confirmation token is expired or does not match this invocation".into(),
                );
            }
        }
        let id = self
            .invocation_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| "invocation handle space exhausted")?;
        let invocation = durable.map_or_else(
            || tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id))),
            |(_, request)| request.invocation,
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let mut receipt_context = tilo_actions::ActionContext::for_resource(
            &offered.1.action_label,
            &offered.1.resource,
            &offered.1.resource_kind,
            &offered.1.resource_label,
        )
        .with_app(&offered.0.app_id, &offered.0.app_id);
        if let Some(ambient) = &context {
            receipt_context = receipt_context.enriched_by(ambient);
        }
        let journal_key = if let Some(journal) = journal.as_mut() {
            let key = InvocationKey {
                room: offered.0.route.cluster_id.clone(),
                host: journal.host(),
                invocation,
                owner: durable.map_or_else(
                    || InvocationOwner::Local {
                        actor: actor.id.clone(),
                    },
                    |(caller, _)| caller.owner(),
                ),
            };
            let request = durable.map_or_else(|| serde_json::json!({
                "peer_id": peer_id, "app_id": offered.0.app_id, "record_id": offered.0.route.record_id,
                "resource": offered.1.resource, "action": offered.1.action, "argument": argument,
            }), |(_, request)| request.intent());
            let intent = serde_json::json!({ "request": request,
                "contract": offered.1.contract, "operation_kind": offered.1.operation_kind,
                "context": receipt_context, "app_id": offered.0.app_id,
                "action": offered.1.action, "revision": revision });
            if !journal.admit(key.clone(), intent, id)? {
                return Err("invocation already admitted".into());
            }
            Some(key)
        } else {
            None
        };
        drop(journal_guard);
        self.pending_invocations
            .lock()
            .map_err(|_| "the invocation registry is unavailable".to_string())?
            .insert(
                id,
                PendingInvocation {
                    network_evidence: durable.and_then(|(caller, _)| caller.network.clone()),
                    journal_key: journal_key.clone(),
                    peer_id,
                    tx,
                    update: tilo_actions::InvocationUpdate::Accepted { invocation },
                    last_sequence: 0,
                    actor: actor.id.clone(),
                    cancellable: matches!(
                        offered.1.contract.execution,
                        tilo_actions::ExecutionMode::Background { cancellable: true }
                            | tilo_actions::ExecutionMode::Streaming {
                                cancellable: true,
                                ..
                            }
                    ),
                    retryable_on_disconnect: matches!(
                        offered.1.contract.retry,
                        tilo_actions::RetryPolicy::Idempotent
                            | tilo_actions::RetryPolicy::RetryWithInvocationId
                    ),
                    execution: offered.1.contract.execution.clone(),
                    result: offered.1.contract.result.clone(),
                    operation_kind: offered.1.operation_kind,
                    cluster_id: offered.0.route.cluster_id.clone(),
                    context: receipt_context,
                    last_activity: std::time::Instant::now(),
                    cancellation_requested_at: None,
                },
            );
        if sender
            .send(InvocationEnvelope {
                id,
                invocation,
                resource: offered.1.resource.clone(),
                action: offered.1.action.clone(),
                argument: argument.map(str::to_string),
                actor: actor.id.clone(),
                context,
            })
            .is_err()
        {
            if let Some(pending) = self
                .pending_invocations
                .lock()
                .ok()
                .and_then(|mut jobs| jobs.remove(&id))
            {
                let _ = self.persist_invocation(
                    &pending,
                    invocation_journal::unknown(invocation),
                    true,
                );
            }
            return Err("the application invocation stream closed; outcome unknown".into());
        }
        Ok(InvocationJob { id, completion: rx })
    }

    pub fn invocation_status(&self, id: u64) -> Option<tilo_actions::InvocationUpdate> {
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

    pub fn invocation_statuses(&self, actor: &str) -> Vec<(u64, tilo_actions::InvocationUpdate)> {
        self.expire_stale_invocations();
        let mut jobs = self
            .pending_invocations
            .lock()
            .map(|pending| {
                pending
                    .iter()
                    .filter(|(_, job)| job.actor == actor && !job.is_certified())
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
        if job.actor != actor || job.is_certified() {
            return Err("only the invocation owner may cancel it".into());
        }
        if !job.cancellable {
            return Err("the action did not declare cooperative cancellation".into());
        }
        let job = pending
            .get_mut(&id)
            .ok_or_else(|| "the invocation is not running".to_string())?;
        self.persist_cancellation(job)?;
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
                    .map(|job| job.cancellable && job.actor == actor && !job.is_certified())
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
    ///
    /// # Errors
    ///
    /// Returns an error when workspace authority, task ownership, or a live
    /// task-capable subscriber is unavailable.
    pub fn dispatch_workspace_task(&self, cluster_id: &str, task_id: &str) -> Result<u64, String> {
        if self.services.legacy_workspace.is_none() {
            return Err("task mutation service unsupported".into());
        }
        let membership = self
            .membership
            .as_ref()
            .ok_or_else(|| "the task broker has no workspace authority".to_string())?;
        let task_snapshot = {
            let tree = membership.snapshot()?;
            let lookup = match tree
                .workspaces
                .iter()
                .find(|workspace| workspace.room.to_string() == cluster_id)
            {
                Some(workspace) => match workspace.tasks.iter().find(|task| task.id == task_id) {
                    Some(task) => Ok((workspace.id, task.clone())),
                    None => Err("the task no longer exists".to_string()),
                },
                None => Err("the task workspace is no longer hosted".to_string()),
            };
            // The copied id and cloned task are the complete snapshot needed
            // below; never retain workspace authority across subscriber work.
            drop(tree);
            lookup
        };
        let (_workspace_id, task) = task_snapshot?;
        let agent_id = task
            .assigned_agent
            .clone()
            .filter(|_| task.route != qos_desktop::WorkspaceTaskRoute::Manual)
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
                qos_desktop::WorkspaceTaskRoute::RulesOnly => {
                    runtime_collab::TaskRunPlanning::RulesOnly
                }
                qos_desktop::WorkspaceTaskRoute::Model => runtime_collab::TaskRunPlanning::Model,
                qos_desktop::WorkspaceTaskRoute::Manual => {
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
        let actor = format!("agent:{agent_id}");
        self.services
            .legacy_workspace
            .as_ref()
            .ok_or("task mutation service unsupported")?
            .record_task_update(
                cluster_id,
                &task.id,
                &actor,
                Some(WorkspaceTaskStatus::InProgress),
                format!("Broker dispatched live run {run_id}"),
            )?;
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
        let tree = membership.snapshot()?;
        let workspace = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == run.cluster_id)
            .ok_or_else(|| "the task workspace is no longer hosted".to_string())?;
        let cancelled = workspace
            .tasks
            .iter()
            .find(|task| task.id == run.task_id)
            .is_some_and(|task| task.status == WorkspaceTaskStatus::Cancelled);
        drop(tree);
        Ok(cancelled)
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
            // The terminal marker is durable in the run registry before the
            // workspace projection is updated below.
            drop(runs);
        }
        let actor = format!("agent:{}", run.agent_id);
        self.services
            .legacy_workspace
            .as_ref()
            .ok_or("task mutation service unsupported")?
            .record_task_update(&run.cluster_id, &run.task_id, &actor, status, summary)?;
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "task lease, source, target, and action identifiers are all required to prevent a run from invoking another task's action"
    )]
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
        if !matches!(
            confirmation_required,
            tilo_actions::ConfirmationPolicy::None
        ) {
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
        if let Some(realtime) = self.services.realtime.as_ref() {
            realtime.disconnect(peer_id);
        }
        if let Ok(mut surfaces) = self.surfaces.lock()
            && surfaces.remove(&peer_id).is_some()
        {
            self.surface_generation.fetch_add(1, Ordering::Relaxed);
        }
        if let Ok(mut roots) = self.action_directory_roots.lock() {
            roots.remove(&peer_id);
        }
        self.refresh_action_directory_root_snapshot();
        if let Ok(mut capabilities) = self.stable_invocation_subscribers.lock() {
            capabilities.remove(&peer_id);
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
                    let update = tilo_actions::InvocationUpdate::Failed {
                        invocation: invocation.update.invocation(),
                        error: tilo_actions::ActionFailure {
                            code: completion.code.clone(),
                            message: completion.message.clone(),
                            details: None,
                            retryable: invocation.retryable_on_disconnect,
                        },
                    };
                    let (update, completion) = if invocation.journal_key.is_some() {
                        (
                            invocation_journal::unknown(invocation.update.invocation()),
                            InvocationCompletion {
                                code: "action_outcome_unknown".into(),
                                message: "application outcome unknown after disconnect".into(),
                            },
                        )
                    } else {
                        (update, completion)
                    };
                    if self
                        .persist_invocation(
                            &invocation,
                            update.clone(),
                            invocation.journal_key.is_some(),
                        )
                        .is_err()
                    {
                        continue;
                    }
                    self.record_completion(id, invocation.completion_owner(), update.clone());
                    if let Ok(mut requests) = self.cancellation_requests.lock() {
                        requests.remove(&id);
                    }
                    let _ = invocation.tx.send(completion);
                    self.record_effect_receipt(&invocation, &update);
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
        if let Some(cert) = &admitted.certificate {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| capnp::Error::failed("clock unavailable".into()))?
                .as_nanos()
                .min(u128::from(u64::MAX)) as u64;
            tilo_user_identity::verify_device_cert_at(cert, now)
                .map_err(|_| capnp::Error::failed("certificate no longer valid".into()))?;
            if let Some(authority) = self.certificate_authority.get() {
                let room = uuid::Uuid::parse_str(&route.cluster_id)
                    .map_err(|_| capnp::Error::failed("invalid exact room".into()))?;
                authority
                    .authorize_join(room, cert)
                    .map_err(capnp::Error::failed)?;
            }
        }
        let membership = self.membership.as_ref().ok_or_else(|| {
            capnp::Error::failed("certified session lost its membership authority".into())
        })?;
        let mut current = admit_verified_join(
            &membership.snapshot().map_err(capnp::Error::failed)?,
            route,
            admitted.verified.clone(),
        )
        .map_err(|error| {
            capnp::Error::failed(format!(
                "certified collaboration session is no longer admitted: {error}"
            ))
        })?;
        current.certificate = admitted.certificate.clone();
        if let Some(agent_id) = &admitted.agent_id {
            admit_agent_delegation(
                &membership.snapshot().map_err(capnp::Error::failed)?,
                route,
                &mut current,
                agent_id,
            )
            .map_err(|error| {
                capnp::Error::failed(format!(
                    "certified agent session is no longer admitted: {error}"
                ))
            })?;
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
    ) -> Result<AuthorizedDocumentRoute, capnp::Error> {
        if route.app_id.is_empty() {
            return Err(capnp::Error::failed(
                "certified document operation does not match its joined route".into(),
            ));
        }
        let payload = collab_types::validate_document_payload_route_versioned(
            &route.app_id,
            &route.cluster_id,
            &route.record_id,
            envelope,
        )
        .map_err(|error| {
            capnp::Error::failed(format!(
                "certified document operation has an invalid routed payload: {error}"
            ))
        })?;
        let channel = match &payload {
            collab_types::VersionedRoutedDocumentPayload::Cleartext(decoded) => {
                collab_types::document_channel_id_for_version(
                    decoded.version,
                    &route.app_id,
                    &route.cluster_id,
                    &route.record_id,
                )
            }
            collab_types::VersionedRoutedDocumentPayload::Encrypted { channel, .. } => *channel,
        };
        let broker_channels = self.broker_channels.lock().map_err(|_| {
            capnp::Error::failed("collaboration governance state is unavailable".into())
        })?;
        if !broker_channels.document_encryption_allowed(channel, envelope.encryption.as_ref()) {
            return Err(capnp::Error::failed(
                "plaintext document operation rejected after MLS migration".into(),
            ));
        }
        drop(broker_channels);
        let membership = self.membership.as_ref().ok_or_else(|| {
            capnp::Error::failed("certified session lost its membership authority".into())
        })?;
        let tree = membership.snapshot().map_err(capnp::Error::failed)?;
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
            instance: envelope.artifact,
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
        Ok(AuthorizedDocumentRoute {
            artifact: envelope.artifact,
            channel,
            payload,
        })
    }

    fn publisher(&self) -> &Arc<PeerPublisher> {
        &self.publisher
    }

    /// Converts once into the transport-neutral peer presence envelope.
    /// Peer replication does not embed either local RPC dialect.
    fn publish_presence(&self, route: &CrdtRoute, key: &PresenceKey, payload: &PresencePayload) {
        let Some(presence) = crate::collab_presence::to_peer(key, payload) else {
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
                ..
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
            | CollabEvent::DocumentSnapshotRequested { .. }
            | CollabEvent::Invocation { .. }
            | CollabEvent::InvocationCancellation { .. }
            | CollabEvent::ActionCatalogue { .. }
            | CollabEvent::PublishedInvocationResult { .. }
            | CollabEvent::WorkspaceControl { .. }
            | CollabEvent::TaskRun(_)
            | CollabEvent::TaskActionInvocationResult { .. }
            | CollabEvent::TaskRunCancellation { .. }
            | CollabEvent::RealtimeCommandResult { .. }
            | CollabEvent::NearbyTransferCommandResult { .. }
            | CollabEvent::Realtime(_) => {
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

    /// Direct a late-join snapshot request to one established subscriber on
    /// the exact route. Lowest broker peer id makes selection deterministic;
    /// the requester is excluded so it never snapshots its own empty state.
    fn request_document_snapshot(&self, channel_id: &str, requester: u64) -> bool {
        let Ok(mut channels) = self.channels.lock() else {
            return false;
        };
        let Some(channel) = channels.get_mut(channel_id) else {
            return false;
        };
        channel.context.sort_by_key(|subscriber| subscriber.peer_id);
        let Some(index) = channel
            .context
            .iter()
            .position(|subscriber| subscriber.peer_id != requester)
        else {
            return false;
        };
        if channel.context.get(index).is_some_and(|subscriber| {
            subscriber
                .tx
                .send(ContextEnvelope::SnapshotRequested)
                .is_ok()
        }) {
            true
        } else {
            channel.context.remove(index);
            false
        }
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
    worker: Option<WorkerBinding>,
    state: Arc<CapnpBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
    /// Peer id assigned at accept time. Fanout uses it to skip the
    /// author when replaying its own frames back at itself.
    peer_id: u64,
    lease: Rc<ConnectionLease>,
}

struct ConnectionLease {
    state: Arc<CapnpBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
    peer_id: u64,
}

struct ConnectionExit {
    state: Arc<CapnpBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
    peer_id: u64,
}

impl Drop for ConnectionExit {
    fn drop(&mut self) {
        self.state.remove_connection(self.peer_id);
        self.directories.remove(self.peer_id);
    }
}

impl Drop for ConnectionLease {
    fn drop(&mut self) {
        self.state.remove_connection(self.peer_id);
        self.directories.remove(self.peer_id);
    }
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
                state.certificate_authority.get().map(Arc::as_ref),
            )
            .map_err(|error| {
                capnp::Error::failed(format!("collaboration join identity rejected: {error}"))
            })?;
            if let Some(worker) = &self.worker {
                if route != worker.route
                    || verified_identity.verified.user_public_key != worker.root
                    || verified_identity.verified.device_public_key != worker.device
                {
                    return Err(capnp::Error::failed(
                        "worker descriptor route or certificate mismatch".into(),
                    ));
                }
            } else if state.durable_owner_required && !route.app_id.is_empty() {
                return Err(capnp::Error::failed(
                    "application requires host-owned worker descriptor admission".into(),
                ));
            }
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
                        &membership.snapshot().map_err(capnp::Error::failed)?,
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
                directories: Rc::clone(&self.directories),
                channel_id,
                peer_id,
                verified_identity,
                lease: Some(Rc::clone(&self.lease)),
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
    directories: Rc<LocalActionDirectoryRegistry>,
    channel_id: String,
    peer_id: u64,
    verified_identity: AdmittedJoinIdentity,
    /// Production handles share the root RPC connection lease. Direct unit
    /// fixtures use `None` and retain the legacy one-handle cleanup behavior.
    lease: Option<Rc<ConnectionLease>>,
}

impl Drop for CapnpChannelHandle {
    fn drop(&mut self) {
        if self.lease.is_none() {
            self.state.remove_connection(self.peer_id);
            self.directories.remove(self.peer_id);
        }
    }
}

impl CapnpChannelHandle {
    /// A record-scoped connection names its record inside the channel id
    /// (the SDK joins `<cluster>#record:<id>`), parsed back out here.
    fn route(&self) -> CrdtRoute {
        CrdtRoute::from_channel_key(&self.channel_id)
    }

    fn realtime_context(&self) -> Result<crate::services::RealtimeRequestContext, capnp::Error> {
        self.state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        let route = self.route();
        if route.app_id.is_empty() || route.cluster_id.is_empty() {
            return Err(capnp::Error::failed(
                "realtime continuity requires an exact certified app/workspace route".into(),
            ));
        }
        Ok(crate::services::RealtimeRequestContext::authenticated(
            self.peer_id,
            route.app_id,
            route.cluster_id,
        ))
    }
}

fn delegate_realtime_command(
    services: &HostServices,
    context: &crate::services::RealtimeRequestContext,
    claimed_app: &str,
    command: &[u8],
) -> Result<Vec<u8>, capnp::Error> {
    if claimed_app != context.app_id() {
        return Err(capnp::Error::failed(
            "realtime app id must match the certified connection route".into(),
        ));
    }
    let service = services.realtime.as_ref().ok_or_else(|| {
        capnp::Error::failed("realtime continuity is unsupported by this host".into())
    })?;
    service
        .command(context, command)
        .map_err(capnp::Error::failed)
}

impl channel_handle::Server for CapnpChannelHandle {
    fn request_document_snapshot(
        self: Rc<Self>,
        _params: channel_handle::RequestDocumentSnapshotParams,
        _results: channel_handle::RequestDocumentSnapshotResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let _ = self
            .state
            .request_document_snapshot(&self.channel_id, self.peer_id);
        Promise::ok(())
    }

    fn realtime_command(
        self: Rc<Self>,
        params: channel_handle::RealtimeCommandParams,
        mut results: channel_handle::RealtimeCommandResults,
    ) -> Promise<(), capnp::Error> {
        let context = match self.realtime_context() {
            Ok(context) => context,
            Err(error) => return Promise::err(error),
        };
        let request = match params.get() {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let claimed_app = match request.get_app_id().and_then(|value| {
            value
                .to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value,
            Err(error) => return Promise::err(error),
        };
        let command = match request.get_command_json() {
            Ok(bytes) if bytes.len() <= ui::realtime_continuity::MAX_REALTIME_WIRE_BYTES => bytes,
            Ok(_) => {
                return Promise::err(capnp::Error::failed(
                    "realtime command exceeds local protocol bound".into(),
                ));
            }
            Err(error) => return Promise::err(error),
        };
        match delegate_realtime_command(&self.state.services, &context, claimed_app, command) {
            Ok(reply) if reply.len() <= ui::realtime_continuity::MAX_REALTIME_WIRE_BYTES => {
                results.get().set_reply_json(&reply);
                Promise::ok(())
            }
            Ok(_) => Promise::err(capnp::Error::failed(
                "realtime reply exceeds local protocol bound".into(),
            )),
            Err(error) => Promise::err(error),
        }
    }

    fn subscribe_realtime(
        self: Rc<Self>,
        _params: channel_handle::SubscribeRealtimeParams,
        mut results: channel_handle::SubscribeRealtimeResults,
    ) -> Promise<(), capnp::Error> {
        let context = match self.realtime_context() {
            Ok(context) => context,
            Err(error) => return Promise::err(error),
        };
        let Some(service) = self.state.services.realtime.as_ref() else {
            return Promise::err(capnp::Error::failed(
                "realtime continuity is unsupported by this host".into(),
            ));
        };
        let rx = match service.subscribe(&context) {
            Ok(rx) => rx,
            Err(error) => return Promise::err(capnp::Error::failed(error)),
        };
        let client: realtime_stream::Client = capnp_rpc::new_client(RealtimeStreamServer {
            rx: Rc::new(AsyncMutex::new(rx)),
        });
        results.get().set_stream(client);
        Promise::ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "receipt validation and persistence share one RPC boundary so acknowledgement cannot precede durable local state"
    )]
    fn record_local_effect(
        self: Rc<Self>,
        params: channel_handle::RecordLocalEffectParams,
        _results: channel_handle::RecordLocalEffectResults,
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
        let resource = match request
            .get_resource()
            .map_err(|error| error.to_string())
            .and_then(|value| value.to_str().map_err(|error| error.to_string()))
        {
            Ok(value) => value.to_string(),
            Err(error) => return Promise::err(capnp::Error::failed(error)),
        };
        let action = match request
            .get_action()
            .map_err(|error| error.to_string())
            .and_then(|value| value.to_str().map_err(|error| error.to_string()))
        {
            Ok(value) => value.to_string(),
            Err(error) => return Promise::err(capnp::Error::failed(error)),
        };
        let update = match request
            .get_update_json()
            .map_err(|error| capnp::Error::failed(error.to_string()))
            .and_then(|value| {
                value
                    .to_str()
                    .map_err(|error| capnp::Error::failed(error.to_string()))
            })
            .and_then(|json| {
                serde_json::from_str::<tilo_actions::InvocationUpdate>(json)
                    .map_err(|error| capnp::Error::failed(error.to_string()))
            }) {
            Ok(update) => update,
            Err(error) => return Promise::err(error),
        };
        let Some(status) = tilo_actions::EffectReceiptStatus::from_terminal(&update) else {
            return Promise::err(capnp::Error::failed(
                "only a terminal effect may enter workspace activity".into(),
            ));
        };
        let Some((surface, offered)) = self
            .state
            .surfaces
            .lock()
            .ok()
            .and_then(|surfaces| surfaces.get(&self.peer_id).cloned())
            .and_then(|surface| {
                let offered = surface
                    .actions
                    .iter()
                    .find(|candidate| candidate.resource == resource && candidate.action == action)
                    .cloned()?;
                Some((surface, offered))
            })
        else {
            return Promise::err(capnp::Error::failed(
                "the application no longer publishes that action".into(),
            ));
        };
        if offered.operation_kind != tilo_actions::OperationKind::Effect {
            return Promise::err(capnp::Error::failed(
                "only Effects produce durable invocation receipts".into(),
            ));
        }
        let mut context = tilo_actions::ActionContext::for_resource(
            offered.action_label,
            offered.resource,
            offered.resource_kind,
            offered.resource_label,
        )
        .with_app(&surface.app_id, &surface.app_id);
        let actor = current.broker_actor().id;
        if self.state.durable_owner_required {
            context.purpose = None;
            let receipt = crate::policy::redacted_effect_receipt(
                update.invocation(),
                &actor,
                context,
                status,
            );
            let state = self.state.clone();
            let route = self.route();
            let identity = self.verified_identity.clone();
            return Promise::from_future(async move {
                let _order = state.owner_commit_order.lock().await;
                state.revalidate_admission(&route, &identity)?;
                state
                    .commit_owner_request(qos_peer_state::owner_commit::OwnerCommitRequest {
                        room: surface.route.cluster_id,
                        operation: qos_peer_state::owner_commit::CommitOperation::EffectReceipt {
                            invocation: update.invocation().0,
                            redacted: serde_json::to_vec(&receipt)
                                .map_err(|e| capnp::Error::failed(e.to_string()))?,
                        },
                    })
                    .await?;
                state.revalidate_admission(&route, &identity)?;
                Ok(())
            });
        }
        context.purpose = None;
        match self
            .state
            .services
            .legacy_workspace
            .as_ref()
            .ok_or_else(|| "native effect projection unsupported".to_string())
            .and_then(|projection| {
                projection.record_effect(
                    &surface.route.cluster_id,
                    update.invocation(),
                    &actor,
                    context,
                    status,
                )
            }) {
            Ok(()) => Promise::ok(()),
            Err(error) => Promise::err(capnp::Error::failed(error)),
        }
    }

    fn open_workspace_control(
        self: Rc<Self>,
        params: channel_handle::OpenWorkspaceControlParams,
        mut results: channel_handle::OpenWorkspaceControlResults,
    ) -> Promise<(), capnp::Error> {
        let route = self.route();
        if let Err(error) =
            revalidate_workspace_control(&self.state, &route, &self.verified_identity)
        {
            return Promise::err(error);
        }
        let Some(room) = qos_desktop::RoomId::parse(&route.cluster_id) else {
            return Promise::err(capnp::Error::failed(
                "workspace control requires a workspace-scoped channel".into(),
            ));
        };
        let target_bytes = match params.get().and_then(
            runtime_collab::collab_capnp::channel_handle::open_workspace_control_params::Reader::get_target_device,
        ) {
            Ok(bytes) if bytes.len() == 32 => {
                let mut target = [0_u8; 32];
                target.copy_from_slice(bytes);
                target
            }
            _ => {
                return Promise::err(capnp::Error::failed(
                    "target device must be an exact 32-byte identity".into(),
                ));
            }
        };
        let Some(service) = self.state.services.workspace_control.as_ref() else {
            return Promise::err(capnp::Error::failed("workspace control unsupported".into()));
        };
        let state = self.state.clone();
        let identity = self.verified_identity.clone();
        let revalidate =
            Arc::new(move || revalidate_workspace_control(&state, &route, &identity).map(|_| ()));
        let client = match service.open(room, target_bytes, revalidate) {
            Ok(client) => client,
            Err(error) => return Promise::err(error),
        };
        results.get().set_handle(client);
        Promise::ok(())
    }

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
            let _owner_order = if state.durable_owner_required {
                Some(state.owner_commit_order.lock().await)
            } else {
                None
            };
            if state.durable_owner_required {
                state.revalidate_admission(&route, &verified_identity)?;
                state
                    .commit_owner_request(qos_peer_state::owner_commit::OwnerCommitRequest {
                        room: route.cluster_id.clone(),
                        operation: qos_peer_state::owner_commit::CommitOperation::Document {
                            app: route.app_id.clone(),
                            record: route.record_id.clone(),
                            envelope: Box::new(envelope.clone()),
                        },
                    })
                    .await?;
            }
            let current_identity = state.revalidate_admission(&route, &verified_identity)?;
            let authorized =
                state.authorize_certified_document(&route, &current_identity, &envelope)?;
            handle_authorized_signed_op_send(
                &state,
                peer_id,
                &route,
                &current_identity,
                authorized,
                envelope,
            );
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
            interaction_sequences: Mutex::new(HashMap::new()),
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
                    || resource.contains(tilo_actions::KEY_SEP)
                    || action.contains(tilo_actions::KEY_SEP)
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
                        "" | "document" => tilo_actions::OperationKind::Document,
                        "effect" => tilo_actions::OperationKind::Effect,
                        "view" => tilo_actions::OperationKind::View,
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
                                "view" => tilo_actions::ActionContract::view(),
                                _ => tilo_actions::ActionContract::default(),
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
                    !matches!(
                        action.contract.discovery,
                        tilo_actions::DiscoveryScope::Live
                    )
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
            state.surface_generation.fetch_add(1, Ordering::Relaxed);
            state.publish_action_catalogue();
            Ok(())
        })
    }

    fn publish_action_directory(
        self: Rc<Self>,
        params: channel_handle::PublishActionDirectoryParams,
        _results: channel_handle::PublishActionDirectoryResults,
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
            Err(error) => return Promise::err(error),
        };
        if let Err(error) =
            validate_directory_identifier(&app_id, "app id", DIRECTORY_MAX_IDENTIFIER_BYTES, true)
        {
            return Promise::err(error);
        }
        if self.route().app_id.is_empty() || app_id != self.route().app_id {
            return Promise::err(capnp::Error::failed(
                "publishActionDirectory appId must match the certified connection route".into(),
            ));
        }
        let directory = match request.get_directory() {
            Ok(directory) => directory,
            Err(error) => return Promise::err(error),
        };
        let route = self.route();
        self.directories.directories.borrow_mut().insert(
            self.peer_id,
            RegisteredActionDirectory {
                app_id: app_id.clone(),
                route: route.clone(),
                directory,
            },
        );
        if let Ok(mut roots) = self.state.action_directory_roots.lock() {
            roots.insert(
                self.peer_id,
                ActionDirectoryRoot {
                    peer_id: self.peer_id,
                    app_id,
                    cluster_id: route.cluster_id,
                    record_id: route.record_id,
                },
            );
        }
        self.state.refresh_action_directory_root_snapshot();
        // The directory can alter remote discoverability without an eager
        // action snapshot changing, so notify legacy catalogue subscribers.
        self.state.publish_action_catalogue();
        Promise::ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one broker boundary must copy bounded input, enforce current policy, and copy a capability page without retaining it"
    )]
    fn query_action_directory(
        self: Rc<Self>,
        params: channel_handle::QueryActionDirectoryParams,
        mut results: channel_handle::QueryActionDirectoryResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let request = match params.get().and_then(
            runtime_collab::collab_capnp::channel_handle::query_action_directory_params::Reader::get_query,
        ) {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let peer_id = request.get_peer_id();
        // A global root is intentionally not synthesised from every app here:
        // that would force a scan of all live directories for each keystroke.
        // Callers discover live app ids from the small authenticated catalogue,
        // then page one source at a time with an opaque source cursor.
        if peer_id == 0 {
            return Promise::err(capnp::Error::failed(
                "a directory page must name one live application peer".into(),
            ));
        }
        let revision = match request.get_revision().and_then(|value| {
            value
                .to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value.to_owned(),
            Err(error) => return Promise::err(error),
        };
        let needle = match request.get_needle().and_then(|value| {
            value
                .to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value.to_owned(),
            Err(error) => return Promise::err(error),
        };
        let parent_resource = match request.get_parent_resource().and_then(|value| {
            value
                .to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        }) {
            Ok(value) => value.to_owned(),
            Err(error) => return Promise::err(error),
        };
        let cursor = match request.get_cursor() {
            Ok(cursor) => cursor.to_vec(),
            Err(error) => return Promise::err(error),
        };
        if let Err(error) = validate_directory_query_input(&runtime_collab::ActionDirectoryQuery {
            peer_id,
            revision: revision.clone(),
            cursor: cursor.clone(),
            needle: needle.clone(),
            parent_resource: parent_resource.clone(),
            // Input validation is independent of numeric page budgets; these
            // placeholders avoid delaying it until after a capability lookup.
            max_results: 1,
            max_bytes: 1,
            max_work: 1,
            max_depth: 0,
        }) {
            return Promise::err(error);
        }
        let max_results = request
            .get_max_results()
            .min(CapnpBrokerState::DIRECTORY_MAX_RESULTS);
        let max_bytes = request
            .get_max_bytes()
            .min(CapnpBrokerState::DIRECTORY_MAX_BYTES);
        let max_work = request
            .get_max_work()
            .min(CapnpBrokerState::DIRECTORY_MAX_WORK);
        let max_depth = request
            .get_max_depth()
            .min(CapnpBrokerState::DIRECTORY_MAX_DEPTH);
        if max_results == 0 || max_bytes == 0 || max_work == 0 {
            return Promise::err(capnp::Error::failed(
                "directory query budgets must be non-zero".into(),
            ));
        }
        let cluster = self.route().cluster_id;
        let actor = self.verified_identity.broker_actor();
        let registered = match self.directories.registered(peer_id, &cluster) {
            Ok(registered) => registered,
            Err(error) => return Promise::err(error),
        };
        let state = self.state.clone();
        Promise::from_future(async move {
            let mut forward = registered.directory.query_request();
            {
                let mut query = forward.get().init_query();
                query.set_peer_id(peer_id);
                query.set_revision(&revision);
                query.set_cursor(&cursor);
                query.set_needle(&needle);
                query.set_parent_resource(&parent_resource);
                query.set_max_results(max_results);
                query.set_max_bytes(max_bytes);
                query.set_max_work(max_work);
                query.set_max_depth(max_depth);
            }
            let response = forward.send().promise.await?;
            let page = response.get()?.get_page()?;
            let bytes = page.total_size()?.word_count.saturating_mul(8);
            if bytes > u64::from(max_bytes) {
                return Err(capnp::Error::failed(
                    "application exceeded directory byte budget".into(),
                ));
            }
            let page = runtime_collab::decode_action_directory_page(page)?;
            let page = filter_directory_page(
                &state,
                &cluster,
                &actor,
                &registered.app_id,
                page,
                max_results,
            )?;
            write_directory_page(results.get().init_page(), &page)?;
            Ok(())
        })
    }

    fn resolve_published_action(
        self: Rc<Self>,
        params: channel_handle::ResolvePublishedActionParams,
        mut results: channel_handle::ResolvePublishedActionResults,
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
        let peer_id = request.get_peer_id();
        let read_text = |value: capnp::Result<capnp::text::Reader<'_>>| {
            value
                .and_then(|value| {
                    value
                        .to_str()
                        .map_err(|error| capnp::Error::failed(error.to_string()))
                })
                .map(str::to_owned)
        };
        let revision = match read_text(request.get_revision()) {
            Ok(value) => value,
            Err(error) => return Promise::err(error),
        };
        let resource = match read_text(request.get_resource()) {
            Ok(value) => value,
            Err(error) => return Promise::err(error),
        };
        let action = match read_text(request.get_action()) {
            Ok(value) => value,
            Err(error) => return Promise::err(error),
        };
        let cluster = self.route().cluster_id;
        if let Err(error) =
            validate_directory_target_input(&cluster, &revision, &resource, &action, None, None)
        {
            return Promise::err(error);
        }
        let actor = self.verified_identity.broker_actor();
        let registered = match self.directories.registered(peer_id, &cluster) {
            Ok(registered) => registered,
            Err(error) => return Promise::err(error),
        };
        let state = self.state.clone();
        Promise::from_future(async move {
            let mut forward = registered.directory.resolve_request();
            {
                let mut resolve = forward.get();
                resolve.set_revision(&revision);
                resolve.set_resource(&resource);
                resolve.set_action(&action);
            }
            let response = forward.send().promise.await?;
            let resolution = response.get()?.get_resolution()?;
            let resolution = runtime_collab::decode_action_directory_resolution(resolution)?;
            let mut out = results.get().init_resolution();
            match resolution {
                runtime_collab::ActionDirectoryResolution::Found(row)
                    if row.resource == resource
                        && row.action.as_deref() == Some(action.as_str())
                        && permits_directory_row(
                            &state,
                            &cluster,
                            &actor,
                            &registered.app_id,
                            &row,
                        )? =>
                {
                    write_directory_row(out.init_found(), &row);
                }
                runtime_collab::ActionDirectoryResolution::Found(_)
                | runtime_collab::ActionDirectoryResolution::NotFound => out.set_not_found(()),
                runtime_collab::ActionDirectoryResolution::StaleRevision => {
                    out.set_stale_revision(());
                }
                runtime_collab::ActionDirectoryResolution::Unavailable => out.set_unavailable(()),
            }
            Ok(())
        })
    }

    fn subscribe_invocations(
        self: Rc<Self>,
        params: channel_handle::SubscribeInvocationsParams,
        mut results: channel_handle::SubscribeInvocationsResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let stable = match params.get() {
            Ok(request) => request.get_stable_invocation_ids(),
            Err(error) => return Promise::err(error),
        };
        if let Ok(mut capabilities) = self.state.stable_invocation_subscribers.lock() {
            if stable {
                capabilities.insert(self.peer_id);
            } else {
                capabilities.remove(&self.peer_id);
            }
        } else {
            return Promise::err(capnp::Error::failed(
                "application capability registry unavailable".into(),
            ));
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
        if self.state.services.legacy_workspace.is_none() {
            return Promise::err(capnp::Error::failed(
                "task mutation service unsupported".into(),
            ));
        }
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
                        .and_then(|value| Ok(value.to_str()?.to_owned()))
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
        let route = self.route();
        let identity = self.verified_identity.clone();
        Promise::from_future(async move {
            let _owner_order = if state.durable_owner_required {
                Some(state.owner_commit_order.lock().await)
            } else {
                None
            };
            if state.durable_owner_required {
                state.revalidate_admission(&route, &identity)?;
            }
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
            let registry = state
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
                    .validate(&tilo_actions::ActionOutput::default())
                    .map_err(|error| {
                        capnp::Error::failed(format!(
                            "legacy completion violated its published result contract: {error}"
                        ))
                    })?;
            }
            let pending = registry
                .get(&id)
                .ok_or_else(|| capnp::Error::failed("unknown invocation id".into()))?;
            let invocation = pending.update.invocation();
            let update = if completion.code.is_empty() {
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                }
            } else {
                tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: completion.code.clone(),
                        message: completion.message.clone(),
                        details: None,
                        retryable: false,
                    },
                }
            };
            let staged = pending.clone();
            drop(registry);
            state
                .commit_effect_before_terminal(&staged, &update)
                .await?;
            if state.durable_owner_required {
                state.revalidate_admission(&route, &identity)?;
            }
            let mut registry = state
                .pending_invocations
                .lock()
                .map_err(|_| capnp::Error::failed("invocation registry unavailable".into()))?;
            let pending = registry
                .get(&id)
                .filter(|pending| pending.peer_id == peer_id && pending.update == staged.update)
                .ok_or_else(|| {
                    capnp::Error::failed(
                        "invocation changed while committing; outcome unknown".into(),
                    )
                })?;
            state
                .persist_invocation(pending, update.clone(), false)
                .map_err(capnp::Error::failed)?;
            let pending = registry
                .remove(&id)
                .ok_or_else(|| capnp::Error::failed("unknown invocation id".into()))?;
            drop(registry);
            state.record_completion(id, pending.completion_owner(), update.clone());
            if let Ok(mut requests) = state.cancellation_requests.lock() {
                requests.remove(&id);
            }
            let _ = pending.tx.send(completion);
            state.record_effect_receipt(&pending, &update);
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
        Promise::from_future(async move {
            let request = params.get()?;
            let update =
                serde_json::from_str(request.get_update_json()?.to_str()?).map_err(|error| {
                    capnp::Error::failed(format!("invalid invocation update: {error}"))
                })?;
            self.state
                .apply_invocation_update(
                    self.peer_id,
                    request.get_id(),
                    update,
                    Some((self.route(), self.verified_identity.clone())),
                )
                .await
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
            None,
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
        let initial = match self.state.action_catalogue_json_for(&cluster, &actor, None) {
            Ok(snapshot) => Arc::<str>::from(snapshot),
            Err(error) => return Promise::err(capnp::Error::failed(error)),
        };
        let (tx, rx) = watch::channel(Some(initial));
        if let Ok(mut subscribers) = self.state.action_catalogue_subscribers.lock() {
            subscribers.insert(self.peer_id, (cluster, actor, tx));
        } else {
            return Promise::err(capnp::Error::failed(
                "action catalogue subscriber registry unavailable".into(),
            ));
        }
        let client: action_catalogue_stream::Client =
            capnp_rpc::new_client(ActionCatalogueStreamServer {
                reader: Rc::new(AsyncMutex::new(ActionCatalogueReader { rx, initial: true })),
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
        let requested_peer_id = request.get_peer_id();
        let peer_id = invocation_target_peer(requested_peer_id, self.peer_id);
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
                tilo_actions::ConfirmationPolicy::None => state.start_invocation_confirmed_for(
                    peer_id,
                    &resource,
                    &action,
                    argument.as_deref(),
                    &actor,
                    None,
                    None,
                ),
                tilo_actions::ConfirmationPolicy::Confirm => {
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
                tilo_actions::ConfirmationPolicy::Phrase { .. } => {
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
            let tree = match membership.snapshot() {
                Ok(tree) => tree,
                Err(error) => return Promise::err(capnp::Error::failed(error)),
            };
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
        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id)));
        let update = match self
            .state
            .services
            .launcher
            .as_ref()
            .ok_or_else(|| "application launch unsupported".to_string())
            .and_then(|service| service.launch(&app_id, &workspace))
        {
            Ok(_) => tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            },
            Err(error) => tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
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
        else {
            return Promise::err(capnp::Error::failed(
                "reportContinuation requires a durable shell tab id".into(),
            ));
        };
        let Some(room) = qos_desktop::RoomId::parse(&route.cluster_id) else {
            return Promise::err(capnp::Error::failed(
                "reportContinuation route has no durable workspace room".into(),
            ));
        };
        let metadata = match request.get_metadata() {
            Ok(bytes) => bytes.to_vec(),
            Err(error) => return Promise::err(error),
        };
        match self
            .state
            .services
            .continuations
            .as_ref()
            .ok_or_else(|| "continuation reporting unsupported".to_string())
            .and_then(|service| {
                service.report(
                    room,
                    app_id,
                    instance_id,
                    request.get_schema_version(),
                    metadata,
                )
            }) {
            Ok(()) => Promise::ok(()),
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
        else {
            return Promise::err(capnp::Error::failed(
                "clearContinuation requires a durable shell tab id".into(),
            ));
        };
        let Some(room) = qos_desktop::RoomId::parse(&route.cluster_id) else {
            return Promise::err(capnp::Error::failed(
                "clearContinuation route has no durable workspace room".into(),
            ));
        };
        match self
            .state
            .services
            .continuations
            .as_ref()
            .ok_or_else(|| "continuation clearing unsupported".to_string())
            .and_then(|service| service.clear(room, &app_id, instance_id))
        {
            Ok(()) => Promise::ok(()),
            Err(error) => Promise::err(capnp::Error::failed(error)),
        }
    }
}

/// Validate every untrusted field before it reaches shell chrome, then
/// apply current authority. `Ok(false)` means a structurally valid row is
/// simply not visible to this actor; it must not suppress permitted
/// siblings on the same source page.
#[allow(
    clippy::too_many_lines,
    reason = "directory row authorization validates every capability-bearing field before the result reaches shell search"
)]
fn permits_directory_row(
    state: &CapnpBrokerState,
    cluster_id: &str,
    actor: &BrokerActor,
    app_id: &str,
    row: &runtime_collab::ActionDirectoryRow,
) -> Result<bool, capnp::Error> {
    validate_directory_identifier(
        &row.parent_resource,
        "parent resource",
        DIRECTORY_MAX_IDENTIFIER_BYTES,
        false,
    )?;
    validate_directory_identifier(
        &row.resource,
        "resource",
        DIRECTORY_MAX_IDENTIFIER_BYTES,
        true,
    )?;
    validate_directory_identifier(
        &row.resource_kind,
        "resource kind",
        DIRECTORY_MAX_KIND_BYTES,
        true,
    )?;
    validate_directory_text(
        &row.resource_label,
        "resource label",
        DIRECTORY_MAX_LABEL_BYTES,
        true,
    )?;
    match (&row.action, &row.descriptor) {
        (Some(action), Some(descriptor)) => {
            validate_directory_identifier(action, "action", DIRECTORY_MAX_IDENTIFIER_BYTES, true)?;
            if descriptor.resource != row.resource
                || descriptor.action != *action
                || descriptor.resource_kind != row.resource_kind
                || descriptor.resource_label != row.resource_label
                || row.has_children
                || row.visibility_action.as_deref() != Some(action)
                || row.visibility_contract_json != descriptor.contract_json
            {
                return Err(capnp::Error::failed(
                    "directory action row does not match its descriptor".into(),
                ));
            }
            validate_directory_text(
                &descriptor.action_label,
                "action label",
                DIRECTORY_MAX_LABEL_BYTES,
                true,
            )?;
            if descriptor
                .description
                .as_deref()
                .is_some_and(|description| {
                    description.len() > DIRECTORY_MAX_DESCRIPTION_BYTES
                        || description.contains('\0')
                })
            {
                return Err(capnp::Error::failed(
                    "invalid action directory description".into(),
                ));
            }
            if !matches!(
                descriptor.operation_kind.as_str(),
                "document" | "effect" | "view"
            ) {
                return Err(capnp::Error::failed(
                    "invalid action directory operation kind".into(),
                ));
            }
            if descriptor.params_json.len() > DIRECTORY_MAX_PARAMS_BYTES
                || serde_json::from_str::<Vec<tilo_actions::ActionParam>>(&descriptor.params_json)
                    .is_err()
            {
                return Err(capnp::Error::failed(
                    "invalid action directory parameters".into(),
                ));
            }
            match &descriptor.input {
                runtime_collab::ExportedInput::None => {}
                runtime_collab::ExportedInput::Text {
                    prompt,
                    placeholder,
                    ..
                } => {
                    validate_directory_text(
                        prompt,
                        "input prompt",
                        DIRECTORY_MAX_LABEL_BYTES,
                        true,
                    )?;
                    validate_directory_text(
                        placeholder,
                        "input placeholder",
                        DIRECTORY_MAX_LABEL_BYTES,
                        false,
                    )?;
                }
            }
            state.permits_directory_action(
                cluster_id,
                actor,
                app_id,
                action,
                &descriptor.contract_json,
            )
        }
        (None, None) => {
            if row.visibility_action.is_none() || row.visibility_contract_json.is_empty() {
                return Err(capnp::Error::failed(
                    "directory resource row has no visibility contract".into(),
                ));
            }
            let visibility_action = row.visibility_action.as_deref().unwrap_or_default();
            validate_directory_identifier(
                visibility_action,
                "visibility action",
                DIRECTORY_MAX_IDENTIFIER_BYTES,
                true,
            )?;
            state.permits_directory_action(
                cluster_id,
                actor,
                app_id,
                visibility_action,
                &row.visibility_contract_json,
            )
        }
        _ => Err(capnp::Error::failed(
            "directory row mixes resource and action shapes".into(),
        )),
    }
}

fn filter_directory_page(
    state: &CapnpBrokerState,
    cluster_id: &str,
    actor: &BrokerActor,
    app_id: &str,
    mut page: runtime_collab::ActionDirectoryPage,
    max_results: u32,
) -> Result<runtime_collab::ActionDirectoryPage, capnp::Error> {
    if page.rows.len() > usize::try_from(max_results).unwrap_or(usize::MAX) {
        return Err(capnp::Error::failed(
            "application exceeded directory result budget".into(),
        ));
    }
    let mut visible = Vec::with_capacity(page.rows.len());
    for row in page.rows {
        if permits_directory_row(state, cluster_id, actor, app_id, &row)? {
            visible.push(row);
        }
    }
    page.rows = visible;
    Ok(page)
}

/// Convert the already validated, exact directory row into the inert action
/// shape used by the invocation lifecycle.  This deliberately accepts only
/// an action row: containers are discoverable but never invokable.
fn published_directory_action(
    row: &runtime_collab::ActionDirectoryRow,
) -> Result<PublishedAction, capnp::Error> {
    let action = row
        .action
        .as_deref()
        .ok_or_else(|| capnp::Error::failed("directory container is not invokable".into()))?;
    let descriptor = row
        .descriptor
        .as_ref()
        .ok_or_else(|| capnp::Error::failed("directory action has no descriptor".into()))?;
    if descriptor.resource != row.resource || descriptor.action != action {
        return Err(capnp::Error::failed(
            "directory descriptor address changed during conversion".into(),
        ));
    }
    let operation_kind = match descriptor.operation_kind.as_str() {
        "document" => tilo_actions::OperationKind::Document,
        "effect" => tilo_actions::OperationKind::Effect,
        "view" => tilo_actions::OperationKind::View,
        _ => {
            return Err(capnp::Error::failed(
                "invalid directory operation kind".into(),
            ));
        }
    };
    let params = serde_json::from_str(&descriptor.params_json)
        .map_err(|error| capnp::Error::failed(format!("invalid directory parameters: {error}")))?;
    let contract = serde_json::from_str(&descriptor.contract_json)
        .map_err(|error| capnp::Error::failed(format!("invalid directory contract: {error}")))?;
    let input = match &descriptor.input {
        runtime_collab::ExportedInput::None => PublishedInput::None,
        runtime_collab::ExportedInput::Text {
            prompt,
            placeholder,
            wants_content,
        } => PublishedInput::Text {
            prompt: prompt.clone(),
            placeholder: placeholder.clone(),
            wants_content: *wants_content,
        },
    };
    Ok(PublishedAction {
        resource: descriptor.resource.clone(),
        action: descriptor.action.clone(),
        resource_kind: descriptor.resource_kind.clone(),
        resource_label: descriptor.resource_label.clone(),
        action_label: descriptor.action_label.clone(),
        description: descriptor.description.clone(),
        operation_kind,
        params,
        contract,
        input,
    })
}

fn revalidate_workspace_control(
    state: &CapnpBrokerState,
    route: &CrdtRoute,
    identity: &AdmittedJoinIdentity,
) -> Result<AdmittedJoinIdentity, capnp::Error> {
    let current = state.revalidate_admission(route, identity)?;
    if !workspace_control_role_allowed(current.role) {
        return Err(capnp::Error::failed(
            "workspace control requires current editor authority".into(),
        ));
    }
    Ok(current)
}

fn workspace_control_role_allowed(role: collab_types::Role) -> bool {
    matches!(role, collab_types::Role::Owner | collab_types::Role::Editor)
}

/// Apply a signed op arriving via capnp `send`: same validation /
/// audit / redaction / compaction pipeline, then
/// fans out via the shared [`FanoutState`].
#[cfg(test)]
fn handle_signed_op_send(
    state: &Arc<CapnpBrokerState>,
    peer_id: u64,
    route: &CrdtRoute,
    verified_identity: &AdmittedJoinIdentity,
    envelope: SignedOp,
) {
    let artifact = envelope.artifact;
    handle_signed_op_send_for_artifact(
        state,
        peer_id,
        route,
        verified_identity,
        artifact,
        envelope,
    );
}

fn handle_signed_op_send_for_artifact(
    state: &Arc<CapnpBrokerState>,
    peer_id: u64,
    route: &CrdtRoute,
    verified_identity: &AdmittedJoinIdentity,
    artifact: ArtifactId,
    envelope: SignedOp,
) {
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
            let mut mls_vault = None;
            if let Ok(mut ch) = state.broker_channels.lock() {
                mls_vault.clone_from(&ch.mls_vault);
                let ch_state = ch.state_for(artifact);
                ch_state.record_validated_op(envelope.clone());
                if envelope.op_class == collab_types::OpClass::Compaction
                    && let Ok(cp) = collab_types::parse_compaction_payload(&envelope)
                {
                    let _ = ch_state.apply_compaction(&cp);
                }
            }
            if envelope.op_class == collab_types::OpClass::Compaction
                && let Some(metadata) = envelope.encryption
                && let Some(vault) = mls_vault
                && let Err(error) = vault.mint_snapshot_receipt(metadata.channel, &envelope)
            {
                tracing::warn!(%error, "encrypted snapshot was accepted but durable receipt minting failed");
            }
            if envelope.op_class == collab_types::OpClass::Redact {
                apply_redaction_from_envelope(&envelope, artifact, &state.broker_channels);
            }
            // Republishing is not vouching.
            if !state.durable_owner_required {
                state.publisher().signed_document(route, envelope.clone());
            }
            let from_user_id = CapnpBrokerState::participant_id(verified_identity);
            let event = CollabEvent::Document {
                envelope: Box::new(envelope),
                from_user_id,
                decrypted_payload: None,
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

fn handle_authorized_signed_op_send(
    state: &Arc<CapnpBrokerState>,
    peer_id: u64,
    route: &CrdtRoute,
    verified_identity: &AdmittedJoinIdentity,
    authorized: AuthorizedDocumentRoute,
    envelope: SignedOp,
) {
    debug_assert_eq!(authorized.artifact, envelope.artifact);
    debug_assert_eq!(
        authorized.channel,
        match &authorized.payload {
            collab_types::VersionedRoutedDocumentPayload::Cleartext(decoded) => {
                collab_types::document_channel_id_for_version(
                    decoded.version,
                    &route.app_id,
                    &route.cluster_id,
                    &route.record_id,
                )
            }
            collab_types::VersionedRoutedDocumentPayload::Encrypted { channel, .. } => *channel,
        }
    );
    handle_signed_op_send_for_artifact(
        state,
        peer_id,
        route,
        verified_identity,
        authorized.artifact,
        envelope,
    );
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
    /// Broker-owned monotonic sequence per invocation. Publishers describe
    /// phases and targets but cannot reorder or resurrect their visual state.
    interaction_sequences: Mutex<HashMap<tilo_actions::InvocationId, u64>>,
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

    fn publish_interaction(
        self: Rc<Self>,
        params: presence_handle::PublishInteractionParams,
        _results: presence_handle::PublishInteractionResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self.revalidate() {
            return Promise::err(error);
        }
        Promise::from_future(async move {
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            let json = r.get_interaction_json()?.to_str().map_err(|error| {
                capnp::Error::failed(format!("invalid interaction utf8: {error}"))
            })?;
            if json.len() > 64 * 1024 {
                return Err(capnp::Error::failed(
                    "interaction payload exceeds 64 KiB".into(),
                ));
            }
            let mut update: tilo_actions::SemanticInteraction = serde_json::from_str(json)
                .map_err(|error| capnp::Error::failed(format!("invalid interaction: {error}")))?;
            let actor = self.verified_identity.broker_actor();
            let normalized_actor = tilo_actions::InteractionActor {
                id: CapnpBrokerState::participant_id(&self.verified_identity),
                kind: if actor.is_agent {
                    tilo_actions::InteractionActorKind::Agent
                } else {
                    tilo_actions::InteractionActorKind::Person
                },
            };
            let sequence = {
                let mut sequences = self.interaction_sequences.lock().map_err(|_| {
                    capnp::Error::failed("interaction sequence registry unavailable".into())
                })?;
                let current = sequences.entry(update.invocation).or_insert(0);
                let next = current.saturating_add(1);
                *current = next;
                // Persist the monotonic sequence before validating and
                // publishing the interaction outside this mutex.
                drop(sequences);
                next
            };
            update = update.normalized_by_broker(normalized_actor, sequence);
            update
                .validate()
                .map_err(|error| capnp::Error::failed(format!("invalid interaction: {error}")))?;
            self.publish_keyed(&key_bytes, PresencePayload::Interaction(Box::new(update)));
            Ok(())
        })
    }

    fn publish_status(
        self: Rc<Self>,
        params: presence_handle::PublishStatusParams,
        _results: presence_handle::PublishStatusResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self.revalidate() {
            return Promise::err(error);
        }
        Promise::from_future(async move {
            let request = params.get()?;
            let key_bytes = request.get_key_bytes()?.to_vec();
            let raw = request
                .get_text()?
                .to_str()
                .map_err(|error| capnp::Error::failed(format!("invalid status utf8: {error}")))?;
            let text = raw.split_whitespace().collect::<Vec<_>>().join(" ");
            if text.chars().count() > ui::MAX_STATUS_CHARS {
                return Err(capnp::Error::failed(
                    "status exceeds local protocol bound".into(),
                ));
            }
            self.publish_keyed(
                &key_bytes,
                PresencePayload::Status {
                    text,
                    automatic: request.get_automatic(),
                },
            );
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

struct RealtimeStreamServer {
    rx: Rc<AsyncMutex<mpsc::UnboundedReceiver<Vec<u8>>>>,
}

impl realtime_stream::Server for RealtimeStreamServer {
    fn read(
        self: Rc<Self>,
        _params: realtime_stream::ReadParams,
        mut results: realtime_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            let mut guard = rx.lock().await;
            let event = guard.recv().await.ok_or_else(|| {
                capnp::Error::disconnected("realtime continuity stream closed".into())
            })?;
            drop(guard);
            results.get().set_event_json(&event);
            Ok(())
        })
    }
}

struct InvocationStreamServer {
    rx: Rc<AsyncMutex<mpsc::UnboundedReceiver<InvocationEnvelope>>>,
}

struct TaskRunStreamServer {
    rx: Rc<AsyncMutex<mpsc::UnboundedReceiver<TaskRunEnvelope>>>,
}

struct ActionCatalogueStreamServer {
    reader: Rc<AsyncMutex<ActionCatalogueReader>>,
}

struct ActionCatalogueReader {
    rx: watch::Receiver<ActionCatalogueSnapshot>,
    initial: bool,
}

impl ActionCatalogueReader {
    async fn next(&mut self) -> Result<Arc<str>, ()> {
        if self.initial {
            self.initial = false;
        } else {
            self.rx.changed().await.map_err(|_| ())?;
        }
        let snapshot = self.rx.borrow_and_update().clone();
        snapshot.ok_or(())
    }
}

impl action_catalogue_stream::Server for ActionCatalogueStreamServer {
    fn read(
        self: Rc<Self>,
        _params: action_catalogue_stream::ReadParams,
        mut results: action_catalogue_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let reader = self.reader.clone();
        Promise::from_future(async move {
            let snapshot = reader.lock().await.next().await.map_err(|()| {
                capnp::Error::disconnected("action catalogue stream closed".into())
            })?;
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
            result.set_invocation_uuid(invocation.invocation.0.as_bytes());
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
                PresencePayload::Following { target } => {
                    payload.set_following(target.as_deref().unwrap_or(""));
                }
                PresencePayload::Interaction(update) => {
                    let json = serde_json::to_string(&update).map_err(|error| {
                        capnp::Error::failed(format!("encode semantic interaction: {error}"))
                    })?;
                    payload.set_interaction_json(&json);
                }
                PresencePayload::Status { text, automatic } => {
                    let mut status = payload.init_status();
                    status.set_text(&text);
                    status.set_automatic(automatic);
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
                ContextEnvelope::SnapshotRequested => {
                    ctx.set_snapshot_requested(true);
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
/// [`crate::collab_broker`] uses for peer replication.
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
#[allow(
    clippy::too_many_arguments,
    clippy::needless_pass_by_value,
    reason = "the broker thread takes ownership of the exact shared handles it wires into its isolated LocalSet"
)]
pub fn spawn(
    socket_path: PathBuf,
    fanout: &Arc<FanoutState>,
    broker_channels: Arc<Mutex<BrokerChannels>>,
    publisher: Arc<PeerPublisher>,
    membership: Option<WorkspaceTreeHandle>,
    services: HostServices,
    quit: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    spawn_configured(
        socket_path,
        fanout,
        broker_channels,
        publisher,
        membership,
        None,
        services,
        quit,
        None,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SocketIdentity {
    device: u64,
    inode: u64,
}

fn socket_identity(path: &Path) -> std::io::Result<SocketIdentity> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "broker socket path is not a Unix socket",
        ));
    }
    Ok(SocketIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn socket_lock_path(path: &Path) -> PathBuf {
    let mut lock = path.as_os_str().to_os_string();
    lock.push(".lock");
    PathBuf::from(lock)
}

fn acquire_socket_path_lease(path: &Path) -> std::io::Result<File> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(socket_lock_path(path))?;
    if !lock.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "broker socket lease is not a regular file",
        ));
    }
    lock.try_lock().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "broker socket path is owned by another broker",
        )
    })?;
    Ok(lock)
}

fn remove_socket_if_owned(path: &Path, identity: SocketIdentity) -> std::io::Result<bool> {
    let current = match socket_identity(path) {
        Ok(current) => current,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if current != identity {
        return Ok(false);
    }
    std::fs::remove_file(path)?;
    Ok(true)
}

fn private_socket_parent(path: &Path) -> std::io::Result<&Path> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "broker socket requires a parent directory",
        )
    })?;
    let metadata = std::fs::symlink_metadata(parent)?;
    // SAFETY: geteuid takes no arguments and has no failure mode.
    let effective_uid = unsafe { libc::geteuid() };
    if !metadata.file_type().is_dir()
        || metadata.uid() != effective_uid
        || metadata.mode() & 0o077 != 0
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "broker socket parent must be an owner-only directory",
        ));
    }
    Ok(parent)
}

fn socket_stage_path(path: &Path) -> std::io::Result<PathBuf> {
    let parent = private_socket_parent(path)?;
    let leaf_len = path
        .file_name()
        .map(|leaf| leaf.as_bytes().len())
        .filter(|len| *len > 0)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "broker socket requires a filename",
            )
        })?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    let mut leaf = String::with_capacity(leaf_len);
    if leaf_len > 1 {
        leaf.push('.');
    }
    while leaf.len() < leaf_len {
        let remaining = leaf_len - leaf.len();
        leaf.push_str(&token[..remaining.min(token.len())]);
    }
    Ok(parent.join(leaf))
}

#[cfg(test)]
std::thread_local! {
    static FAIL_STAGED_SOCKET_IDENTITY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn staged_socket_identity(path: &Path) -> std::io::Result<SocketIdentity> {
    #[cfg(test)]
    if FAIL_STAGED_SOCKET_IDENTITY.with(|fail| fail.replace(false)) {
        return Err(std::io::Error::other(
            "injected staged socket identity failure",
        ));
    }
    socket_identity(path)
}

fn bind_socket_stage(
    path: &Path,
) -> std::io::Result<(std::os::unix::net::UnixListener, SocketStage)> {
    for _ in 0..8 {
        let socket = socket_stage_path(path)?;
        if socket == path {
            continue;
        }
        let mut stage = SocketStage {
            socket,
            identity: None,
            bound: false,
            published: false,
        };
        match std::os::unix::net::UnixListener::bind(&stage.socket) {
            Ok(listener) => {
                stage.bound = true;
                stage.identity = Some(staged_socket_identity(&stage.socket)?);
                return Ok((listener, stage));
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::AddrInUse | std::io::ErrorKind::AlreadyExists
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "broker socket staging name collision",
    ))
}

fn rename_socket_noreplace(source: &Path, destination: &Path) -> std::io::Result<()> {
    let source = CString::new(source.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "broker socket staging path contains NUL",
        )
    })?;
    let destination = CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "broker socket destination path contains NUL",
        )
    })?;
    #[cfg(target_os = "linux")]
    // SAFETY: both pointers refer to live NUL-terminated path buffers.
    // RENAME_NOREPLACE atomically refuses an occupied destination.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    // SAFETY: both pointers refer to live NUL-terminated path buffers.
    // RENAME_EXCL atomically refuses an occupied destination.
    let result = unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let result = -1;
    if result == 0 {
        return Ok(());
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic no-replace broker socket publication is unsupported",
    ));
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EEXIST) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                "broker socket destination appeared during publication",
            ));
        }
        Err(error)
    }
}

struct SocketStage {
    socket: PathBuf,
    identity: Option<SocketIdentity>,
    bound: bool,
    published: bool,
}

impl Drop for SocketStage {
    fn drop(&mut self) {
        if !self.published
            && let Some(identity) = self.identity
        {
            let _ = remove_socket_if_owned(&self.socket, identity);
        } else if !self.published && self.bound {
            if std::fs::symlink_metadata(&self.socket)
                .is_ok_and(|metadata| metadata.file_type().is_socket())
            {
                let _ = std::fs::remove_file(&self.socket);
            }
        }
    }
}

/// Remove only a proven stale socket, then bind and retain inode custody.
/// A live socket, symlink, non-socket, or replacement inode is never unlinked.
fn bind_owned_socket(
    path: &Path,
) -> std::io::Result<(std::os::unix::net::UnixListener, SocketCustody)> {
    private_socket_parent(path)?;
    let lease = acquire_socket_path_lease(path)?;
    match socket_identity(path) {
        Ok(stale_candidate) => match std::os::unix::net::UnixStream::connect(path) {
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    "broker socket is already accepting connections",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                if !remove_socket_if_owned(path, stale_candidate)? {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AddrInUse,
                        "broker socket changed during stale-socket recovery",
                    ));
                }
            }
            Err(error) => return Err(error),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let (listener, mut stage) = bind_socket_stage(path)?;
    let identity = stage.identity.expect("bound stage records its identity");
    rename_socket_noreplace(&stage.socket, path)?;
    stage.published = true;
    if socket_identity(path)? != identity {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "broker socket pathname changed during publication",
        ));
    }
    drop(stage);
    Ok((
        listener,
        SocketCustody {
            path: path.to_path_buf(),
            identity,
            _lease: lease,
        },
    ))
}

struct SocketCustody {
    path: PathBuf,
    identity: SocketIdentity,
    _lease: File,
}

impl Drop for SocketCustody {
    fn drop(&mut self) {
        let _ = remove_socket_if_owned(&self.path, self.identity);
    }
}

struct BrokerThreadShutdown {
    state: Arc<CapnpBrokerState>,
    fanout: Arc<FanoutState>,
}

impl Drop for BrokerThreadShutdown {
    fn drop(&mut self) {
        self.fanout.shutdown_capnp(&self.state);
    }
}

pub(super) fn spawn_configured(
    socket_path: PathBuf,
    fanout: &Arc<FanoutState>,
    broker_channels: Arc<Mutex<BrokerChannels>>,
    publisher: Arc<PeerPublisher>,
    membership: Option<WorkspaceTreeHandle>,
    trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
    services: HostServices,
    quit: Arc<AtomicBool>,
    config: Option<&super::peer_actions::NativePeerBrokerConfig>,
) -> std::io::Result<JoinHandle<()>> {
    let wired = publisher.is_wired();
    let (state, bridge_rx) = CapnpBrokerState::new_configured(
        fanout,
        broker_channels,
        publisher,
        membership,
        trusted_devices,
        config,
        services,
    );
    {
        let journal = state
            .journal
            .lock()
            .map_err(|_| std::io::Error::other("invocation journal unavailable"))?;
        journal
            .as_ref()
            .map_err(|error| std::io::Error::other(error.clone()))?;
    }
    // Bind on the caller's thread: from the worker thread, a bind
    // failure surfaces only via a log line.
    let (std_listener, socket_custody) = bind_owned_socket(&socket_path)?;
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

    let attached_state = Arc::clone(&state);
    let fanout_for_thread = Arc::clone(fanout);
    let socket_path_for_thread = socket_path.clone();
    let thread = std::thread::Builder::new()
        .name("qos-collab-broker-capnp".into())
        .spawn(move || {
            // Declaration order is intentional: shutdown first makes every
            // surviving facade inert and releases the journal, then socket
            // custody removes only this broker's still-owned inode.
            let _socket_custody = socket_custody;
            let _shutdown = BrokerThreadShutdown {
                state: Arc::clone(&state),
                fanout: fanout_for_thread,
            };
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
                let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge_rx));
                if let Err(error) = state.start_desktop_commands() {
                    if let Ok(mut failure) = state.owner_commit_failure.lock() {
                        failure.get_or_insert(error);
                    }
                    return;
                }
                state.start_peer_action_bridge(Rc::clone(&directories));
                let listener = match tokio::net::UnixListener::from_std(std_listener) {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::warn!(
                            "capnp broker: adopting {} into the reactor failed: {e}",
                            socket_path_for_thread.display()
                        );
                        return;
                    }
                };
                loop {
                    #[cfg(test)]
                    {
                        let callbacks = match state.thread_callbacks.lock() {
                            Ok(mut callbacks) => std::mem::take(&mut *callbacks),
                            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
                        };
                        for callback in callbacks {
                            callback();
                        }
                    }
                    let workers = state
                        .workers
                        .lock()
                        .map(|mut workers| std::mem::take(&mut *workers))
                        .unwrap_or_default();
                    for worker in workers {
                        let state = state.clone();
                        let directories = directories.clone();
                        let peer_id = state.next_peer_id();
                        tokio::task::spawn_local(async move {
                            let result = async {
                                let stream = tokio::net::UnixStream::from_std(worker.stream)
                                    .map_err(capnp::Error::from)?;
                                drive_connection(
                                    stream,
                                    state,
                                    directories,
                                    peer_id,
                                    Some(worker.binding),
                                )
                                .await
                            }
                            .await;
                            if let Err(error) = result {
                                let error_class = match error.kind {
                                    capnp::ErrorKind::Failed => "failed",
                                    capnp::ErrorKind::Overloaded => "overloaded",
                                    capnp::ErrorKind::Disconnected => "disconnected",
                                    capnp::ErrorKind::Unimplemented => "unimplemented",
                                    _ => "protocol",
                                };
                                tracing::warn!(
                                    peer_id,
                                    phase = "local_worker_rpc",
                                    error_class,
                                    "capnp broker: local worker disconnected"
                                );
                            }
                        });
                    }
                    state.drain_action_directory_bridge(&directories);
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
                    let directories = Rc::clone(&directories);
                    let peer_id = state.next_peer_id();
                    tokio::task::spawn_local(async move {
                        if let Err(e) =
                            drive_connection(stream, state, directories, peer_id, None).await
                        {
                            tracing::warn!("capnp broker: peer-{peer_id} disconnected: {e}");
                        }
                    });
                }
            });
        })?;
    fanout.attach_capnp(attached_state);

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
/// Peek at an incoming descriptor without consuming Cap'n Proto's segment
/// header. Raw endpoint diagnostics intentionally omit the launcher marker;
/// only a marker-bearing connection is subject to shell-process attestation.
async fn has_collab_launch_marker(stream: &tokio::net::UnixStream) -> Result<bool, capnp::Error> {
    for _ in 0..LAUNCH_RECORD_RACE_RETRIES {
        stream.readable().await.map_err(|error| {
            capnp::Error::failed(format!("collaboration socket unreadable: {error}"))
        })?;
        let mut marker = [0_u8; collab_types::local_transport::COLLAB_LAUNCH_MAGIC.len()];
        // SAFETY: `marker` is a writable fixed-size buffer and `stream` owns
        // the live socket for this entire call. MSG_PEEK leaves the Cap'n
        // Proto header untouched for legacy/raw clients.
        let read = unsafe {
            libc::recv(
                stream.as_raw_fd(),
                marker.as_mut_ptr().cast(),
                marker.len(),
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        let marker_len = libc::ssize_t::try_from(marker.len())
            .map_err(|_| capnp::Error::failed("launcher header exceeds ssize_t".into()))?;
        if read >= marker_len {
            return Ok(marker == collab_types::local_transport::COLLAB_LAUNCH_MAGIC);
        }
        if read == 0 {
            return Err(capnp::Error::disconnected(
                "collaboration peer closed before its protocol header".into(),
            ));
        }
        if read < 0
            && !matches!(
                std::io::Error::last_os_error().kind(),
                std::io::ErrorKind::WouldBlock
            )
        {
            return Err(capnp::Error::failed(format!(
                "could not inspect collaboration protocol header: {}",
                std::io::Error::last_os_error()
            )));
        }
        tokio::time::sleep(LAUNCH_RECORD_RACE_DELAY).await;
    }
    Err(capnp::Error::failed(
        "collaboration peer sent an incomplete protocol header".into(),
    ))
}

async fn authenticate_launcher_handoff(
    stream: &mut tokio::net::UnixStream,
    services: &HostServices,
) -> Result<(), capnp::Error> {
    if !has_collab_launch_marker(stream).await? {
        return Ok(());
    }
    let credentials = stream.peer_cred().map_err(|error| {
        capnp::Error::failed(format!(
            "could not read collaboration peer credentials: {error}"
        ))
    })?;
    // A socket pathname is not authority. The kernel must identify a process
    // from this user before it can attest the compositor's launch record.
    if credentials.uid() != unsafe { libc::geteuid() } {
        return Err(capnp::Error::failed(
            "collaboration service rejected a peer from another user".into(),
        ));
    }
    let pid = credentials
        .pid()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
        .ok_or_else(|| capnp::Error::failed("collaboration peer has no usable pid".into()))?;

    let mut marker = [0_u8; collab_types::local_transport::COLLAB_LAUNCH_MAGIC.len()];
    stream.read_exact(&mut marker).await.map_err(|error| {
        capnp::Error::failed(format!(
            "could not read collaboration launch marker: {error}"
        ))
    })?;
    if marker != collab_types::local_transport::COLLAB_LAUNCH_MAGIC {
        return Err(capnp::Error::failed(
            "collaboration launch marker changed while being read".into(),
        ));
    }
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length).await.map_err(|error| {
        capnp::Error::failed(format!(
            "could not read collaboration launch handoff length: {error}"
        ))
    })?;
    let length = usize::try_from(u32::from_le_bytes(length)).map_err(|_| {
        capnp::Error::failed("collaboration launch handoff length overflows usize".into())
    })?;
    if length == 0 || length > MAX_COLLAB_LAUNCH_HANDOFF_BYTES {
        return Err(capnp::Error::failed(
            "collaboration launch handoff has an invalid length".into(),
        ));
    }
    let mut bytes = vec![0_u8; length];
    stream.read_exact(&mut bytes).await.map_err(|error| {
        capnp::Error::failed(format!(
            "could not read collaboration launch handoff: {error}"
        ))
    })?;
    let handoff: CollabLaunchHandoff = serde_json::from_slice(&bytes).map_err(|error| {
        capnp::Error::failed(format!("invalid collaboration launch handoff: {error}"))
    })?;
    if handoff.protocol != collab_types::local_transport::COLLAB_LAUNCH_PROTOCOL_VERSION
        || handoff.app_id.trim().is_empty()
        || handoff.workspace_id.trim().is_empty()
    {
        return Err(capnp::Error::failed(
            "collaboration launch handoff has an unsupported or empty identity".into(),
        ));
    }

    // `spawn_launch` records immediately after `Command::spawn`; the child
    // can connect first on a fast machine. Retry only this narrow registration
    // race. A direct qos-launcher command never obtains a record and fails
    // closed instead of becoming a way around admin/admission checks.
    for attempt in 0..=LAUNCH_RECORD_RACE_RETRIES {
        if services.launcher.as_ref().is_some_and(|service| {
            service.attest_process(pid, &handoff.app_id, &handoff.workspace_id)
        }) {
            return Ok(());
        }
        if attempt < LAUNCH_RECORD_RACE_RETRIES {
            tokio::time::sleep(LAUNCH_RECORD_RACE_DELAY).await;
        }
    }
    Err(capnp::Error::failed(
        "collaboration launch is not an authenticated shell-managed process".into(),
    ))
}

#[allow(
    clippy::future_not_send,
    reason = "Cap'n Proto RPC and its action-directory registry are Rc-backed and intentionally run only on the broker LocalSet"
)]
async fn drive_connection(
    stream: tokio::net::UnixStream,
    state: Arc<CapnpBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
    peer_id: u64,
    worker: Option<WorkerBinding>,
) -> Result<(), capnp::Error> {
    let _exit = ConnectionExit {
        state: Arc::clone(&state),
        directories: Rc::clone(&directories),
        peer_id,
    };
    let mut stream = stream;
    if worker.is_none() {
        authenticate_launcher_handoff(&mut stream, &state.services).await?;
    }
    let (reader, writer) = stream.into_split();
    let reader = reader.compat();
    let writer = writer.compat_write();
    let network = Box::new(twoparty::VatNetwork::new(
        futures::io::BufReader::new(reader),
        futures::io::BufWriter::new(writer),
        rpc_twoparty_capnp::Side::Server,
        capnp::message::ReaderOptions::default(),
    ));
    let lease = Rc::new(ConnectionLease {
        state: Arc::clone(&state),
        directories: Rc::clone(&directories),
        peer_id,
    });
    let service = CapnpCollabService {
        worker,
        state,
        directories,
        peer_id,
        lease,
    };
    let client: collab_service::Client = capnp_rpc::new_client(service);
    let rpc = RpcSystem::new(network, Some(client.client));
    rpc.await
}

#[cfg(any(test, feature = "test-support"))]
// Test helpers throughout this module pair `_sk`/`_pk` (secret/public
// key) and `owner`/`editor` role names — the clearest naming for this
// domain, and renaming would obscure the keypair relationship rather
// than clarify it.
#[allow(
    clippy::similar_names,
    reason = "sk/pk and owner/editor pairings are the conventional, intentional names for a keypair and a two-role fixture"
)]
pub mod tests {
    use super::*;
    #[cfg(test)]
    use crate::test_support::WorkspaceTree;
    use crate::test_support::WorkspaceTreeHandle;
    use std::cell::RefCell;
    #[cfg(test)]
    use std::os::unix::fs::PermissionsExt;
    use std::rc::Rc;
    #[cfg(test)]
    use std::time::SystemTime;

    #[cfg(test)]
    use crate::collab_outbound::recording::{RecordingSink, Sent};
    #[cfg(test)]
    use collab_types::{CapOverride, Ed25519PublicKey, Role, Subject, sign_op};
    #[cfg(test)]
    use ed25519_dalek::Signer as _;
    #[cfg(test)]
    use ed25519_dalek::SigningKey;

    #[cfg(test)]
    fn lifecycle_socket(directory: &tempfile::TempDir, name: &str) -> PathBuf {
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
            .expect("secure temporary broker directory");
        directory.path().join(format!("{name}.sock"))
    }

    #[test]
    fn socket_parent_must_be_owner_only_before_any_artifact_is_created() {
        let directory = tempfile::tempdir().expect("temporary socket directory");
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755))
            .expect("make parent deliberately non-private");
        let path = directory.path().join("rejected.sock");

        let error = bind_owned_socket(&path)
            .err()
            .expect("non-private parent must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(
            std::fs::read_dir(directory.path())
                .expect("inspect rejected parent")
                .next()
                .is_none(),
            "parent rejection must precede socket, lease, or staging artifacts"
        );
    }

    #[test]
    fn socket_custody_reclaims_only_stale_owned_inodes() {
        let directory = tempfile::tempdir().expect("temporary socket directory");
        let live_path = lifecycle_socket(&directory, "live");
        let live = std::os::unix::net::UnixListener::bind(&live_path).expect("live socket");
        let error = bind_owned_socket(&live_path)
            .err()
            .expect("live broker socket must not be reclaimed");
        assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
        assert!(socket_identity(&live_path).is_ok());
        drop(live);

        let stale_path = lifecycle_socket(&directory, "stale");
        let stale = std::os::unix::net::UnixListener::bind(&stale_path).expect("stale socket");
        drop(stale);
        let (replacement, custody) =
            bind_owned_socket(&stale_path).expect("proven stale socket is reclaimed");
        assert!(std::os::unix::net::UnixStream::connect(&stale_path).is_ok());
        let contender_error = bind_owned_socket(&stale_path)
            .err()
            .expect("lifetime lease excludes a competing socket owner");
        assert_eq!(contender_error.kind(), std::io::ErrorKind::AddrInUse);
        drop(replacement);
        drop(custody);
        assert!(
            !stale_path.exists(),
            "owned socket inode is removed on final custody release"
        );

        let replaced_path = lifecycle_socket(&directory, "replacement");
        let original =
            std::os::unix::net::UnixListener::bind(&replaced_path).expect("original socket");
        let original_identity = socket_identity(&replaced_path).expect("original identity");
        // Retain the old inode while replacing its pathname: unlinking its last
        // name would let the filesystem reuse that inode for the new socket.
        let displaced_path = lifecycle_socket(&directory, "displaced");
        std::fs::rename(&replaced_path, &displaced_path).expect("retain original socket inode");
        assert_eq!(
            socket_identity(&displaced_path).expect("retained original identity"),
            original_identity
        );
        let replacement =
            std::os::unix::net::UnixListener::bind(&replaced_path).expect("replacement socket");
        assert_ne!(
            socket_identity(&replaced_path).expect("replacement identity"),
            original_identity,
            "the replacement fixture must exercise a distinct live inode"
        );
        assert!(
            !remove_socket_if_owned(&replaced_path, original_identity)
                .expect("replacement identity check"),
            "an old owner must not unlink a replacement socket"
        );
        assert!(std::os::unix::net::UnixStream::connect(&replaced_path).is_ok());
        drop(replacement);
        drop(original);
        std::fs::remove_file(&displaced_path).expect("remove retained original pathname");

        let racing_path = lifecycle_socket(&directory, "publish-race");
        let (staged, stage) = bind_socket_stage(&racing_path).expect("private socket staging");
        let racing =
            std::os::unix::net::UnixListener::bind(&racing_path).expect("racing destination");
        let racing_identity = socket_identity(&racing_path).expect("racing socket identity");
        let error = rename_socket_noreplace(&stage.socket, &racing_path)
            .expect_err("no-replace publication must preserve a racing destination");
        assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
        assert_eq!(
            socket_identity(&racing_path).expect("racing socket survives"),
            racing_identity
        );
        assert!(std::os::unix::net::UnixStream::connect(&racing_path).is_ok());
        drop(staged);
        drop(stage);
        assert_eq!(
            socket_identity(&racing_path).expect("stage cleanup preserves destination"),
            racing_identity
        );
        drop(racing);

        let entries_before_failure = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>();
        let identity_failure_path = lifecycle_socket(&directory, "identity-failure");
        FAIL_STAGED_SOCKET_IDENTITY.with(|fail| fail.set(true));
        assert!(bind_socket_stage(&identity_failure_path).is_err());
        let entries_after_failure = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            entries_after_failure, entries_before_failure,
            "staged metadata failure leaves no socket artifact"
        );

        #[cfg(target_os = "macos")]
        {
            let parent_len = directory.path().as_os_str().as_bytes().len();
            let separator = usize::from(!directory.path().as_os_str().is_empty());
            let leaf_len = 103usize
                .checked_sub(parent_len + separator)
                .expect("temporary path leaves room for a valid Unix socket");
            let long_path = directory.path().join("x".repeat(leaf_len));
            assert_eq!(long_path.as_os_str().as_bytes().len(), 103);
            let (listener, custody) = bind_owned_socket(&long_path)
                .expect("valid near-limit final path must stage without expansion");
            assert!(std::os::unix::net::UnixStream::connect(&long_path).is_ok());
            drop(listener);
            drop(custody);
        }
    }

    #[cfg(test)]
    struct LifecycleAuthority;

    #[cfg(test)]
    impl crate::peer_actions::CertificateAuthority for LifecycleAuthority {
        fn authorize(
            &self,
            _: uuid::Uuid,
            _: &tilo_user_identity::DeviceCert,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    #[cfg(test)]
    struct LifecycleSink;

    #[cfg(test)]
    impl crate::collab_outbound::PeerPublishSink for LifecycleSink {
        fn app_document(&self, _: String, _: String, _: String, _: collab_types::SignedOp) {}

        fn app_presence(&self, _: String, _: String, _: collab_types::PeerPresence) {}

        fn scrollback_request(&self, _: ArtifactId, _: u64, _: u16) {}
    }

    #[cfg(test)]
    struct HealthyOwner;

    #[cfg(test)]
    impl qos_peer_state::owner_commit::OwnerCommitService for HealthyOwner {
        fn commit(
            &self,
            _: qos_peer_state::owner_commit::OwnerCommitRequest,
        ) -> qos_peer_state::owner_commit::CommitFuture {
            Box::pin(async { Err("unused lifecycle commit".into()) })
        }

        fn health(&self) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn stopped_state_releases_journal_and_makes_surviving_facade_inert() {
        let directory = tempfile::tempdir().expect("temporary journal directory");
        let fanout = Arc::new(FanoutState::new());
        let config = crate::peer_actions::NativePeerBrokerConfig {
            journal_root: directory.path().join("invocations"),
            certificate_authority: Arc::new(LifecycleAuthority),
        };
        let (state, _bridge_rx) = CapnpBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::with_owner_commit(Arc::new(LifecycleSink), Arc::new(HealthyOwner)),
            Some(WorkspaceTree::default().shared().authority()),
            None,
            Some(&config),
            HostServices::default(),
        );
        let facade = crate::collab_broker::AppActionBroker::from_state_for_test(Arc::clone(&state));
        assert!(
            facade.peer_action_attachment().is_ok(),
            "live facade sees the opened invocation journal"
        );
        let state_for_poison = Arc::clone(&state);
        assert!(
            std::thread::spawn(move || {
                let _journal = state_for_poison.journal.lock().expect("journal lock");
                panic!("poison journal lock for shutdown recovery");
            })
            .join()
            .is_err()
        );

        state.stop();

        assert!(state.is_stopped());
        assert!(facade.peer_action_attachment().is_err());
        assert!(facade.surfaces().is_empty());
        assert!(facade.statuses("owner").is_empty());
        let reopened = InvocationJournal::open(&config.journal_root)
            .expect("stopped broker releases the invocation-journal writer");
        drop(reopened);
    }

    #[test]
    fn admitted_live_operation_keeps_its_identity_when_shutdown_races_after_effect() {
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            None,
            None,
        );
        let state_for_operation = Arc::clone(&state);
        let (effect_tx, effect_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let operation = std::thread::spawn(move || {
            state_for_operation.while_live(|_| {
                effect_tx.send(()).expect("effect admitted");
                release_rx.recv().expect("release admitted operation");
                Ok(71_u64)
            })
        });
        effect_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("operation reached its admitted effect");
        state.request_stop();
        release_tx.send(()).expect("release operation");
        assert_eq!(
            operation.join().expect("operation thread"),
            Ok(71),
            "an operation admitted before shutdown keeps its non-retryable identity"
        );
        state.stop();
    }

    #[test]
    fn public_sink_can_drop_its_broker_without_self_join_or_early_unlink() {
        let directory = tempfile::tempdir().expect("temporary socket directory");
        let socket = lifecycle_socket(&directory, "self-drop");
        let fanout = Arc::new(FanoutState::new());
        let config = crate::peer_actions::NativePeerBrokerConfig {
            journal_root: directory.path().join("self-drop-invocations"),
            certificate_authority: Arc::new(LifecycleAuthority),
        };
        let broker = crate::collab_broker::spawn_with_test_services(
            socket.clone(),
            None,
            PeerPublisher::disabled(),
            Some(&config),
            HostServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::clone(&fanout),
        )
        .expect("test broker");
        let surviving_facade = broker.app_action_broker().expect("live broker facade");
        let state = fanout.capnp_state().expect("attached broker state");
        let holder = Arc::new(Mutex::new(Some(broker)));
        let holder_for_sink = Arc::clone(&holder);
        let socket_for_sink = socket.clone();
        let (sink_done, sink_finished) = std::sync::mpsc::sync_channel(1);
        let subscription = holder
            .lock()
            .expect("broker holder")
            .as_ref()
            .expect("broker")
            .subscribe_rooms(
                vec!["self-drop-room".into()],
                Box::new(move |_, _| {
                    let broker = holder_for_sink
                        .lock()
                        .expect("broker holder in sink")
                        .take()
                        .expect("broker dropped once");
                    drop(broker);
                    assert!(
                        std::os::unix::net::UnixListener::bind(&socket_for_sink).is_err(),
                        "self-drop keeps the owned pathname until this callback returns"
                    );
                    sink_done.send(()).expect("signal sink completion");
                }),
            );
        let fanout_for_callback = Arc::clone(&fanout);
        state.enqueue_thread_callback(Box::new(move || {
            fanout_for_callback.fanout_to_route(
                &CollabEvent::FollowChanged { peer_id: None },
                u64::MAX,
                &CrdtRoute::cluster("self-drop-room"),
            );
        }));
        sink_finished
            .recv_timeout(Duration::from_secs(2))
            .expect("broker-thread public sink completed without self-join");
        drop(subscription);

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while socket.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            surviving_facade.peer_action_attachment().is_err(),
            "facade retained across thread exit is inert"
        );
        let reopened_journal = InvocationJournal::open(&config.journal_root)
            .expect("thread exit releases the journal before unlink");
        drop(reopened_journal);
        fanout.fanout_to_route(
            &CollabEvent::FollowChanged {
                peer_id: Some("stale".into()),
            },
            u64::MAX,
            &CrdtRoute::cluster("self-drop-room"),
        );
        let replacement = crate::collab_broker::spawn_with_test_services(
            socket,
            None,
            PeerPublisher::disabled(),
            Some(&config),
            HostServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        )
        .expect("broker restarts after self-drop thread exits");
        drop(replacement);
    }

    #[test]
    fn off_thread_public_sink_can_drop_broker_without_join_deadlock() {
        let directory = tempfile::tempdir().expect("temporary socket directory");
        let socket = lifecycle_socket(&directory, "off-thread-drop");
        let fanout = Arc::new(FanoutState::new());
        let broker = crate::collab_broker::spawn_with_test_services(
            socket.clone(),
            None,
            PeerPublisher::disabled(),
            None,
            HostServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::clone(&fanout),
        )
        .expect("test broker");
        let holder = Arc::new(Mutex::new(Some(broker)));
        let holder_for_sink = Arc::clone(&holder);
        let (sink_done, sink_finished) = std::sync::mpsc::sync_channel(1);
        let subscription = holder
            .lock()
            .expect("broker holder")
            .as_ref()
            .expect("broker")
            .subscribe_rooms(
                vec!["off-thread-room".into()],
                Box::new(move |_, _| {
                    let broker = holder_for_sink
                        .lock()
                        .expect("broker holder in sink")
                        .take()
                        .expect("broker dropped once");
                    drop(broker);
                    sink_done.send(()).expect("signal sink completion");
                }),
            );
        let fanout_for_thread = Arc::clone(&fanout);
        let caller = std::thread::spawn(move || {
            fanout_for_thread.fanout_to_route(
                &CollabEvent::FollowChanged { peer_id: None },
                u64::MAX,
                &CrdtRoute::cluster("off-thread-room"),
            );
        });
        sink_finished
            .recv_timeout(Duration::from_secs(2))
            .expect("off-thread sink drop joined broker without deadlock");
        caller.join().expect("off-thread fanout caller");
        drop(subscription);

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while socket.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!socket.exists());
    }

    #[cfg(test)]
    struct DropBrokerOnHealth {
        broker: Arc<Mutex<Option<crate::collab_broker::BrokerHandle>>>,
    }

    #[cfg(test)]
    impl qos_peer_state::owner_commit::OwnerCommitService for DropBrokerOnHealth {
        fn commit(
            &self,
            _: qos_peer_state::owner_commit::OwnerCommitRequest,
        ) -> qos_peer_state::owner_commit::CommitFuture {
            Box::pin(async { Err("unused lifecycle commit".into()) })
        }

        fn health(&self) -> Result<(), String> {
            drop(
                self.broker
                    .lock()
                    .map_err(|_| "broker holder unavailable")?
                    .take(),
            );
            Ok(())
        }
    }

    #[test]
    fn owner_health_callback_can_drop_broker_without_facade_join_deadlock() {
        let directory = tempfile::tempdir().expect("temporary broker directory");
        let socket = lifecycle_socket(&directory, "owner-health-drop");
        let journal_root = directory.path().join("owner-health-invocations");
        let holder = Arc::new(Mutex::new(None));
        let publisher = PeerPublisher::with_owner_commit(
            Arc::new(LifecycleSink),
            Arc::new(DropBrokerOnHealth {
                broker: Arc::clone(&holder),
            }),
        );
        let membership = WorkspaceTree::default().shared();
        let config = crate::peer_actions::NativePeerBrokerConfig {
            journal_root: journal_root.clone(),
            certificate_authority: Arc::new(LifecycleAuthority),
        };
        let broker = crate::collab_broker::spawn_with_test_services(
            socket.clone(),
            Some(membership.authority()),
            publisher,
            Some(&config),
            HostServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        )
        .expect("test broker");
        let facade = broker.app_action_broker().expect("live broker facade");
        *holder.lock().expect("broker holder") = Some(broker);
        let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            result_tx
                .send(facade.peer_action_attachment())
                .expect("send facade result");
        });
        let result = result_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("owner health callback returned without join deadlock");
        assert!(
            result.is_err(),
            "shutdown requested by the health callback fences its facade result"
        );

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while socket.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!socket.exists());
        let reopened = InvocationJournal::open(&journal_root)
            .expect("health callback shutdown releases journal");
        drop(reopened);
    }

    #[cfg(test)]
    struct DropOtherBrokerOnHealth {
        broker: Arc<Mutex<Option<crate::collab_broker::BrokerHandle>>>,
        socket: PathBuf,
        journal_root: PathBuf,
        proof: Arc<Mutex<Option<Result<(), String>>>>,
    }

    #[cfg(test)]
    impl qos_peer_state::owner_commit::OwnerCommitService for DropOtherBrokerOnHealth {
        fn commit(
            &self,
            _: qos_peer_state::owner_commit::OwnerCommitRequest,
        ) -> qos_peer_state::owner_commit::CommitFuture {
            Box::pin(async { Err("unused lifecycle commit".into()) })
        }

        fn health(&self) -> Result<(), String> {
            drop(
                self.broker
                    .lock()
                    .map_err(|_| "other broker holder unavailable")?
                    .take(),
            );
            let result = if self.socket.exists() {
                Err("unrelated broker drop returned before socket cleanup".into())
            } else {
                InvocationJournal::open(&self.journal_root)
                    .map(drop)
                    .map_err(|error| format!("unrelated broker journal remained locked: {error}"))
            };
            *self
                .proof
                .lock()
                .map_err(|_| "other broker proof unavailable")? = Some(result);
            Ok(())
        }
    }

    #[test]
    fn facade_guard_does_not_detach_an_unrelated_broker_drop() {
        let directory = tempfile::tempdir().expect("temporary broker directory");
        let other_socket = lifecycle_socket(&directory, "other");
        let other_journal = directory.path().join("other-invocations");
        let other_config = crate::peer_actions::NativePeerBrokerConfig {
            journal_root: other_journal.clone(),
            certificate_authority: Arc::new(LifecycleAuthority),
        };
        let other = crate::collab_broker::spawn_with_test_services(
            other_socket.clone(),
            Some(WorkspaceTree::default().shared().authority()),
            PeerPublisher::with_owner_commit(Arc::new(LifecycleSink), Arc::new(HealthyOwner)),
            Some(&other_config),
            HostServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        )
        .expect("other broker");
        let other_holder = Arc::new(Mutex::new(Some(other)));
        let proof = Arc::new(Mutex::new(None));

        let broker_socket = lifecycle_socket(&directory, "guarded");
        let broker_config = crate::peer_actions::NativePeerBrokerConfig {
            journal_root: directory.path().join("guarded-invocations"),
            certificate_authority: Arc::new(LifecycleAuthority),
        };
        let broker = crate::collab_broker::spawn_with_test_services(
            broker_socket,
            Some(WorkspaceTree::default().shared().authority()),
            PeerPublisher::with_owner_commit(
                Arc::new(LifecycleSink),
                Arc::new(DropOtherBrokerOnHealth {
                    broker: Arc::clone(&other_holder),
                    socket: other_socket,
                    journal_root: other_journal,
                    proof: Arc::clone(&proof),
                }),
            ),
            Some(&broker_config),
            HostServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        )
        .expect("guarded broker");
        let facade = broker.app_action_broker().expect("guarded broker facade");

        facade
            .peer_action_attachment()
            .expect("unrelated broker drop does not fence this broker");
        proof
            .lock()
            .expect("other broker proof")
            .take()
            .expect("health callback recorded cleanup")
            .expect("unrelated broker drop synchronously released resources");
        drop(broker);
    }

    #[test]
    fn broker_thread_unwind_releases_journal_and_owned_socket() {
        let directory = tempfile::tempdir().expect("temporary broker directory");
        let socket = lifecycle_socket(&directory, "unwind");
        let config = crate::peer_actions::NativePeerBrokerConfig {
            journal_root: directory.path().join("unwind-invocations"),
            certificate_authority: Arc::new(LifecycleAuthority),
        };
        let fanout = Arc::new(FanoutState::new());
        let broker = crate::collab_broker::spawn_with_test_services(
            socket.clone(),
            None,
            PeerPublisher::disabled(),
            Some(&config),
            HostServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::clone(&fanout),
        )
        .expect("test broker");
        let facade = broker.app_action_broker().expect("live broker facade");
        fanout
            .capnp_state()
            .expect("attached broker state")
            .enqueue_thread_callback(Box::new(|| panic!("broker-thread unwind fixture")));

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while socket.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!socket.exists(), "unwind releases owned socket custody");
        assert!(
            facade.peer_action_attachment().is_err(),
            "unwind makes surviving broker facades inert"
        );
        let reopened = InvocationJournal::open(&config.journal_root)
            .expect("unwind releases the invocation journal");
        drop(reopened);
        drop(broker);

        let replacement = crate::collab_broker::spawn_with_test_services(
            socket,
            None,
            PeerPublisher::disabled(),
            Some(&config),
            HostServices::default(),
            None,
            Arc::new(Mutex::new(BrokerChannels::default())),
            Arc::new(FanoutState::new()),
        )
        .expect("broker restarts after broker-thread unwind");
        drop(replacement);
    }

    #[test]
    fn connection_exit_cleanup_is_idempotent_and_preserves_survivors_and_replacements() {
        let fixture = local_directory_fixture();
        let survivor = fixture.peer_id + 1;
        let replacement = survivor + 1;
        let survivor_root = ActionDirectoryRoot {
            peer_id: survivor,
            app_id: fixture.route.app_id.clone(),
            cluster_id: fixture.cluster_id.clone(),
            record_id: "survivor".into(),
        };
        fixture
            .state
            .action_directory_roots
            .lock()
            .expect("directory roots")
            .insert(survivor, survivor_root.clone());
        fixture.directories.directories.borrow_mut().insert(
            survivor,
            RegisteredActionDirectory {
                app_id: fixture.route.app_id.clone(),
                route: fixture.route.clone(),
                directory: fixture.directory.clone(),
            },
        );
        for peer_id in [fixture.peer_id, survivor] {
            fixture
                .state
                .surfaces
                .lock()
                .expect("published surfaces")
                .insert(
                    peer_id,
                    PublishedSurface {
                        peer_id,
                        app_id: fixture.route.app_id.clone(),
                        route: fixture.route.clone(),
                        actions: Vec::new(),
                        live: true,
                    },
                );
        }

        drop(ConnectionExit {
            state: Arc::clone(&fixture.state),
            directories: Rc::clone(&fixture.directories),
            peer_id: fixture.peer_id,
        });
        drop(ConnectionLease {
            state: Arc::clone(&fixture.state),
            directories: Rc::clone(&fixture.directories),
            peer_id: fixture.peer_id,
        });

        assert!(
            !fixture
                .state
                .surfaces
                .lock()
                .expect("published surfaces")
                .contains_key(&fixture.peer_id)
        );
        assert!(
            !fixture
                .directories
                .directories
                .borrow()
                .contains_key(&fixture.peer_id)
        );
        assert!(
            fixture
                .state
                .surfaces
                .lock()
                .expect("published surfaces")
                .contains_key(&survivor)
        );
        assert!(
            fixture
                .directories
                .directories
                .borrow()
                .contains_key(&survivor)
        );

        fixture
            .state
            .action_directory_roots
            .lock()
            .expect("directory roots")
            .insert(
                replacement,
                ActionDirectoryRoot {
                    peer_id: replacement,
                    app_id: fixture.route.app_id.clone(),
                    cluster_id: fixture.cluster_id.clone(),
                    record_id: "replacement".into(),
                },
            );
        fixture.state.refresh_action_directory_root_snapshot();
        let peer_ids = fixture
            .state
            .action_directory_roots()
            .into_iter()
            .map(|root| root.peer_id)
            .collect::<std::collections::HashSet<_>>();
        assert!(!peer_ids.contains(&fixture.peer_id));
        assert!(peer_ids.contains(&survivor));
        assert!(peer_ids.contains(&replacement));
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum DirectoryResolveMode {
        Found,
        Stale,
        NotFound,
    }

    /// A real `Cap'n Proto` directory endpoint retained by the `LocalSet` test
    /// below. It answers only bounded inert rows, but is deliberately a
    /// capability client rather than a mock of the shell bridge.
    pub struct TestActionDirectory {
        revision: String,
        pub resolve_mode: DirectoryResolveMode,
        result: tilo_actions::ResultSpec,
        confirmation_required: bool,
        query_count: usize,
        resolve_count: usize,
    }

    impl TestActionDirectory {
        fn contract_json(&self) -> String {
            let mut contract = tilo_actions::ActionContract::view();
            contract.result = self.result.clone();
            if self.confirmation_required {
                contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
            }
            serde_json::to_string(&contract).expect("test action contract")
        }

        fn row(&self) -> runtime_collab::ActionDirectoryRow {
            let contract_json = self.contract_json();
            let descriptor = runtime_collab::ExportedAction {
                resource: "workspace:atlas".into(),
                action: "open".into(),
                resource_kind: "workspace".into(),
                resource_label: "Atlas".into(),
                action_label: "Open Atlas".into(),
                description: Some("Open the Atlas workspace".into()),
                operation_kind: "view".into(),
                params_json: "[]".into(),
                contract_json: contract_json.clone(),
                input: runtime_collab::ExportedInput::None,
            };
            runtime_collab::ActionDirectoryRow {
                parent_resource: String::new(),
                resource: descriptor.resource.clone(),
                resource_kind: descriptor.resource_kind.clone(),
                resource_label: descriptor.resource_label.clone(),
                resource_presentation_json: String::new(),
                action: Some(descriptor.action.clone()),
                descriptor: Some(descriptor),
                has_children: false,
                visibility_action: Some("open".into()),
                visibility_contract_json: contract_json,
            }
        }
    }

    struct TestActionDirectoryServer {
        state: Rc<RefCell<TestActionDirectory>>,
    }

    #[allow(
        refining_impl_trait,
        reason = "the generated Cap'n Proto test endpoint has the same concrete Promise shape as production endpoints"
    )]
    impl action_directory::Server for TestActionDirectoryServer {
        fn query(
            self: Rc<Self>,
            _params: action_directory::QueryParams,
            mut results: action_directory::QueryResults,
        ) -> Promise<(), capnp::Error> {
            let mut state = self.state.borrow_mut();
            state.query_count = state.query_count.saturating_add(1);
            let page = runtime_collab::ActionDirectoryPage {
                revision: state.revision.clone(),
                rows: vec![state.row()],
                next_cursor: Vec::new(),
                exhausted: true,
                stale_cursor: false,
                budget_exhausted: false,
            };
            write_directory_page(results.get().init_page(), &page)
                .expect("test directory page encodes");
            Promise::ok(())
        }

        fn resolve(
            self: Rc<Self>,
            _params: action_directory::ResolveParams,
            mut results: action_directory::ResolveResults,
        ) -> Promise<(), capnp::Error> {
            let mut state = self.state.borrow_mut();
            state.resolve_count = state.resolve_count.saturating_add(1);
            let mut out = results.get().init_resolution();
            match state.resolve_mode {
                DirectoryResolveMode::Found => {
                    write_directory_row(out.reborrow().init_found(), &state.row());
                }
                DirectoryResolveMode::Stale => out.set_stale_revision(()),
                DirectoryResolveMode::NotFound => out.set_not_found(()),
            }
            Promise::ok(())
        }
    }

    #[cfg(test)]
    include!("owner_commit_tests.rs");

    pub struct LocalDirectoryFixture {
        _fanout: Arc<FanoutState>,
        state: Arc<CapnpBrokerState>,
        directories: Rc<LocalActionDirectoryRegistry>,
        directory: action_directory::Client,
        directory_state: Rc<RefCell<TestActionDirectory>>,
        pub broker: crate::collab_broker::AppActionBroker,
        pub membership: Option<WorkspaceTreeHandle>,
        pub cluster_id: String,
        pub route: CrdtRoute,
        pub peer_id: u64,
        invocation_rx: tokio::sync::mpsc::UnboundedReceiver<InvocationEnvelope>,
    }

    impl LocalDirectoryFixture {
        pub fn query_count(&self) -> usize {
            self.directory_state.borrow().query_count
        }
        pub fn disconnect_directory(&self) {
            self.directories.remove(self.peer_id);
        }
        pub fn reconnect_directory(&self) {
            self.directories.directories.borrow_mut().insert(
                self.peer_id,
                RegisteredActionDirectory {
                    app_id: self.route.app_id.clone(),
                    route: self.route.clone(),
                    directory: self.directory.clone(),
                },
            );
        }

        pub fn set_confirmation_required(&self, required: bool) {
            self.directory_state.borrow_mut().confirmation_required = required;
        }

        pub fn set_result_spec(&self, result: tilo_actions::ResultSpec) {
            self.directory_state.borrow_mut().result = result;
        }

        fn invocation_channel(&self) -> channel_handle::Client {
            capnp_rpc::new_client(CapnpChannelHandle {
                state: Arc::clone(&self.state),
                directories: Rc::clone(&self.directories),
                channel_id: self.route.channel_key(),
                peer_id: self.peer_id,
                verified_identity: AdmittedJoinIdentity {
                    certificate: None,
                    verified: VerifiedJoinIdentity {
                        user_public_key: [0; 32],
                        device_public_key: [0; 32],
                    },
                    principal_id: "fixture-application".into(),
                    display_handle: None,
                    role: CapRole::Editor,
                    agent_id: None,
                },
                lease: None,
            })
        }

        /// Exercise the production peer-bound update RPC, including result-contract validation.
        pub async fn complete_invocation(
            &self,
            id: u64,
            update: &tilo_actions::InvocationUpdate,
        ) -> Result<(), capnp::Error> {
            let client = self.invocation_channel();
            let mut request = client.update_invocation_request();
            request.get().set_id(id);
            request
                .get()
                .set_update_json(&serde_json::to_string(update).expect("canonical update"));
            request.send().promise.await?;
            Ok(())
        }

        pub fn set_resolve_mode(&self, mode: DirectoryResolveMode) {
            self.directory_state.borrow_mut().resolve_mode = mode;
        }

        pub fn take_invocation_actor(&mut self) -> Option<String> {
            self.invocation_rx
                .try_recv()
                .ok()
                .map(|invocation| invocation.actor)
        }

        pub fn take_invocation_argument(&mut self) -> Option<Option<String>> {
            self.invocation_rx
                .try_recv()
                .ok()
                .map(|invocation| invocation.argument)
        }

        pub fn resolve_count(&self) -> usize {
            self.directory_state.borrow().resolve_count
        }
    }

    #[cfg(test)]
    fn local_directory_fixture() -> LocalDirectoryFixture {
        let mut tree = WorkspaceTree::default();
        let mut workspace = crate::test_support::Workspace::test_default(
            crate::test_support::WorkspaceId(71),
            "Directory test room",
        );
        workspace.owner = Some("fixture-owner".to_string());
        tree.workspaces.push(workspace);
        tree.set_active(crate::test_support::WorkspaceId(71));
        local_directory_fixture_with_membership(tree.shared())
    }

    pub fn local_directory_fixture_with_membership(
        membership: crate::test_support::WorkspaceTreeHandle,
    ) -> LocalDirectoryFixture {
        local_directory_fixture_with_peer(membership, Some(71))
    }

    pub fn local_directory_fixture_with_first_allocated_peer(
        membership: crate::test_support::WorkspaceTreeHandle,
    ) -> LocalDirectoryFixture {
        local_directory_fixture_with_peer(membership, None)
    }

    fn local_directory_fixture_with_peer(
        membership: crate::test_support::WorkspaceTreeHandle,
        peer_id: Option<u64>,
    ) -> LocalDirectoryFixture {
        local_directory_fixture_with_devices(membership, peer_id, None)
    }

    fn local_directory_fixture_with_devices(
        membership: WorkspaceTreeHandle,
        peer_id: Option<u64>,
        trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
    ) -> LocalDirectoryFixture {
        let mut fixture = local_directory_fixture_with_authority(
            membership.authority(),
            membership.services(),
            peer_id,
            trusted_devices,
        );
        fixture.membership = Some(membership);
        fixture
    }

    pub fn local_directory_fixture_with_authority(
        membership: crate::services::WorkspaceAuthorityHandle,
        services: HostServices,
        peer_id: Option<u64>,
        trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
    ) -> LocalDirectoryFixture {
        let cluster_id = membership
            .snapshot()
            .unwrap()
            .active_workspace()
            .expect("fixture active room")
            .room
            .to_string();
        let route = CrdtRoute::record("dev.qos.atlas", cluster_id.clone(), "atlas");
        let fanout = Arc::new(FanoutState::new());
        let (state, bridge_rx) = CapnpBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(membership.clone()),
            trusted_devices,
            None,
            services,
        );
        let peer_id = peer_id.unwrap_or_else(|| state.next_peer_id());
        let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge_rx));
        let directory_state = Rc::new(RefCell::new(TestActionDirectory {
            revision: "directory-r1".into(),
            resolve_mode: DirectoryResolveMode::Found,
            result: tilo_actions::ResultSpec::None,
            confirmation_required: false,
            query_count: 0,
            resolve_count: 0,
        }));
        let directory: action_directory::Client =
            capnp_rpc::new_client(TestActionDirectoryServer {
                state: Rc::clone(&directory_state),
            });
        directories.directories.borrow_mut().insert(
            peer_id,
            RegisteredActionDirectory {
                app_id: route.app_id.clone(),
                route: route.clone(),
                directory: directory.clone(),
            },
        );
        state
            .action_directory_roots
            .lock()
            .expect("directory roots")
            .insert(
                peer_id,
                ActionDirectoryRoot {
                    peer_id,
                    app_id: route.app_id.clone(),
                    cluster_id: cluster_id.clone(),
                    record_id: route.record_id.clone(),
                },
            );
        state.refresh_action_directory_root_snapshot();
        let (invocation_tx, invocation_rx) = tokio::sync::mpsc::unbounded_channel();
        state
            .invocation_subscribers
            .lock()
            .expect("invocation subscribers")
            .insert(peer_id, invocation_tx);
        fanout.attach_capnp(Arc::clone(&state));
        LocalDirectoryFixture {
            _fanout: fanout,
            broker: crate::collab_broker::AppActionBroker::from_state_for_test(Arc::clone(&state)),
            membership: None,
            state,
            directories,
            directory,
            directory_state,
            cluster_id,
            route,
            peer_id,
            invocation_rx,
        }
    }

    #[allow(
        clippy::future_not_send,
        reason = "the directory bridge is deliberately driven on the LocalSet because its Cap'n Proto state contains Rc values"
    )]
    pub async fn drain_directory_bridge(fixture: &LocalDirectoryFixture) {
        fixture
            .state
            .drain_action_directory_bridge(&fixture.directories);
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
    }

    #[test]
    fn directory_queue_refuses_oversized_query_text_and_cursor_before_contacting_the_app() {
        let fixture = local_directory_fixture();
        let queue = fixture.broker.action_directory_request_queue();
        let mut oversized_text = runtime_collab::ActionDirectoryQuery {
            peer_id: fixture.peer_id,
            revision: String::new(),
            cursor: Vec::new(),
            needle: "x".repeat(DIRECTORY_MAX_QUERY_NEEDLE_BYTES.saturating_add(1)),
            parent_resource: String::new(),
            max_results: 1,
            max_bytes: 1,
            max_work: 1,
            max_depth: 1,
        };
        assert!(matches!(
            queue.try_query(
                BrokerActor::person("actor-a"),
                &fixture.cluster_id,
                oversized_text.clone()
            ),
            Err(ActionDirectoryBridgeError::InvalidRequest)
        ));
        oversized_text.needle.clear();
        oversized_text.cursor = vec![0; DIRECTORY_MAX_CURSOR_BYTES.saturating_add(1)];
        assert!(matches!(
            queue.try_query(
                BrokerActor::person("actor-a"),
                &fixture.cluster_id,
                oversized_text
            ),
            Err(ActionDirectoryBridgeError::InvalidRequest)
        ));
        assert!(matches!(
            queue.try_resolve(
                BrokerActor::person("actor-a"),
                "x".repeat(DIRECTORY_MAX_IDENTIFIER_BYTES.saturating_add(1)),
                fixture.peer_id,
                "directory-r1",
                "workspace:atlas",
                "open",
            ),
            Err(ActionDirectoryBridgeError::InvalidRequest)
        ));
        assert!(matches!(
            queue.try_start_resolved(
                BrokerActor::person("actor-a"),
                &fixture.cluster_id,
                fixture.peer_id,
                "directory-r1",
                "workspace:atlas",
                "open",
                Some("x".repeat(DIRECTORY_MAX_PARAMS_BYTES.saturating_add(1))),
                None,
            ),
            Err(ActionDirectoryBridgeError::InvalidRequest)
        ));
        assert!(matches!(
            queue.try_start_resolved(
                BrokerActor::person("actor-a"),
                &fixture.cluster_id,
                fixture.peer_id,
                "directory-r1",
                "workspace:atlas",
                "open",
                None,
                Some(DirectoryActionConfirmation::Phrase(
                    "x".repeat(DIRECTORY_MAX_CONFIRMATION_BYTES.saturating_add(1)),
                )),
            ),
            Err(ActionDirectoryBridgeError::InvalidRequest)
        ));
        assert_eq!(
            fixture.directory_state.borrow().query_count,
            0,
            "invalid local requests are rejected before a directory capability is contacted"
        );
    }

    #[test]
    fn directory_wire_rows_refuse_control_framing_in_identifiers_before_policy_or_invocation() {
        let fixture = local_directory_fixture();
        let mut row = fixture.directory_state.borrow().row();
        row.resource = "workspace:atlas\u{1f}admin".into();
        assert!(
            permits_directory_row(
                &fixture.state,
                &fixture.cluster_id,
                &BrokerActor::person("actor-a"),
                &fixture.route.app_id,
                &row,
            )
            .is_err(),
            "a resource cannot inject the shell result-id separator"
        );
        let mut row = fixture.directory_state.borrow().row();
        row.action = Some("open\nadmin".into());
        assert!(
            permits_directory_row(
                &fixture.state,
                &fixture.cluster_id,
                &BrokerActor::person("actor-a"),
                &fixture.route.app_id,
                &row,
            )
            .is_err(),
            "control characters cannot turn one wire action into several shell fields"
        );
    }

    #[test]
    fn retained_directory_root_pages_reach_exact_bound_and_tail_without_query_sorting() {
        let fixture = local_directory_fixture();
        {
            let mut roots = fixture
                .state
                .action_directory_roots
                .lock()
                .expect("directory roots");
            for peer_id in 1..=129_u64 {
                roots.insert(
                    peer_id,
                    ActionDirectoryRoot {
                        peer_id,
                        app_id: "dev.qos.test".into(),
                        cluster_id: fixture.cluster_id.clone(),
                        record_id: format!("root-{peer_id}"),
                    },
                );
            }
            drop(roots);
        }
        fixture.state.refresh_action_directory_root_snapshot();
        let first = fixture.state.action_directory_root_page(None, 128);
        assert_eq!(first.roots.len(), 128);
        assert_eq!(first.next_after_peer, Some(128));
        let second = fixture
            .state
            .action_directory_root_page(first.next_after_peer, 128);
        assert_eq!(second.roots.len(), 1);
        assert_eq!(second.roots[0].peer_id, 129);
        assert_eq!(second.next_after_peer, None);
        let repeat = fixture.state.action_directory_root_page(None, 1);
        assert_eq!(repeat.roots[0].peer_id, 1);
        assert_eq!(repeat.generation, second.generation);
        fixture.state.remove_connection(129);
        let after_disconnect = fixture.state.action_directory_root_page(Some(128), 1);
        assert!(after_disconnect.roots.is_empty());
        assert!(after_disconnect.generation > second.generation);
    }

    #[test]
    fn invocation_journal_admission_precedes_real_application_delivery() {
        let root = tempfile::tempdir().expect("private test root");
        let base = local_directory_fixture();
        let membership = base.membership.as_ref().unwrap().clone();
        membership.write().persist_path = Some(root.path().join("state.json"));
        drop(base);
        let mut fixture = local_directory_fixture_with_membership(membership);
        let offered = PublishedSurface {
            peer_id: fixture.peer_id,
            app_id: fixture.route.app_id.clone(),
            route: fixture.route.clone(),
            actions: vec![
                published_directory_action(&fixture.directory_state.borrow().row()).unwrap(),
            ],
            live: true,
        };
        fixture
            .state
            .surfaces
            .lock()
            .unwrap()
            .insert(fixture.peer_id, offered);
        let actor = fixture
            .membership
            .as_ref()
            .unwrap()
            .read()
            .active_workspace()
            .unwrap()
            .owner
            .clone()
            .unwrap();
        let job = fixture
            .state
            .start_invocation(fixture.peer_id, "workspace:atlas", "open", None, &actor)
            .unwrap();
        let delivered = fixture
            .invocation_rx
            .try_recv()
            .expect("actual app invocation stream");
        assert_eq!(delivered.id, job.id);
        let bytes = std::fs::read(root.path().join("state.json.invocations/records.json")).expect(
            "the real broker must persist admission before delivering an application effect",
        );
        let snapshot: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(snapshot["records"].as_array().unwrap().len(), 1);
        assert_eq!(snapshot["records"][0]["wire_id"], job.id);
    }

    #[cfg(test)]
    fn certified_join(
        workspace: &str,
        root: &SigningKey,
        device: &SigningKey,
    ) -> runtime_collab::CertifiedJoinIdentity {
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Test device".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut cert, root).expect("root signs device cert");
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
        let mut cert: tilo_user_identity::DeviceCert =
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

    #[test]
    fn workspace_control_requires_editor_authority() {
        assert!(workspace_control_role_allowed(Role::Owner));
        assert!(workspace_control_role_allowed(Role::Editor));
        assert!(!workspace_control_role_allowed(Role::Commenter));
        assert!(!workspace_control_role_allowed(Role::Viewer));
        assert!(!workspace_control_role_allowed(Role::Denied));
    }

    #[cfg(test)]
    fn verified_device(root: &SigningKey) -> VerifiedJoinIdentity {
        VerifiedJoinIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: [42; 32],
        }
    }

    #[cfg(test)]
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
        let mut workspace = crate::test_support::Workspace::test_default(
            crate::test_support::WorkspaceId(1),
            "Member room",
        );
        let route = CrdtRoute::cluster(workspace.room.to_string());
        workspace.grant_capability(crate::test_support::CapOverride::membership(
            crate::test_support::Subject::person(&principal),
            workspace.room.to_string(),
            crate::test_support::Verb::Write,
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
        let mut workspace = crate::test_support::Workspace::test_default(
            crate::test_support::WorkspaceId(44),
            "Agent room",
        );
        workspace.owner = Some(principal);
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
        assert_eq!(admitted.broker_actor().id, "agent:scribe");
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
        let (state, _bridge_rx) = CapnpBrokerState::new(
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
        let mut workspace = crate::test_support::Workspace::test_default(
            crate::test_support::WorkspaceId(45),
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
        let mut workspace = crate::test_support::Workspace::test_default(
            crate::test_support::WorkspaceId(2),
            "Revocation room",
        );
        let route = CrdtRoute::cluster(workspace.room.to_string());
        let membership = workspace.grant_capability(crate::test_support::CapOverride::membership(
            crate::test_support::Subject::person(&member_id),
            workspace.room.to_string(),
            crate::test_support::Verb::Read,
        ));
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);

        assert!(admit_verified_join(&tree, &route, verified_device(&stranger)).is_err());
        let admitted = admit_verified_join(&tree, &route, verified_device(&member))
            .expect("member starts admitted");
        let membership_tree = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(membership_tree.clone()),
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
        let mut member_workspace = crate::test_support::Workspace::test_default(
            crate::test_support::WorkspaceId(3),
            "Member room",
        );
        member_workspace.set_owner(&principal);
        let other_workspace = crate::test_support::Workspace::test_default(
            crate::test_support::WorkspaceId(4),
            "Other room",
        );
        let replay_route = CrdtRoute::cluster(other_workspace.room.to_string());
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(member_workspace);
        tree.workspaces.push(other_workspace);

        assert!(admit_verified_join(&tree, &replay_route, verified_device(&root)).is_err());
    }

    #[cfg(test)]
    fn assert_replay_and_forgery_are_audited(
        state: &Arc<CapnpBrokerState>,
        broker_channels: &Arc<Mutex<BrokerChannels>>,
        route: &CrdtRoute,
        admitted: &AdmittedJoinIdentity,
        op: &SignedOp,
    ) {
        let artifact = op.artifact;
        let accepted_before = broker_channels
            .lock()
            .expect("broker channels")
            .state_for(artifact)
            .scrollback_for(artifact, u64::MAX, 10)
            .0;
        handle_signed_op_send(state, 1, route, admitted, op.clone());
        let mut forged = op.clone();
        forged.seq = 2;
        forged.payload = DocumentMutation::Incremental(b"tampered".to_vec())
            .encode()
            .expect("typed incremental");
        handle_signed_op_send(state, 1, route, admitted, forged);

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
            accepted_before,
            "replay and forgery must not change the accepted scrollback",
        );
    }

    #[cfg(test)]
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

    #[allow(
        clippy::too_many_lines,
        reason = "the typed document publication test retains its full authorization and payload assertions as one scenario"
    )]
    #[test]
    fn certified_document_send_uses_live_route_role_and_typed_snapshot() {
        let root = SigningKey::from_bytes(&[24; 32]);
        let device = SigningKey::from_bytes(&[25; 32]);
        let principal = principal_id_for_root(root.verifying_key().to_bytes());
        let mut workspace = crate::test_support::Workspace::test_default(
            crate::test_support::WorkspaceId(5),
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
        let publisher =
            PeerPublisher::new(sink.clone() as Arc<dyn crate::collab_outbound::PeerPublishSink>);
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::clone(&broker_channels),
            publisher,
            Some(membership.clone()),
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
        let authorized = state
            .authorize_certified_document(&route, &admitted, &op)
            .expect("live owner may edit the joined document");
        handle_authorized_signed_op_send(&state, 1, &route, &admitted, authorized, op.clone());

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

        let legacy_version = collab_types::DocumentWireVersion::QosV1;
        let legacy_artifact = collab_types::document_artifact_id_for_version(
            legacy_version,
            &route.app_id,
            &route.cluster_id,
            &route.record_id,
        );
        let mut legacy = SignedOp::new_unsigned(
            DocumentMutation::Incremental(b"legacy retained edit".to_vec())
                .encode_for(legacy_version)
                .expect("legacy typed mutation"),
            legacy_artifact,
            Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes()),
            collab_types::document_attestation_id_for_version(
                legacy_version,
                &route.app_id,
                &route.cluster_id,
                &route.record_id,
            ),
            collab_types::OpClass::Splice,
            SystemTime::now(),
            2,
        );
        sign_op(&mut legacy, &device).expect("device signs retained legacy edit");
        let authorized = state
            .authorize_certified_document(&route, &admitted, &legacy)
            .expect("authenticated retained legacy route remains writable");
        assert!(matches!(
            &authorized.payload,
            collab_types::VersionedRoutedDocumentPayload::Cleartext(
                collab_types::DecodedDocumentMutation {
                    version: collab_types::DocumentWireVersion::QosV1,
                    mutation: DocumentMutation::Incremental(bytes),
                }
            ) if bytes == b"legacy retained edit"
        ));
        handle_authorized_signed_op_send(&state, 1, &route, &admitted, authorized, legacy.clone());
        assert_eq!(
            postcard::from_bytes::<SignedOp>(&received.try_recv().expect("legacy fanout").op_bytes)
                .expect("legacy signed envelope"),
            legacy,
            "fanout must preserve the exact authenticated legacy envelope"
        );
        assert_eq!(
            broker_channels
                .lock()
                .expect("broker channels")
                .state_for(legacy_artifact)
                .op_log
                .len(),
            1,
            "legacy and current durable artifacts remain distinct"
        );

        let mut mixed = legacy.clone();
        mixed.attestation_id =
            document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id);
        sign_op(&mut mixed, &device).expect("re-sign mixed namespace envelope");
        assert!(
            state
                .authorize_certified_document(&route, &admitted, &mixed)
                .is_err(),
            "artifact and attestation must select one exact route namespace"
        );
        let mut mixed = legacy.clone();
        mixed.payload = DocumentMutation::Incremental(b"mixed payload".to_vec())
            .encode()
            .expect("current typed mutation");
        sign_op(&mut mixed, &device).expect("re-sign mixed payload envelope");
        assert!(
            state
                .authorize_certified_document(&route, &admitted, &mixed)
                .is_err(),
            "legacy route ids cannot carry a current payload"
        );

        let expected_channel =
            collab_types::document_channel_id(&route.app_id, &route.cluster_id, &route.record_id);
        let mut encrypted = SignedOp::new_unsigned_with_encryption(
            b"opaque MLS application frame".to_vec(),
            artifact,
            Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes()),
            document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id),
            collab_types::OpClass::Splice,
            SystemTime::now(),
            2,
            collab_types::EncryptionMetadata {
                epoch: 7,
                channel: expected_channel,
            },
        );
        sign_op(&mut encrypted, &device).expect("device signs opaque MLS envelope");
        let authorized = state
            .authorize_certified_document(&route, &admitted, &encrypted)
            .expect("broker authorizes signed clear metadata without decrypting");
        assert!(matches!(
            authorized.payload,
            collab_types::VersionedRoutedDocumentPayload::Encrypted { channel, epoch: 7 }
                if channel == expected_channel
        ));
        handle_signed_op_send(&state, 1, &route, &admitted, encrypted.clone());
        let delivered = received.try_recv().expect("opaque MLS fanout");
        assert_eq!(
            postcard::from_bytes::<SignedOp>(&delivered.op_bytes).expect("opaque envelope"),
            encrypted,
            "broker must relay ciphertext byte-for-byte"
        );

        let mut wrong_channel = encrypted.clone();
        wrong_channel.encryption.as_mut().expect("metadata").channel =
            collab_types::ChannelId::new();
        sign_op(&mut wrong_channel, &device).expect("re-sign adversarial envelope");
        assert!(
            state
                .authorize_certified_document(&route, &admitted, &wrong_channel)
                .is_err(),
            "validly signed ciphertext for another MLS channel must not be routed"
        );

        let legacy_channel = collab_types::document_channel_id_for_version(
            legacy_version,
            &route.app_id,
            &route.cluster_id,
            &route.record_id,
        );
        let mut legacy_encrypted = SignedOp::new_unsigned_with_encryption(
            b"opaque legacy MLS application frame".to_vec(),
            legacy_artifact,
            Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes()),
            collab_types::document_attestation_id_for_version(
                legacy_version,
                &route.app_id,
                &route.cluster_id,
                &route.record_id,
            ),
            collab_types::OpClass::Splice,
            SystemTime::now(),
            3,
            collab_types::EncryptionMetadata {
                epoch: 8,
                channel: legacy_channel,
            },
        );
        sign_op(&mut legacy_encrypted, &device).expect("device signs legacy MLS envelope");
        state
            .authorize_certified_document(&route, &admitted, &legacy_encrypted)
            .expect("exact legacy MLS channel is admitted");
        legacy_encrypted
            .encryption
            .as_mut()
            .expect("legacy encryption metadata")
            .channel = expected_channel;
        sign_op(&mut legacy_encrypted, &device).expect("re-sign cross-namespace MLS envelope");
        assert!(
            state
                .authorize_certified_document(&route, &admitted, &legacy_encrypted)
                .is_err(),
            "legacy route ids cannot select the current MLS governance channel"
        );

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
        let (state, _bridge_rx) = CapnpBrokerState::new(
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
                decrypted_payload: None,
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

    #[derive(Clone)]
    struct CountingCatalogueAuthority {
        tree: Arc<Mutex<qos_desktop::WorkspaceTree>>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
        failed: Arc<std::sync::atomic::AtomicBool>,
    }

    impl crate::services::WorkspaceAuthority for CountingCatalogueAuthority {
        fn snapshot(&self) -> Result<qos_desktop::WorkspaceTree, String> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if self.failed.load(std::sync::atomic::Ordering::Relaxed) {
                return Err("catalogue authority unavailable".into());
            }
            self.tree
                .lock()
                .map_err(|_| "catalogue authority unavailable".to_string())
                .map(|tree| tree.clone())
        }
    }

    fn catalogue_surface(peer_id: u64, cluster: &str, resource: &str) -> PublishedSurface {
        PublishedSurface {
            peer_id,
            app_id: "dev.qos.files".into(),
            route: CrdtRoute::record("dev.qos.files", cluster, format!("record-{peer_id}")),
            actions: vec![PublishedAction {
                resource: resource.into(),
                action: "open".into(),
                resource_kind: "files.entry".into(),
                resource_label: resource.into(),
                action_label: "Open".into(),
                description: None,
                operation_kind: tilo_actions::OperationKind::View,
                params: Vec::new(),
                contract: tilo_actions::ActionContract {
                    permission: tilo_actions::PermissionRequirement::Write,
                    ..Default::default()
                },
                input: PublishedInput::None,
            }],
            live: true,
        }
    }

    struct CountingCatalogueFixture {
        state: Arc<CapnpBrokerState>,
        tree: Arc<Mutex<qos_desktop::WorkspaceTree>>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
        failed: Arc<std::sync::atomic::AtomicBool>,
        cluster: String,
    }

    fn counting_catalogue_state(owner: &str) -> CountingCatalogueFixture {
        let mut workspace = crate::test_support::Workspace::test_default(
            crate::test_support::WorkspaceId(29),
            "Catalogue room",
        );
        workspace.set_owner(owner);
        let cluster = workspace.room.to_string();
        let mut tree = qos_desktop::WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let tree = Arc::new(Mutex::new(tree));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = CapnpBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(Arc::new(CountingCatalogueAuthority {
                tree: tree.clone(),
                calls: calls.clone(),
                failed: failed.clone(),
            })),
            None,
            None,
            HostServices::default(),
        );
        CountingCatalogueFixture {
            state,
            tree,
            calls,
            failed,
            cluster,
        }
    }

    fn catalogue_test_channel() -> (
        watch::Sender<ActionCatalogueSnapshot>,
        watch::Receiver<ActionCatalogueSnapshot>,
    ) {
        watch::channel(Some(Arc::<str>::from("{}")))
    }

    fn current_catalogue(
        receiver: &mut watch::Receiver<ActionCatalogueSnapshot>,
    ) -> ActionCatalogueSnapshot {
        receiver.borrow_and_update().clone()
    }

    #[test]
    fn catalogue_publication_computes_once_per_exact_audience_and_keeps_only_latest_state() {
        let CountingCatalogueFixture {
            state,
            calls,
            cluster,
            ..
        } = counting_catalogue_state("owner");
        let actor = BrokerActor::person("owner");
        let (first_tx, mut first_rx) = catalogue_test_channel();
        let (second_tx, mut second_rx) = catalogue_test_channel();
        state.action_catalogue_subscribers.lock().unwrap().extend([
            (1, (cluster.clone(), actor.clone(), first_tx)),
            (2, (cluster.clone(), actor, second_tx)),
        ]);

        state
            .surfaces
            .lock()
            .unwrap()
            .insert(11, catalogue_surface(11, &cluster, "files.entry:first"));
        state.publish_action_catalogue();
        state
            .surfaces
            .lock()
            .unwrap()
            .insert(12, catalogue_surface(12, &cluster, "files.entry:second"));
        state.publish_action_catalogue();

        assert_eq!(
            calls.load(std::sync::atomic::Ordering::Relaxed),
            2,
            "each synchronous publication computes one snapshot for a shared exact audience"
        );
        for receiver in [&mut first_rx, &mut second_rx] {
            let latest: serde_json::Value =
                serde_json::from_str(&current_catalogue(receiver).unwrap()).unwrap();
            assert_eq!(latest["surfaces"].as_array().unwrap().len(), 2);
            assert!(
                !receiver.has_changed().unwrap(),
                "a slow reader retains no obsolete complete catalogue"
            );
        }
    }

    #[test]
    fn catalogue_publication_separates_person_and_agent_with_the_same_textual_id() {
        let CountingCatalogueFixture {
            state,
            tree,
            calls,
            cluster,
            ..
        } = counting_catalogue_state("same");
        state
            .surfaces
            .lock()
            .unwrap()
            .insert(11, catalogue_surface(11, &cluster, "files.entry:owner"));
        let mut other = crate::test_support::Workspace::test_default(
            crate::test_support::WorkspaceId(30),
            "Other catalogue room",
        );
        other.set_owner("same");
        let other_cluster = other.room.to_string();
        tree.lock().unwrap().workspaces.push(other);
        let (person_tx, mut person_rx) = catalogue_test_channel();
        let (agent_tx, mut agent_rx) = catalogue_test_channel();
        let (other_tx, mut other_rx) = catalogue_test_channel();
        state.action_catalogue_subscribers.lock().unwrap().extend([
            (1, (cluster.clone(), BrokerActor::person("same"), person_tx)),
            (2, (cluster, BrokerActor::agent("same"), agent_tx)),
            (3, (other_cluster, BrokerActor::person("same"), other_tx)),
        ]);

        state.publish_action_catalogue();

        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 3);
        let person = current_catalogue(&mut person_rx).unwrap();
        let agent = current_catalogue(&mut agent_rx).unwrap();
        let other = current_catalogue(&mut other_rx).unwrap();
        assert!(person.contains("files.entry:owner"));
        assert!(!agent.contains("files.entry:owner"));
        assert!(!other.contains("files.entry:owner"));
    }

    #[test]
    fn catalogue_publication_rechecks_changed_authority_on_the_next_turn() {
        let CountingCatalogueFixture {
            state,
            tree,
            calls,
            cluster,
            ..
        } = counting_catalogue_state("owner");
        state
            .surfaces
            .lock()
            .unwrap()
            .insert(11, catalogue_surface(11, &cluster, "files.entry:owner"));
        let (tx, mut rx) = catalogue_test_channel();
        state
            .action_catalogue_subscribers
            .lock()
            .unwrap()
            .insert(1, (cluster, BrokerActor::person("owner"), tx));
        state.publish_action_catalogue();
        assert!(
            current_catalogue(&mut rx)
                .unwrap()
                .contains("files.entry:owner")
        );

        tree.lock().unwrap().workspaces[0].set_owner("replacement");
        state.publish_action_catalogue();
        assert!(
            !current_catalogue(&mut rx)
                .unwrap()
                .contains("files.entry:owner")
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn catalogue_stream_starts_with_latest_state_and_rejects_invalidated_pending_state() {
        let (initial_tx, initial_rx) = watch::channel(Some(Arc::<str>::from("initial")));
        let mut initial_reader = ActionCatalogueReader {
            rx: initial_rx,
            initial: true,
        };
        assert_eq!(&*initial_reader.next().await.unwrap(), "initial");
        drop(initial_tx);
        assert!(
            initial_reader.next().await.is_err(),
            "a closed stream with no pending state disconnects"
        );

        let (tx, rx) = watch::channel(Some(Arc::<str>::from("initial")));
        tx.send(Some(Arc::<str>::from("newer"))).unwrap();
        let mut reader = ActionCatalogueReader { rx, initial: true };
        assert_eq!(&*reader.next().await.unwrap(), "newer");

        tx.send(Some(Arc::<str>::from("unread"))).unwrap();
        tx.send_replace(None);
        drop(tx);
        assert!(
            reader.next().await.is_err(),
            "projection failure invalidates an unread stale snapshot"
        );
    }

    #[test]
    fn catalogue_projection_failure_invalidates_and_removes_its_audience() {
        let CountingCatalogueFixture {
            state,
            calls,
            failed,
            cluster,
            ..
        } = counting_catalogue_state("owner");
        state
            .surfaces
            .lock()
            .unwrap()
            .insert(11, catalogue_surface(11, &cluster, "files.entry:owner"));
        let (tx, mut rx) = catalogue_test_channel();
        let (second_tx, mut second_rx) = catalogue_test_channel();
        let (closed_tx, closed_rx) = catalogue_test_channel();
        drop(closed_rx);
        state
            .action_catalogue_subscribers
            .lock()
            .unwrap()
            .insert(1, (cluster.clone(), BrokerActor::person("owner"), tx));
        state.action_catalogue_subscribers.lock().unwrap().insert(
            2,
            (cluster.clone(), BrokerActor::person("owner"), second_tx),
        );
        state
            .action_catalogue_subscribers
            .lock()
            .unwrap()
            .insert(3, (cluster, BrokerActor::person("owner"), closed_tx));
        state.publish_action_catalogue();
        assert!(
            current_catalogue(&mut rx)
                .unwrap()
                .contains("files.entry:owner"),
            "the current authorized catalogue is initially available"
        );
        assert!(
            current_catalogue(&mut second_rx)
                .unwrap()
                .contains("files.entry:owner")
        );
        assert!(
            !state
                .action_catalogue_subscribers
                .lock()
                .unwrap()
                .contains_key(&3),
            "closed catalogue subscribers are removed"
        );

        failed.store(true, std::sync::atomic::Ordering::Relaxed);
        state.publish_action_catalogue();
        assert!(rx.borrow_and_update().is_none());
        assert!(second_rx.borrow_and_update().is_none());
        let subscribers = state.action_catalogue_subscribers.lock().unwrap();
        assert!(!subscribers.contains_key(&1));
        assert!(!subscribers.contains_key(&2));
        assert!(!subscribers.contains_key(&3));
        drop(subscribers);
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    #[allow(
        clippy::too_many_lines,
        clippy::arithmetic_side_effects,
        clippy::unchecked_time_subtraction,
        reason = "the directed-action test keeps capability setup, delivery, and denial assertions in one scenario"
    )]
    #[test]
    fn exact_catalogue_scope_avoids_unrelated_oversized_descriptors() {
        let mut workspace = crate::test_support::Workspace::test_default(
            crate::test_support::WorkspaceId(30),
            "Scoped action room",
        );
        workspace.set_owner("owner");
        let cluster = workspace.room.to_string();
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let membership = tree.shared();
        struct Launcher;
        impl crate::services::LauncherServices for Launcher {
            fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
                vec![crate::services::LaunchableApplication {
                    package_id: "dev.qos.word-processor".into(),
                    display_name: "Writer".into(),
                }]
            }
            fn launch(&self, _: &str, _: &qos_desktop::Workspace) -> Result<(), String> {
                unreachable!("catalogue projection never launches")
            }
            fn attest_process(&self, _: u32, _: &str, _: &str) -> bool {
                false
            }
        }
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = CapnpBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(membership.authority()),
            None,
            None,
            crate::services::HostServices {
                launcher: Some(Arc::new(Launcher)),
                ..Default::default()
            },
        );
        let action =
            |resource: String, action: &str, description: Option<String>| PublishedAction {
                resource,
                action: action.into(),
                resource_kind: "document".into(),
                resource_label: "Document".into(),
                action_label: "Command".into(),
                description,
                operation_kind: tilo_actions::OperationKind::Effect,
                params: Vec::new(),
                contract: tilo_actions::ActionContract {
                    permission: tilo_actions::PermissionRequirement::Write,
                    ..Default::default()
                },
                input: PublishedInput::None,
            };
        let desktop = PublishedSurface {
            peer_id: 1,
            app_id: qos_peer_state::desktop_command::APP_ID.into(),
            route: CrdtRoute::record(qos_peer_state::desktop_command::APP_ID, &cluster, ""),
            actions: vec![action(
                qos_peer_state::desktop_command::RESOURCE.into(),
                qos_peer_state::desktop_command::ACTION,
                None,
            )],
            live: true,
        };
        let mut surfaces = state.surfaces.lock().unwrap();
        surfaces.insert(desktop.peer_id, desktop);
        for index in 0..24u64 {
            surfaces.insert(
                index + 2,
                PublishedSurface {
                    peer_id: index + 2,
                    app_id: "dev.qos.word-processor".into(),
                    route: CrdtRoute::record(
                        "dev.qos.word-processor",
                        &cluster,
                        &format!("record-{index}"),
                    ),
                    actions: vec![action(
                        format!("document:{index}"),
                        "edit",
                        Some("x".repeat(16 * 1024)),
                    )],
                    live: true,
                },
            );
        }
        drop(surfaces);
        let actor = BrokerActor {
            id: "owner".into(),
            is_agent: false,
        };
        let full: serde_json::Value = serde_json::from_str(
            &state
                .action_catalogue_json_for(&cluster, &actor, None)
                .unwrap(),
        )
        .unwrap();
        let full_message = tilo_relay::peer_actions::ActionMessage::BrokerCatalogue {
            host: uuid::Uuid::new_v4(),
            catalogue: full,
        };
        assert!(
            full_message.validate(),
            "the full shape remains structurally valid"
        );
        assert!(
            serde_json::to_vec(&full_message).unwrap().len()
                > tilo_relay::peer_actions::MAX_PRIVATE_BYTES
        );
        let scope = tilo_relay::peer_actions::ExactActionRoute {
            app_id: qos_peer_state::desktop_command::APP_ID.into(),
            record_id: String::new(),
            resource: qos_peer_state::desktop_command::RESOURCE.into(),
            action: qos_peer_state::desktop_command::ACTION.into(),
        };
        let scoped: serde_json::Value = serde_json::from_str(
            &state
                .action_catalogue_json_for(&cluster, &actor, Some(&scope))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(scoped["surfaces"].as_array().unwrap().len(), 1);
        assert_eq!(
            scoped["surfaces"][0]["actions"].as_array().unwrap().len(),
            1
        );
        let denied = BrokerActor {
            id: "stranger".into(),
            is_agent: false,
        };
        let denied: serde_json::Value = serde_json::from_str(
            &state
                .action_catalogue_json_for(&cluster, &denied, Some(&scope))
                .unwrap(),
        )
        .unwrap();
        assert!(
            denied["surfaces"].as_array().unwrap().is_empty(),
            "scope narrows discovery but never grants authorization"
        );
        let launcher_scope = tilo_relay::peer_actions::ExactActionRoute {
            app_id: "dev.qos.word-processor".into(),
            record_id: String::new(),
            resource: "app.dev.qos.word-processor".into(),
            action: "open".into(),
        };
        let launcher: serde_json::Value = serde_json::from_str(
            &state
                .action_catalogue_json_for(&cluster, &actor, Some(&launcher_scope))
                .unwrap(),
        )
        .unwrap();
        assert!(
            launcher["surfaces"].as_array().unwrap().is_empty(),
            "scope cannot add a launcher hidden by another live record of that app"
        );
        let directed = tilo_relay::peer_actions::DirectedAction {
            room: uuid::Uuid::parse_str(&cluster).unwrap(),
            request_id: uuid::Uuid::new_v4(),
            source: [1; 32],
            target: [2; 32],
            message: tilo_relay::peer_actions::ActionMessage::BrokerCatalogue {
                host: uuid::Uuid::new_v4(),
                catalogue: scoped,
            },
        };
        assert!(
            directed.validate(),
            "the exact desktop reply fits transport bounds"
        );
    }

    #[allow(
        clippy::too_many_lines,
        clippy::arithmetic_side_effects,
        clippy::unchecked_time_subtraction,
        reason = "the directed-action test keeps capability setup, delivery, and denial assertions in one scenario"
    )]
    #[test]
    fn live_action_invocation_is_fresh_directed_and_permission_checked() {
        let mut workspace = crate::test_support::Workspace::test_default(
            crate::test_support::WorkspaceId(31),
            "Action room",
        );
        workspace.set_owner("owner");
        let route = CrdtRoute::app("dev.qos.files", workspace.room.to_string());
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let membership = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = CapnpBrokerState::new(
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
                        operation_kind: tilo_actions::OperationKind::View,
                        params: Vec::new(),
                        contract: tilo_actions::ActionContract::view(),
                        input: PublishedInput::None,
                    },
                    PublishedAction {
                        resource: "files.entry:/tmp/report".into(),
                        action: "erase".into(),
                        resource_kind: "files.entry".into(),
                        resource_label: "report".into(),
                        action_label: "Erase".into(),
                        description: None,
                        operation_kind: tilo_actions::OperationKind::Effect,
                        params: Vec::new(),
                        contract: tilo_actions::ActionContract {
                            risk: tilo_actions::RiskLevel::Destructive,
                            confirmation: tilo_actions::ConfirmationPolicy::Confirm,
                            ..tilo_actions::ActionContract::default()
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
                        operation_kind: tilo_actions::OperationKind::Effect,
                        params: Vec::new(),
                        contract: tilo_actions::ActionContract {
                            execution: tilo_actions::ExecutionMode::Background {
                                cancellable: true,
                            },
                            result: tilo_actions::ResultSpec::Job,
                            ..tilo_actions::ActionContract::default()
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
            Some(tilo_actions::InvocationUpdate::Failed { error, .. })
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
            Some(tilo_actions::InvocationUpdate::Failed { error, .. })
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
                operation_kind: tilo_actions::OperationKind::View,
                params: Vec::new(),
                contract: tilo_actions::ActionContract {
                    discovery: tilo_actions::DiscoveryScope::Launchable,
                    ..tilo_actions::ActionContract::view()
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

    #[test]
    fn late_join_snapshot_request_selects_exactly_one_existing_holder() {
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            None,
            None,
        );
        let channel = "dev.tilo.notes#room:test#record:daily";
        let mut first = state.subscribe_context(channel, 7);
        let mut second = state.subscribe_context(channel, 9);
        let _requester = state.subscribe_context(channel, 12);

        assert!(state.request_document_snapshot(channel, 12));
        assert!(matches!(
            first.try_recv(),
            Ok(ContextEnvelope::SnapshotRequested)
        ));
        assert!(second.try_recv().is_err(), "only one holder may respond");
        let requester_only_channel = "dev.tilo.notes#room:test#record:empty";
        let mut requester_only = state.subscribe_context(requester_only_channel, 12);
        assert!(
            !state.request_document_snapshot(requester_only_channel, 12),
            "the requester cannot answer from its own empty state"
        );
        assert!(requester_only.try_recv().is_err());
        assert!(
            !state.request_document_snapshot("missing", 12),
            "a route without a holder is retryable, not falsely acknowledged"
        );
    }

    #[allow(
        clippy::significant_drop_tightening,
        clippy::too_many_lines,
        reason = "the receipt assertions retain the read guard for the complete consistency snapshot"
    )]
    #[test]
    fn remote_agent_effect_terminals_become_redacted_workspace_receipts() {
        let mut workspace = crate::test_support::Workspace::test_default(
            crate::test_support::WorkspaceId(32),
            "Agent effect room",
        );
        workspace.set_owner("owner");
        let cluster_id = workspace.room.to_string();
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let membership = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(membership.clone()),
            None,
        );
        let (tx, _rx) = std::sync::mpsc::channel();
        let invocation = tilo_actions::InvocationId::new();
        let pending = PendingInvocation {
            network_evidence: None,
            journal_key: None,
            peer_id: 7,
            tx,
            update: tilo_actions::InvocationUpdate::Accepted { invocation },
            last_sequence: 0,
            actor: "agent:Scribe".into(),
            cancellable: false,
            retryable_on_disconnect: false,
            execution: tilo_actions::ExecutionMode::Immediate,
            result: tilo_actions::ResultSpec::None,
            operation_kind: tilo_actions::OperationKind::Effect,
            cluster_id,
            context: tilo_actions::ActionContext::for_resource(
                "Publish",
                "document:brief",
                "document",
                "Release brief",
            )
            .with_app("org.qos.Writer", "Writer")
            .with_group("task:release", "Release", "task"),
            last_activity: std::time::Instant::now(),
            cancellation_requested_at: None,
        };
        let update = tilo_actions::InvocationUpdate::Failed {
            invocation,
            error: tilo_actions::ActionFailure {
                code: "publish_denied".into(),
                message: "authorization=secret-value".into(),
                details: Some(serde_json::json!({"body": "private draft"})),
                retryable: false,
            },
        };

        state.record_effect_receipt(&pending, &update);
        state.record_effect_receipt(&pending, &update);

        let succeeded_invocation = tilo_actions::InvocationId::new();
        let mut succeeded_pending = pending;
        succeeded_pending.update = tilo_actions::InvocationUpdate::Accepted {
            invocation: succeeded_invocation,
        };
        state.record_effect_receipt(
            &succeeded_pending,
            &tilo_actions::InvocationUpdate::Succeeded {
                invocation: succeeded_invocation,
                output: tilo_actions::ActionOutput {
                    value: Some(serde_json::json!({"token": "output-secret"})),
                    resources: vec!["files.entry:/private/export".into()],
                },
            },
        );
        let mut private_view = succeeded_pending;
        private_view.operation_kind = tilo_actions::OperationKind::View;
        let view_invocation = tilo_actions::InvocationId::new();
        state.record_effect_receipt(
            &private_view,
            &tilo_actions::InvocationUpdate::Succeeded {
                invocation: view_invocation,
                output: tilo_actions::ActionOutput::default(),
            },
        );

        let tree = membership.read();
        let receipts = &tree.workspaces[0].effect_receipts;
        assert_eq!(
            receipts.len(),
            2,
            "invocation identity deduplicates retry delivery"
        );
        assert_eq!(receipts[0].actor, "agent:Scribe");
        assert_eq!(
            receipts[0]
                .context
                .group
                .as_ref()
                .map(|group| group.id.as_str()),
            Some("task:release")
        );
        let durable = serde_json::to_string(receipts).expect("serialize receipts");
        assert!(!durable.contains("secret-value"));
        assert!(!durable.contains("private draft"));
        assert!(!durable.contains("output-secret"));
        assert!(!durable.contains("private/export"));
        assert!(durable.contains("publish_denied"));
        assert!(receipts.iter().any(|receipt| {
            receipt.invocation == succeeded_invocation
                && receipt.status == tilo_actions::EffectReceiptStatus::Succeeded
        }));
    }

    #[test]
    fn current_connection_invocation_sentinel_cannot_target_another_app() {
        assert_eq!(
            invocation_target_peer(ui::collaboration::CURRENT_CONNECTION_PEER_ID, 41),
            41
        );
        assert_eq!(invocation_target_peer(7, 41), 7);
    }
    #[cfg(test)]
    mod durable_admission_tests {
        mod peer_integration {
            include!("peer_action_integration_tests.rs");
        }
        use super::super::invocation_journal::BarrierFailure;
        use super::*;

        async fn update_app(
            client: &channel_handle::Client,
            id: u64,
            update: &tilo_actions::InvocationUpdate,
        ) -> Result<(), capnp::Error> {
            let mut request = client.update_invocation_request();
            request.get().set_id(id);
            request
                .get()
                .set_update_json(&serde_json::to_string(update).unwrap());
            request.send().promise.await?;
            Ok(())
        }

        const FULL_ID: u128 = 0xa5a5_a5a5_a5a5_a5a5_0000_0000_0000_002b;

        fn durable_fixture(
            root: &std::path::Path,
            membership: Option<WorkspaceTreeHandle>,
        ) -> (
            LocalDirectoryFixture,
            VerifiedActionCaller,
            CallerInvocation,
        ) {
            let root_key = SigningKey::from_bytes(&[51; 32]);
            let device = SigningKey::from_bytes(&[52; 32]);
            let membership = membership.unwrap_or_else(|| {
                let mut tree = WorkspaceTree::default();
                let mut workspace = crate::test_support::Workspace::test_default(
                    crate::test_support::WorkspaceId(71),
                    "Durable room",
                );
                workspace.set_owner(&principal_id_for_root(root_key.verifying_key().to_bytes()));
                tree.workspaces.push(workspace);
                tree.set_active(crate::test_support::WorkspaceId(71));
                tree.persist_path = Some(root.join("state.json"));
                tree.shared()
            });
            let fixture = local_directory_fixture_with_devices(
                membership,
                Some(71),
                Some(Arc::new(vec![
                    device.verifying_key().to_bytes(),
                    SigningKey::from_bytes(&[53; 32]).verifying_key().to_bytes(),
                ])),
            );
            let mut action =
                published_directory_action(&fixture.directory_state.borrow().row()).unwrap();
            action.operation_kind = tilo_actions::OperationKind::Effect;
            action.contract = tilo_actions::ActionContract {
                execution: tilo_actions::ExecutionMode::Background { cancellable: true },
                result: tilo_actions::ResultSpec::Job,
                ..tilo_actions::ActionContract::default()
            };
            assert!(action.contract.validate_declaration().is_empty());
            fixture.state.surfaces.lock().unwrap().insert(
                71,
                PublishedSurface {
                    peer_id: 71,
                    app_id: fixture.route.app_id.clone(),
                    route: fixture.route.clone(),
                    actions: vec![action],
                    live: true,
                },
            );
            fixture
                .state
                .stable_invocation_subscribers
                .lock()
                .unwrap()
                .insert(71);
            let caller = fixture
                .state
                .verify_action_caller(
                    &fixture.cluster_id,
                    &certified_join(&fixture.cluster_id, &root_key, &device)
                        .encode()
                        .unwrap(),
                )
                .unwrap();
            let request = CallerInvocation {
                recipient: None,
                revision: None,
                invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(FULL_ID)),
                host: fixture.state.invocation_host().unwrap(),
                peer_id: 71,
                app_id: fixture.route.app_id.clone(),
                record_id: fixture.route.record_id.clone(),
                resource: "workspace:atlas".into(),
                action: "open".into(),
                argument: None,
            };
            (fixture, caller, request)
        }

        fn effect(root: &std::path::Path) {
            use std::io::Write;
            let path = root.join("effect-counter");
            let before = std::fs::read_to_string(&path)
                .ok()
                .map(|s| s.parse::<u64>().unwrap())
                .unwrap_or(0);
            let mut file = std::fs::File::create(path).unwrap();
            write!(file, "{}", before + 1).unwrap();
            file.sync_all().unwrap();
            std::fs::File::open(root).unwrap().sync_all().unwrap();
        }

        #[tokio::test(flavor = "current_thread")]
        async fn invocation_journal_uuid_collisions_never_reach_application_stream() {
            tokio::task::LocalSet::new()
                .run_until(async {
                    // Both legacy directions matter: a caller can reserve a future
                    // numeric UUID, or attempt to reuse a previously delivered one.
                    for scenario in [
                        "cross-device",
                        "caller-before-legacy",
                        "legacy-before-caller",
                    ] {
                        let root = tempfile::tempdir().unwrap();
                        let (mut fixture, caller, mut request) = durable_fixture(root.path(), None);
                        let other = fixture
                            .state
                            .verify_action_caller(
                                &fixture.cluster_id,
                                &certified_join(
                                    &fixture.cluster_id,
                                    &SigningKey::from_bytes(&[51; 32]),
                                    &SigningKey::from_bytes(&[53; 32]),
                                )
                                .encode()
                                .unwrap(),
                            )
                            .unwrap();
                        if scenario == "caller-before-legacy" {
                            request.invocation =
                                tilo_actions::InvocationId(uuid::Uuid::from_u128(2));
                        }
                        let wire_id = if scenario == "legacy-before-caller" {
                            let job = fixture
                                .state
                                .start_invocation(
                                    71,
                                    &request.resource,
                                    &request.action,
                                    None,
                                    &caller.admitted.principal_id,
                                )
                                .unwrap();
                            request.invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(
                                u128::from(job.id),
                            ));
                            job.id
                        } else {
                            fixture
                                .state
                                .start_verified_invocation(&caller, &request, None)
                                .unwrap()
                                .wire_id
                        };
                        let (_, empty_rx) = mpsc::unbounded_channel();
                        let rx = Rc::new(AsyncMutex::new(std::mem::replace(
                            &mut fixture.invocation_rx,
                            empty_rx,
                        )));
                        let stream: invocation_stream::Client =
                            capnp_rpc::new_client(InvocationStreamServer { rx: rx.clone() });
                        let response = stream.read_request().send().promise.await.unwrap();
                        assert_eq!(response.get().unwrap().get_id(), wire_id);
                        assert_eq!(
                            response.get().unwrap().get_invocation_uuid().unwrap(),
                            request.invocation.0.as_bytes()
                        );
                        effect(root.path());
                        let conflict = if scenario == "caller-before-legacy" {
                            fixture
                                .state
                                .start_invocation(
                                    71,
                                    &request.resource,
                                    &request.action,
                                    None,
                                    &caller.admitted.principal_id,
                                )
                                .map(|_| ())
                        } else {
                            fixture
                                .state
                                .start_verified_invocation(&other, &request, None)
                                .map(|_| ())
                        };
                        assert!(
                            conflict
                                .err()
                                .unwrap()
                                .contains("action_invocation_identity_conflict"),
                            "{scenario}: conflicting UUID must fail before app dispatch"
                        );
                        assert!(
                            rx.lock().await.try_recv().is_err(),
                            "only one application delivery"
                        );
                        assert!(
                            fixture
                                .state
                                .verified_invocation_status(&other, request.invocation)
                                .unwrap()
                                .is_none()
                        );
                        assert!(
                            fixture
                                .state
                                .cancel_verified_invocation(&other, request.invocation)
                                .is_err()
                        );
                        let channel = fixture.invocation_channel();
                        let terminal = tilo_actions::InvocationUpdate::Succeeded {
                            invocation: request.invocation,
                            output: tilo_actions::ActionOutput::default(),
                        };
                        update_app(&channel, wire_id, &terminal).await.unwrap();
                        if scenario != "legacy-before-caller" {
                            let duplicate = fixture
                                .state
                                .start_verified_invocation(&caller, &request, None)
                                .unwrap();
                            assert_eq!(duplicate.wire_id, wire_id);
                            assert_eq!(duplicate.update, terminal);
                        }
                        let membership = fixture.membership.as_ref().unwrap().clone();
                        drop(channel);
                        drop(fixture);
                        let (mut reopened, _, mut again) =
                            durable_fixture(root.path(), Some(membership));
                        again.invocation = request.invocation;
                        assert!(
                            reopened
                                .state
                                .start_verified_invocation(&other, &again, None)
                                .is_err(),
                            "{scenario}: UUID reservation survives terminal completion and restart"
                        );
                        assert!(reopened.invocation_rx.try_recv().is_err());
                        assert_eq!(
                            std::fs::read_to_string(root.path().join("effect-counter")).unwrap(),
                            "1"
                        );
                    }
                })
                .await;
        }

        #[tokio::test(flavor = "current_thread")]
        async fn invocation_journal_real_effect_once_across_concurrent_duplicate_response_loss_and_restart()
         {
            tokio::task::LocalSet::new()
                .run_until(async {
                    let root = tempfile::tempdir().unwrap();
                    let (mut fixture, caller, request) = durable_fixture(root.path(), None);
                    fixture
                        .state
                        .surfaces
                        .lock()
                        .unwrap()
                        .get_mut(&71)
                        .unwrap()
                        .actions[0]
                        .contract
                        .result = tilo_actions::ResultSpec::Json {
                        schema: serde_json::json!({"type":"object"}),
                    };
                    let requests = (0..8)
                        .map(|_| {
                            let state = fixture.state.clone();
                            let caller = caller.clone();
                            let request = request.clone();
                            std::thread::spawn(move || {
                                state
                                    .start_verified_invocation(&caller, &request, None)
                                    .unwrap()
                            })
                        })
                        .collect::<Vec<_>>();
                    let admitted = requests
                        .into_iter()
                        .map(|t| t.join().unwrap())
                        .collect::<Vec<_>>();
                    assert!(
                        admitted
                            .iter()
                            .all(|row| row.wire_id == admitted[0].wire_id)
                    );
                    let (_, empty_rx) = mpsc::unbounded_channel();
                    let rx = Rc::new(AsyncMutex::new(std::mem::replace(
                        &mut fixture.invocation_rx,
                        empty_rx,
                    )));
                    let stream: invocation_stream::Client =
                        capnp_rpc::new_client(InvocationStreamServer { rx: rx.clone() });
                    let response = stream.read_request().send().promise.await.unwrap();
                    let delivered = response.get().unwrap();
                    let delivered_id = delivered.get_id();
                    assert_eq!(
                        delivered.get_invocation_uuid().unwrap(),
                        request.invocation.0.as_bytes()
                    );
                    assert!(
                        rx.lock().await.try_recv().is_err(),
                        "one actual Cap'n Proto application delivery"
                    );
                    effect(root.path());
                    let terminal = tilo_actions::InvocationUpdate::Succeeded {
                        invocation: request.invocation,
                        output: tilo_actions::ActionOutput {
                            value: Some(
                                serde_json::json!({"counter":1,"private":"private-result-token"}),
                            ),
                            resources: vec![],
                        },
                    };
                    // Force receipt persistence to fail independently of the already opened journal.
                    fixture.membership.as_ref().unwrap().write().persist_path =
                        Some(root.path().to_owned());
                    fixture
                        .complete_invocation(delivered_id, &terminal)
                        .await
                        .unwrap();
                    // Discard the original response and retry under freshly verified caller custody.
                    let duplicate = fixture
                        .state
                        .start_verified_invocation(&caller, &request, None)
                        .unwrap();
                    assert_eq!(duplicate.update, terminal);
                    assert!(
                        fixture
                            .state
                            .invocation_statuses(&caller.admitted.principal_id)
                            .is_empty(),
                        "legacy principal-only lookup cannot reveal certified terminal output"
                    );
                    let mut changed = request.clone();
                    changed.argument = Some(String::new());
                    assert!(
                        fixture
                            .state
                            .start_verified_invocation(&caller, &changed, None)
                            .is_err()
                    );
                    let membership = fixture.membership.clone().unwrap();
                    {
                        let mut tree = membership.write();
                        assert_eq!(tree.workspaces[0].effect_receipts.len(), 1);
                        assert!(
                            !serde_json::to_string(&tree.workspaces[0].effect_receipts)
                                .unwrap()
                                .contains("private-result-token")
                        );
                        tree.persist_path = Some(root.path().join("state.json"));
                    }
                    drop(fixture);
                    let (mut restarted, caller, _) = durable_fixture(root.path(), Some(membership));
                    assert_eq!(
                        restarted
                            .state
                            .start_verified_invocation(&caller, &request, None)
                            .unwrap()
                            .update,
                        terminal
                    );
                    assert!(restarted.invocation_rx.try_recv().is_err());
                    assert_eq!(
                        std::fs::read_to_string(root.path().join("effect-counter")).unwrap(),
                        "1"
                    );
                })
                .await;
        }

        #[tokio::test(flavor = "current_thread")]
        async fn invocation_journal_crash_boundaries_never_repeat_real_effect() {
            tokio::task::LocalSet::new().run_until(async {
                for boundary in [BarrierFailure::File, BarrierFailure::AfterAdmission, BarrierFailure::Terminal] {
                    let root = tempfile::tempdir().unwrap();
                    let (mut fixture, caller, request) = durable_fixture(root.path(), None);
                    if boundary != BarrierFailure::Terminal {
                        fixture.state.journal.lock().unwrap().as_mut().unwrap().as_mut().unwrap().fail_at = Some(boundary);
                        assert!(fixture.state.start_verified_invocation(&caller, &request, None).is_err());
                        assert!(fixture.invocation_rx.try_recv().is_err());
                    } else {
                        fixture.state.start_verified_invocation(&caller, &request, None).unwrap();
                        let delivered = fixture.invocation_rx.try_recv().unwrap();
                        effect(root.path());
                        fixture.state.journal.lock().unwrap().as_mut().unwrap().as_mut().unwrap().fail_at = Some(BarrierFailure::File);
                        assert!(fixture.complete_invocation(delivered.id, &tilo_actions::InvocationUpdate::Succeeded { invocation: request.invocation, output: tilo_actions::ActionOutput::default() }).await.is_err());
                    }
                    let membership = fixture.membership.clone().unwrap();
                    drop(fixture);
                    let (mut restarted, caller, _) = durable_fixture(root.path(), Some(membership));
                    if boundary == BarrierFailure::File {
                        assert!(restarted.state.verified_invocation_status(&caller, request.invocation).unwrap().is_none());
                        assert!(!root.path().join("effect-counter").exists());
                    } else {
                        let result = restarted.state.start_verified_invocation(&caller, &request, None).unwrap();
                        assert!(result.uncertain);
                        assert!(matches!(result.update, tilo_actions::InvocationUpdate::Failed { error, .. } if error.code == "action_outcome_unknown" && !error.retryable));
                        assert!(restarted.invocation_rx.try_recv().is_err());
                        if boundary == BarrierFailure::Terminal {
                            assert_eq!(std::fs::read_to_string(root.path().join("effect-counter")).unwrap(), "1");
                        } else { assert!(!root.path().join("effect-counter").exists()); }
                    }
                }
            }).await;
        }

        #[test]
        fn invocation_journal_owner_is_device_scoped_and_live_authority_precedes_duplicate_disclosure()
         {
            let root = tempfile::tempdir().unwrap();
            let (mut fixture, caller, request) = durable_fixture(root.path(), None);
            fixture
                .state
                .start_verified_invocation(&caller, &request, None)
                .unwrap();
            fixture.invocation_rx.try_recv().unwrap();
            let other_device = SigningKey::from_bytes(&[53; 32]);
            let other = fixture
                .state
                .verify_action_caller(
                    &fixture.cluster_id,
                    &certified_join(
                        &fixture.cluster_id,
                        &SigningKey::from_bytes(&[51; 32]),
                        &other_device,
                    )
                    .encode()
                    .unwrap(),
                )
                .unwrap();
            assert!(
                fixture
                    .state
                    .verified_invocation_status(&other, request.invocation)
                    .unwrap()
                    .is_none()
            );
            assert!(
                fixture
                    .state
                    .cancel_verified_invocation(&other, request.invocation)
                    .is_err()
            );
            assert!(
                fixture
                    .state
                    .invocation_statuses(&caller.admitted.principal_id)
                    .is_empty(),
                "legacy root-scoped queries must not disclose another device's private results"
            );
            let mut bad = request.clone();
            bad.invocation = tilo_actions::InvocationId::new();
            bad.record_id = "different-record".into();
            assert!(
                fixture
                    .state
                    .start_verified_invocation(&caller, &bad, None)
                    .is_err()
            );
            fixture.membership.as_ref().unwrap().write().workspaces[0].set_owner("new-owner");
            assert!(
                fixture
                    .state
                    .start_verified_invocation(&caller, &request, None)
                    .is_err()
            );
            assert!(
                fixture
                    .state
                    .verified_invocation_status(&caller, request.invocation)
                    .is_err()
            );
            assert!(fixture.invocation_rx.try_recv().is_err());
        }

        #[test]
        fn invocation_journal_old_apps_wrong_rooms_and_silence_fail_safely() {
            for disconnected in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let (mut fixture, caller, request) = durable_fixture(root.path(), None);
                fixture
                    .state
                    .stable_invocation_subscribers
                    .lock()
                    .unwrap()
                    .clear();
                assert!(
                    fixture
                        .state
                        .start_verified_invocation(&caller, &request, None)
                        .unwrap_err()
                        .contains("stable_identity_unavailable")
                );
                assert!(fixture.invocation_rx.try_recv().is_err());
                fixture
                    .state
                    .stable_invocation_subscribers
                    .lock()
                    .unwrap()
                    .insert(71);
                let original_room = fixture
                    .state
                    .surfaces
                    .lock()
                    .unwrap()
                    .get(&71)
                    .unwrap()
                    .route
                    .cluster_id
                    .clone();
                fixture
                    .state
                    .surfaces
                    .lock()
                    .unwrap()
                    .get_mut(&71)
                    .unwrap()
                    .route
                    .cluster_id = uuid::Uuid::new_v4().to_string();
                assert!(
                    fixture
                        .state
                        .start_verified_invocation(&caller, &request, None)
                        .is_err()
                );
                fixture
                    .state
                    .surfaces
                    .lock()
                    .unwrap()
                    .get_mut(&71)
                    .unwrap()
                    .route
                    .cluster_id = original_room;
                let admitted = fixture
                    .state
                    .start_verified_invocation(&caller, &request, None)
                    .unwrap();
                fixture.invocation_rx.try_recv().unwrap();
                fixture
                    .state
                    .cancel_verified_invocation(&caller, request.invocation)
                    .unwrap();
                if disconnected {
                    fixture.state.remove_connection(71);
                } else {
                    fixture.state.expire_stale_invocations_at(
                        std::time::Instant::now(),
                        Duration::ZERO,
                        Duration::ZERO,
                    );
                }
                let status = fixture
                    .state
                    .verified_invocation_status(&caller, request.invocation)
                    .unwrap()
                    .unwrap();
                assert!(status.uncertain);
                assert!(
                    matches!(status.update, tilo_actions::InvocationUpdate::Failed { error, .. } if error.code == "action_outcome_unknown")
                );
                assert_eq!(
                    fixture
                        .state
                        .start_verified_invocation(&caller, &request, None)
                        .unwrap()
                        .wire_id,
                    admitted.wire_id
                );
                assert!(fixture.invocation_rx.try_recv().is_err());
            }
        }

        #[tokio::test(flavor = "current_thread")]
        async fn invocation_journal_native_stream_negotiates_uuid_capability_explicitly() {
            tokio::task::LocalSet::new()
                .run_until(async {
                    let root = tempfile::tempdir().unwrap();
                    let (fixture, caller, request) = durable_fixture(root.path(), None);
                    let client: channel_handle::Client =
                        capnp_rpc::new_client(CapnpChannelHandle {
                            state: fixture.state.clone(),
                            directories: fixture.directories.clone(),
                            channel_id: fixture.route.channel_key(),
                            peer_id: fixture.peer_id,
                            verified_identity: caller.admitted.clone(),
                            lease: None,
                        });
                    let legacy = client
                        .subscribe_invocations_request()
                        .send()
                        .promise
                        .await
                        .unwrap();
                    let _legacy_stream = legacy.get().unwrap().get_stream().unwrap();
                    assert!(
                        fixture
                            .state
                            .start_verified_invocation(&caller, &request, None)
                            .unwrap_err()
                            .contains("stable_identity_unavailable")
                    );
                    let mut subscription = client.subscribe_invocations_request();
                    subscription.get().set_stable_invocation_ids(true);
                    let capable = subscription.send().promise.await.unwrap();
                    let stream = capable.get().unwrap().get_stream().unwrap();
                    fixture
                        .state
                        .start_verified_invocation(&caller, &request, None)
                        .unwrap();
                    let reply = stream.read_request().send().promise.await.unwrap();
                    assert_eq!(
                        reply.get().unwrap().get_invocation_uuid().unwrap(),
                        request.invocation.0.as_bytes()
                    );
                })
                .await;
        }

        #[tokio::test(flavor = "current_thread")]
        async fn invocation_journal_invalid_updates_never_persist_and_legacy_completion_still_works()
         {
            tokio::task::LocalSet::new().run_until(async {
                let root = tempfile::tempdir().unwrap();
                let (mut fixture, caller, request) = durable_fixture(root.path(), None);
                let channel = fixture.invocation_channel();
                let admitted = fixture.state.start_verified_invocation(&caller, &request, None).unwrap();
                fixture.invocation_rx.try_recv().unwrap();
                let foreign: channel_handle::Client = capnp_rpc::new_client(CapnpChannelHandle {
                    state: fixture.state.clone(), directories: fixture.directories.clone(),
                    channel_id: fixture.route.channel_key(), peer_id: 999,
                    verified_identity: caller.admitted.clone(),
                    lease: None,
                });
                assert!(update_app(&foreign, admitted.wire_id, &tilo_actions::InvocationUpdate::Succeeded {
                    invocation: request.invocation, output: tilo_actions::ActionOutput::default(),
                }).await.is_err());
                let invalid = [
                    tilo_actions::InvocationUpdate::Accepted { invocation: request.invocation },
                    tilo_actions::InvocationUpdate::Progress { invocation: request.invocation, sequence: 1, phase: "".into(), completed: None, total: None, message: None },
                    tilo_actions::InvocationUpdate::Progress { invocation: request.invocation, sequence: 1, phase: "working".into(), completed: Some(2), total: Some(1), message: None },
                    tilo_actions::InvocationUpdate::Item { invocation: request.invocation, sequence: 1, value: serde_json::json!({"private":"value"}) },
                    tilo_actions::InvocationUpdate::Failed { invocation: request.invocation, error: tilo_actions::ActionFailure { code: "bad".into(), message: "failed".into(), details: None, retryable: true } },
                    tilo_actions::InvocationUpdate::Succeeded { invocation: request.invocation, output: tilo_actions::ActionOutput { value: Some(serde_json::json!("undeclared")), resources: vec![] } },
                ];
                for update in invalid {
                    assert!(update_app(&channel, admitted.wire_id, &update).await.is_err());
                    assert!(matches!(fixture.state.verified_invocation_status(&caller, request.invocation).unwrap().unwrap().update, tilo_actions::InvocationUpdate::Accepted { .. }));
                }
                let mut complete = channel.complete_invocation_request();
                complete.get().set_id(admitted.wire_id);
                complete.get().set_code(""); complete.get().set_message("");
                complete.send().promise.await.unwrap();
                assert!(matches!(fixture.state.verified_invocation_status(&caller, request.invocation).unwrap().unwrap().update, tilo_actions::InvocationUpdate::Succeeded { invocation, .. } if invocation == request.invocation));

                // The legacy numeric local caller still receives and completes its original handle.
                let legacy = fixture.state.start_invocation(71, &request.resource, &request.action, None, &caller.admitted.principal_id).unwrap();
                let delivered = fixture.invocation_rx.try_recv().unwrap();
                assert_eq!(delivered.invocation.0.as_u128(), u128::from(legacy.id));
                let mut complete = channel.complete_invocation_request();
                complete.get().set_id(legacy.id); complete.get().set_code(""); complete.get().set_message("");
                complete.send().promise.await.unwrap();
                assert!(legacy.completion.recv().unwrap().code.is_empty());
            }).await;
        }

        #[tokio::test(flavor = "current_thread")]
        async fn invocation_journal_immediate_progress_and_malformed_stream_items_are_rejected() {
            tokio::task::LocalSet::new()
                .run_until(async {
                    for streaming in [false, true] {
                        let root = tempfile::tempdir().unwrap();
                        let (mut fixture, caller, request) = durable_fixture(root.path(), None);
                        let channel = fixture.invocation_channel();
                        fixture
                            .state
                            .surfaces
                            .lock()
                            .unwrap()
                            .get_mut(&71)
                            .unwrap()
                            .actions[0]
                            .contract
                            .execution = if streaming {
                            tilo_actions::ExecutionMode::Streaming {
                                cancellable: true,
                                item_schema: serde_json::json!({"type":"integer"}),
                            }
                        } else {
                            tilo_actions::ExecutionMode::Immediate
                        };
                        if !streaming {
                            fixture
                                .state
                                .surfaces
                                .lock()
                                .unwrap()
                                .get_mut(&71)
                                .unwrap()
                                .actions[0]
                                .contract
                                .result = tilo_actions::ResultSpec::None;
                        }
                        let admitted = fixture
                            .state
                            .start_verified_invocation(&caller, &request, None)
                            .unwrap();
                        fixture.invocation_rx.try_recv().unwrap();
                        let bad = if streaming {
                            tilo_actions::InvocationUpdate::Item {
                                invocation: request.invocation,
                                sequence: 1,
                                value: serde_json::json!("wrong type"),
                            }
                        } else {
                            tilo_actions::InvocationUpdate::Progress {
                                invocation: request.invocation,
                                sequence: 1,
                                phase: "working".into(),
                                completed: None,
                                total: None,
                                message: None,
                            }
                        };
                        assert!(update_app(&channel, admitted.wire_id, &bad).await.is_err());
                        assert!(matches!(
                            fixture
                                .state
                                .verified_invocation_status(&caller, request.invocation)
                                .unwrap()
                                .unwrap()
                                .update,
                            tilo_actions::InvocationUpdate::Accepted { .. }
                        ));
                        if streaming {
                            let item = tilo_actions::InvocationUpdate::Item {
                                invocation: request.invocation,
                                sequence: 1,
                                value: serde_json::json!(7),
                            };
                            update_app(&channel, admitted.wire_id, &item).await.unwrap();
                            assert!(update_app(&channel, admitted.wire_id, &item).await.is_err());
                            assert_eq!(
                                fixture
                                    .state
                                    .verified_invocation_status(&caller, request.invocation)
                                    .unwrap()
                                    .unwrap()
                                    .update,
                                item
                            );
                        }
                    }
                })
                .await;
        }

        #[tokio::test(flavor = "current_thread")]
        async fn invocation_journal_confirmation_uuid_and_lifecycle_remain_authoritative() {
            tokio::task::LocalSet::new()
                .run_until(async {
                    let root = tempfile::tempdir().unwrap();
                    let (mut fixture, caller, request) = durable_fixture(root.path(), None);
                    let channel = fixture.invocation_channel();
                    fixture
                        .state
                        .surfaces
                        .lock()
                        .unwrap()
                        .get_mut(&71)
                        .unwrap()
                        .actions[0]
                        .contract
                        .confirmation = tilo_actions::ConfirmationPolicy::Phrase {
                        phrase: "erase exactly".into(),
                    };
                    let actor = &caller.admitted.principal_id;
                    assert!(
                        fixture
                            .state
                            .start_verified_invocation(&caller, &request, None)
                            .is_err()
                    );
                    assert!(
                        fixture
                            .state
                            .prepare_confirmation(
                                71,
                                &request.resource,
                                &request.action,
                                None,
                                actor,
                                Some("erase")
                            )
                            .is_err()
                    );
                    let token = fixture
                        .state
                        .prepare_confirmation(
                            71,
                            &request.resource,
                            &request.action,
                            None,
                            actor,
                            Some("erase exactly"),
                        )
                        .unwrap();
                    let admitted = fixture
                        .state
                        .start_verified_invocation(&caller, &request, Some(&token))
                        .unwrap();
                    fixture
                        .state
                        .start_verified_invocation(&caller, &request, None)
                        .unwrap();
                    let delivered = fixture.invocation_rx.try_recv().unwrap();
                    assert_eq!(delivered.invocation, request.invocation);
                    let mut new = request.clone();
                    new.invocation = tilo_actions::InvocationId::new();
                    assert!(
                        fixture
                            .state
                            .start_verified_invocation(&caller, &new, Some(&token))
                            .is_err()
                    );
                    let progress = tilo_actions::InvocationUpdate::Progress {
                        invocation: request.invocation,
                        sequence: 1,
                        phase: "working".into(),
                        completed: Some(0),
                        total: Some(1),
                        message: None,
                    };
                    update_app(&channel, delivered.id, &progress).await.unwrap();
                    assert!(update_app(&channel, delivered.id, &progress).await.is_err());
                    assert!(
                        update_app(
                            &channel,
                            delivered.id,
                            &tilo_actions::InvocationUpdate::Succeeded {
                                invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(
                                    u128::from(delivered.id)
                                )),
                                output: tilo_actions::ActionOutput::default()
                            }
                        )
                        .await
                        .is_err()
                    );
                    fixture
                        .state
                        .cancel_verified_invocation(&caller, request.invocation)
                        .unwrap();
                    assert!(
                        !fixture
                            .state
                            .verified_invocation_status(&caller, request.invocation)
                            .unwrap()
                            .unwrap()
                            .update
                            .is_terminal(),
                        "request is not acknowledgement"
                    );
                    let cancelled = tilo_actions::InvocationUpdate::Cancelled {
                        invocation: request.invocation,
                        message: None,
                    };
                    update_app(&channel, delivered.id, &cancelled)
                        .await
                        .unwrap();
                    assert_eq!(
                        fixture
                            .state
                            .verified_invocation_status(&caller, request.invocation)
                            .unwrap()
                            .unwrap()
                            .update,
                        cancelled
                    );
                    assert_eq!(admitted.wire_id, delivered.id);
                })
                .await;
        }
    }
}

#[allow(
    unused_imports,
    reason = "The shared policy extension trait is needed by host-specific workspace implementations."
)]
use crate::policy::WorkspacePolicy as _;

impl CapnpBrokerState {
    async fn apply_invocation_update(
        self: &Arc<Self>,
        peer_id: u64,
        id: u64,
        update: tilo_actions::InvocationUpdate,
        identity: Option<(CrdtRoute, AdmittedJoinIdentity)>,
    ) -> Result<(), capnp::Error> {
        let state = self;
        let _owner_order = if state.durable_owner_required {
            Some(state.owner_commit_order.lock().await)
        } else {
            None
        };
        if state.durable_owner_required {
            if let Some((route, identity)) = &identity {
                state.revalidate_admission(route, identity)?;
            }
        }
        let mut registry = state
            .pending_invocations
            .lock()
            .map_err(|_| capnp::Error::failed("invocation registry unavailable".into()))?;
        let pending = registry
            .get_mut(&id)
            .filter(|pending| pending.peer_id == peer_id)
            .ok_or_else(|| capnp::Error::failed("unknown invocation id".into()))?;
        if update.invocation() != pending.update.invocation() {
            return Err(capnp::Error::failed(
                "invocation update identity mismatch".into(),
            ));
        }
        let mut next_sequence = pending.last_sequence;
        if matches!(&update, tilo_actions::InvocationUpdate::Accepted { .. }) {
            return Err(capnp::Error::failed(
                "invocation acceptance is owned by the broker".into(),
            ));
        }
        if let tilo_actions::InvocationUpdate::Progress {
            sequence,
            phase,
            completed,
            total,
            ..
        } = &update
        {
            if matches!(pending.execution, tilo_actions::ExecutionMode::Immediate) {
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
            next_sequence = *sequence;
        }
        if let tilo_actions::InvocationUpdate::Item {
            sequence, value, ..
        } = &update
        {
            let tilo_actions::ExecutionMode::Streaming { item_schema, .. } = &pending.execution
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
            tilo_actions::validate_json_schema(value, item_schema).map_err(|error| {
                capnp::Error::failed(format!(
                    "streamed item violated its published contract: {error}"
                ))
            })?;
            next_sequence = *sequence;
        }
        if let tilo_actions::InvocationUpdate::Succeeded { output, .. } = &update {
            pending.result.validate(output).map_err(|error| {
                capnp::Error::failed(format!(
                    "invocation result violated its published contract: {error}"
                ))
            })?;
        }
        if let tilo_actions::InvocationUpdate::Failed { error, .. } = &update {
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
        if matches!(&update, tilo_actions::InvocationUpdate::Cancelled { .. })
            && !pending.cancellable
        {
            return Err(capnp::Error::failed(
                "non-cancellable action reported cancellation".into(),
            ));
        }
        let staged = pending.clone();
        drop(registry);
        state
            .commit_effect_before_terminal(&staged, &update)
            .await?;
        if state.durable_owner_required {
            if let Some((route, identity)) = &identity {
                state.revalidate_admission(route, identity)?;
            }
        }
        let mut registry = state
            .pending_invocations
            .lock()
            .map_err(|_| capnp::Error::failed("invocation registry unavailable".into()))?;
        let pending = registry
            .get_mut(&id)
            .filter(|pending| pending.peer_id == peer_id && pending.update == staged.update)
            .ok_or_else(|| {
                capnp::Error::failed("invocation changed while committing; outcome unknown".into())
            })?;
        state
            .persist_invocation(pending, update.clone(), false)
            .map_err(capnp::Error::failed)?;
        pending.last_activity = std::time::Instant::now();
        pending.last_sequence = next_sequence;
        pending.update = update.clone();
        if update.is_terminal() {
            let completion = match &update {
                tilo_actions::InvocationUpdate::Succeeded { .. } => InvocationCompletion {
                    code: String::new(),
                    message: String::new(),
                },
                tilo_actions::InvocationUpdate::Failed { error, .. } => InvocationCompletion {
                    code: error.code.clone(),
                    message: error.message.clone(),
                },
                tilo_actions::InvocationUpdate::Cancelled { message, .. } => InvocationCompletion {
                    code: "action_cancelled".into(),
                    message: message.clone().unwrap_or_default(),
                },
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
            state.record_completion(id, pending.completion_owner(), update.clone());
            if let Ok(mut requests) = state.cancellation_requests.lock() {
                requests.remove(&id);
            }
            let _ = pending.tx.send(completion);
            state.record_effect_receipt(&pending, &update);
        }
        Ok(())
    }
}
