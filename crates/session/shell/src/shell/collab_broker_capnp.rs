// `Promise::from_future(async move {..})` refines capnp-rpc's declared
// `Promise<.., capnp::Error>` return type — the intended pattern, but it
// trips `refining_impl_trait`.
#![allow(
    refining_impl_trait,
    reason = "Capnp server implementations return concrete Promise values from from_future to implement the generated async RPC contract."
)]

//! Native Cap'n Proto server for the Tilo collaboration broker.
//!
//! Answers the `runtime-collab` schema on the shell's collaboration socket.
//!
//! `join_channel` uses the caller's `channel_id` verbatim as the routing
//! key. Document traffic has one path: `send` with a certified signed op.
//! `fetchScrollbackFor` answers from the in-memory op log; an
//! exhausted-but-empty batch triggers no fallback push, callers repeat the
//! fetch instead.

use crate::shell::state::WorkspaceAuthority as _;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use capnp::capability::Promise;
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use getrandom::{SysRng, rand_core::UnwrapErr};
use rand_core::Rng;
use serde::Deserialize;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[cfg(test)]
use collab_types::DocumentMutation;
use collab_types::{
    Actor, ArtifactId, ArtifactKind, CapDecision, RedactedMarker, ResourceRef, SignedOp,
    ValidationError, Verb, document_artifact_id, document_attestation_id, role_permits,
    validate_admitted_inbound_op,
};
use runtime_collab::raw::{
    OpKind, action_directory, action_input, channel_handle, collab_service, context_stream,
    invocation_stream, presence_event, presence_handle, presence_payload, presence_stream,
    realtime_stream, scrollback_batch, stream, task_run_stream, workspace_control_handle,
};
use ui::collaboration::{CollabEvent, PresencePayload};
use ui::presence::PresenceKey;

use crate::shell::collab_broker::{
    AuditEntry, BrokerChannels, CrdtRoute, FanoutState, apply_redaction_from_envelope,
};
use crate::shell::collab_outbound::PeerPublisher;
use crate::shell::state::{
    CapRole, Capability, FsMode, GRANT_SESSION, Principal, WorkspaceId, WorkspaceTaskStatus,
    WorkspaceTree, WorkspaceTreeHandle,
};

/// Launcher preambles are deliberately tiny: they only bind a socket to the
/// shell's already-created process record. The cert presented by the
/// app still decides workspace membership and delegated-agent authority.
const MAX_COLLAB_LAUNCH_HANDOFF_BYTES: usize = 1024;
const LAUNCH_RECORD_RACE_RETRIES: u8 = 30;
const LAUNCH_RECORD_RACE_DELAY: Duration = Duration::from_millis(10);
const MAX_LAUNCH_CONTENT_BYTES: usize = 64 * 1024 * 1024;
const NEARBY_TARGET_HANDLE_TTL: Duration = Duration::from_secs(30);
const NEARBY_TRANSFER_HANDLE_TTL: Duration = Duration::from_secs(120);
const MAX_NEARBY_TRANSFER_HANDLES: usize = 256;
const NEARBY_DESTINATION_HANDLE_TTL: Duration = Duration::from_secs(120);
const FILES_APP_ID: &str = "so.tilo.file-explorer";

#[derive(Clone)]
struct NearbyTargetBinding {
    peer_id: u64,
    route: String,
    target: crate::shell::personal_continuity::DeviceId,
    expires_at: Instant,
}

#[derive(Default)]
struct NearbyTargetRegistry {
    bindings: HashMap<runtime_collab::NearbyTargetHandle, NearbyTargetBinding>,
}

impl NearbyTargetRegistry {
    fn begin_snapshot(
        &mut self,
        peer_id: u64,
        route: &CrdtRoute,
        targets: &[crate::shell::personal_continuity::DeviceId],
        now: Instant,
    ) {
        let route = route.channel_key();
        self.bindings.retain(|_, binding| {
            binding.expires_at > now
                && (binding.peer_id != peer_id
                    || binding.route != route
                    || targets.contains(&binding.target))
        });
    }

    fn issue(
        &mut self,
        peer_id: u64,
        route: &CrdtRoute,
        target: crate::shell::personal_continuity::DeviceId,
        now: Instant,
    ) -> runtime_collab::NearbyTargetHandle {
        self.bindings.retain(|_, binding| binding.expires_at > now);
        let channel = route.channel_key();
        if let Some((handle, binding)) = self.bindings.iter_mut().find(|(_, binding)| {
            binding.peer_id == peer_id && binding.route == channel && binding.target == target
        }) {
            binding.expires_at = now + NEARBY_TARGET_HANDLE_TTL;
            return *handle;
        }
        loop {
            let mut bytes = [0_u8; 16];
            UnwrapErr(SysRng).fill_bytes(&mut bytes);
            let handle = runtime_collab::NearbyTargetHandle::from_issued_bytes(bytes);
            if handle.valid() && !self.bindings.contains_key(&handle) {
                self.bindings.insert(
                    handle,
                    NearbyTargetBinding {
                        peer_id,
                        route: route.channel_key(),
                        target,
                        expires_at: now + NEARBY_TARGET_HANDLE_TTL,
                    },
                );
                return handle;
            }
        }
    }

    fn consume(
        &mut self,
        handle: runtime_collab::NearbyTargetHandle,
        peer_id: u64,
        route: &CrdtRoute,
        now: Instant,
    ) -> Option<crate::shell::personal_continuity::DeviceId> {
        self.bindings.retain(|_, binding| binding.expires_at > now);
        let binding = self.bindings.get(&handle)?;
        if binding.peer_id != peer_id || binding.route != route.channel_key() {
            return None;
        }
        self.bindings.remove(&handle).map(|binding| binding.target)
    }

    fn remove_peer(&mut self, peer_id: u64) {
        self.bindings
            .retain(|_, binding| binding.peer_id != peer_id);
    }
}

#[derive(Clone)]
struct NearbyTransferBinding {
    peer_id: u64,
    route: String,
    transfer_id: [u8; 16],
    direction: runtime_collab::NearbyTransferDirection,
    expires_at: Instant,
}

#[derive(Default)]
struct NearbyTransferRegistry {
    bindings: HashMap<runtime_collab::NearbyTransferHandle, NearbyTransferBinding>,
}

#[derive(Clone)]
struct NearbyDestinationBinding {
    peer_id: u64,
    route: String,
    grant: PathBuf,
    label: String,
    remembered: bool,
    expires_at: Instant,
}

#[derive(Default)]
struct NearbyDestinationRegistry {
    bindings: HashMap<runtime_collab::NearbyDestinationHandle, NearbyDestinationBinding>,
}

impl NearbyDestinationRegistry {
    fn issue(
        &mut self,
        peer_id: u64,
        route: &CrdtRoute,
        grant: PathBuf,
        label: String,
        now: Instant,
    ) -> runtime_collab::NearbyTransferDestination {
        self.bindings.retain(|_, binding| binding.expires_at > now);
        if let Some((handle, binding)) = self.bindings.iter_mut().find(|(_, binding)| {
            binding.peer_id == peer_id
                && binding.route == route.channel_key()
                && binding.grant == grant
        }) {
            binding.expires_at = now + NEARBY_DESTINATION_HANDLE_TTL;
            return runtime_collab::NearbyTransferDestination {
                handle: *handle,
                label: binding.label.clone(),
                remembered_for_session: binding.remembered,
            };
        }
        loop {
            let mut bytes = [0_u8; 16];
            UnwrapErr(SysRng).fill_bytes(&mut bytes);
            let handle = runtime_collab::NearbyDestinationHandle::from_issued_bytes(bytes);
            if handle.valid() && !self.bindings.contains_key(&handle) {
                self.bindings.insert(
                    handle,
                    NearbyDestinationBinding {
                        peer_id,
                        route: route.channel_key(),
                        grant,
                        label: label.clone(),
                        remembered: false,
                        expires_at: now + NEARBY_DESTINATION_HANDLE_TTL,
                    },
                );
                return runtime_collab::NearbyTransferDestination {
                    handle,
                    label,
                    remembered_for_session: false,
                };
            }
        }
    }

    fn resolve(
        &mut self,
        handle: runtime_collab::NearbyDestinationHandle,
        peer_id: u64,
        route: &CrdtRoute,
        remember: bool,
        now: Instant,
    ) -> Option<crate::shell::nearby_transfer::SecureNearbyDestination> {
        self.bindings.retain(|_, binding| binding.expires_at > now);
        let binding = self.bindings.get_mut(&handle)?;
        if binding.peer_id != peer_id || binding.route != route.channel_key() {
            return None;
        }
        binding.remembered |= remember;
        binding.expires_at = now + NEARBY_DESTINATION_HANDLE_TTL;
        crate::shell::nearby_transfer::SecureNearbyDestination::open_grant(&binding.grant).ok()
    }

    fn remove_peer(&mut self, peer_id: u64) {
        self.bindings
            .retain(|_, binding| binding.peer_id != peer_id);
    }
}

impl NearbyTransferRegistry {
    fn begin_snapshot(
        &mut self,
        peer_id: u64,
        route: &CrdtRoute,
        visible_ids: &[[u8; 16]],
        now: Instant,
    ) {
        let route = route.channel_key();
        self.bindings.retain(|_, binding| {
            binding.expires_at > now
                && (binding.peer_id != peer_id
                    || binding.route != route
                    || visible_ids.contains(&binding.transfer_id))
        });
    }

    fn issue(
        &mut self,
        peer_id: u64,
        route: &CrdtRoute,
        transfer_id: [u8; 16],
        direction: runtime_collab::NearbyTransferDirection,
        allow_claim: bool,
        now: Instant,
    ) -> Option<runtime_collab::NearbyTransferHandle> {
        self.bindings.retain(|_, binding| binding.expires_at > now);
        if let Some((handle, binding)) = self
            .bindings
            .iter_mut()
            .find(|(_, binding)| binding.transfer_id == transfer_id)
        {
            if binding.peer_id != peer_id || binding.route != route.channel_key() {
                return None;
            }
            binding.expires_at = now + NEARBY_TRANSFER_HANDLE_TTL;
            return Some(*handle);
        }
        if !allow_claim || self.bindings.len() >= MAX_NEARBY_TRANSFER_HANDLES {
            return None;
        }
        loop {
            let mut bytes = [0_u8; 16];
            UnwrapErr(SysRng).fill_bytes(&mut bytes);
            let handle = runtime_collab::NearbyTransferHandle::from_issued_bytes(bytes);
            if handle.valid() && !self.bindings.contains_key(&handle) {
                self.bindings.insert(
                    handle,
                    NearbyTransferBinding {
                        peer_id,
                        route: route.channel_key(),
                        transfer_id,
                        direction,
                        expires_at: now + NEARBY_TRANSFER_HANDLE_TTL,
                    },
                );
                return Some(handle);
            }
        }
    }

    fn resolve(
        &mut self,
        handle: runtime_collab::NearbyTransferHandle,
        peer_id: u64,
        route: &CrdtRoute,
        direction: runtime_collab::NearbyTransferDirection,
        now: Instant,
    ) -> Option<[u8; 16]> {
        self.bindings.retain(|_, binding| binding.expires_at > now);
        let binding = self.bindings.get_mut(&handle)?;
        if binding.peer_id != peer_id
            || binding.route != route.channel_key()
            || binding.direction != direction
        {
            return None;
        }
        binding.expires_at = now + NEARBY_TRANSFER_HANDLE_TTL;
        Some(binding.transfer_id)
    }

    fn remove_peer(&mut self, peer_id: u64) {
        self.bindings
            .retain(|_, binding| binding.peer_id != peer_id);
    }
}

fn nearby_source_from_grants(
    raw_path: &[u8],
    grants: &[Capability],
) -> Result<crate::shell::nearby_transfer::SecureNearbySource, String> {
    use std::os::unix::ffi::OsStringExt as _;

    let source = PathBuf::from(std::ffi::OsString::from_vec(raw_path.to_vec()));
    if !source.is_absolute() {
        return Err("Nearby Send source must be an absolute Files grant path".into());
    }
    grants
        .iter()
        .find_map(|grant| {
            let Capability::Filesystem { path, .. } = grant else {
                return None;
            };
            crate::shell::nearby_transfer::SecureNearbySource::open_beneath(path, &source).ok()
        })
        .ok_or_else(|| "Nearby Send source is outside this Files instance's grants".into())
}

mod action_host;

fn nearby_policy_enabled(
    policy: crate::shell::continuity_policy::ContinuityPolicySnapshot,
) -> bool {
    policy.permits(crate::shell::continuity_policy::ContinuityCapability::FileTransfer)
}

fn stage_launch_content(
    app_id: &str,
    content_hash: &str,
    workspace: &mut crate::shell::state::Workspace,
) -> Result<Option<PathBuf>, String> {
    if content_hash.is_empty() {
        return Ok(None);
    }
    if content_hash.len() != 64
        || !content_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("launch content hash must be a lowercase 64-character blake3 value".into());
    }
    let bytes = qdoc::blobstore::get(content_hash)
        .ok_or_else(|| "launch content is unavailable on this device".to_owned())?;
    if bytes.is_empty()
        || bytes.len() > MAX_LAUNCH_CONTENT_BYTES
        || !qdoc::blobstore::verify(content_hash, &bytes)
    {
        return Err("launch content failed integrity validation".into());
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join("tilo-opened");
    std::fs::create_dir_all(&dir).map_err(|error| format!("create launch staging: {error}"))?;
    #[cfg(unix)]
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("protect launch staging: {error}"))?;
    let extension = match qdoc::blobstore::sniff(&bytes).mime.as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ if bytes.get(4..8).is_some_and(|header| header == b"ftyp") => "mp4",
        _ if bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) => "webm",
        _ if bytes.starts_with(b"RIFF")
            && bytes.get(8..12).is_some_and(|header| header == b"AVI ") =>
        {
            "avi"
        }
        _ => "bin",
    };
    let path = dir.join(format!("{content_hash}.{extension}"));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(|error| format!("stage launch content: {error}"))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|error| format!("inspect existing launch content: {error}"))?;
            if !metadata.file_type().is_file() {
                return Err("existing launch content is not a regular file".into());
            }
            let existing = std::fs::read(&path)
                .map_err(|error| format!("read existing launch content: {error}"))?;
            if existing.len() > MAX_LAUNCH_CONTENT_BYTES
                || !qdoc::blobstore::verify(content_hash, &existing)
            {
                return Err("existing launch content failed integrity validation".into());
            }
        }
        Err(error) => return Err(format!("stage launch content: {error}")),
    }
    workspace.grant_app(
        app_id,
        &Capability::Filesystem {
            path: dir,
            mode: FsMode::Read,
            scope: GRANT_SESSION,
        },
    );
    Ok(Some(path))
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

    /// The caller this admitted connection is, confined to its room.
    fn caller(&self, room: &str) -> tilo_actions::remote::Caller {
        match &self.agent_id {
            Some(agent) => tilo_actions::remote::Caller::local_agent(agent.clone()),
            None => tilo_actions::remote::Caller::local_person(self.principal_id.clone()),
        }
        .in_room(room)
    }

    fn policy_actor(&self, room: &str) -> Actor {
        match &self.agent_id {
            Some(agent) => Actor::agent(agent),
            None => Actor::person(Some(self.role), self.principal_id.clone()),
        }
        .in_room(room.to_string())
    }
}

pub(crate) fn principal_id_for_root(user_public_key: [u8; 32]) -> String {
    match Principal::person_from_key(collab_types::Ed25519PublicKey::from_bytes(user_public_key)) {
        Principal::Person(id) => id,
        _ => hex::encode(user_public_key),
    }
}

/// The capability verb an action's declared permission requires, if any.
fn permission_verb(requirement: tilo_actions::PermissionRequirement) -> Option<Verb> {
    match requirement {
        tilo_actions::PermissionRequirement::None => None,
        tilo_actions::PermissionRequirement::Read => Some(Verb::Read),
        tilo_actions::PermissionRequirement::Comment => Some(Verb::Comment),
        tilo_actions::PermissionRequirement::Write => Some(Verb::Write),
        tilo_actions::PermissionRequirement::Invite => Some(Verb::Invite),
        tilo_actions::PermissionRequirement::Admin => Some(Verb::Admin),
    }
}

fn admit_verified_join(
    tree: &WorkspaceTree,
    route: &CrdtRoute,
    verified: VerifiedJoinIdentity,
) -> Result<AdmittedJoinIdentity, String> {
    let workspace = tree
        .desktop
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
        .desktop
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
) -> Result<AdmittedJoinIdentity, String> {
    let Some(presentation) = runtime_collab::CertifiedJoinIdentity::decode(identity_bytes)
        .map_err(|error| format!("certified join presentation is malformed: {error}"))?
    else {
        return Err("collaboration join requires a certified device presentation".into());
    };
    let cert: tilo_user_identity::DeviceCert =
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
    pub view: Option<tilo_actions::ResourceViewDescriptor>,
    pub input: PublishedInput,
}

fn validate_view_action_binding(action: &PublishedAction, role: &str) -> Result<(), capnp::Error> {
    if action.operation_kind != tilo_actions::OperationKind::View {
        return Err(capnp::Error::failed(format!(
            "resource view {role} action must be a view operation"
        )));
    }
    if action.contract.confirmation != tilo_actions::ConfirmationPolicy::None {
        return Err(capnp::Error::failed(format!(
            "resource view {role} action must not require confirmation"
        )));
    }
    if action.params.iter().any(|parameter| parameter.required) {
        return Err(capnp::Error::failed(format!(
            "resource view {role} action must not require caller parameters"
        )));
    }
    Ok(())
}

/// Recheck the resource-view invariants after decoding an untrusted action
/// surface. A view with a mount must carry that action in the same atomic
/// publication/page so the broker can prove the binding before exposing it.
fn validate_resource_view_descriptors(actions: &[PublishedAction]) -> Result<(), capnp::Error> {
    #[derive(Default)]
    struct ResourceViews<'a> {
        ids: std::collections::HashSet<&'a str>,
        open_actions: std::collections::HashSet<&'a str>,
        primary_count: usize,
    }

    let mut resources = std::collections::HashMap::<&str, ResourceViews<'_>>::new();
    for action in actions {
        let Some(view) = &action.view else {
            continue;
        };
        if view.id.is_empty() || view.id.contains(tilo_actions::KEY_SEP) {
            return Err(capnp::Error::failed(
                "resource view id must be non-empty and contain no separator".into(),
            ));
        }
        if view.label.trim().is_empty() {
            return Err(capnp::Error::failed(
                "resource view label must be non-empty".into(),
            ));
        }
        if view.open_action != action.action {
            return Err(capnp::Error::failed(
                "resource view must be attached to its open action".into(),
            ));
        }
        validate_view_action_binding(action, "open")?;

        let seen = resources.entry(&action.resource).or_default();
        if !seen.ids.insert(&view.id) {
            return Err(capnp::Error::failed(
                "resource view ids must be unique per resource".into(),
            ));
        }
        if !seen.open_actions.insert(&view.open_action) {
            return Err(capnp::Error::failed(
                "at most one resource view may reference an open action".into(),
            ));
        }
        if view.primary {
            seen.primary_count = seen.primary_count.saturating_add(1);
            if seen.primary_count > 1 {
                return Err(capnp::Error::failed(
                    "at most one resource view may be primary".into(),
                ));
            }
        }

        let Some(mount) = &view.mount else {
            continue;
        };
        if mount.action.trim().is_empty()
            || mount.capability_param.trim().is_empty()
            || mount.view_param.trim().is_empty()
            || mount.capability_param == mount.view_param
        {
            return Err(capnp::Error::failed(
                "resource view mount requires an action and distinct parameter names".into(),
            ));
        }
        let mount_action = actions.iter().find(|candidate| {
            candidate.resource == action.resource && candidate.action == mount.action
        });
        let Some(mount_action) = mount_action else {
            return Err(capnp::Error::failed(
                "resource view mount action is not exported by its resource".into(),
            ));
        };
        validate_view_action_binding(mount_action, "mount")?;
        for (role, name) in [
            ("capability", mount.capability_param.as_str()),
            ("view", mount.view_param.as_str()),
        ] {
            if !mount_action.params.iter().any(|parameter| {
                parameter.name.as_ref() == name
                    && parameter.ty == tilo_actions::ParamType::Text
                    && !parameter.required
            }) {
                return Err(capnp::Error::failed(format!(
                    "resource view mount {role} parameter `{name}` must name an optional text parameter"
                )));
            }
        }
    }
    Ok(())
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
pub(crate) struct InvocationEnvelope {
    id: u64,
    pub(crate) resource: String,
    action: String,
    argument: Option<String>,
    pub(crate) actor: String,
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

struct PendingInvocation {
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
        self.directories.borrow_mut().remove(&peer_id);
    }
}

/// Inert result returned from the broker `LocalSet` to the action host.  It
/// never carries a Cap'n Proto capability across threads.
#[derive(Debug)]
pub(crate) enum ActionDirectoryBridgeResponse {
    Page(runtime_collab::ActionDirectoryPage),
    Resolution(runtime_collab::ActionDirectoryResolution),
}

/// A bounded bridge failure.  `Busy` is deliberate backpressure: palette
/// code must retain its last cached results rather than blocking a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActionDirectoryBridgeError {
    Busy,
    InvalidRequest,
    Unavailable,
    TimedOut,
}

pub(crate) type ActionDirectoryBridgeReply =
    Result<ActionDirectoryBridgeResponse, ActionDirectoryBridgeError>;

enum ActionDirectoryBridgeRequest {
    Query {
        actor: BrokerActor,
        cluster_id: String,
        query: runtime_collab::ActionDirectoryQuery,
        reply: tilo_actions::remote::Completer<ActionDirectoryBridgeReply>,
    },
    Resolve {
        actor: BrokerActor,
        cluster_id: String,
        peer_id: u64,
        revision: String,
        resource: String,
        action: String,
        reply: tilo_actions::remote::Completer<ActionDirectoryBridgeReply>,
    },
}

/// The action host's way to an application's directory capability.  The
/// receiver is drained only by the broker's `LocalSet`, where its Rc-backed
/// capability clients remain valid; the answer arrives as a
/// [`tilo_actions::remote::Pending`], completed on that `LocalSet`.
#[derive(Clone)]
pub(crate) struct ActionDirectoryRequestQueue {
    sender: std::sync::mpsc::SyncSender<ActionDirectoryBridgeRequest>,
}

impl ActionDirectoryRequestQueue {
    const CAPACITY: usize = 64;

    /// # Errors
    ///
    /// Returns an error when the query is invalid, the bounded broker queue is
    /// full, or the broker is unavailable.
    pub(crate) fn try_query(
        &self,
        actor: BrokerActor,
        cluster_id: impl Into<String>,
        query: runtime_collab::ActionDirectoryQuery,
    ) -> Result<tilo_actions::remote::Pending<ActionDirectoryBridgeReply>, ActionDirectoryBridgeError>
    {
        if validate_directory_query_input(&query).is_err() {
            return Err(ActionDirectoryBridgeError::InvalidRequest);
        }
        let (reply, response) = tilo_actions::remote::pending();
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
        reason = "Directory resolution identifies the actor, workspace cluster, peer, revision, resource, and action independently."
    )]
    /// # Errors
    ///
    /// Returns an error when the target is invalid, the bounded broker queue
    /// is full, or the broker is unavailable.
    pub(crate) fn try_resolve(
        &self,
        actor: BrokerActor,
        cluster_id: impl Into<String>,
        peer_id: u64,
        revision: impl Into<String>,
        resource: impl Into<String>,
        action: impl Into<String>,
    ) -> Result<tilo_actions::remote::Pending<ActionDirectoryBridgeReply>, ActionDirectoryBridgeError>
    {
        let cluster_id = cluster_id.into();
        let revision = revision.into();
        let resource = resource.into();
        let action = action.into();
        if validate_directory_target_input(&cluster_id, &revision, &resource, &action, None)
            .is_err()
        {
            return Err(ActionDirectoryBridgeError::InvalidRequest);
        }
        let (reply, response) = tilo_actions::remote::pending();
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

struct RealtimeBinding {
    id: crate::shell::realtime_session::RealtimeSessionId,
    app: crate::shell::realtime_session::VerifiedAppRegistration,
    source: crate::shell::personal_continuity::DeviceId,
    owner_peer: u64,
    authorization: Option<crate::shell::realtime_session::CaptureAuthorizationId>,
    target_peer: Option<u64>,
    capture_leases: Vec<crate::portal::capture_broker::CaptureLeaseGrant>,
    imported: bool,
}

struct RealtimeSubscriber {
    app: crate::shell::realtime_session::VerifiedAppRegistration,
    tx: mpsc::UnboundedSender<Vec<u8>>,
}

struct RealtimeBridge {
    broker: Arc<Mutex<crate::shell::realtime_session::RealtimeSessionBroker>>,
    fabric: Arc<crate::shell::realtime_session::BrokerRealtimeFabricAdapter>,
    capture: Arc<Mutex<crate::portal::CaptureBroker>>,
    sessions: HashMap<String, RealtimeBinding>,
    subscribers: HashMap<u64, RealtimeSubscriber>,
    pending_imported_offers: VecDeque<crate::shell::realtime_session::ImportedRealtimeFabricOffer>,
    consent_capabilities_issued: std::collections::HashSet<String>,
}

impl Default for RealtimeBridge {
    fn default() -> Self {
        let broker = Arc::new(Mutex::new(
            crate::shell::realtime_session::RealtimeSessionBroker::new(),
        ));
        Self {
            fabric: Arc::new(
                crate::shell::realtime_session::BrokerRealtimeFabricAdapter::new(Arc::clone(
                    &broker,
                )),
            ),
            broker,
            capture: Arc::new(Mutex::new(
                crate::portal::CaptureBroker::with_system_defaults(),
            )),
            sessions: HashMap::new(),
            subscribers: HashMap::new(),
            pending_imported_offers: VecDeque::new(),
            consent_capabilities_issued: std::collections::HashSet::new(),
        }
    }
}

impl RealtimeBridge {
    fn new_token(&self) -> String {
        loop {
            let mut bytes = [0_u8; 16];
            UnwrapErr(SysRng).fill_bytes(&mut bytes);
            let token = hex::encode(bytes);
            if !self.sessions.contains_key(&token) {
                return token;
            }
        }
    }

    fn notify(&mut self, peer: u64, event: &ui::realtime_continuity::RealtimeEvent) -> bool {
        let Ok(bytes) = serde_json::to_vec(event) else {
            return false;
        };
        if bytes.len() > ui::realtime_continuity::MAX_REALTIME_WIRE_BYTES {
            return false;
        }
        let delivered = self
            .subscribers
            .get(&peer)
            .is_some_and(|subscriber| subscriber.tx.send(bytes).is_ok());
        if !delivered {
            self.subscribers.remove(&peer);
        }
        delivered
    }

    fn required_capture_kinds(
        &self,
        id: crate::shell::realtime_session::RealtimeSessionId,
    ) -> Option<Vec<crate::portal::CaptureKind>> {
        let media = self.broker.lock().ok()?.session(id)?.intent.media;
        let mut kinds = Vec::with_capacity(4);
        if media.microphone() {
            kinds.push(crate::portal::CaptureKind::Microphone);
        }
        if media.camera() {
            kinds.push(crate::portal::CaptureKind::Camera);
        }
        if media.screen_share() {
            kinds.push(crate::portal::CaptureKind::Screen);
        }
        if media.share_audio() {
            kinds.push(crate::portal::CaptureKind::SystemAudio);
        }
        Some(kinds)
    }
}

fn realtime_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn broker_realtime_controls(
    controls: ui::realtime_continuity::RealtimeControlIntent,
) -> crate::shell::realtime_session::RealtimeControlState {
    crate::shell::realtime_session::RealtimeControlState {
        microphone_muted: controls.microphone_muted,
        camera_muted: controls.camera_muted,
        screen_share_paused: controls.screen_share_paused,
        share_audio_muted: controls.share_audio_muted,
    }
}

fn app_realtime_controls(
    controls: crate::shell::realtime_session::RealtimeControlState,
) -> ui::realtime_continuity::RealtimeControlIntent {
    ui::realtime_continuity::RealtimeControlIntent {
        microphone_muted: controls.microphone_muted,
        camera_muted: controls.camera_muted,
        screen_share_paused: controls.screen_share_paused,
        share_audio_muted: controls.share_audio_muted,
    }
}

fn acquire_realtime_capture_leases(
    capture: &Arc<Mutex<crate::portal::CaptureBroker>>,
    app: &crate::shell::realtime_session::VerifiedAppRegistration,
    session: &str,
    target: &crate::shell::personal_continuity::DeviceId,
    opaque_authorizations: &[String],
    required: &[crate::portal::CaptureKind],
    now: std::time::Instant,
) -> Result<
    Vec<crate::portal::capture_broker::CaptureLeaseGrant>,
    crate::portal::capture_broker::CaptureBrokerError,
> {
    let binding =
        crate::portal::CaptureSessionBinding::from_verified(app, session, target.as_str())?;
    let authorizations = opaque_authorizations
        .iter()
        .map(|authorization| crate::portal::CaptureAuthorizationId::from_opaque(authorization))
        .collect::<Result<Vec<_>, _>>()?;
    capture
        .lock()
        .map_err(|_| crate::portal::capture_broker::CaptureBrokerError::Unavailable)?
        .acquire_required(&authorizations, &binding, required, now)
}

fn realtime_capture_leases_ready(
    capture: &Arc<Mutex<crate::portal::CaptureBroker>>,
    app: &crate::shell::realtime_session::VerifiedAppRegistration,
    session: &str,
    target: &crate::shell::personal_continuity::DeviceId,
    leases: &[crate::portal::capture_broker::CaptureLeaseGrant],
    now: std::time::Instant,
) -> Result<(), crate::portal::capture_broker::CaptureBrokerError> {
    let binding =
        crate::portal::CaptureSessionBinding::from_verified(app, session, target.as_str())?;
    let mut capture = capture
        .lock()
        .map_err(|_| crate::portal::capture_broker::CaptureBrokerError::Unavailable)?;
    capture.reap_expired(now);
    for grant in leases {
        capture.resolve(grant.lease_id, &binding, now)?;
    }
    drop(capture);
    Ok(())
}

fn release_realtime_capture_session(
    capture: &Arc<Mutex<crate::portal::CaptureBroker>>,
    app: &crate::shell::realtime_session::VerifiedAppRegistration,
    session: &str,
    target: &crate::shell::personal_continuity::DeviceId,
) {
    if let Ok(binding) =
        crate::portal::CaptureSessionBinding::from_verified(app, session, target.as_str())
        && let Ok(mut capture) = capture.lock()
    {
        capture.session_ended(&binding);
    }
}

fn release_realtime_capture_leases(
    capture: &Arc<Mutex<crate::portal::CaptureBroker>>,
    leases: &[crate::portal::capture_broker::CaptureLeaseGrant],
) {
    if let Ok(mut capture) = capture.lock() {
        for grant in leases {
            capture.release(grant.lease_id);
        }
    }
}

fn revoke_realtime_capture_authorizations(
    capture: &Arc<Mutex<crate::portal::CaptureBroker>>,
    opaque_authorizations: &[String],
) {
    let ids = opaque_authorizations
        .iter()
        .filter_map(|authorization| {
            crate::portal::CaptureAuthorizationId::from_opaque(authorization).ok()
        })
        .collect::<Vec<_>>();
    if let Ok(mut capture) = capture.lock() {
        capture.revoke_authorizations(&ids);
    }
}

fn teardown_imported_realtime_consent(realtime: &mut RealtimeBridge, session: &str) {
    let Some((id, app, source, peer, leases)) = realtime
        .sessions
        .get(session)
        .filter(|binding| binding.imported)
        .map(|binding| {
            (
                binding.id,
                binding.app.clone(),
                binding.source.clone(),
                binding.owner_peer,
                binding.capture_leases.clone(),
            )
        })
    else {
        realtime.consent_capabilities_issued.remove(session);
        return;
    };
    let target = realtime
        .broker
        .lock()
        .ok()
        .and_then(|broker| broker.session(id))
        .and_then(|view| view.target);
    if let Some(target) = target {
        release_realtime_capture_session(&realtime.capture, &app, session, &target);
        let now = std::time::Instant::now();
        let rolled_back = realtime
            .fabric
            .fail_imported(id.fabric_id(), &source, &target, now);
        if rolled_back.is_ok() {
            // `fail_imported` authenticates the exact imported source/target
            // binding and authors Cancel, but deliberately rolls the broker
            // shadow back to SourceLive. This target has no authority to keep
            // that remote-source shadow alive after local policy revocation,
            // so end only the same verified app/session/source binding.
            if let Ok(mut broker) = realtime.broker.lock() {
                let _ = broker.apply_media_action(
                    &app,
                    id,
                    &source,
                    crate::shell::realtime_session::RealtimeMediaAction::Leave,
                    now,
                );
            }
        }
    }
    release_realtime_capture_leases(&realtime.capture, &leases);
    realtime.consent_capabilities_issued.remove(session);
    let _ = realtime.notify(
        peer,
        &ui::realtime_continuity::RealtimeEvent::Ended {
            session: session.to_string(),
        },
    );
    realtime.sessions.remove(session);
}

#[cfg(test)]
struct RealtimeConsentMintHook {
    minted: std::sync::mpsc::SyncSender<()>,
    resume: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
fn realtime_consent_mint_hook() -> &'static Mutex<Option<RealtimeConsentMintHook>> {
    static HOOK: std::sync::OnceLock<Mutex<Option<RealtimeConsentMintHook>>> =
        std::sync::OnceLock::new();
    HOOK.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
fn pause_realtime_consent_after_mint() {
    let hook = realtime_consent_mint_hook()
        .lock()
        .ok()
        .and_then(|mut hook| hook.take());
    if let Some(hook) = hook {
        let _ = hook.minted.send(());
        let _ = hook.resume.recv_timeout(std::time::Duration::from_secs(5));
    }
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
    action_directory_roots: Mutex<HashMap<u64, ActionDirectoryRoot>>,
    action_directory_root_snapshot: Mutex<ActionDirectoryRootSnapshot>,
    /// Directed invocation stream registered by each application connection.
    invocation_subscribers: Mutex<HashMap<u64, mpsc::UnboundedSender<InvocationEnvelope>>>,
    pending_invocations: Mutex<HashMap<u64, PendingInvocation>>,
    completed_invocations: Mutex<HashMap<u64, (String, tilo_actions::InvocationUpdate)>>,
    confirmation_grants: Mutex<HashMap<String, ConfirmationGrant>>,
    cancellation_requests: Mutex<std::collections::HashSet<u64>>,
    task_subscribers: Mutex<HashMap<u64, TaskSubscription>>,
    pending_task_runs: Mutex<HashMap<u64, PendingTaskRun>>,
    task_run_id: AtomicU64,
    invocation_id: AtomicU64,
    /// Remote-action invocation ids to broker jobs, shared by every caller
    /// path so a retry is idempotent whichever way it arrives.
    remote_invocations: Mutex<tilo_actions::remote::InvocationLedger<u64>>,
    realtime: Mutex<RealtimeBridge>,
    nearby_targets: Mutex<NearbyTargetRegistry>,
    nearby_transfers: Mutex<NearbyTransferRegistry>,
    nearby_destinations: Mutex<NearbyDestinationRegistry>,
    workspace_control_network: Mutex<Option<std::sync::Weak<crate::shell::network::ShellNetwork>>>,
}

/// Cloneable production seam between the certified device fabric and the
/// app-facing realtime broker. The compositor owns one clone for pumping;
/// broker state owns the concrete adapter and all application routing.
#[derive(Clone)]
pub struct RealtimeFabricAppBridge {
    state: Arc<CapnpBrokerState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RealtimeSourceUiView {
    pub session: String,
    pub room_label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RealtimeConsentUiView {
    pub session: String,
    pub room_label: String,
    pub source: crate::shell::personal_continuity::DeviceId,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RealtimeContinuityUiView {
    pub sources: Vec<RealtimeSourceUiView>,
    pub consents: Vec<RealtimeConsentUiView>,
}

impl RealtimeFabricAppBridge {
    pub(crate) fn from_state(state: Arc<CapnpBrokerState>) -> Self {
        Self { state }
    }

    #[must_use]
    pub fn adapter(&self) -> Option<crate::shell::realtime_session::SharedRealtimeFabricAdapter> {
        self.state.realtime.lock().ok().map(|realtime| {
            let adapter: crate::shell::realtime_session::SharedRealtimeFabricAdapter =
                realtime.fabric.clone();
            adapter
        })
    }

    /// Shared target-local capture authority for compositor-owned continuity
    /// runtimes. The broker remains shell-private; applications receive only
    /// the semantic consent/session projection.
    pub(crate) fn realtime_capture_broker(
        &self,
    ) -> Option<Arc<Mutex<crate::portal::CaptureBroker>>> {
        self.state.realtime_capture_broker()
    }

    /// Project newly authenticated fabric edges into exact certified local
    /// application subscriptions. Called from the compositor pump after the
    /// device-fabric runtime has drained inbound transport actions.
    pub fn pump_app_events(&self) {
        self.state.pump_realtime_fabric_app_events();
    }

    /// Bounded shell-only projection for compositor chrome. Certified device
    /// identities never enter the application contract.
    #[must_use]
    pub fn ui_view(&self) -> RealtimeContinuityUiView {
        use crate::shell::realtime_session::RealtimeSessionPhase;

        let Ok(realtime) = self.state.realtime.lock() else {
            return RealtimeContinuityUiView::default();
        };
        let Ok(broker) = realtime.broker.lock() else {
            return RealtimeContinuityUiView::default();
        };
        let mut view = RealtimeContinuityUiView::default();
        for (token, binding) in &realtime.sessions {
            let Some(session) = broker.session(binding.id) else {
                continue;
            };
            if !binding.imported && session.phase == RealtimeSessionPhase::SourceLive {
                view.sources.push(RealtimeSourceUiView {
                    session: token.clone(),
                    room_label: session.intent.room.room_id().to_string(),
                });
            } else if binding.imported
                && session.phase == RealtimeSessionPhase::AwaitingTargetAccept
                && !realtime.consent_capabilities_issued.contains(token)
            {
                view.consents.push(RealtimeConsentUiView {
                    session: token.clone(),
                    room_label: session.intent.room.room_id().to_string(),
                    source: session.source,
                });
            }
        }
        view.sources.sort_by(|left, right| {
            left.room_label
                .cmp(&right.room_label)
                .then_with(|| left.session.cmp(&right.session))
        });
        view.consents.sort_by(|left, right| {
            left.room_label
                .cmp(&right.room_label)
                .then_with(|| left.session.cmp(&right.session))
        });
        view
    }

    /// Start an exact source-to-target handoff selected in trusted shell UI.
    ///
    /// # Errors
    ///
    /// Rejects disabled continuity, stale/non-source sessions, identity
    /// mismatches, offline targets, and invalid broker transitions.
    pub fn propose_to(
        &self,
        session: &str,
        target: &crate::shell::personal_continuity::DeviceId,
    ) -> Result<(), String> {
        if !crate::shell::continuity_policy::ContinuityPolicySnapshot::load()
            .permits(crate::shell::continuity_policy::ContinuityCapability::RealtimeSessions)
        {
            return Err("realtime session continuity is disabled".into());
        }
        let realtime = self
            .state
            .realtime
            .lock()
            .map_err(|_| "realtime broker is unavailable".to_string())?;
        let binding = realtime
            .sessions
            .get(session)
            .ok_or_else(|| "unknown realtime session".to_string())?;
        if binding.imported {
            return Err("only a live source session can choose a destination".into());
        }
        let (id, app, source) = (binding.id, binding.app.clone(), binding.source.clone());
        realtime
            .broker
            .lock()
            .map_err(|_| "realtime broker is unavailable".to_string())?
            .propose_target(&app, id, &source, target.clone(), std::time::Instant::now())
            .map_err(|error| error.to_string())?;
        if let Err(error) =
            realtime
                .fabric
                .offer_to(id, target, realtime_unix_ms().saturating_add(30_000))
        {
            let _ = realtime.broker.lock().map(|mut broker| {
                let _ = broker.target_failed(&app, id, &source, target, std::time::Instant::now());
            });
            return Err(error.to_string());
        }
        Ok(())
    }

    /// Resolve one imported offer from compositor-owned consent UI. Approval
    /// chooses one currently available local source for every requested
    /// facet and sends only one-shot opaque capabilities to the verified app.
    ///
    /// # Errors
    ///
    /// Rejects stale offers, disabled policy, unavailable capture sources, or
    /// any mismatch between the imported session and this local target.
    pub fn resolve_consent(&self, session: &str, approved: bool) -> Result<(), String> {
        self.resolve_consent_with_policy(session, approved, || {
            crate::shell::continuity_policy::ContinuityPolicySnapshot::load()
                .permits(crate::shell::continuity_policy::ContinuityCapability::RealtimeSessions)
        })
    }

    fn resolve_consent_with_policy(
        &self,
        session: &str,
        approved: bool,
        policy_enabled: impl Fn() -> bool,
    ) -> Result<(), String> {
        if !policy_enabled() {
            if let Ok(mut realtime) = self.state.realtime.lock() {
                teardown_imported_realtime_consent(&mut realtime, session);
            }
            return Err("realtime session continuity is disabled".into());
        }
        let (id, app, source, target, peer, required, capture) = {
            let mut realtime = self
                .state
                .realtime
                .lock()
                .map_err(|_| "realtime broker is unavailable".to_string())?;
            let binding = realtime
                .sessions
                .get(session)
                .ok_or_else(|| "unknown realtime session".to_string())?;
            if !binding.imported {
                return Err("only an imported offer can be authorized here".into());
            }
            if realtime.consent_capabilities_issued.contains(session) {
                return Err("capture consent is already being applied".into());
            }
            let target = realtime
                .broker
                .lock()
                .map_err(|_| "realtime broker is unavailable".to_string())?
                .session(binding.id)
                .and_then(|view| view.target)
                .ok_or_else(|| "realtime target binding is unavailable".to_string())?;
            let required = realtime
                .required_capture_kinds(binding.id)
                .ok_or_else(|| "unknown realtime session".to_string())?;
            let result = (
                binding.id,
                binding.app.clone(),
                binding.source.clone(),
                target,
                binding.owner_peer,
                required,
                Arc::clone(&realtime.capture),
            );
            if !realtime
                .consent_capabilities_issued
                .insert(session.to_string())
            {
                return Err("capture consent is already being applied".into());
            }
            result
        };
        if !approved {
            let realtime = self
                .state
                .realtime
                .lock()
                .map_err(|_| "realtime broker is unavailable".to_string())?;
            let declined = realtime
                .fabric
                .fail_imported(id.fabric_id(), &source, &target, std::time::Instant::now())
                .map_err(|error| error.to_string());
            drop(realtime);
            if let Err(error) = declined {
                if let Ok(mut realtime) = self.state.realtime.lock() {
                    realtime.consent_capabilities_issued.remove(session);
                }
                return Err(error);
            }
            if let Ok(mut realtime) = self.state.realtime.lock() {
                realtime.consent_capabilities_issued.remove(session);
                let _ = realtime.notify(
                    peer,
                    &ui::realtime_continuity::RealtimeEvent::Ended {
                        session: session.to_string(),
                    },
                );
                realtime.sessions.remove(session);
            }
            return Ok(());
        }
        let selections = (|| {
            let capture = capture
                .lock()
                .map_err(|_| "target capture broker is unavailable".to_string())?;
            required
                .iter()
                .map(|kind| {
                    capture
                        .enumerate(*kind)
                        .into_iter()
                        .next()
                        .map(|source| (*kind, source.id))
                        .ok_or_else(|| format!("no local {kind:?} source is available"))
                })
                .collect::<Result<Vec<_>, _>>()
        })();
        let selections = match selections {
            Ok(selections) => selections,
            Err(error) => {
                if let Ok(mut realtime) = self.state.realtime.lock() {
                    realtime.consent_capabilities_issued.remove(session);
                }
                return Err(error);
            }
        };
        let authorizations = self.state.authorize_realtime_capture_batch(
            &app,
            session,
            &target,
            &selections,
            std::time::Instant::now(),
        );
        let authorizations = match authorizations {
            Ok(authorizations) => authorizations,
            Err(error) => {
                if let Ok(mut realtime) = self.state.realtime.lock() {
                    realtime.consent_capabilities_issued.remove(session);
                }
                return Err(error);
            }
        };
        #[cfg(test)]
        pause_realtime_consent_after_mint();
        let mut realtime = match self.state.realtime.lock() {
            Ok(realtime) => realtime,
            Err(_) => {
                revoke_realtime_capture_authorizations(&capture, &authorizations);
                return Err("realtime broker is unavailable".into());
            }
        };
        let live_binding = realtime.sessions.get(session).is_some_and(|binding| {
            binding.imported
                && binding.id == id
                && binding.app == app
                && binding.source == source
                && binding.owner_peer == peer
        });
        let live_session = realtime
            .broker
            .lock()
            .ok()
            .and_then(|broker| broker.session(id))
            .is_some_and(|view| {
                view.source == source
                    && view.target.as_ref() == Some(&target)
                    && view.phase
                        == crate::shell::realtime_session::RealtimeSessionPhase::AwaitingTargetAccept
            });
        let current_required = realtime.required_capture_kinds(id);
        let still_reserved = realtime.consent_capabilities_issued.contains(session);
        let policy_enabled = policy_enabled();
        if !policy_enabled {
            teardown_imported_realtime_consent(&mut realtime, session);
            drop(realtime);
            revoke_realtime_capture_authorizations(&capture, &authorizations);
            return Err("realtime session continuity was disabled during consent".into());
        }
        if !live_binding
            || !live_session
            || current_required.as_ref() != Some(&required)
            || !still_reserved
        {
            realtime.consent_capabilities_issued.remove(session);
            drop(realtime);
            revoke_realtime_capture_authorizations(&capture, &authorizations);
            return Err("realtime consent expired while capture was being prepared".into());
        }
        let delivered = realtime.notify(
            peer,
            &ui::realtime_continuity::RealtimeEvent::CaptureAuthorized {
                session: session.to_string(),
                capture_authorizations: authorizations.clone(),
            },
        );
        if !delivered {
            realtime.consent_capabilities_issued.remove(session);
            let _ = realtime.fabric.fail_imported(
                id.fabric_id(),
                &source,
                &target,
                std::time::Instant::now(),
            );
            realtime.sessions.remove(session);
            drop(realtime);
            revoke_realtime_capture_authorizations(&capture, &authorizations);
            return Err("realtime target application disconnected during consent".into());
        }
        Ok(())
    }
}

/// Find `id` in `resource`'s subtree.
fn find_resource<'a>(
    resource: &'a tilo_actions::Resource,
    id: &str,
) -> Option<&'a tilo_actions::Resource> {
    if resource.id.as_str() == id {
        return Some(resource);
    }
    resource
        .children
        .iter()
        .find_map(|child| find_resource(child, id))
}

/// Decode a workspace governance invocation's typed `argument` into the
/// permission-profile operation `spec` encodes.
fn decode_permission_profile_operation(
    spec: &tilo_actions::ActionSpec,
    argument: Option<&str>,
) -> Result<crate::shell::workspace_settings::PermissionProfileOperation, String> {
    let json = argument
        .and_then(|value| value.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX))
        .ok_or_else(|| "workspace governance action requires typed arguments".to_string())?;
    let arguments = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json)
        .map_err(|error| format!("invalid workspace governance arguments: {error}"))?;
    let encoded = spec
        .build_json(&arguments)
        .map_err(|error| format!("invalid workspace governance arguments: {error}"))?;
    if encoded.type_id == tilo_actions::resource::INVALID_ARGUMENTS_TYPE {
        return Err(format!(
            "invalid workspace governance arguments: {}",
            String::from_utf8_lossy(&encoded.payload)
        ));
    }
    if encoded.type_id != crate::shell::workspace_settings::PERMISSION_PROFILE_ACTION_TYPE {
        return Err("workspace governance action encoded an unexpected operation".into());
    }
    serde_json::from_slice(&encoded.payload)
        .map_err(|error| format!("invalid workspace governance operation: {error}"))
}

/// Apply a permission-profile operation as `caller` to one workspace and
/// mirror it into the room document.
fn apply_permission_profile_operation(
    tree: &mut WorkspaceTree,
    workspace_index: usize,
    workspace_id: WorkspaceId,
    caller: &Actor,
    operation: crate::shell::workspace_settings::PermissionProfileOperation,
) -> Result<(), String> {
    match operation {
        crate::shell::workspace_settings::PermissionProfileOperation::Save {
            name,
            description,
            rules,
        } => {
            let profile = collab_types::PermissionProfile {
                name,
                description,
                rules,
            };
            tree.desktop.workspaces[workspace_index]
                .save_permission_profile_as_actor(caller, profile.clone())
                .map_err(|error| error.to_string())?;
            tree.doc_upsert_permission_profile(workspace_id, &profile);
        }
        crate::shell::workspace_settings::PermissionProfileOperation::Assign {
            profile,
            subject_kind,
            subject,
            expires_unix,
        } => {
            let subject = match subject_kind.trim().to_ascii_lowercase().as_str() {
                "person" => collab_types::Subject::person(subject.trim()),
                "app" => collab_types::Subject::app(subject.trim()),
                "agent" => collab_types::Subject::agent(subject.trim()),
                _ => return Err("subject_kind must be person, app, or agent".into()),
            };
            let expiry = (expires_unix > 0)
                .then_some(expires_unix)
                .map_or(collab_types::Expiry::Never, collab_types::Expiry::AtUnix);
            let ids = tree.desktop.workspaces[workspace_index]
                .assign_permission_profile_as_actor(caller, &profile, &subject, expiry)
                .map_err(|error| error.to_string())?;
            let capabilities = tree.desktop.workspaces[workspace_index]
                .capabilities
                .iter()
                .filter(|capability| ids.contains(&capability.id))
                .cloned()
                .collect::<Vec<_>>();
            for capability in capabilities {
                tree.doc_insert_capability(workspace_id, &capability);
            }
        }
    }
    Ok(())
}

impl CapnpBrokerState {
    #[allow(
        clippy::too_many_lines,
        reason = "offer admission, exact app routing, and lifecycle projection share one bounded broker lock so tokens cannot race subscriber delivery"
    )]
    fn pump_realtime_fabric_app_events(&self) {
        let fabric = match self.realtime.lock() {
            Ok(realtime) => Arc::clone(&realtime.fabric),
            Err(_) => return,
        };
        let offers = fabric.drain_imported_offers();
        let cutovers = fabric.drain_cutovers();
        let lifecycle_events = fabric.drain_lifecycle_events();
        let Ok(mut realtime) = self.realtime.lock() else {
            return;
        };
        for imported in offers {
            if realtime.pending_imported_offers.len()
                == crate::shell::realtime_session::MAX_REALTIME_SESSIONS
            {
                realtime.pending_imported_offers.pop_front();
            }
            realtime.pending_imported_offers.push_back(imported);
        }
        let pending = realtime.pending_imported_offers.len();
        for _ in 0..pending {
            let Some(imported) = realtime.pending_imported_offers.pop_front() else {
                break;
            };
            let Some(peer) = realtime
                .subscribers
                .iter()
                .filter(|(_, subscriber)| {
                    subscriber.app.app_id() == imported.offer.app_id
                        && subscriber.app.workspace_id() == imported.offer.workspace_id
                })
                .map(|(peer, _)| *peer)
                .min()
            else {
                realtime.pending_imported_offers.push_back(imported);
                continue;
            };
            let id = imported.offer.session_id.into_broker_id();
            let live = realtime
                .broker
                .lock()
                .ok()
                .and_then(|broker| broker.session(id))
                .is_some_and(|view| {
                    view.app_id == imported.offer.app_id
                        && view.workspace_id == imported.offer.workspace_id
                        && view.source == imported.source
                        && view.target.as_ref() == Some(&imported.target)
                });
            if !live {
                continue;
            }
            let Ok(app) = crate::shell::realtime_session::VerifiedAppRegistration::new_verified(
                imported.offer.app_id.clone(),
                imported.offer.workspace_id.clone(),
            ) else {
                continue;
            };
            let Ok(intent) = imported.offer.intent() else {
                continue;
            };
            let token = realtime.new_token();
            realtime.sessions.insert(
                token.clone(),
                RealtimeBinding {
                    id,
                    app,
                    source: imported.source,
                    owner_peer: peer,
                    authorization: None,
                    target_peer: None,
                    capture_leases: Vec::new(),
                    imported: true,
                },
            );
            let event = ui::realtime_continuity::RealtimeEvent::Offer {
                session: token,
                intent: ui::realtime_continuity::RealtimeResumeIntent {
                    provider: intent.room.provider().to_string(),
                    room_id: intent.room.room_id().to_string(),
                    semantic_position: intent.semantic_position,
                    media: ui::realtime_continuity::RealtimeMediaFacets {
                        microphone: intent.media.microphone(),
                        camera: intent.media.camera(),
                        screen_share: intent.media.screen_share(),
                        share_audio: intent.media.share_audio(),
                    },
                    controls: app_realtime_controls(intent.controls),
                },
            };
            realtime.notify(peer, &event);
        }
        for directive in cutovers {
            if let Some((token, peer)) = realtime.sessions.iter().find_map(|(token, binding)| {
                (!binding.imported && binding.id == directive.session_id)
                    .then_some((token.clone(), binding.owner_peer))
            }) {
                realtime.notify(
                    peer,
                    &ui::realtime_continuity::RealtimeEvent::StopSource { session: token },
                );
            }
        }
        for event in lifecycle_events {
            let bindings = realtime
                .sessions
                .iter()
                .filter_map(|(token, binding)| {
                    (binding.id == event.session_id).then_some((
                        token.clone(),
                        binding.owner_peer,
                        binding.capture_leases.clone(),
                    ))
                })
                .collect::<Vec<_>>();
            for (token, peer, leases) in bindings {
                let terminal = !matches!(
                    &event.kind,
                    crate::shell::realtime_session::BrokerEventKind::RolledBackToSource { .. }
                );
                let app_event = if terminal {
                    ui::realtime_continuity::RealtimeEvent::Ended {
                        session: token.clone(),
                    }
                } else {
                    ui::realtime_continuity::RealtimeEvent::RolledBack {
                        session: token.clone(),
                    }
                };
                realtime.notify(peer, &app_event);
                release_realtime_capture_leases(&realtime.capture, &leases);
                if terminal {
                    realtime.sessions.remove(&token);
                    realtime.consent_capabilities_issued.remove(&token);
                } else if let Some(binding) = realtime.sessions.get_mut(&token) {
                    binding.authorization = None;
                    binding.target_peer = None;
                    binding.capture_leases.clear();
                }
            }
        }
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
                invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id))),
                error: tilo_actions::ActionFailure {
                    code: completion.code.clone(),
                    message: completion.message.clone(),
                    details: None,
                    retryable: pending.retryable_on_disconnect,
                },
            };
            self.record_effect_receipt(&pending, &update);
            self.record_completion(id, pending.actor.clone(), update);
            if let Ok(mut requests) = self.cancellation_requests.lock() {
                requests.remove(&id);
            }
            let _ = pending.tx.send(completion);
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

    fn record_effect_receipt(
        &self,
        pending: &PendingInvocation,
        update: &tilo_actions::InvocationUpdate,
    ) {
        if pending.operation_kind != tilo_actions::OperationKind::Effect {
            return;
        }
        let Some(status) = tilo_actions::EffectReceiptStatus::from_terminal(update) else {
            return;
        };
        let Some(membership) = &self.membership else {
            return;
        };
        let mut tree = membership.write();
        let Some(workspace_id) = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == pending.cluster_id)
            .map(|workspace| workspace.id)
        else {
            return;
        };
        let _ = tree.append_workspace_effect_receipt(
            workspace_id,
            update.invocation(),
            &pending.actor,
            pending.context.clone(),
            status,
        );
    }

    /// So tests can register channel state before pushing signed
    /// envelopes through the capnp path.
    pub fn broker_channels(&self) -> Arc<Mutex<BrokerChannels>> {
        Arc::clone(&self.broker_channels)
    }

    /// Shared target-local capture broker used by the portal consent path and
    /// realtime IPC. Local handles and lease IDs never leave the shell.
    #[allow(
        dead_code,
        reason = "the portal host consumes this narrow handle when presenting target-local capture consent"
    )]
    pub(crate) fn realtime_capture_broker(
        &self,
    ) -> Option<Arc<Mutex<crate::portal::CaptureBroker>>> {
        self.realtime
            .lock()
            .ok()
            .map(|realtime| Arc::clone(&realtime.capture))
    }

    /// Mint one target-local consent capability after trusted shell UI has
    /// selected a concrete local source. The session/app/workspace/target and
    /// requested kind are all re-derived from broker state before minting.
    #[allow(
        dead_code,
        reason = "called by the target capture-consent host once that host is attached to the collaboration service lifecycle"
    )]
    pub(crate) fn authorize_realtime_capture(
        &self,
        app: &crate::shell::realtime_session::VerifiedAppRegistration,
        session: &str,
        target: &crate::shell::personal_continuity::DeviceId,
        source_id: &str,
        kind: crate::portal::CaptureKind,
        now: std::time::Instant,
    ) -> Result<String, String> {
        let realtime = self
            .realtime
            .lock()
            .map_err(|_| "realtime broker is unavailable".to_string())?;
        let binding = realtime
            .sessions
            .get(session)
            .ok_or_else(|| "unknown realtime session".to_string())?;
        let view = realtime
            .broker
            .lock()
            .map_err(|_| "realtime broker is unavailable".to_string())?
            .session(binding.id)
            .ok_or_else(|| "unknown realtime session".to_string())?;
        if &binding.app != app || view.target.as_ref() != Some(target) {
            return Err("realtime capture binding mismatch".into());
        }
        let required = realtime
            .required_capture_kinds(binding.id)
            .ok_or_else(|| "unknown realtime session".to_string())?;
        if !required.contains(&kind) {
            return Err("capture kind is not active for this realtime session".into());
        }
        let capture_binding =
            crate::portal::CaptureSessionBinding::from_verified(app, session, target.as_str())
                .map_err(|error| error.to_string())?;
        let capture = Arc::clone(&realtime.capture);
        drop(realtime);
        capture
            .lock()
            .map_err(|_| "target capture broker is unavailable".to_string())?
            .authorize(&capture_binding, source_id, kind, now)
            .map(crate::portal::CaptureAuthorizationId::to_opaque)
            .map_err(|error| error.to_string())
    }

    fn authorize_realtime_capture_batch(
        &self,
        app: &crate::shell::realtime_session::VerifiedAppRegistration,
        session: &str,
        target: &crate::shell::personal_continuity::DeviceId,
        selections: &[(crate::portal::CaptureKind, String)],
        now: std::time::Instant,
    ) -> Result<Vec<String>, String> {
        let realtime = self
            .realtime
            .lock()
            .map_err(|_| "realtime broker is unavailable".to_string())?;
        let binding = realtime
            .sessions
            .get(session)
            .ok_or_else(|| "unknown realtime session".to_string())?;
        let view = realtime
            .broker
            .lock()
            .map_err(|_| "realtime broker is unavailable".to_string())?
            .session(binding.id)
            .ok_or_else(|| "unknown realtime session".to_string())?;
        let required = realtime
            .required_capture_kinds(binding.id)
            .ok_or_else(|| "unknown realtime session".to_string())?;
        let selected_kinds = selections.iter().map(|(kind, _)| *kind).collect::<Vec<_>>();
        if &binding.app != app
            || view.target.as_ref() != Some(target)
            || selected_kinds.len() != required.len()
            || required.iter().any(|kind| !selected_kinds.contains(kind))
        {
            return Err("realtime capture binding mismatch".into());
        }
        let capture_binding =
            crate::portal::CaptureSessionBinding::from_verified(app, session, target.as_str())
                .map_err(|error| error.to_string())?;
        let capture = Arc::clone(&realtime.capture);
        drop(realtime);
        let selections = selections
            .iter()
            .map(|(kind, source)| (source.clone(), *kind))
            .collect::<Vec<_>>();
        capture
            .lock()
            .map_err(|_| "target capture broker is unavailable".to_string())?
            .authorize_batch(&capture_binding, &selections, now)
            .map(|ids| {
                ids.into_iter()
                    .map(crate::portal::CaptureAuthorizationId::to_opaque)
                    .collect()
            })
            .map_err(|error| error.to_string())
    }

    fn new(
        fanout: &Arc<FanoutState>,
        broker_channels: Arc<Mutex<BrokerChannels>>,
        publisher: Arc<PeerPublisher>,
        membership: Option<WorkspaceTreeHandle>,
        trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
    ) -> (
        Arc<Self>,
        std::sync::mpsc::Receiver<ActionDirectoryBridgeRequest>,
    ) {
        let (bridge_tx, bridge_rx) =
            std::sync::mpsc::sync_channel(ActionDirectoryRequestQueue::CAPACITY);
        (
            Arc::new(Self {
                channels: Mutex::new(HashMap::new()),
                fanout: Arc::downgrade(fanout),
                broker_channels,
                publisher,
                membership,
                trusted_devices,
                surfaces: Mutex::new(HashMap::new()),
                surface_generation: AtomicU64::new(0),
                archived_surfaces: Mutex::new(HashMap::new()),
                action_directory_bridge: ActionDirectoryRequestQueue { sender: bridge_tx },
                action_directory_roots: Mutex::new(HashMap::new()),
                action_directory_root_snapshot: Mutex::new(ActionDirectoryRootSnapshot::default()),
                invocation_subscribers: Mutex::new(HashMap::new()),
                pending_invocations: Mutex::new(HashMap::new()),
                completed_invocations: Mutex::new(HashMap::new()),
                confirmation_grants: Mutex::new(HashMap::new()),
                cancellation_requests: Mutex::new(std::collections::HashSet::new()),
                task_subscribers: Mutex::new(HashMap::new()),
                pending_task_runs: Mutex::new(HashMap::new()),
                task_run_id: AtomicU64::new(1),
                invocation_id: AtomicU64::new(1),
                remote_invocations: Mutex::new(tilo_actions::remote::InvocationLedger::default()),
                realtime: Mutex::new(RealtimeBridge::default()),
                nearby_targets: Mutex::new(NearbyTargetRegistry::default()),
                nearby_transfers: Mutex::new(NearbyTransferRegistry::default()),
                nearby_destinations: Mutex::new(NearbyDestinationRegistry::default()),
                workspace_control_network: Mutex::new(None),
            }),
            bridge_rx,
        )
    }

    #[cfg(test)]
    pub(crate) fn test_broker_with_idle_directory_queue()
    -> crate::shell::collab_broker::AppActionBroker {
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = Self::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            None,
            None,
        );
        crate::shell::collab_broker::AppActionBroker::from_state_for_test(state)
    }

    #[cfg(test)]
    pub(crate) fn test_broker_with_membership(
        membership: crate::shell::state::WorkspaceTreeHandle,
    ) -> crate::shell::collab_broker::AppActionBroker {
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = Self::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(membership),
            None,
        );
        crate::shell::collab_broker::AppActionBroker::from_state_for_test(state)
    }

    /// Stand in for an application's invocation stream on `peer_id`.
    #[cfg(test)]
    pub(crate) fn capture_invocations_for_test(
        &self,
        peer_id: u64,
    ) -> mpsc::UnboundedReceiver<InvocationEnvelope> {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut subscribers) = self.invocation_subscribers.lock() {
            subscribers.insert(peer_id, tx);
        }
        rx
    }

    #[cfg(test)]
    pub(crate) fn replace_action_directory_roots_for_test(&self, roots: Vec<ActionDirectoryRoot>) {
        if let Ok(mut registered) = self.action_directory_roots.lock() {
            registered.clear();
            registered.extend(roots.into_iter().map(|root| (root.peer_id, root)));
        }
        self.refresh_action_directory_root_snapshot();
    }

    #[cfg(test)]
    pub(crate) fn replace_published_surfaces_for_test(&self, surfaces: Vec<PublishedSurface>) {
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

    fn attach_workspace_control_network(&self, network: &Arc<crate::shell::network::ShellNetwork>) {
        if let Ok(mut slot) = self.workspace_control_network.lock() {
            *slot = Some(Arc::downgrade(network));
        }
    }

    fn workspace_control_network(&self) -> Option<Arc<crate::shell::network::ShellNetwork>> {
        self.workspace_control_network
            .lock()
            .ok()
            .and_then(|network| network.as_ref().and_then(std::sync::Weak::upgrade))
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
    pub(crate) fn action_directory_request_queue(&self) -> ActionDirectoryRequestQueue {
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
                        reply.complete(response);
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
                        reply.complete(response);
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
        validate_directory_target_input(cluster_id, revision, resource, action, None)
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
        let tree = membership.read();
        let workspace = tree
            .desktop
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

    /// Execute the shell-owned permission-profile surface advertised with
    /// peer id zero. Unlike an application action there is no UI process to
    /// forward to: the broker already owns the workspace capability authority,
    /// so it validates, mutates, records history, and returns one terminal
    /// lifecycle update atomically under that authority.
    fn invoke_workspace_profile_action(
        &self,
        cluster_id: &str,
        actor: &BrokerActor,
        resource_id: &str,
        action_id: &str,
        argument: Option<&str>,
        confirmation: &serde_json::Value,
    ) -> Result<(u64, tilo_actions::InvocationUpdate), String> {
        let membership = self
            .membership
            .as_ref()
            .ok_or_else(|| "the action broker has no workspace authority".to_string())?;
        let mut tree = membership.write();
        let workspace_index = tree
            .desktop
            .workspaces
            .iter()
            .position(|workspace| workspace.room.to_string() == cluster_id)
            .ok_or_else(|| "the application workspace is no longer hosted".to_string())?;
        let caller = if actor.is_agent {
            tree.desktop.workspaces[workspace_index]
                .actor_for_agent(actor.id.strip_prefix("agent:").unwrap_or(&actor.id))
        } else {
            tree.desktop.workspaces[workspace_index].actor_for_person(&actor.id)
        };
        let surface = crate::shell::workspace_settings::permission_profile_resource_with_access(
            &tree.desktop.workspaces[workspace_index],
            true,
        );
        let resource = find_resource(&surface, resource_id)
            .ok_or_else(|| "unknown workspace governance resource".to_string())?;
        let spec = resource
            .actions
            .iter()
            .find(|candidate| candidate.id == action_id)
            .ok_or_else(|| "unknown workspace governance action".to_string())?;
        if confirmation
            .get("confirm")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        {
            return Err("explicit confirmation is required".into());
        }
        if tree.desktop.workspaces[workspace_index].decide(
            &caller,
            &ResourceRef::Action {
                app: crate::shell::state::WORKSPACE_GOVERNANCE_APP_ID.into(),
                action: action_id.into(),
            },
            Verb::Admin,
        ) != CapDecision::Allow
        {
            return Err("the caller is not allowed to manage permission profiles".into());
        }
        let operation = decode_permission_profile_operation(spec, argument)?;
        let workspace_id = tree.desktop.workspaces[workspace_index].id;
        apply_permission_profile_operation(
            &mut tree,
            workspace_index,
            workspace_id,
            &caller,
            operation,
        )?;
        let id = self.invocation_id.fetch_add(1, Ordering::Relaxed);
        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id)));
        let update = tilo_actions::InvocationUpdate::Succeeded {
            invocation,
            output: tilo_actions::ActionOutput::default(),
        };
        let context = tilo_actions::ActionContext::for_resource(
            &spec.label,
            resource.id.as_str(),
            &resource.kind,
            &resource.label,
        )
        .with_app(
            crate::shell::state::WORKSPACE_GOVERNANCE_APP_ID,
            "Workspace permissions",
        );
        let _ = tree.append_workspace_effect_receipt(
            workspace_id,
            invocation,
            &actor.id,
            context,
            tilo_actions::EffectReceiptStatus::Succeeded,
        );
        tree.desktop.external_revision = tree.desktop.external_revision.wrapping_add(1);
        tree.flush_to_disk();
        drop(tree);
        self.record_completion(id, actor.id.clone(), update.clone());
        Ok((id, update))
    }

    /// Address one operation back to the exact connection that published it.
    /// The returned receiver resolves only after the application reports a
    /// structured completion; dropping the connection resolves as a refusal.
    ///
    /// # Errors
    ///
    /// Returns an error if the surface or action is unavailable or the
    /// invocation cannot be sent to its owning connection.
    #[cfg(test)]
    pub(crate) fn invoke(
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

    /// Prepare a one-use confirmation token bound to this exact invocation
    /// of an eager surface action.
    ///
    /// # Errors
    ///
    /// Returns an error when the action, phrase, or confirmation registry is invalid.
    #[cfg(test)]
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
        let evidence = match &offered.contract.confirmation {
            tilo_actions::ConfirmationPolicy::None => {
                return Err("the action does not require confirmation".into());
            }
            tilo_actions::ConfirmationPolicy::Confirm => {
                tilo_actions::remote::Confirmation::Confirmed
            }
            tilo_actions::ConfirmationPolicy::Phrase { .. } => {
                tilo_actions::remote::Confirmation::Phrase(phrase.unwrap_or_default().to_string())
            }
        };
        self.confirmation_token(peer_id, &offered, argument, actor, Some(&evidence))
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "the action does not require confirmation".into())
    }

    /// Check `evidence` against the offered action's confirmation policy and,
    /// when one is required, mint the one-use grant that binds it to this
    /// exact peer, address, argument and actor. The one confirmation path
    /// every caller goes through.
    pub(crate) fn confirmation_token(
        &self,
        peer_id: u64,
        offered: &PublishedAction,
        argument: Option<&str>,
        actor: &str,
        evidence: Option<&tilo_actions::remote::Confirmation>,
    ) -> Result<Option<String>, tilo_actions::remote::ActionHostError> {
        use tilo_actions::remote::{ActionHostError, Confirmation};
        match (&offered.contract.confirmation, evidence) {
            (tilo_actions::ConfirmationPolicy::None, _) => return Ok(None),
            (tilo_actions::ConfirmationPolicy::Confirm, Some(Confirmation::Confirmed)) => {}
            (
                tilo_actions::ConfirmationPolicy::Phrase { phrase: expected },
                Some(Confirmation::Phrase(phrase)),
            ) if phrase.len() <= DIRECTORY_MAX_CONFIRMATION_BYTES && phrase == expected => {}
            _ => return Err(ActionHostError::ConfirmationRequired),
        }
        let unavailable = || ActionHostError::Unavailable {
            reason: "the confirmation registry is unavailable".into(),
        };
        let token = uuid::Uuid::new_v4().to_string();
        let now = std::time::Instant::now();
        let expires = now
            .checked_add(std::time::Duration::from_secs(60))
            .ok_or_else(unavailable)?;
        let mut grants = self.confirmation_grants.lock().map_err(|_| unavailable())?;
        grants.retain(|_, grant| grant.expires >= now);
        grants.insert(
            token.clone(),
            ConfirmationGrant {
                peer_id,
                resource: offered.resource.clone(),
                action: offered.action.clone(),
                argument_digest: argument_digest(argument),
                actor: actor.to_string(),
                expires,
            },
        );
        drop(grants);
        Ok(Some(token))
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
    ) -> Result<InvocationJob, String> {
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
            .desktop
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
        let id = self.invocation_id.fetch_add(1, Ordering::Relaxed);
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
        self.pending_invocations
            .lock()
            .map_err(|_| "the invocation registry is unavailable".to_string())?
            .insert(
                id,
                PendingInvocation {
                    peer_id,
                    tx,
                    update: tilo_actions::InvocationUpdate::Accepted {
                        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(
                            id,
                        ))),
                    },
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
                resource: offered.1.resource.clone(),
                action: offered.1.action.clone(),
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
    ///
    /// # Errors
    ///
    /// Returns an error when workspace authority, task ownership, or a live
    /// task-capable subscriber is unavailable.
    pub fn dispatch_workspace_task(&self, cluster_id: &str, task_id: &str) -> Result<u64, String> {
        let membership = self
            .membership
            .as_ref()
            .ok_or_else(|| "the task broker has no workspace authority".to_string())?;
        let task_snapshot = {
            let tree = membership.read();
            let lookup = match tree
                .desktop
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
        let (workspace_id, task) = task_snapshot?;
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
        // The durable status and audit entry are committed; do not hold the
        // workspace write lock while returning to the Cap'n Proto caller.
        drop(tree);
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
            .desktop
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
        let membership = self
            .membership
            .as_ref()
            .ok_or_else(|| "the task broker has no workspace authority".to_string())?;
        let mut tree = membership.write();
        let workspace_id = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == run.cluster_id)
            .map(|workspace| workspace.id)
            .ok_or_else(|| "the task workspace is no longer hosted".to_string())?;
        let actor = format!("agent:{}", run.agent_id);
        if let Some(status) = status {
            let _ = tree.set_workspace_task_status(workspace_id, &run.task_id, status, &actor);
        }
        let _ = tree.append_workspace_task_audit(workspace_id, &run.task_id, &actor, summary);
        // Both task mutations are complete before the caller receives its
        // update acknowledgement, so release the workspace lock now.
        drop(tree);
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

    #[allow(
        clippy::too_many_lines,
        reason = "connection teardown intentionally keeps every peer-owned resource in one auditable path"
    )]
    fn remove_connection(&self, peer_id: u64) {
        if let Ok(mut surfaces) = self.surfaces.lock()
            && surfaces.remove(&peer_id).is_some()
        {
            self.surface_generation.fetch_add(1, Ordering::Relaxed);
        }
        if let Ok(mut roots) = self.action_directory_roots.lock() {
            roots.remove(&peer_id);
        }
        self.refresh_action_directory_root_snapshot();
        if let Ok(mut subscribers) = self.invocation_subscribers.lock() {
            subscribers.remove(&peer_id);
        }
        if let Ok(mut subscribers) = self.task_subscribers.lock() {
            subscribers.remove(&peer_id);
        }
        if let Ok(mut targets) = self.nearby_targets.lock() {
            targets.remove_peer(peer_id);
        }
        if let Ok(mut transfers) = self.nearby_transfers.lock() {
            transfers.remove_peer(peer_id);
        }
        if let Ok(mut destinations) = self.nearby_destinations.lock() {
            destinations.remove_peer(peer_id);
        }
        if let Ok(mut realtime) = self.realtime.lock() {
            realtime.subscribers.remove(&peer_id);
            let target_sessions = realtime
                .sessions
                .iter()
                .filter_map(|(token, binding)| {
                    (binding.target_peer == Some(peer_id)).then(|| {
                        realtime
                            .broker
                            .lock()
                            .ok()?
                            .session(binding.id)
                            .and_then(|view| {
                                view.target.map(|target| {
                                    (
                                        token.clone(),
                                        binding.id,
                                        binding.app.clone(),
                                        binding.source.clone(),
                                        binding.owner_peer,
                                        target,
                                        view.source_live,
                                        binding.imported,
                                    )
                                })
                            })
                    })?
                })
                .collect::<Vec<_>>();
            for (token, id, app, source, owner_peer, target, source_live, imported) in
                target_sessions
            {
                release_realtime_capture_session(&realtime.capture, &app, &token, &target);
                if source_live {
                    let failed = if imported {
                        realtime.fabric.fail_imported(
                            id.fabric_id(),
                            &source,
                            &target,
                            std::time::Instant::now(),
                        )
                    } else {
                        realtime
                            .broker
                            .lock()
                            .map_err(|_| crate::shell::realtime_session::RealtimeSessionError::InvalidTransition)
                            .and_then(|mut broker| {
                                broker
                                    .target_failed(&app, id, &source, &target, std::time::Instant::now())
                                    .map(|_| ())
                            })
                    };
                    if failed.is_ok() {
                        realtime.notify(
                            owner_peer,
                            &ui::realtime_continuity::RealtimeEvent::RolledBack {
                                session: token.clone(),
                            },
                        );
                    }
                    if let Some(binding) = realtime.sessions.get_mut(&token) {
                        binding.authorization = None;
                        binding.target_peer = None;
                        binding.capture_leases.clear();
                    }
                } else {
                    if let Ok(mut broker) = realtime.broker.lock() {
                        let _ = broker.apply_media_action(
                            &app,
                            id,
                            &target,
                            crate::shell::realtime_session::RealtimeMediaAction::Leave,
                            std::time::Instant::now(),
                        );
                    }
                    realtime.notify(
                        owner_peer,
                        &ui::realtime_continuity::RealtimeEvent::Ended {
                            session: token.clone(),
                        },
                    );
                    realtime.sessions.remove(&token);
                }
            }
            let owned = realtime
                .sessions
                .iter()
                .filter_map(|(token, binding)| {
                    (binding.owner_peer == peer_id).then_some((
                        token.clone(),
                        binding.id,
                        binding.app.clone(),
                        binding.source.clone(),
                        binding.imported,
                    ))
                })
                .collect::<Vec<_>>();
            for (token, id, app, source, imported) in owned {
                let target = realtime
                    .broker
                    .lock()
                    .ok()
                    .and_then(|broker| broker.session(id))
                    .and_then(|view| view.target);
                if let Some(target) = target.as_ref() {
                    release_realtime_capture_session(&realtime.capture, &app, &token, target);
                }
                if imported {
                    if let Some(target) = target.as_ref() {
                        let _ = realtime.fabric.fail_imported(
                            id.fabric_id(),
                            &source,
                            target,
                            std::time::Instant::now(),
                        );
                    }
                } else if let Ok(mut broker) = realtime.broker.lock() {
                    let _ = broker.apply_media_action(
                        &app,
                        id,
                        &source,
                        crate::shell::realtime_session::RealtimeMediaAction::Leave,
                        std::time::Instant::now(),
                    );
                }
                realtime.sessions.remove(&token);
            }
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
                        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(
                            id,
                        ))),
                        error: tilo_actions::ActionFailure {
                            code: completion.code.clone(),
                            message: completion.message.clone(),
                            details: None,
                            retryable: invocation.retryable_on_disconnect,
                        },
                    };
                    self.record_effect_receipt(&invocation, &update);
                    self.record_completion(id, invocation.actor.clone(), update);
                    if let Ok(mut requests) = self.cancellation_requests.lock() {
                        requests.remove(&id);
                    }
                    let _ = invocation.tx.send(completion);
                }
            }
        }
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
    ) -> Result<collab_types::RoutedDocumentPayload, capnp::Error> {
        let expected_artifact =
            document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id);
        let expected_attestation =
            document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id);
        if route.app_id.is_empty()
            || envelope.artifact != expected_artifact
            || envelope.attestation_id != expected_attestation
        {
            return Err(capnp::Error::failed(
                "certified document operation does not match its joined route".into(),
            ));
        }
        let mutation = collab_types::validate_document_payload_route(
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
        let channel =
            collab_types::document_channel_id(&route.app_id, &route.cluster_id, &route.record_id);
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
        let tree = membership.read();
        let workspace = tree
            .desktop
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
            | CollabEvent::RemoteAction { .. }
            | CollabEvent::WorkspaceControl { .. }
            | CollabEvent::TaskRun(_)
            | CollabEvent::TaskActionInvocationResult { .. }
            | CollabEvent::TaskRunCancellation { .. }
            | CollabEvent::NearbyTransferCommandResult { .. }
            | CollabEvent::RealtimeCommandResult { .. }
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
    directories: Rc<LocalActionDirectoryRegistry>,
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
                target: "tilo_collab_probe",
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
            state.publisher().local_route(&route);

            let handle = CapnpChannelHandle {
                state: state.clone(),
                directories: Rc::clone(&self.directories),
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
    directories: Rc<LocalActionDirectoryRegistry>,
    channel_id: String,
    peer_id: u64,
    verified_identity: AdmittedJoinIdentity,
}

const MAX_PRIVATE_PLAYBACK_REPORT_BYTES: usize = 256 * 1024;

fn continuation_route(
    route: &CrdtRoute,
    claimed_app_id: &str,
    operation: &str,
) -> Result<
    (
        crate::shell::state::RoomId,
        String,
        crate::shell::personal_continuity::AppInstanceId,
    ),
    String,
> {
    if route.app_id.is_empty() || claimed_app_id != route.app_id {
        return Err(format!(
            "{operation} appId must match the certified connection route"
        ));
    }
    let instance_id = route
        .record_id
        .strip_prefix("continuation:")
        .and_then(|id| id.parse::<u64>().ok())
        .map(crate::shell::personal_continuity::AppInstanceId)
        .ok_or_else(|| format!("{operation} requires a durable shell tab id"))?;
    let room = crate::shell::state::RoomId::parse(&route.cluster_id)
        .ok_or_else(|| format!("{operation} route has no durable workspace room"))?;
    Ok((room, claimed_app_id.to_owned(), instance_id))
}

fn report_continuation_for_route(
    reports: &mut crate::shell::personal_continuity_registry::ContinuationRegistry,
    policy: crate::shell::continuity_policy::ContinuityPolicySnapshot,
    route: &CrdtRoute,
    claimed_app_id: &str,
    schema_version: u16,
    metadata: Vec<u8>,
) -> Result<(), String> {
    if !policy.permits(crate::shell::continuity_policy::ContinuityCapability::AppContinuations) {
        return Err("app continuation is disabled".into());
    }
    let (room, app_id, instance_id) =
        continuation_route(route, claimed_app_id, "reportContinuation")?;
    reports
        .report(room, app_id, instance_id, schema_version, metadata)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn clear_continuation_for_route(
    reports: &mut crate::shell::personal_continuity_registry::ContinuationRegistry,
    route: &CrdtRoute,
    claimed_app_id: &str,
) -> Result<bool, String> {
    let (room, app_id, instance_id) =
        continuation_route(route, claimed_app_id, "clearContinuation")?;
    Ok(reports.clear(room, &app_id, instance_id))
}

fn playback_report_owner(route: &CrdtRoute, peer_id: u64) -> String {
    format!(
        "{}#{}#{}#{peer_id}",
        route.cluster_id, route.app_id, route.record_id
    )
}

impl Drop for CapnpChannelHandle {
    fn drop(&mut self) {
        media::handoff::remove_external_reports(&playback_report_owner(
            &self.route(),
            self.peer_id,
        ));
        self.state.remove_connection(self.peer_id);
        self.directories.remove(self.peer_id);
    }
}

impl CapnpChannelHandle {
    /// A record-scoped connection names its record inside the channel id
    /// (the SDK joins `<cluster>#record:<id>`), parsed back out here.
    fn route(&self) -> CrdtRoute {
        CrdtRoute::from_channel_key(&self.channel_id)
    }

    fn realtime_registration(
        &self,
    ) -> Result<
        (
            crate::shell::realtime_session::VerifiedAppRegistration,
            crate::shell::personal_continuity::DeviceId,
        ),
        capnp::Error,
    > {
        self.state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        let route = self.route();
        if route.app_id.is_empty() || route.cluster_id.is_empty() {
            return Err(capnp::Error::failed(
                "realtime continuity requires a certified app/workspace route".into(),
            ));
        }
        let app = crate::shell::realtime_session::VerifiedAppRegistration::new_verified(
            route.app_id,
            route.cluster_id,
        )
        .map_err(|error| capnp::Error::failed(error.to_string()))?;
        let local = self
            .state
            .workspace_control_network()
            .and_then(|network| network.local_device_fabric_identity())
            .map(|(device, _)| device)
            .ok_or_else(|| {
                capnp::Error::failed("realtime continuity requires a certified local device".into())
            })?;
        Ok((app, local))
    }

    fn nearby_registration(
        &self,
    ) -> Result<
        (
            CrdtRoute,
            Arc<crate::shell::network::ShellNetwork>,
            Vec<Capability>,
        ),
        capnp::Error,
    > {
        let route = self.route();
        self.state
            .revalidate_admission(&route, &self.verified_identity)?;
        if route.app_id != FILES_APP_ID || route.cluster_id.is_empty() {
            return Err(capnp::Error::failed(
                "Nearby Send requires the certified Files application route".into(),
            ));
        }
        if !nearby_policy_enabled(crate::shell::continuity_policy::ContinuityPolicySnapshot::load())
        {
            return Err(capnp::Error::failed(
                "Nearby Send is disabled by continuity privacy settings".into(),
            ));
        }
        let network = self.state.workspace_control_network().ok_or_else(|| {
            capnp::Error::failed("Nearby Send requires the certified device fabric".into())
        })?;
        let membership = self.state.membership.as_ref().ok_or_else(|| {
            capnp::Error::failed("Nearby Send lost its workspace grant authority".into())
        })?;
        let grants = membership
            .read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == route.cluster_id)
            .and_then(|workspace| workspace.app_grants_snapshot().remove(FILES_APP_ID))
            .unwrap_or_default();
        Ok((route, network, grants))
    }
}

fn nearby_transfer_reply(
    results: &mut channel_handle::NearbyTransferCommandResults,
    reply: runtime_collab::NearbyTransferReply,
) -> Promise<(), capnp::Error> {
    if !reply.valid() {
        return Promise::err(capnp::Error::failed(
            "Nearby Send produced an invalid bounded reply".into(),
        ));
    }
    let bytes = match serde_json::to_vec(&reply) {
        Ok(bytes) if bytes.len() <= runtime_collab::MAX_NEARBY_TRANSFER_WIRE_BYTES => bytes,
        Ok(_) => {
            return Promise::err(capnp::Error::failed(
                "Nearby Send reply exceeds the local protocol bound".into(),
            ));
        }
        Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
    };
    results.get().set_reply_json(&bytes);
    Promise::ok(())
}

impl channel_handle::Server for CapnpChannelHandle {
    /// The caller-side door to published actions: one remote-action request
    /// frame in, one answer out, as the caller this channel was admitted as.
    fn remote_action(
        self: Rc<Self>,
        params: channel_handle::RemoteActionParams,
        mut results: channel_handle::RemoteActionResults,
    ) -> Promise<(), capnp::Error> {
        use tilo_actions::remote::{RemoteActionFrame, serve};
        let current = match self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            Ok(current) => current,
            Err(error) => return Promise::err(error),
        };
        let frame = match params
            .get()
            .and_then(channel_handle::remote_action_params::Reader::get_frame)
        {
            Ok(frame) => frame,
            Err(error) => return Promise::err(error),
        };
        let request = match RemoteActionFrame::decode(frame) {
            Ok(request) => request,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let caller = current.caller(&self.route().cluster_id);
        let host = crate::shell::remote_actions::BrokerActionHost::for_connection(
            crate::shell::collab_broker::AppActionBroker::from_state(Arc::clone(&self.state)),
            self.peer_id,
        );
        let Some(answer) = serve(&host, &caller, request.message) else {
            return Promise::err(capnp::Error::failed(
                "only remote-action requests are answered".into(),
            ));
        };
        // Awaited on this local set, which is also where an application's
        // directory answers; the connection's other calls keep flowing.
        Promise::from_future(async move {
            let answer = answer.await.ok_or_else(|| {
                capnp::Error::failed("the action host dropped the request".into())
            })?;
            let bytes = RemoteActionFrame::new(answer)
                .encode()
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
            results.get().set_frame(&bytes);
            Ok(())
        })
    }

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

    fn nearby_transfer_command(
        self: Rc<Self>,
        params: channel_handle::NearbyTransferCommandParams,
        mut results: channel_handle::NearbyTransferCommandResults,
    ) -> Promise<(), capnp::Error> {
        let (route, network, grants) = match self.nearby_registration() {
            Ok(binding) => binding,
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
        if claimed_app != route.app_id {
            return Promise::err(capnp::Error::failed(
                "Nearby Send app id must match the certified connection route".into(),
            ));
        }
        let command_bytes = match request.get_command_json() {
            Ok(bytes) if bytes.len() <= runtime_collab::MAX_NEARBY_TRANSFER_WIRE_BYTES => bytes,
            Ok(_) => {
                return Promise::err(capnp::Error::failed(
                    "Nearby Send command exceeds the local protocol bound".into(),
                ));
            }
            Err(error) => return Promise::err(error),
        };
        let command: runtime_collab::NearbyTransferCommand =
            match serde_json::from_slice(command_bytes) {
                Ok(command) => command,
                Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
            };
        if !command.valid() {
            return Promise::err(capnp::Error::failed(
                "Nearby Send command is structurally invalid".into(),
            ));
        }
        let reply = match command {
            runtime_collab::NearbyTransferCommand::Snapshot => {
                let local = network
                    .local_device_fabric_identity()
                    .map(|(device, _)| device);
                let targets = network
                    .device_fabric()
                    .snapshots()
                    .values()
                    .filter(|snapshot| {
                        snapshot.presence.online && local.as_ref() != Some(&snapshot.owner)
                    })
                    .take(runtime_collab::MAX_NEARBY_TRANSFER_TARGETS)
                    .map(|snapshot| (snapshot.owner.clone(), snapshot.presence.label.clone()))
                    .collect::<Vec<_>>();
                let Ok(mut registry) = self.state.nearby_targets.lock() else {
                    return Promise::err(capnp::Error::failed(
                        "Nearby Send target authority is unavailable".into(),
                    ));
                };
                let now = Instant::now();
                let device_ids = targets
                    .iter()
                    .map(|(device, _)| device.clone())
                    .collect::<Vec<_>>();
                registry.begin_snapshot(self.peer_id, &route, &device_ids, now);
                let raw_transfers =
                    crate::shell::device_fabric_runtime::nearby_transfer_statuses(&network);
                let Ok(mut transfer_registry) = self.state.nearby_transfers.lock() else {
                    return Promise::err(capnp::Error::failed(
                        "Nearby Send transfer authority is unavailable".into(),
                    ));
                };
                let visible_ids = raw_transfers
                    .iter()
                    .map(|status| status.transfer_id)
                    .collect::<Vec<_>>();
                transfer_registry.begin_snapshot(self.peer_id, &route, &visible_ids, now);
                let transfers = raw_transfers
                    .into_iter()
                    .filter_map(|status| {
                        let handle = transfer_registry.issue(
                            self.peer_id,
                            &route,
                            status.transfer_id,
                            status.direction,
                            true,
                            now,
                        )?;
                        Some(runtime_collab::NearbyTransferStatus {
                            handle,
                            name: status.name,
                            device_label: status.device_label,
                            direction: status.direction,
                            phase: status.phase,
                            transferred_bytes: status.transferred_bytes,
                            total_bytes: status.total_bytes,
                        })
                    })
                    .collect();
                let destinations = {
                    let Ok(mut destinations) = self.state.nearby_destinations.lock() else {
                        return Promise::err(capnp::Error::failed(
                            "Nearby Send destination authority is unavailable".into(),
                        ));
                    };
                    grants
                        .iter()
                        .filter_map(|grant| {
                            let Capability::Filesystem {
                                path,
                                mode: FsMode::ReadWrite,
                                ..
                            } = grant
                            else {
                                return None;
                            };
                            let grant = std::fs::canonicalize(path).ok()?;
                            if !std::fs::metadata(&grant).ok()?.is_dir() {
                                return None;
                            }
                            let label = grant
                                .file_name()
                                .and_then(|name| name.to_str())
                                .filter(|name| !name.is_empty())
                                .unwrap_or("Workspace files")
                                .chars()
                                .take(runtime_collab::MAX_NEARBY_TRANSFER_LABEL_BYTES)
                                .collect::<String>();
                            Some(destinations.issue(self.peer_id, &route, grant, label, now))
                        })
                        .take(runtime_collab::MAX_NEARBY_TRANSFER_DESTINATIONS)
                        .collect::<Vec<_>>()
                };
                runtime_collab::NearbyTransferReply::Snapshot {
                    snapshot: runtime_collab::NearbyTransferSnapshot {
                        targets: targets
                            .into_iter()
                            .map(|(device, label)| runtime_collab::NearbyTransferTarget {
                                handle: registry.issue(self.peer_id, &route, device, now),
                                label,
                                online: true,
                            })
                            .collect(),
                        destinations,
                        transfers,
                    },
                }
            }
            runtime_collab::NearbyTransferCommand::Send {
                target,
                source_path,
            } => {
                let source = match nearby_source_from_grants(&source_path, &grants) {
                    Ok(source) => source,
                    Err(reason) => {
                        return nearby_transfer_reply(
                            &mut results,
                            runtime_collab::NearbyTransferReply::Rejected { reason },
                        );
                    }
                };
                let target = self
                    .state
                    .nearby_targets
                    .lock()
                    .ok()
                    .and_then(|mut registry| {
                        registry.consume(target, self.peer_id, &route, Instant::now())
                    });
                let Some(target) = target else {
                    return nearby_transfer_reply(
                        &mut results,
                        runtime_collab::NearbyTransferReply::Rejected {
                            reason: "Nearby Send target expired; reopen the device picker".into(),
                        },
                    );
                };
                let staging_owner = format!("{}:{}", self.peer_id, route.channel_key());
                match crate::shell::device_fabric_runtime::request_nearby_drop_secure(
                    &network,
                    &target,
                    source,
                    &staging_owner,
                ) {
                    Ok(transfer_id) => {
                        let handle =
                            self.state
                                .nearby_transfers
                                .lock()
                                .ok()
                                .and_then(|mut registry| {
                                    registry.issue(
                                        self.peer_id,
                                        &route,
                                        transfer_id,
                                        runtime_collab::NearbyTransferDirection::Sending,
                                        true,
                                        Instant::now(),
                                    )
                                });
                        match handle {
                            Some(handle) => runtime_collab::NearbyTransferReply::Queued { handle },
                            None => {
                                let _ = crate::shell::device_fabric_runtime::cancel_nearby_transfer(
                                    &network,
                                    transfer_id,
                                );
                                runtime_collab::NearbyTransferReply::Rejected {
                                    reason: "Nearby Send transfer authority is full".into(),
                                }
                            }
                        }
                    }
                    Err(reason) => runtime_collab::NearbyTransferReply::Rejected { reason },
                }
            }
            runtime_collab::NearbyTransferCommand::Cancel { handle } => {
                let transfer_id =
                    self.state
                        .nearby_transfers
                        .lock()
                        .ok()
                        .and_then(|mut registry| {
                            registry.resolve(
                                handle,
                                self.peer_id,
                                &route,
                                runtime_collab::NearbyTransferDirection::Sending,
                                Instant::now(),
                            )
                        });
                let Some(transfer_id) = transfer_id else {
                    return nearby_transfer_reply(
                        &mut results,
                        runtime_collab::NearbyTransferReply::Rejected {
                            reason:
                                "Nearby Send transfer expired or belongs to another Files workspace"
                                    .into(),
                        },
                    );
                };
                match crate::shell::device_fabric_runtime::cancel_nearby_transfer(
                    &network,
                    transfer_id,
                ) {
                    Ok(()) => runtime_collab::NearbyTransferReply::Accepted,
                    Err(reason) => runtime_collab::NearbyTransferReply::Rejected { reason },
                }
            }
            runtime_collab::NearbyTransferCommand::Decide {
                handle,
                accept,
                destination,
                remember_for_session,
            } => {
                let transfer_id =
                    self.state
                        .nearby_transfers
                        .lock()
                        .ok()
                        .and_then(|mut registry| {
                            registry.resolve(
                                handle,
                                self.peer_id,
                                &route,
                                runtime_collab::NearbyTransferDirection::Receiving,
                                Instant::now(),
                            )
                        });
                let Some(transfer_id) = transfer_id else {
                    return nearby_transfer_reply(
                        &mut results,
                        runtime_collab::NearbyTransferReply::Rejected {
                            reason:
                                "Nearby Send transfer expired or belongs to another Files workspace"
                                    .into(),
                        },
                    );
                };
                let destination =
                    if accept {
                        destination.and_then(|destination| {
                            self.state.nearby_destinations.lock().ok().and_then(
                                |mut destinations| {
                                    destinations.resolve(
                                        destination,
                                        self.peer_id,
                                        &route,
                                        remember_for_session,
                                        Instant::now(),
                                    )
                                },
                            )
                        })
                    } else {
                        None
                    };
                if accept && destination.is_none() {
                    return nearby_transfer_reply(
                        &mut results,
                        runtime_collab::NearbyTransferReply::Rejected {
                            reason: "Nearby Send destination expired; choose it again".into(),
                        },
                    );
                }
                match crate::shell::device_fabric_runtime::decide_nearby_transfer_at(
                    &network,
                    transfer_id,
                    accept,
                    destination,
                ) {
                    Ok(()) => runtime_collab::NearbyTransferReply::Accepted,
                    Err(reason) => runtime_collab::NearbyTransferReply::Rejected { reason },
                }
            }
            runtime_collab::NearbyTransferCommand::Retry { handle } => {
                let transfer_id =
                    self.state
                        .nearby_transfers
                        .lock()
                        .ok()
                        .and_then(|mut registry| {
                            registry.resolve(
                                handle,
                                self.peer_id,
                                &route,
                                runtime_collab::NearbyTransferDirection::Sending,
                                Instant::now(),
                            )
                        });
                let Some(transfer_id) = transfer_id else {
                    return nearby_transfer_reply(
                        &mut results,
                        runtime_collab::NearbyTransferReply::Rejected {
                            reason:
                                "Nearby Send transfer expired or belongs to another Files workspace"
                                    .into(),
                        },
                    );
                };
                match crate::shell::device_fabric_runtime::retry_nearby_transfer(
                    &network,
                    transfer_id,
                ) {
                    Ok(()) => runtime_collab::NearbyTransferReply::Accepted,
                    Err(reason) => runtime_collab::NearbyTransferReply::Rejected { reason },
                }
            }
        };
        nearby_transfer_reply(&mut results, reply)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one authenticated RPC boundary validates and applies every realtime transition atomically so route binding and cutover acknowledgement cannot drift"
    )]
    fn realtime_command(
        self: Rc<Self>,
        params: channel_handle::RealtimeCommandParams,
        mut results: channel_handle::RealtimeCommandResults,
    ) -> Promise<(), capnp::Error> {
        let (app, local) = match self.realtime_registration() {
            Ok(binding) => binding,
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
        if claimed_app != app.app_id() {
            return Promise::err(capnp::Error::failed(
                "realtime app id must match the certified connection route".into(),
            ));
        }
        let bytes = match request.get_command_json() {
            Ok(bytes) if bytes.len() <= ui::realtime_continuity::MAX_REALTIME_WIRE_BYTES => bytes,
            Ok(_) => {
                return Promise::err(capnp::Error::failed(
                    "realtime command exceeds local protocol bound".into(),
                ));
            }
            Err(error) => return Promise::err(error),
        };
        let command: ui::realtime_continuity::RealtimeCommand = match serde_json::from_slice(bytes)
        {
            Ok(command) => command,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        if !command.valid() {
            return Promise::err(capnp::Error::failed(
                "realtime command contains an unsafe semantic locator".into(),
            ));
        }
        let Ok(mut bridge) = self.state.realtime.lock() else {
            return Promise::err(capnp::Error::failed(
                "realtime broker is unavailable".into(),
            ));
        };
        let now = std::time::Instant::now();
        let reply = match command {
            ui::realtime_continuity::RealtimeCommand::Register { intent } => {
                let mut facets = Vec::with_capacity(4);
                if intent.media.microphone {
                    facets.push(crate::shell::realtime_session::RealtimeMediaFacet::Microphone);
                }
                if intent.media.camera {
                    facets.push(crate::shell::realtime_session::RealtimeMediaFacet::Camera);
                }
                if intent.media.screen_share {
                    facets.push(crate::shell::realtime_session::RealtimeMediaFacet::ScreenShare);
                }
                if intent.media.share_audio {
                    facets.push(crate::shell::realtime_session::RealtimeMediaFacet::ShareAudio);
                }
                let registered = crate::shell::realtime_session::RealtimeResumeIntent {
                    room: match crate::shell::realtime_session::SemanticRoomLocator::new(
                        intent.provider,
                        intent.room_id,
                    ) {
                        Ok(room) => room,
                        Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
                    },
                    semantic_position: intent.semantic_position,
                    media: match crate::shell::realtime_session::RealtimeMediaSet::new(facets) {
                        Ok(media) => media,
                        Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
                    },
                    controls: broker_realtime_controls(intent.controls),
                };
                let id = match bridge
                    .broker
                    .lock()
                    .map_err(|_| {
                        crate::shell::realtime_session::RealtimeSessionError::InvalidTransition
                    })
                    .and_then(|mut broker| broker.register(&app, local.clone(), registered, now))
                {
                    Ok(id) => id,
                    Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
                };
                let token = bridge.new_token();
                bridge.sessions.insert(
                    token.clone(),
                    RealtimeBinding {
                        id,
                        app,
                        source: local,
                        owner_peer: self.peer_id,
                        authorization: None,
                        target_peer: None,
                        capture_leases: Vec::new(),
                        imported: false,
                    },
                );
                ui::realtime_continuity::RealtimeReply::Registered { session: token }
            }
            ui::realtime_continuity::RealtimeCommand::Accept {
                session,
                capture_authorizations,
            } => {
                let Some(binding) = bridge.sessions.get(&session) else {
                    return Promise::err(capnp::Error::failed("unknown realtime session".into()));
                };
                if binding.app != app {
                    return Promise::err(capnp::Error::failed(
                        "realtime session binding mismatch".into(),
                    ));
                }
                if binding.authorization.is_some() || binding.target_peer.is_some() {
                    return Promise::err(capnp::Error::failed(
                        "realtime target already accepted".into(),
                    ));
                }
                let (id, source, imported) = (binding.id, binding.source.clone(), binding.imported);
                let Some(required) = bridge.required_capture_kinds(id) else {
                    return Promise::err(capnp::Error::failed("unknown realtime session".into()));
                };
                let leases = match acquire_realtime_capture_leases(
                    &bridge.capture,
                    &app,
                    &session,
                    &local,
                    &capture_authorizations,
                    &required,
                    now,
                ) {
                    Ok(leases) => leases,
                    Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
                };
                let authorization =
                    if imported {
                        if let Err(error) =
                            bridge
                                .fabric
                                .accept_imported(id.fabric_id(), &source, &local, now)
                        {
                            release_realtime_capture_leases(&bridge.capture, &leases);
                            return Promise::err(capnp::Error::failed(error.to_string()));
                        }
                        None
                    } else {
                        let authorization = match bridge.broker.lock().map_err(|_| {
                        crate::shell::realtime_session::RealtimeSessionError::InvalidTransition
                    }).and_then(|mut broker| {
                        broker.authorize_target_capture(&app, id, &source, &local, now)
                    }) {
                        Ok(authorization) => authorization,
                        Err(error) => {
                            release_realtime_capture_leases(&bridge.capture, &leases);
                            return Promise::err(capnp::Error::failed(error.to_string()));
                        }
                    };
                        if let Err(error) = bridge.broker.lock().map_err(|_| {
                        crate::shell::realtime_session::RealtimeSessionError::InvalidTransition
                    }).and_then(|mut broker| {
                        broker.accept_target(&app, id, &source, &local, authorization, now)
                    }) {
                        release_realtime_capture_leases(&bridge.capture, &leases);
                        return Promise::err(capnp::Error::failed(error.to_string()));
                    }
                        Some(authorization)
                    };
                if let Some(binding) = bridge.sessions.get_mut(&session) {
                    binding.authorization = authorization;
                    binding.target_peer = Some(self.peer_id);
                    binding.capture_leases = leases;
                }
                bridge.consent_capabilities_issued.remove(&session);
                ui::realtime_continuity::RealtimeReply::Accepted
            }
            ui::realtime_continuity::RealtimeCommand::MediaReady { session } => {
                let Some(binding) = bridge.sessions.get(&session) else {
                    return Promise::err(capnp::Error::failed("unknown realtime session".into()));
                };
                if binding.app != app {
                    return Promise::err(capnp::Error::failed(
                        "realtime session binding mismatch".into(),
                    ));
                }
                if !binding.imported && binding.authorization.is_none() {
                    return Promise::err(capnp::Error::failed(
                        "fresh target capture authorization is required".into(),
                    ));
                }
                if binding.target_peer != Some(self.peer_id) {
                    return Promise::err(capnp::Error::failed(
                        "realtime target binding mismatch".into(),
                    ));
                }
                let (id, owner_peer, source, leases, imported, authorization) = (
                    binding.id,
                    binding.owner_peer,
                    binding.source.clone(),
                    binding.capture_leases.clone(),
                    binding.imported,
                    binding.authorization,
                );
                if let Err(error) = realtime_capture_leases_ready(
                    &bridge.capture,
                    &app,
                    &session,
                    &local,
                    &leases,
                    now,
                ) {
                    if imported {
                        let _ = bridge
                            .fabric
                            .fail_imported(id.fabric_id(), &source, &local, now);
                    } else if let Ok(mut broker) = bridge.broker.lock() {
                        let _ = broker.target_failed(&app, id, &source, &local, now);
                    }
                    if let Some(binding) = bridge.sessions.get_mut(&session) {
                        binding.authorization = None;
                        binding.target_peer = None;
                        binding.capture_leases.clear();
                    }
                    release_realtime_capture_session(&bridge.capture, &app, &session, &local);
                    bridge.notify(
                        owner_peer,
                        &ui::realtime_continuity::RealtimeEvent::RolledBack {
                            session: session.clone(),
                        },
                    );
                    return Promise::err(capnp::Error::failed(error.to_string()));
                }
                let directive = if imported {
                    bridge
                        .fabric
                        .imported_media_ready(id.fabric_id(), &source, &local, now)
                        .map(|()| None)
                } else {
                    let Some(authorization) = authorization else {
                        return Promise::err(capnp::Error::failed(
                            "fresh target capture authorization is required".into(),
                        ));
                    };
                    bridge
                        .broker
                        .lock()
                        .map_err(|_| {
                            crate::shell::realtime_session::RealtimeSessionError::InvalidTransition
                        })
                        .and_then(|mut broker| {
                            broker
                                .media_ready(&app, id, &source, &local, authorization, now)
                                .map(Some)
                        })
                };
                let directive = match directive {
                    Ok(directive) => directive,
                    Err(error) => {
                        if imported {
                            let _ =
                                bridge
                                    .fabric
                                    .fail_imported(id.fabric_id(), &source, &local, now);
                        } else if let Ok(mut broker) = bridge.broker.lock() {
                            let _ = broker.target_failed(&app, id, &source, &local, now);
                        }
                        if let Some(binding) = bridge.sessions.get_mut(&session) {
                            binding.authorization = None;
                            binding.target_peer = None;
                            binding.capture_leases.clear();
                        }
                        release_realtime_capture_session(&bridge.capture, &app, &session, &local);
                        bridge.notify(
                            owner_peer,
                            &ui::realtime_continuity::RealtimeEvent::RolledBack {
                                session: session.clone(),
                            },
                        );
                        return Promise::err(capnp::Error::failed(error.to_string()));
                    }
                };
                if directive.is_some_and(|directive| directive.stop_source) {
                    bridge.notify(
                        owner_peer,
                        &ui::realtime_continuity::RealtimeEvent::StopSource {
                            session: session.clone(),
                        },
                    );
                }
                ui::realtime_continuity::RealtimeReply::MediaReady
            }
            ui::realtime_continuity::RealtimeCommand::TargetFailed { session } => {
                let Some(binding) = bridge.sessions.get(&session) else {
                    return Promise::err(capnp::Error::failed("unknown realtime session".into()));
                };
                if binding.app != app {
                    return Promise::err(capnp::Error::failed(
                        "realtime session binding mismatch".into(),
                    ));
                }
                if binding.target_peer != Some(self.peer_id) {
                    return Promise::err(capnp::Error::failed(
                        "realtime target binding mismatch".into(),
                    ));
                }
                let (id, owner_peer, source) =
                    (binding.id, binding.owner_peer, binding.source.clone());
                let failed = if binding.imported {
                    bridge
                        .fabric
                        .fail_imported(id.fabric_id(), &source, &local, now)
                } else {
                    bridge
                        .broker
                        .lock()
                        .map_err(|_| {
                            crate::shell::realtime_session::RealtimeSessionError::InvalidTransition
                        })
                        .and_then(|mut broker| {
                            broker
                                .target_failed(&app, id, &source, &local, now)
                                .map(|_| ())
                        })
                };
                if let Err(error) = failed {
                    return Promise::err(capnp::Error::failed(error.to_string()));
                }
                if let Some(binding) = bridge.sessions.get_mut(&session) {
                    binding.authorization = None;
                    binding.target_peer = None;
                    binding.capture_leases.clear();
                }
                release_realtime_capture_session(&bridge.capture, &app, &session, &local);
                bridge.notify(
                    owner_peer,
                    &ui::realtime_continuity::RealtimeEvent::RolledBack {
                        session: session.clone(),
                    },
                );
                ui::realtime_continuity::RealtimeReply::RolledBack
            }
        };
        drop(bridge);
        let bytes = match serde_json::to_vec(&reply) {
            Ok(bytes) => bytes,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        results.get().set_reply_json(&bytes);
        Promise::ok(())
    }

    fn subscribe_realtime(
        self: Rc<Self>,
        _params: channel_handle::SubscribeRealtimeParams,
        mut results: channel_handle::SubscribeRealtimeResults,
    ) -> Promise<(), capnp::Error> {
        let (app, _) = match self.realtime_registration() {
            Ok(binding) => binding,
            Err(error) => return Promise::err(error),
        };
        let (tx, rx) = mpsc::unbounded_channel();
        let Ok(mut bridge) = self.state.realtime.lock() else {
            return Promise::err(capnp::Error::failed("realtime broker unavailable".into()));
        };
        bridge
            .subscribers
            .insert(self.peer_id, RealtimeSubscriber { app, tx });
        drop(bridge);
        self.state.pump_realtime_fabric_app_events();
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
        let Some(membership) = &self.state.membership else {
            return Promise::err(capnp::Error::failed(
                "the action broker has no workspace authority".into(),
            ));
        };
        let mut tree = membership.write();
        let Some(workspace_id) = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == surface.route.cluster_id)
            .map(|workspace| workspace.id)
        else {
            return Promise::err(capnp::Error::failed(
                "the application workspace is no longer hosted".into(),
            ));
        };
        // Local invocations have no task ambient context; retain this as an
        // ordinary semantic action record.
        context.purpose = None;
        let _ = tree.append_workspace_effect_receipt(
            workspace_id,
            update.invocation(),
            &actor,
            context,
            status,
        );
        // Receipt persistence is complete; do not retain workspace authority
        // while resolving the Cap'n Proto promise.
        drop(tree);
        Promise::ok(())
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
        let Some(room) = crate::shell::state::RoomId::parse(&route.cluster_id) else {
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
        let Ok(target) = iroh::EndpointId::from_bytes(&target_bytes) else {
            return Promise::err(capnp::Error::failed(
                "invalid target device identity".into(),
            ));
        };
        let Some(network) = self.state.workspace_control_network() else {
            return Promise::err(capnp::Error::failed(
                "workspace-control network is not attached".into(),
            ));
        };
        let workspace = crate::shell::network::room_topic_id(room);
        let session = match network.open_workspace_control(workspace, target) {
            Ok(session) => session,
            Err(error) => return Promise::err(capnp::Error::failed(error)),
        };
        let initial_route = match session.route() {
            crate::network::WorkspaceControlRoute::Direct { .. } => {
                runtime_collab::WorkspaceControlRoute::Direct
            }
            crate::network::WorkspaceControlRoute::Relayed { via } => {
                runtime_collab::WorkspaceControlRoute::Relayed {
                    via: *via.as_bytes(),
                }
            }
        };
        let server = CapnpWorkspaceControlHandle {
            state: self.state.clone(),
            route,
            verified_identity: self.verified_identity.clone(),
            network: network.clone(),
            session: Rc::new(AsyncMutex::new(session)),
            inbound: Rc::new(AsyncMutex::new(network.subscribe_workspace_control())),
            initial: Rc::new(AsyncMutex::new(Some(
                runtime_collab::WorkspaceControlEvent::WaitingForConsent {
                    route: initial_route,
                },
            ))),
        };
        let client: workspace_control_handle::Client = capnp_rpc::new_client(server);
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
                let view = row
                    .has_view()
                    .then(|| row.get_view())
                    .transpose()?
                    .map(|view| {
                        let description = view.get_description()?.to_str()?.to_owned();
                        Ok::<_, capnp::Error>(tilo_actions::ResourceViewDescriptor {
                            id: view.get_id()?.to_str()?.to_owned(),
                            label: view.get_label()?.to_str()?.to_owned(),
                            description: (!description.is_empty()).then_some(description),
                            open_action: view.get_open_action()?.to_str()?.to_owned(),
                            primary: view.get_primary(),
                            mount: view
                                .has_mount()
                                .then(|| view.get_mount())
                                .transpose()?
                                .map(|mount| {
                                    Ok::<_, capnp::Error>(tilo_actions::ResourceViewMountSpec {
                                        action: mount.get_action()?.to_str()?.to_owned(),
                                        capability_param: mount
                                            .get_capability_param()?
                                            .to_str()?
                                            .to_owned(),
                                        view_param: mount.get_view_param()?.to_str()?.to_owned(),
                                    })
                                })
                                .transpose()?,
                        })
                    })
                    .transpose()?;
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
                    view,
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
            validate_resource_view_descriptors(&actions)?;
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
        Promise::ok(())
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
        let caller = current.caller(&self.route().cluster_id);
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
            let invocation = uuid::Uuid::parse_str(request.get_invocation()?.to_str()?)
                .map(tilo_actions::InvocationId)
                .map_err(|_| capnp::Error::failed("task action needs an invocation id".into()))?;
            let target_peer = request.get_peer_id();
            // Admitted through the same idempotency ledger as every other
            // caller path, so a retry cannot start the action twice and the
            // invocation is observable and cancellable by its id.
            let synthesized = tilo_actions::remote::InvokeRequest {
                invocation,
                target: tilo_actions::remote::RemoteActionTarget {
                    provider: target_peer.to_string(),
                    address: tilo_actions::ActionAddress::new(&resource, &action),
                },
                argument: argument
                    .clone()
                    .map_or(tilo_actions::remote::InvokeArgument::None, |argument| {
                        tilo_actions::remote::InvokeArgument::Text(argument)
                    }),
                context: None,
                confirmation: None,
                revision: None,
            };
            let run_id = request.get_run_id();
            let job = state
                .remote_invocations
                .lock()
                .map_err(|_| capnp::Error::failed("the invocation ledger is unavailable".into()))?
                .admit(&caller, &synthesized, || {
                    state
                        .start_task_action(
                            peer_id,
                            &agent_id,
                            run_id,
                            &lease,
                            target_peer,
                            &resource,
                            &action,
                            argument.as_deref(),
                        )
                        .map(|job| job.id)
                        .map_err(|reason| tilo_actions::remote::ActionHostError::Refused { reason })
                })
                .map(|(job, _)| *job)
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
            let update = state.invocation_status(job).ok_or_else(|| {
                capnp::Error::failed("accepted task action has no broker state".into())
            })?;
            results.get().set_id(job);
            results.get().set_update_json(
                &serde_json::to_string(&update)
                    .map_err(|error| capnp::Error::failed(error.to_string()))?,
            );
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
                    .validate(&tilo_actions::ActionOutput::default())
                    .map_err(|error| {
                        capnp::Error::failed(format!(
                            "legacy completion violated its published result contract: {error}"
                        ))
                    })?;
            }
            let pending = registry
                .remove(&id)
                .ok_or_else(|| capnp::Error::failed("unknown invocation id".into()))?;
            let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id)));
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
            drop(registry);
            state.record_effect_receipt(&pending, &update);
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
            let update: tilo_actions::InvocationUpdate =
                serde_json::from_str(json).map_err(|error| {
                    capnp::Error::failed(format!("invalid invocation update: {error}"))
                })?;
            let expected = tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id)));
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
                pending.last_sequence = *sequence;
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
                pending.last_sequence = *sequence;
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
                    tilo_actions::InvocationUpdate::Cancelled { message, .. } => {
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
                state.record_effect_receipt(&pending, &update);
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
        let metadata = match request.get_metadata() {
            Ok(bytes) => bytes.to_vec(),
            Err(error) => return Promise::err(error),
        };
        match crate::shell::personal_continuity_registry::global_registry()
            .lock()
            .map_err(|_| "continuation registry unavailable".to_string())
            .and_then(|mut reports| {
                report_continuation_for_route(
                    &mut reports,
                    crate::shell::continuity_policy::ContinuityPolicySnapshot::load(),
                    &route,
                    &app_id,
                    request.get_schema_version(),
                    metadata,
                )
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
        match crate::shell::personal_continuity_registry::global_registry()
            .lock()
            .map_err(|_| "continuation registry unavailable".to_string())
        {
            Ok(mut reports) => match clear_continuation_for_route(&mut reports, &route, &app_id) {
                Ok(_) => Promise::ok(()),
                Err(error) => Promise::err(capnp::Error::failed(error)),
            },
            Err(error) => Promise::err(capnp::Error::failed(error)),
        }
    }

    fn report_playback(
        self: Rc<Self>,
        params: channel_handle::ReportPlaybackParams,
        _results: channel_handle::ReportPlaybackResults,
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
                "reportPlayback appId must match the certified connection route".into(),
            ));
        }
        if crate::shell::state::RoomId::parse(&route.cluster_id).is_none() {
            return Promise::err(capnp::Error::failed(
                "reportPlayback route has no durable workspace room".into(),
            ));
        }
        let bytes = match request.get_reports_json() {
            Ok(bytes) if bytes.len() <= MAX_PRIVATE_PLAYBACK_REPORT_BYTES => bytes,
            Ok(_) => {
                return Promise::err(capnp::Error::failed(
                    "reportPlayback payload exceeds session-local limit".into(),
                ));
            }
            Err(error) => return Promise::err(error),
        };
        let reports: Vec<media::handoff::PlaybackReport> = match serde_json::from_slice(bytes) {
            Ok(reports) => reports,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        if reports.len() > media::handoff::MAX_PLAYBACK_REPORTS
            || !reports
                .iter()
                .all(|report| report.validate_route(&app_id, &route.cluster_id))
        {
            return Promise::err(capnp::Error::failed(
                "reportPlayback contains an invalid or foreign media session".into(),
            ));
        }
        media::handoff::replace_external_reports(
            playback_report_owner(&route, self.peer_id),
            reports,
        );
        Promise::ok(())
    }

    fn wifi_continuity_command(
        self: Rc<Self>,
        params: channel_handle::WifiContinuityCommandParams,
        mut results: channel_handle::WifiContinuityCommandResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
        {
            return Promise::err(error);
        }
        let route = self.route();
        if route.app_id != "so.tilo.settings" {
            return Promise::err(capnp::Error::failed(
                "Wi-Fi continuity is restricted to the certified Settings application".into(),
            ));
        }
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
        if claimed_app != route.app_id {
            return Promise::err(capnp::Error::failed(
                "Wi-Fi continuity app id must match the certified connection route".into(),
            ));
        }
        let bytes = match request.get_command_json() {
            Ok(bytes) if bytes.len() <= runtime_collab::MAX_WIFI_CONTINUITY_WIRE_BYTES => bytes,
            Ok(_) => {
                return Promise::err(capnp::Error::failed(
                    "Wi-Fi continuity command exceeds the local protocol bound".into(),
                ));
            }
            Err(error) => return Promise::err(error),
        };
        let command = match serde_json::from_slice::<runtime_collab::WifiContinuityCommand>(bytes) {
            Ok(command) if command.valid() => command,
            Ok(_) => {
                return Promise::err(capnp::Error::failed(
                    "Wi-Fi continuity command is structurally invalid".into(),
                ));
            }
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let Some(network) = self.state.workspace_control_network() else {
            return Promise::err(capnp::Error::failed(
                "certified personal-device network is unavailable".into(),
            ));
        };
        let security = |security| match security {
            runtime_collab::PortableWifiSecurity::Wpa2 => os_actions::WifiSecurity::Wpa2,
            runtime_collab::PortableWifiSecurity::Wpa3 => os_actions::WifiSecurity::Wpa3,
        };
        let reply = match command {
            runtime_collab::WifiContinuityCommand::Lookup {
                ssid,
                security: mode,
            } => network
                .shared_wifi_password(&ssid, security(mode))
                .map_or(runtime_collab::WifiContinuityReply::NotFound, |password| {
                    runtime_collab::WifiContinuityReply::Password { password }
                }),
            runtime_collab::WifiContinuityCommand::Share {
                ssid,
                security: mode,
                password,
            } => {
                let password = zeroize::Zeroizing::new(password);
                match network.share_wifi_credential(&ssid, security(mode), &password) {
                    Ok(()) => runtime_collab::WifiContinuityReply::Shared { ssid },
                    Err(reason) => runtime_collab::WifiContinuityReply::Rejected { reason },
                }
            }
            runtime_collab::WifiContinuityCommand::ShareSavedProfile { profile_uuid } => {
                match os_actions::portable_wifi_profile_secret(&profile_uuid).and_then(|profile| {
                    network
                        .share_wifi_credential(&profile.ssid, profile.security, &profile.password)
                        .map(|()| profile.ssid)
                }) {
                    Ok(ssid) => runtime_collab::WifiContinuityReply::Shared { ssid },
                    Err(reason) => runtime_collab::WifiContinuityReply::Rejected { reason },
                }
            }
        };
        let reply = match serde_json::to_vec(&reply) {
            Ok(reply) if reply.len() <= runtime_collab::MAX_WIFI_CONTINUITY_WIRE_BYTES => reply,
            Ok(_) => {
                return Promise::err(capnp::Error::failed(
                    "Wi-Fi continuity reply exceeds the local protocol bound".into(),
                ));
            }
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        results.get().set_reply_json(&reply);
        Promise::ok(())
    }
}

struct CapnpWorkspaceControlHandle {
    state: Arc<CapnpBrokerState>,
    route: CrdtRoute,
    verified_identity: AdmittedJoinIdentity,
    network: Arc<crate::shell::network::ShellNetwork>,
    session: Rc<AsyncMutex<crate::network::WorkspaceControlSession>>,
    inbound: Rc<
        AsyncMutex<
            tokio::sync::mpsc::UnboundedReceiver<
                crate::shell::network::WorkspaceControlNetworkEvent,
            >,
        >,
    >,
    initial: Rc<AsyncMutex<Option<runtime_collab::WorkspaceControlEvent>>>,
}

fn workspace_control_event(
    update: crate::network::WorkspaceControlUpdate,
) -> runtime_collab::WorkspaceControlEvent {
    match update {
        crate::network::WorkspaceControlUpdate::ConsentGranted {
            surface,
            width,
            height,
        } => runtime_collab::WorkspaceControlEvent::StreamStarted {
            surface,
            width,
            height,
        },
        crate::network::WorkspaceControlUpdate::Frame {
            surface,
            encoded_h264,
            timestamp,
        } => runtime_collab::WorkspaceControlEvent::Frame {
            surface,
            encoded_h264,
            timestamp,
        },
        crate::network::WorkspaceControlUpdate::Closed(reason) => {
            let reason = match reason {
                crate::network::WorkspaceControlCloseReason::Denied(message) => {
                    runtime_collab::WorkspaceControlCloseReason::Denied(message)
                }
                crate::network::WorkspaceControlCloseReason::Revoked => {
                    runtime_collab::WorkspaceControlCloseReason::Revoked
                }
                crate::network::WorkspaceControlCloseReason::TargetDisconnected => {
                    runtime_collab::WorkspaceControlCloseReason::TargetDisconnected
                }
                crate::network::WorkspaceControlCloseReason::RelayDisconnected => {
                    runtime_collab::WorkspaceControlCloseReason::RelayDisconnected
                }
                crate::network::WorkspaceControlCloseReason::LocalDisconnect => {
                    runtime_collab::WorkspaceControlCloseReason::LocalDisconnect
                }
            };
            runtime_collab::WorkspaceControlEvent::Closed(reason)
        }
    }
}

fn closed_workspace_control_event(
    session: &crate::network::WorkspaceControlSession,
) -> Option<runtime_collab::WorkspaceControlEvent> {
    let crate::network::WorkspaceControlStatus::Closed(reason) = session.status() else {
        return None;
    };
    Some(workspace_control_event(
        crate::network::WorkspaceControlUpdate::Closed(reason.clone()),
    ))
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
    let presentation = if row.resource_presentation_json.is_empty() {
        None
    } else {
        serde_json::from_str::<tilo_actions::ResourcePresentation>(&row.resource_presentation_json)
            .ok()
    };
    if !row.resource_presentation_json.is_empty()
        && !presentation.as_ref().is_some_and(|value| value.is_valid())
    {
        return Err(capnp::Error::failed(
            "invalid resource presentation metadata".into(),
        ));
    }
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
                || presentation.is_some()
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
            let published = published_directory_action(row)?;
            validate_resource_view_descriptors(std::slice::from_ref(&published))?;
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
    let page_actions = page
        .rows
        .iter()
        .filter(|row| row.action.is_some())
        .map(published_directory_action)
        .collect::<Result<Vec<_>, _>>()?;
    validate_resource_view_descriptors(&page_actions)?;
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
    let view = descriptor
        .view
        .as_ref()
        .map(|view| tilo_actions::ResourceViewDescriptor {
            id: view.id.clone(),
            label: view.label.clone(),
            description: view.description.clone(),
            open_action: view.open_action.clone(),
            primary: view.primary,
            mount: view
                .mount
                .as_ref()
                .map(|mount| tilo_actions::ResourceViewMountSpec {
                    action: mount.action.clone(),
                    capability_param: mount.capability_param.clone(),
                    view_param: mount.view_param.clone(),
                }),
        });
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
        view,
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

impl workspace_control_handle::Server for CapnpWorkspaceControlHandle {
    fn read(
        self: Rc<Self>,
        _params: workspace_control_handle::ReadParams,
        mut results: workspace_control_handle::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let this = self;
        Promise::from_future(async move {
            let initial_event = {
                let mut initial = this.initial.lock().await;
                initial.take()
            };
            if let Some(event) = initial_event {
                let bytes = postcard::to_allocvec(&event)
                    .map_err(|error| capnp::Error::failed(error.to_string()))?;
                results.get().set_event(&bytes);
                return Ok(());
            }
            loop {
                let closed = {
                    let session = this.session.lock().await;
                    closed_workspace_control_event(&session)
                };
                if let Some(event) = closed {
                    let bytes = postcard::to_allocvec(&event)
                        .map_err(|error| capnp::Error::failed(error.to_string()))?;
                    results.get().set_event(&bytes);
                    return Ok(());
                }
                if let Err(error) =
                    revalidate_workspace_control(&this.state, &this.route, &this.verified_identity)
                {
                    let event = {
                        let mut session = this.session.lock().await;
                        workspace_control_event(session.deny(error.to_string()))
                    };
                    let bytes = postcard::to_allocvec(&event)
                        .map_err(|error| capnp::Error::failed(error.to_string()))?;
                    results.get().set_event(&bytes);
                    return Ok(());
                }
                let network_event =
                    match tokio::time::timeout(std::time::Duration::from_millis(250), async {
                        this.inbound.lock().await.recv().await
                    })
                    .await
                    {
                        Ok(Some(event)) => event,
                        Ok(None) => {
                            return Err(capnp::Error::disconnected(
                                "workspace-control observer closed".into(),
                            ));
                        }
                        Err(_) => continue,
                    };
                let mut session = this.session.lock().await;
                let update = match network_event {
                    crate::shell::network::WorkspaceControlNetworkEvent::Message(event)
                        if event.workspace_topic == session.workspace() =>
                    {
                        session.observe_from(event.transport_peer, &event.message)
                    }
                    crate::shell::network::WorkspaceControlNetworkEvent::PeerLeft {
                        workspace_topic,
                        peer,
                    } if workspace_topic == session.workspace() => session.peer_departed(peer),
                    crate::shell::network::WorkspaceControlNetworkEvent::PeerRevoked {
                        workspace_topic,
                        peer,
                    } if workspace_topic == session.workspace() && peer == session.target() => {
                        Some(session.deny("target device certificate was revoked"))
                    }
                    _ => None,
                };
                if let Some(update) = update {
                    let bytes = postcard::to_allocvec(&workspace_control_event(update))
                        .map_err(|error| capnp::Error::failed(error.to_string()))?;
                    results.get().set_event(&bytes);
                    return Ok(());
                }
            }
        })
    }

    fn send_input(
        self: Rc<Self>,
        params: workspace_control_handle::SendInputParams,
        _results: workspace_control_handle::SendInputResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) =
            revalidate_workspace_control(&self.state, &self.route, &self.verified_identity)
        {
            return Promise::err(error);
        }
        let bytes = match params.get().and_then(
            runtime_collab::collab_capnp::workspace_control_handle::send_input_params::Reader::get_event,
        ) {
            Ok(bytes) => bytes,
            Err(error) => return Promise::err(error),
        };
        let input: runtime_collab::WorkspaceControlInput = match postcard::from_bytes(bytes) {
            Ok(input) => input,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let this = self;
        Promise::from_future(async move {
            {
                let session = this.session.lock().await;
                let input = match input {
                    runtime_collab::WorkspaceControlInput::PointerMotion { x, y } => {
                        crate::network::InputEvent::PointerMotion { x, y }
                    }
                    runtime_collab::WorkspaceControlInput::PointerButton { button, pressed } => {
                        crate::network::InputEvent::PointerButton { button, pressed }
                    }
                    runtime_collab::WorkspaceControlInput::Key {
                        keycode,
                        pressed,
                        modifiers,
                    } => crate::network::InputEvent::Key {
                        keysym: keycode,
                        pressed,
                        modifiers,
                    },
                    runtime_collab::WorkspaceControlInput::KeyboardFocus { focused } => {
                        let surface = match session.status() {
                            crate::network::WorkspaceControlStatus::Active { surface, .. } => {
                                *surface
                            }
                            _ => {
                                return Err(capnp::Error::failed(
                                    "target has not granted control".into(),
                                ));
                            }
                        };
                        crate::network::InputEvent::KeyboardFocus { surface, focused }
                    }
                };
                let published = this
                    .network
                    .publish_workspace_control_input(&session, &input)
                    .map_err(capnp::Error::failed);
                drop(session);
                published
            }
        })
    }

    fn close(
        self: Rc<Self>,
        _params: workspace_control_handle::CloseParams,
        _results: workspace_control_handle::CloseResults,
    ) -> Promise<(), capnp::Error> {
        let session = self.session.clone();
        Promise::from_future(async move {
            session.lock().await.disconnect();
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
            state.publisher().signed_document(route, envelope.clone());
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
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            let raw = r
                .get_text()?
                .to_str()
                .map_err(|error| capnp::Error::failed(format!("invalid status utf8: {error}")))?;
            let text = raw.split_whitespace().collect::<Vec<_>>().join(" ");
            if text.chars().count() > ui::MAX_STATUS_CHARS {
                return Err(capnp::Error::failed("status exceeds 96 characters".into()));
            }
            self.publish_keyed(
                &key_bytes,
                PresencePayload::Status {
                    text,
                    automatic: r.get_automatic(),
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

struct InvocationStreamServer {
    rx: Rc<AsyncMutex<mpsc::UnboundedReceiver<InvocationEnvelope>>>,
}

struct RealtimeStreamServer {
    rx: Rc<AsyncMutex<mpsc::UnboundedReceiver<Vec<u8>>>>,
}

struct TaskRunStreamServer {
    rx: Rc<AsyncMutex<mpsc::UnboundedReceiver<TaskRunEnvelope>>>,
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

impl realtime_stream::Server for RealtimeStreamServer {
    fn read(
        self: Rc<Self>,
        _params: realtime_stream::ReadParams,
        mut results: realtime_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            let mut guard = rx.lock().await;
            let bytes = guard.recv().await.ok_or_else(|| {
                capnp::Error::disconnected("realtime lifecycle stream closed".into())
            })?;
            drop(guard);
            results.get().set_event_json(&bytes);
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
    trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
    workspace_control_network: Option<Arc<crate::shell::network::ShellNetwork>>,
    quit: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    let _ = std::fs::remove_file(&socket_path);
    let wired = publisher.is_wired();
    let (state, bridge_rx) = CapnpBrokerState::new(
        fanout,
        broker_channels,
        publisher,
        membership,
        trusted_devices,
    );
    if let Some(network) = workspace_control_network.as_ref() {
        state.attach_workspace_control_network(network);
    }
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
        .name("tilo-collab-broker-capnp".into())
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
                let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge_rx));
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
                        if let Err(e) = drive_connection(stream, state, directories, peer_id).await
                        {
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
/// Peek at an incoming descriptor without consuming Cap'n Proto's segment
/// header. Raw endpoint diagnostics intentionally omit the launcher marker;
/// only a marker-bearing connection is subject to shell-process attestation.
async fn has_collab_launch_marker(stream: &tokio::net::UnixStream) -> Result<bool, capnp::Error> {
    for _ in 0..LAUNCH_RECORD_RACE_RETRIES {
        stream.readable().await.map_err(|error| {
            capnp::Error::failed(format!("collaboration socket unreadable: {error}"))
        })?;
        let mut marker = [0_u8; launcher::services::COLLAB_LAUNCH_MAGIC.len()];
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
            return Ok(marker == launcher::services::COLLAB_LAUNCH_MAGIC);
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

    let handoff = read_collab_launch_handoff(stream).await?;
    if handoff.protocol != launcher::services::COLLAB_LAUNCH_PROTOCOL_VERSION
        || handoff.app_id.trim().is_empty()
        || handoff.workspace_id.trim().is_empty()
    {
        return Err(capnp::Error::failed(
            "collaboration launch handoff has an unsupported or empty identity".into(),
        ));
    }
    attest_collab_launch(pid, &handoff).await
}

/// Read the launcher's collaboration handoff: the launch marker, then one
/// little-endian length-prefixed JSON frame.
async fn read_collab_launch_handoff<S: tokio::io::AsyncRead + Unpin>(
    stream: &mut S,
) -> Result<CollabLaunchHandoff, capnp::Error> {
    let mut marker = [0_u8; launcher::services::COLLAB_LAUNCH_MAGIC.len()];
    stream.read_exact(&mut marker).await.map_err(|error| {
        capnp::Error::failed(format!(
            "could not read collaboration launch marker: {error}"
        ))
    })?;
    if marker != launcher::services::COLLAB_LAUNCH_MAGIC {
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
    serde_json::from_slice(&bytes).map_err(|error| {
        capnp::Error::failed(format!("invalid collaboration launch handoff: {error}"))
    })
}

async fn attest_collab_launch(pid: u32, handoff: &CollabLaunchHandoff) -> Result<(), capnp::Error> {
    // `spawn_launch` records immediately after `Command::spawn`; the child
    // can connect first on a fast machine. Retry only this narrow registration
    // race. A direct tilo-launcher command never obtains a record and fails
    // closed instead of becoming a way around admin/admission checks.
    for attempt in 0..=LAUNCH_RECORD_RACE_RETRIES {
        if crate::shell::launcher::attest_trusted_service_launch_for_pid(
            pid,
            &handoff.app_id,
            &handoff.workspace_id,
        )
        .is_some()
        {
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
) -> Result<(), capnp::Error> {
    let mut stream = stream;
    authenticate_launcher_handoff(&mut stream).await?;
    let (reader, writer) = stream.into_split();
    let reader = reader.compat();
    let writer = writer.compat_write();
    let network = Box::new(twoparty::VatNetwork::new(
        futures::io::BufReader::new(reader),
        futures::io::BufWriter::new(writer),
        rpc_twoparty_capnp::Side::Server,
        capnp::message::ReaderOptions::default(),
    ));
    let service = CapnpCollabService {
        state,
        directories,
        peer_id,
    };
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
pub(crate) mod tests {
    use super::*;
    use crate::shell::state::FreshId as _;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::SystemTime;
    use tilo_actions::remote::ActionHost as _;

    /// The collaboration handoff as the launcher writes it: `QCL1`, a
    /// little-endian `u32` length, then JSON. Empty handoffs are refused.
    #[tokio::test]
    async fn collab_launch_handoff_bytes_are_pinned() {
        let mut wire = b"QCL1\x3f\0\0\0".to_vec();
        wire.extend_from_slice(
            br#"{"protocol":1,"app_id":"so.tilo.notes","workspace_id":"room-7"}"#,
        );
        let handoff = read_collab_launch_handoff(&mut wire.as_slice())
            .await
            .unwrap();
        assert_eq!(handoff.protocol, 1);
        assert_eq!(handoff.app_id, "so.tilo.notes");
        assert_eq!(handoff.workspace_id, "room-7");
        let mut empty: &[u8] = b"QCL1\0\0\0\0";
        assert!(read_collab_launch_handoff(&mut empty).await.is_err());
    }

    use crate::shell::collab_outbound::recording::{RecordingSink, Sent};
    use crate::shell::search::providers::live_app_actions::LiveAppActionsProvider;
    use crate::shell::search::{InvocationDisposition, SearchContext, SearchProvider};
    use automerge::{AutoCommit, ObjId};
    use collab_types::{CapOverride, Ed25519PublicKey, Role, Subject, sign_op};
    use ed25519_dalek::Signer as _;
    use ed25519_dalek::SigningKey;

    fn capture_test_app(
        workspace: &str,
    ) -> crate::shell::realtime_session::VerifiedAppRegistration {
        crate::shell::realtime_session::VerifiedAppRegistration::new_verified(
            "so.tilo.chat",
            workspace,
        )
        .expect("verified test app")
    }

    fn capture_test_broker() -> Arc<Mutex<crate::portal::CaptureBroker>> {
        let mut capture = crate::portal::CaptureBroker::new();
        capture
            .register_source(
                "mic",
                "Microphone",
                crate::portal::CaptureKind::Microphone,
                crate::portal::LocalCaptureHandle::PipeWireNode(7),
            )
            .expect("register mic");
        capture
            .register_source(
                "camera",
                "Camera",
                crate::portal::CaptureKind::Camera,
                crate::portal::LocalCaptureHandle::PipeWireNode(8),
            )
            .expect("register camera");
        Arc::new(Mutex::new(capture))
    }

    #[test]
    fn realtime_capture_ipc_requires_exact_complete_one_shot_authorizations() {
        let now = std::time::Instant::now();
        let app = capture_test_app("work");
        let wrong_app = capture_test_app("personal");
        let session = "0123456789abcdef0123456789abcdef";
        let target =
            crate::shell::personal_continuity::DeviceId::new("target-local").expect("target");
        let wrong_target =
            crate::shell::personal_continuity::DeviceId::new("target-other").expect("wrong target");
        let capture = capture_test_broker();
        let binding =
            crate::portal::CaptureSessionBinding::from_verified(&app, session, target.as_str())
                .expect("capture binding");
        let wrong_binding = crate::portal::CaptureSessionBinding::from_verified(
            &wrong_app,
            session,
            target.as_str(),
        )
        .expect("wrong binding");
        let (microphone, camera) = {
            let mut broker = capture.lock().expect("capture broker");
            (
                broker
                    .authorize(&binding, "mic", crate::portal::CaptureKind::Microphone, now)
                    .expect("microphone authorization")
                    .to_opaque(),
                broker
                    .authorize(&binding, "camera", crate::portal::CaptureKind::Camera, now)
                    .expect("camera authorization")
                    .to_opaque(),
            )
        };
        let required = [
            crate::portal::CaptureKind::Microphone,
            crate::portal::CaptureKind::Camera,
        ];

        assert!(
            acquire_realtime_capture_leases(
                &capture,
                &wrong_app,
                session,
                &target,
                &[microphone.clone(), camera.clone()],
                &required,
                now,
            )
            .is_err()
        );
        assert!(
            capture
                .lock()
                .expect("capture broker")
                .take_privacy_events()
                .is_empty()
        );
        assert_eq!(wrong_binding.workspace_id(), "personal");
        assert!(
            acquire_realtime_capture_leases(
                &capture,
                &app,
                session,
                &wrong_target,
                &[microphone.clone(), camera.clone()],
                &required,
                now,
            )
            .is_err()
        );

        let leases = acquire_realtime_capture_leases(
            &capture,
            &app,
            session,
            &target,
            &[microphone.clone(), camera.clone()],
            &required,
            now,
        )
        .expect("exact authorization set");
        realtime_capture_leases_ready(&capture, &app, session, &target, &leases, now)
            .expect("leases ready");
        assert!(
            acquire_realtime_capture_leases(
                &capture,
                &app,
                session,
                &target,
                &[microphone, camera],
                &required,
                now,
            )
            .is_err()
        );

        release_realtime_capture_session(&capture, &app, session, &target);
        assert!(
            realtime_capture_leases_ready(&capture, &app, session, &target, &leases, now).is_err()
        );
    }

    #[test]
    fn realtime_capture_ipc_rejects_expiry_and_lock_teardown_before_cutover() {
        let now = std::time::Instant::now();
        let app = capture_test_app("work");
        let session = "fedcba9876543210fedcba9876543210";
        let target =
            crate::shell::personal_continuity::DeviceId::new("target-local").expect("target");
        let capture = capture_test_broker();
        let binding =
            crate::portal::CaptureSessionBinding::from_verified(&app, session, target.as_str())
                .expect("capture binding");
        let expired = capture
            .lock()
            .expect("capture broker")
            .authorize(&binding, "mic", crate::portal::CaptureKind::Microphone, now)
            .expect("authorization")
            .to_opaque();
        assert!(
            acquire_realtime_capture_leases(
                &capture,
                &app,
                session,
                &target,
                &[expired],
                &[crate::portal::CaptureKind::Microphone],
                now + crate::portal::capture_broker::CAPTURE_AUTHORIZATION_TTL
                    + std::time::Duration::from_millis(1),
            )
            .is_err()
        );

        let fresh = capture
            .lock()
            .expect("capture broker")
            .authorize(&binding, "mic", crate::portal::CaptureKind::Microphone, now)
            .expect("fresh authorization")
            .to_opaque();
        let leases = acquire_realtime_capture_leases(
            &capture,
            &app,
            session,
            &target,
            &[fresh],
            &[crate::portal::CaptureKind::Microphone],
            now,
        )
        .expect("fresh lease");
        capture.lock().expect("capture broker").screen_locked();
        assert!(
            realtime_capture_leases_ready(&capture, &app, session, &target, &leases, now).is_err()
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
        let channel = "so.tilo.notes#room:test#record:daily";
        let mut first = state.subscribe_context(channel, 7);
        let mut second = state.subscribe_context(channel, 9);
        let _requester = state.subscribe_context(channel, 12);

        assert!(state.request_document_snapshot(channel, 12));
        assert!(matches!(
            first.try_recv(),
            Ok(ContextEnvelope::SnapshotRequested)
        ));
        assert!(second.try_recv().is_err(), "only one holder may respond");
        assert!(
            !state.request_document_snapshot("missing", 12),
            "a route without a holder is retryable, not falsely acknowledged"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "The two-peer production fixture follows offer, acceptance, media readiness, source stop, and target-live cutover end to end."
    )]
    fn production_realtime_bridge_delivers_two_peer_offer_and_cutover() {
        use crate::shell::realtime_session::{
            BrokerRealtimeFabricAdapter, RealtimeFabricAdapter, RealtimeFabricSignal,
            RealtimeMediaFacet, RealtimeMediaSet, RealtimeResumeIntent, RealtimeSessionBroker,
            SemanticRoomLocator, VerifiedAppRegistration,
        };

        fn state_with_adapter() -> (
            Arc<CapnpBrokerState>,
            Arc<Mutex<RealtimeSessionBroker>>,
            Arc<BrokerRealtimeFabricAdapter>,
        ) {
            let fanout = Arc::new(FanoutState::new());
            let (state, _) = CapnpBrokerState::new(
                &fanout,
                Arc::new(Mutex::new(BrokerChannels::default())),
                PeerPublisher::disabled(),
                None,
                None,
            );
            let broker = Arc::new(Mutex::new(RealtimeSessionBroker::new()));
            let fabric = Arc::new(BrokerRealtimeFabricAdapter::with_app_verifier(
                Arc::clone(&broker),
                |_| true,
            ));
            let mut realtime = state.realtime.lock().expect("realtime bridge");
            realtime.broker = Arc::clone(&broker);
            realtime.fabric = Arc::clone(&fabric);
            drop(realtime);
            (state, broker, fabric)
        }

        let now = std::time::Instant::now();
        let source =
            crate::shell::personal_continuity::DeviceId::new("laptop").expect("source device");
        let target =
            crate::shell::personal_continuity::DeviceId::new("tablet").expect("target device");
        let app =
            VerifiedAppRegistration::new_verified("so.tilo.calls", "work").expect("verified app");
        let intent = RealtimeResumeIntent {
            room: SemanticRoomLocator::new("meet", "standup").expect("semantic room"),
            semantic_position: Some("main-stage".into()),
            media: RealtimeMediaSet::new([RealtimeMediaFacet::Microphone]).expect("media set"),
            controls: broker_realtime_controls(ui::realtime_continuity::RealtimeControlIntent {
                microphone_muted: true,
                camera_muted: false,
                screen_share_paused: true,
                share_audio_muted: true,
            }),
        };
        let (source_state, source_broker, source_fabric) = state_with_adapter();
        let (target_state, target_broker, target_fabric) = state_with_adapter();
        let (source_tx, mut source_rx) = mpsc::unbounded_channel();
        source_state
            .realtime
            .lock()
            .expect("source bridge")
            .subscribers
            .insert(
                7,
                RealtimeSubscriber {
                    app: app.clone(),
                    tx: source_tx,
                },
            );
        let (target_tx, mut target_rx) = mpsc::unbounded_channel();
        target_state
            .realtime
            .lock()
            .expect("target bridge")
            .subscribers
            .insert(
                9,
                RealtimeSubscriber {
                    app: app.clone(),
                    tx: target_tx,
                },
            );
        let id = source_broker
            .lock()
            .expect("source broker")
            .register(&app, source.clone(), intent, now)
            .expect("register source");
        source_broker
            .lock()
            .expect("source broker")
            .propose_target(&app, id, &source, target.clone(), now)
            .expect("propose target");
        source_state
            .realtime
            .lock()
            .expect("source bridge")
            .sessions
            .insert(
                "source-session".into(),
                RealtimeBinding {
                    id,
                    app,
                    source: source.clone(),
                    owner_peer: 7,
                    authorization: None,
                    target_peer: None,
                    capture_leases: Vec::new(),
                    imported: false,
                },
            );
        source_fabric
            .offer_to(id, &target, 20_000)
            .expect("queue exact-target offer");
        let offer = source_fabric.drain_egress().pop().expect("offer egress");
        assert_eq!(offer.target, target);
        let RealtimeFabricSignal::Offer(projected) = offer.signal else {
            panic!("expected offer");
        };
        let fabric_id = projected.session_id;
        assert!(target_fabric.receive_authenticated(
            &source,
            &target,
            RealtimeFabricSignal::Offer(projected),
            now,
            10_000,
        ));
        target_state.pump_realtime_fabric_app_events();
        let offer_event: ui::realtime_continuity::RealtimeEvent =
            serde_json::from_slice(&target_rx.try_recv().expect("target app offer"))
                .expect("offer event");
        let ui::realtime_continuity::RealtimeEvent::Offer { session, intent } = offer_event else {
            panic!("expected app offer");
        };
        assert_eq!(
            intent.controls,
            ui::realtime_continuity::RealtimeControlIntent {
                microphone_muted: true,
                camera_muted: false,
                screen_share_paused: true,
                share_audio_muted: true,
            }
        );
        target_fabric
            .accept_imported(fabric_id, &source, &target, now)
            .expect("target-local accept");
        let accepted = target_fabric.drain_egress().pop().expect("accept egress");
        assert_eq!(accepted.target, source);
        assert!(source_fabric.receive_authenticated(
            &target,
            &source,
            accepted.signal,
            now,
            10_000,
        ));
        target_fabric
            .imported_media_ready(fabric_id, &source, &target, now)
            .expect("target media ready");
        let ready = target_fabric.drain_egress().pop().expect("ready egress");
        assert_eq!(ready.target, source);
        assert!(source_fabric.receive_authenticated(&target, &source, ready.signal, now, 10_000,));
        source_state.pump_realtime_fabric_app_events();
        let stop_event: ui::realtime_continuity::RealtimeEvent =
            serde_json::from_slice(&source_rx.try_recv().expect("source app stop"))
                .expect("stop event");
        assert_eq!(
            stop_event,
            ui::realtime_continuity::RealtimeEvent::StopSource {
                session: "source-session".into(),
            }
        );
        let stop = source_fabric.drain_egress().pop().expect("stop egress");
        assert_eq!(stop.target, target);
        assert!(target_fabric.receive_authenticated(&source, &target, stop.signal, now, 10_000,));
        assert_eq!(
            target_broker
                .lock()
                .expect("target broker")
                .session(fabric_id.into_broker_id())
                .expect("imported session")
                .phase,
            crate::shell::realtime_session::RealtimeSessionPhase::TargetLive
        );
        assert!(session.len() <= ui::realtime_continuity::MAX_REALTIME_TOKEN_BYTES);
    }

    #[test]
    fn realtime_consent_reserves_once_and_revokes_minted_batch_after_disconnect() {
        use crate::shell::realtime_session::{
            BrokerRealtimeFabricAdapter, RealtimeFabricAdapter, RealtimeFabricSignal,
            RealtimeMediaFacet, RealtimeMediaSet, RealtimeResumeIntent, RealtimeSessionBroker,
            SemanticRoomLocator, VerifiedAppRegistration,
        };

        let fanout = Arc::new(FanoutState::new());
        let (state, _) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            None,
            None,
        );
        let target_broker = Arc::new(Mutex::new(RealtimeSessionBroker::new()));
        let target_fabric = Arc::new(BrokerRealtimeFabricAdapter::with_app_verifier(
            Arc::clone(&target_broker),
            |_| true,
        ));
        let (subscriber, _events) = mpsc::unbounded_channel();
        let app =
            VerifiedAppRegistration::new_verified("so.tilo.calls", "work").expect("verified app");
        {
            let mut realtime = state.realtime.lock().expect("target bridge");
            realtime.broker = Arc::clone(&target_broker);
            realtime.fabric = Arc::clone(&target_fabric);
            realtime.subscribers.insert(
                9,
                RealtimeSubscriber {
                    app: app.clone(),
                    tx: subscriber,
                },
            );
        }

        let now = std::time::Instant::now();
        let source =
            crate::shell::personal_continuity::DeviceId::new("laptop").expect("source device");
        let target =
            crate::shell::personal_continuity::DeviceId::new("tablet").expect("target device");
        let source_broker = Arc::new(Mutex::new(RealtimeSessionBroker::new()));
        let source_fabric = Arc::new(BrokerRealtimeFabricAdapter::with_app_verifier(
            Arc::clone(&source_broker),
            |_| true,
        ));
        let intent = RealtimeResumeIntent {
            room: SemanticRoomLocator::new("meet", "standup").expect("semantic room"),
            semantic_position: None,
            media: RealtimeMediaSet::new([RealtimeMediaFacet::Microphone]).expect("media set"),
            controls: Default::default(),
        };
        let id = source_broker
            .lock()
            .expect("source broker")
            .register(&app, source.clone(), intent, now)
            .expect("register source");
        source_broker
            .lock()
            .expect("source broker")
            .propose_target(&app, id, &source, target.clone(), now)
            .expect("propose target");
        source_fabric
            .offer_to(id, &target, 20_000)
            .expect("queue offer");
        let offer = source_fabric.drain_egress().pop().expect("offer egress");
        let RealtimeFabricSignal::Offer(offer) = offer.signal else {
            panic!("expected offer");
        };
        assert!(target_fabric.receive_authenticated(
            &source,
            &target,
            RealtimeFabricSignal::Offer(offer),
            now,
            10_000,
        ));
        state.pump_realtime_fabric_app_events();
        let session = {
            let realtime = state.realtime.lock().expect("target bridge");
            realtime
                .sessions
                .keys()
                .next()
                .expect("imported session")
                .clone()
        };

        // Pause after minting but before the exact binding is revalidated. A
        // duplicate must fail immediately, and removing the imported binding
        // while paused must revoke the whole just-minted batch.
        let (minted_tx, minted_rx) = std::sync::mpsc::sync_channel(1);
        let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);
        *realtime_consent_mint_hook().lock().expect("consent hook") =
            Some(RealtimeConsentMintHook {
                minted: minted_tx,
                resume: resume_rx,
            });
        let worker_bridge = RealtimeFabricAppBridge::from_state(Arc::clone(&state));
        let worker_session = session.clone();
        let worker = std::thread::spawn(move || {
            worker_bridge.resolve_consent_with_policy(&worker_session, true, || true)
        });
        minted_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("consent worker reached post-mint checkpoint");
        assert!(
            state
                .realtime
                .lock()
                .expect("target bridge")
                .consent_capabilities_issued
                .contains(&session),
            "worker must reserve consent before capture minting"
        );
        assert!(
            RealtimeFabricAppBridge::from_state(Arc::clone(&state))
                .resolve_consent_with_policy(&session, true, || true)
                .is_err(),
            "a concurrent duplicate may not mint another batch"
        );
        state
            .realtime
            .lock()
            .expect("target bridge")
            .sessions
            .remove(&session);
        resume_tx.send(()).expect("resume consent worker");
        assert!(
            worker.join().expect("consent worker").is_err(),
            "disconnect must invalidate consent before delivery"
        );
        let realtime = state.realtime.lock().expect("target bridge");
        assert!(!realtime.consent_capabilities_issued.contains(&session));
        assert_eq!(
            realtime
                .capture
                .lock()
                .expect("capture broker")
                .pending_authorization_count(),
            0,
            "stale consent must revoke every minted capability"
        );
    }

    #[test]
    fn realtime_policy_disable_tears_down_exact_pending_consent() {
        use crate::shell::realtime_session::{
            BrokerRealtimeFabricAdapter, RealtimeFabricAdapter, RealtimeFabricSignal,
            RealtimeMediaFacet, RealtimeMediaSet, RealtimeResumeIntent, RealtimeSessionBroker,
            SemanticRoomLocator, VerifiedAppRegistration,
        };

        let fanout = Arc::new(FanoutState::new());
        let (state, _) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            None,
            None,
        );
        let target_broker = Arc::new(Mutex::new(RealtimeSessionBroker::new()));
        let target_fabric = Arc::new(BrokerRealtimeFabricAdapter::with_app_verifier(
            Arc::clone(&target_broker),
            |_| true,
        ));
        let (subscriber, mut events) = mpsc::unbounded_channel();
        let app =
            VerifiedAppRegistration::new_verified("so.tilo.calls", "work").expect("verified app");
        {
            let mut realtime = state.realtime.lock().expect("target bridge");
            realtime.broker = Arc::clone(&target_broker);
            realtime.fabric = Arc::clone(&target_fabric);
            realtime.subscribers.insert(
                9,
                RealtimeSubscriber {
                    app: app.clone(),
                    tx: subscriber,
                },
            );
        }

        let now = std::time::Instant::now();
        let source =
            crate::shell::personal_continuity::DeviceId::new("laptop").expect("source device");
        let target =
            crate::shell::personal_continuity::DeviceId::new("tablet").expect("target device");
        let source_broker = Arc::new(Mutex::new(RealtimeSessionBroker::new()));
        let source_fabric = Arc::new(BrokerRealtimeFabricAdapter::with_app_verifier(
            Arc::clone(&source_broker),
            |_| true,
        ));
        let intent = RealtimeResumeIntent {
            room: SemanticRoomLocator::new("meet", "standup").expect("semantic room"),
            semantic_position: None,
            media: RealtimeMediaSet::new([RealtimeMediaFacet::Microphone]).expect("media set"),
            controls: Default::default(),
        };
        let id = source_broker
            .lock()
            .expect("source broker")
            .register(&app, source.clone(), intent, now)
            .expect("register source");
        source_broker
            .lock()
            .expect("source broker")
            .propose_target(&app, id, &source, target.clone(), now)
            .expect("propose target");
        source_fabric
            .offer_to(id, &target, 20_000)
            .expect("queue offer");
        let egress = source_fabric.drain_egress().pop().expect("offer egress");
        let RealtimeFabricSignal::Offer(offer) = egress.signal else {
            panic!("expected offer");
        };
        let fabric_id = offer.session_id;
        assert!(target_fabric.receive_authenticated(
            &source,
            &target,
            RealtimeFabricSignal::Offer(offer),
            now,
            10_000,
        ));
        state.pump_realtime_fabric_app_events();
        let offered: ui::realtime_continuity::RealtimeEvent =
            serde_json::from_slice(&events.try_recv().expect("target app offer"))
                .expect("offer event");
        let ui::realtime_continuity::RealtimeEvent::Offer { session, .. } = offered else {
            panic!("expected app offer");
        };

        assert!(
            RealtimeFabricAppBridge::from_state(Arc::clone(&state))
                .resolve_consent_with_policy(&session, true, || false)
                .is_err()
        );
        let ended: ui::realtime_continuity::RealtimeEvent =
            serde_json::from_slice(&events.try_recv().expect("target app teardown"))
                .expect("ended event");
        assert_eq!(
            ended,
            ui::realtime_continuity::RealtimeEvent::Ended {
                session: session.clone(),
            }
        );
        let realtime = state.realtime.lock().expect("target bridge");
        assert!(!realtime.sessions.contains_key(&session));
        assert!(!realtime.consent_capabilities_issued.contains(&session));
        drop(realtime);
        assert!(
            target_broker
                .lock()
                .expect("target broker")
                .session(fabric_id.into_broker_id())
                .is_none()
        );
        let cancel = target_fabric.drain_egress().pop().expect("cancel egress");
        assert_eq!(cancel.target, source);
        assert_eq!(cancel.signal, RealtimeFabricSignal::Cancel(fabric_id));
    }

    #[test]
    fn launch_content_rejects_bad_or_missing_hash_and_keeps_plain_launch_empty() {
        let mut workspace = crate::shell::state::Workspace::new(
            crate::shell::state::WorkspaceId::new(),
            "launcher test",
        );
        assert_eq!(
            stage_launch_content("so.tilo.photos", "", &mut workspace).expect("plain launch"),
            None
        );
        assert!(stage_launch_content("so.tilo.photos", "not-a-hash", &mut workspace).is_err());
        assert!(stage_launch_content("so.tilo.photos", &"a".repeat(64), &mut workspace).is_err());
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial(process_env)]
    fn launch_content_refuses_corrupt_or_symlinked_staging_targets() {
        let runtime = tempfile::tempdir().expect("runtime directory");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", runtime.path());
        let hash = qdoc::blobstore::put(b"verified launch fixture").expect("store fixture");
        let staged = runtime
            .path()
            .join("tilo-opened")
            .join(format!("{hash}.bin"));
        std::fs::create_dir_all(staged.parent().expect("staging parent")).expect("staging dir");
        std::fs::write(&staged, b"corrupt").expect("corrupt fixture");
        let mut workspace = crate::shell::state::Workspace::new(
            crate::shell::state::WorkspaceId::new(),
            "launcher test",
        );
        assert!(stage_launch_content("so.tilo.photos", &hash, &mut workspace).is_err());
        std::fs::remove_file(&staged).expect("remove corrupt fixture");
        let target = runtime.path().join("outside");
        std::fs::write(&target, b"outside").expect("outside fixture");
        std::os::unix::fs::symlink(&target, &staged).expect("symlink fixture");
        assert!(stage_launch_content("so.tilo.photos", &hash, &mut workspace).is_err());
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum DirectoryResolveMode {
        Found,
        Stale,
        NotFound,
    }

    /// A real `Cap'n Proto` directory endpoint retained by the `LocalSet` test
    /// below. It answers only bounded inert rows, but is deliberately a
    /// capability client rather than a mock of the shell bridge.
    pub(crate) struct TestActionDirectory {
        revision: String,
        pub(crate) resolve_mode: DirectoryResolveMode,
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
                view: None,
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
        if let Some(view) = &descriptor.view {
            let mut out = out.reborrow().init_view();
            out.set_id(&view.id);
            out.set_label(&view.label);
            out.set_description(view.description.as_deref().unwrap_or(""));
            out.set_open_action(&view.open_action);
            out.set_primary(view.primary);
            if let Some(mount) = &view.mount {
                let mut out = out.init_mount();
                out.set_action(&mount.action);
                out.set_capability_param(&mount.capability_param);
                out.set_view_param(&mount.view_param);
            }
        }
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
        out.set_resource_presentation_json(&row.resource_presentation_json);
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

    pub(crate) struct LocalDirectoryFixture {
        state: Arc<CapnpBrokerState>,
        directories: Rc<LocalActionDirectoryRegistry>,
        directory: action_directory::Client,
        directory_state: Rc<RefCell<TestActionDirectory>>,
        pub(crate) broker: crate::shell::collab_broker::AppActionBroker,
        chrome: crate::shell::chrome_state::Chrome,
        cluster_id: String,
        route: CrdtRoute,
        peer_id: u64,
        invocation_rx: tokio::sync::mpsc::UnboundedReceiver<InvocationEnvelope>,
    }

    impl LocalDirectoryFixture {
        pub(crate) fn set_confirmation_required(&self, required: bool) {
            self.directory_state.borrow_mut().confirmation_required = required;
        }

        pub(crate) fn set_result_spec(&self, result: tilo_actions::ResultSpec) {
            self.directory_state.borrow_mut().result = result;
        }

        /// Exercise the production peer-bound update RPC, including result-contract validation.
        pub(crate) async fn complete_invocation(
            &self,
            id: u64,
            update: &tilo_actions::InvocationUpdate,
        ) -> Result<(), capnp::Error> {
            let client: channel_handle::Client = capnp_rpc::new_client(CapnpChannelHandle {
                state: Arc::clone(&self.state),
                directories: Rc::clone(&self.directories),
                channel_id: self.route.channel_key(),
                peer_id: self.peer_id,
                verified_identity: AdmittedJoinIdentity {
                    verified: VerifiedJoinIdentity {
                        user_public_key: [0; 32],
                        device_public_key: [0; 32],
                    },
                    principal_id: "fixture-application".into(),
                    display_handle: None,
                    role: CapRole::Editor,
                    agent_id: None,
                },
            });
            let mut request = client.update_invocation_request();
            request.get().set_id(id);
            request
                .get()
                .set_update_json(&serde_json::to_string(update).expect("canonical update"));
            request.send().promise.await?;
            Ok(())
        }

        pub(crate) fn set_resolve_mode(&self, mode: DirectoryResolveMode) {
            self.directory_state.borrow_mut().resolve_mode = mode;
        }

        pub(crate) fn take_invocation_actor(&mut self) -> Option<String> {
            self.invocation_rx
                .try_recv()
                .ok()
                .map(|invocation| invocation.actor)
        }

        pub(crate) fn take_invocation_argument(&mut self) -> Option<Option<String>> {
            self.invocation_rx
                .try_recv()
                .ok()
                .map(|invocation| invocation.argument)
        }

        pub(crate) fn resolve_count(&self) -> usize {
            self.directory_state.borrow().resolve_count
        }
    }

    fn local_directory_fixture() -> LocalDirectoryFixture {
        let mut tree = WorkspaceTree::default();
        let mut workspace = crate::shell::state::Workspace::new(
            crate::shell::state::WorkspaceId(71),
            "Directory test room",
        );
        workspace.owner = Some(crate::shell::state::local_user_id_for_room(&workspace.room));
        tree.desktop.workspaces.push(workspace);
        tree.set_active(crate::shell::state::WorkspaceId(71));
        local_directory_fixture_with_membership(tree.shared())
    }

    pub(crate) fn local_directory_fixture_with_membership(
        membership: crate::shell::state::WorkspaceTreeHandle,
    ) -> LocalDirectoryFixture {
        local_directory_fixture_with_peer(membership, Some(71))
    }

    pub(crate) fn local_directory_fixture_with_first_allocated_peer(
        membership: crate::shell::state::WorkspaceTreeHandle,
    ) -> LocalDirectoryFixture {
        local_directory_fixture_with_peer(membership, None)
    }

    fn local_directory_fixture_with_peer(
        membership: crate::shell::state::WorkspaceTreeHandle,
        peer_id: Option<u64>,
    ) -> LocalDirectoryFixture {
        let cluster_id = membership
            .read()
            .desktop
            .active_workspace()
            .expect("directory fixture requires an active workspace")
            .room
            .to_string();
        let route = CrdtRoute::record("so.tilo.atlas", cluster_id.clone(), "atlas");
        let fanout = Arc::new(FanoutState::new());
        let (state, bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(Arc::clone(&membership)),
            None,
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
        LocalDirectoryFixture {
            broker: crate::shell::collab_broker::AppActionBroker::from_state_for_test(Arc::clone(
                &state,
            )),
            chrome: crate::shell::chrome_state::ChromeState::new_with_handle(membership),
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
    pub(crate) async fn drain_directory_bridge(fixture: &LocalDirectoryFixture) {
        fixture
            .state
            .drain_action_directory_bridge(&fixture.directories);
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
    }

    fn query_live_directory(
        provider: &LiveAppActionsProvider,
        chrome: &crate::shell::chrome_state::Chrome,
        doc: &AutoCommit,
        root: &ObjId,
    ) -> Vec<crate::shell::search::SearchResult> {
        let chrome = chrome.borrow();
        provider.query(SearchContext {
            needle: "open",
            wanted: &[],
            chrome: &chrome,
            doc,
            doc_root: root,
        })
    }

    fn only_directory_result(
        results: Vec<crate::shell::search::SearchResult>,
    ) -> crate::shell::search::SearchResult {
        results
            .into_iter()
            .find(|result| result.id.starts_with("directory\u{1f}"))
            .expect("one directory action result")
    }

    #[allow(
        clippy::future_not_send,
        reason = "the test helper awaits only LocalSet-bound directory work holding Rc-backed Cap'n Proto state"
    )]
    async fn load_directory_result(
        fixture: &LocalDirectoryFixture,
        provider: &LiveAppActionsProvider,
        doc: &AutoCommit,
        root: &ObjId,
    ) -> crate::shell::search::SearchResult {
        let initial = query_live_directory(provider, &fixture.chrome, doc, root);
        assert!(
            initial
                .iter()
                .all(|result| !result.id.starts_with("directory\u{1f}")),
            "first provider query must not synchronously fabricate a directory row"
        );
        drain_directory_bridge(fixture).await;
        only_directory_result(query_live_directory(provider, &fixture.chrome, doc, root))
    }

    #[test]
    fn directory_queue_refuses_oversized_query_text_and_cursor_before_contacting_the_app() {
        let fixture = local_directory_fixture();
        let queue = fixture.state.action_directory_request_queue();
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
        let mut oversized_argument =
            crate::shell::remote_actions::BrokerActionHost::new(fixture.broker.clone()).invoke(
                &tilo_actions::remote::Caller::local_person("actor-a").in_room(&fixture.cluster_id),
                tilo_actions::remote::InvokeRequest {
                    invocation: tilo_actions::InvocationId::new(),
                    target: tilo_actions::remote::RemoteActionTarget {
                        provider: fixture.peer_id.to_string(),
                        address: tilo_actions::ActionAddress::new("workspace:atlas", "open"),
                    },
                    argument: tilo_actions::remote::InvokeArgument::Text(
                        "x".repeat(DIRECTORY_MAX_PARAMS_BYTES.saturating_add(1)),
                    ),
                    context: None,
                    confirmation: None,
                    revision: Some("directory-r1".into()),
                },
            );
        assert!(matches!(
            oversized_argument.poll_ready(),
            tilo_actions::remote::PendingPoll::Ready(Err(
                tilo_actions::remote::ActionHostError::InvalidRequest { .. }
            ))
        ));
        assert_eq!(
            fixture.directory_state.borrow().query_count,
            0,
            "invalid local requests are rejected before a directory capability is contacted"
        );
    }

    #[test]
    fn directory_wire_rows_refuse_untrusted_presentation_and_control_framing() {
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
        let mut row = fixture.directory_state.borrow().row();
        row.action = None;
        row.descriptor = None;
        row.resource_presentation_json = serde_json::json!({
            "detail": "x".repeat(tilo_actions::ResourcePresentation::MAX_DETAIL_BYTES + 1)
        })
        .to_string();
        assert!(
            permits_directory_row(
                &fixture.state,
                &fixture.cluster_id,
                &BrokerActor::person("actor-a"),
                &fixture.route.app_id,
                &row,
            )
            .is_err(),
            "oversized app metadata cannot reach shell chrome"
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
                        app_id: "so.tilo.test".into(),
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
    #[allow(
        clippy::too_many_lines,
        reason = "one LocalSet lifecycle test deliberately keeps every queue/cache transition in temporal order"
    )]
    fn localset_directory_queue_drives_pending_start_refusal_actor_cache_and_invalidation() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("current-thread test runtime");
        let local = tokio::task::LocalSet::new();
        runtime.block_on(local.run_until(async {
            let mut fixture = local_directory_fixture();
            let room = crate::shell::state::RoomId::parse(&fixture.cluster_id)
                .expect("fixture route is a room id");
            let directory_actor = fixture.chrome.borrow().local_user_for_room(&room);

            // A directory start stays pending until the LocalSet has the
            // owner re-resolve the row; no caller blocks the compositor on
            // an RPC. The shared confirmation check refuses it there.
            fixture.directory_state.borrow_mut().confirmation_required = true;
            let mut confirmation = crate::shell::remote_actions::BrokerActionHost::new(
                fixture.broker.clone(),
            )
            .invoke(
                &tilo_actions::remote::Caller::local_person(&directory_actor)
                    .in_room(&fixture.cluster_id),
                tilo_actions::remote::InvokeRequest {
                    invocation: tilo_actions::InvocationId::new(),
                    target: tilo_actions::remote::RemoteActionTarget {
                        provider: fixture.peer_id.to_string(),
                        address: tilo_actions::ActionAddress::new("workspace:atlas", "open"),
                    },
                    argument: tilo_actions::remote::InvokeArgument::None,
                    context: None,
                    confirmation: None,
                    revision: Some("directory-r1".into()),
                },
            );
            assert!(matches!(
                confirmation.poll_ready(),
                tilo_actions::remote::PendingPoll::Waiting
            ));
            drain_directory_bridge(&fixture).await;
            let confirmation_reply = tokio::time::timeout(Duration::from_millis(100), confirmation)
                .await
                .expect("LocalSet reply deadline")
                .expect("the host answers");
            assert!(
                matches!(
                    confirmation_reply,
                    Err(tilo_actions::remote::ActionHostError::ConfirmationRequired)
                ),
                "unexpected confirmation reply: {confirmation_reply:?}"
            );
            fixture.directory_state.borrow_mut().confirmation_required = false;

            let provider = LiveAppActionsProvider::new(fixture.broker.clone());
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT.clone();
            let _same_room_actor = load_directory_result(&fixture, &provider, &doc, &root).await;
            assert_eq!(fixture.directory_state.borrow().query_count, 1);

            // Re-querying the same root uses its room-derived actor and page;
            // changing `ChromeState::local_user_id` must not retarget it.
            fixture.chrome.borrow_mut().local_user_id = "unrelated-active-room-user".into();
            let room_actor_result = only_directory_result(query_live_directory(
                &provider,
                &fixture.chrome,
                &doc,
                &root,
            ));
            assert_eq!(fixture.directory_state.borrow().query_count, 1);

            let begin = room_actor_result
                .on_begin_invoke
                .as_ref()
                .expect("directory result starts through an atomic broker ticket");
            let InvocationDisposition::Pending(ticket) = begin(
                &mut doc,
                &root,
                &fixture.chrome,
                &tilo_actions::ActionArgs::none(),
            )
            .expect("directory invocation request enqueues") else {
                panic!("directory action must not claim accepted before LocalSet admission");
            };
            assert!(ticket().expect("pending ticket").is_none());
            drain_directory_bridge(&fixture).await;
            let accepted = ticket()
                .expect("accepted ticket")
                .expect("LocalSet accepted directory action");
            assert!(accepted.broker_invocation_id.is_some());
            assert_eq!(
                fixture
                    .invocation_rx
                    .try_recv()
                    .expect("admitted invocation reaches application stream")
                    .actor,
                directory_actor
            );

            // Stale and revoked exact resolutions both invalidate the page
            // captured by the provider, then force a fresh bounded query.
            fixture.directory_state.borrow_mut().resolve_mode = DirectoryResolveMode::Stale;
            let stale = room_actor_result
                .on_begin_invoke
                .as_ref()
                .expect("directory ticket")(
                &mut doc,
                &root,
                &fixture.chrome,
                &tilo_actions::ActionArgs::none(),
            )
            .expect("stale request queues");
            let InvocationDisposition::Pending(stale) = stale else {
                panic!("stale action must be asynchronous");
            };
            drain_directory_bridge(&fixture).await;
            assert!(stale().is_err(), "stale exact resolution refuses the cached row");
            fixture.directory_state.borrow_mut().resolve_mode = DirectoryResolveMode::Found;
            let _refreshed_after_stale = load_directory_result(&fixture, &provider, &doc, &root).await;
            assert_eq!(fixture.directory_state.borrow().query_count, 2);

            fixture.directory_state.borrow_mut().resolve_mode = DirectoryResolveMode::NotFound;
            let revoked_source = only_directory_result(query_live_directory(
                &provider,
                &fixture.chrome,
                &doc,
                &root,
            ));
            let InvocationDisposition::Pending(revoked) = revoked_source
                .on_begin_invoke
                .as_ref()
                .expect("directory ticket")(
                &mut doc,
                &root,
                &fixture.chrome,
                &tilo_actions::ActionArgs::none(),
            )
            .expect("revoked request queues") else {
                panic!("revoked action must be asynchronous");
            };
            drain_directory_bridge(&fixture).await;
            assert!(revoked().is_err(), "revoked exact resolution refuses the cached row");
            fixture.directory_state.borrow_mut().resolve_mode = DirectoryResolveMode::Found;
            let disconnected_source = load_directory_result(&fixture, &provider, &doc, &root).await;
            assert_eq!(fixture.directory_state.borrow().query_count, 3);

            fixture.directories.remove(fixture.peer_id);
            let InvocationDisposition::Pending(disconnected) = disconnected_source
                .on_begin_invoke
                .as_ref()
                .expect("directory ticket")(
                &mut doc,
                &root,
                &fixture.chrome,
                &tilo_actions::ActionArgs::none(),
            )
            .expect("disconnect request queues") else {
                panic!("disconnect action must be asynchronous");
            };
            drain_directory_bridge(&fixture).await;
            assert!(disconnected().is_err(), "disconnect invalidates the cached directory row");
            assert!(
                query_live_directory(&provider, &fixture.chrome, &doc, &root).is_empty(),
                "after disconnect the provider must not render its old cached row"
            );

            fixture.directories.directories.borrow_mut().insert(
                fixture.peer_id,
                RegisteredActionDirectory {
                    app_id: fixture.route.app_id.clone(),
                    route: fixture.route.clone(),
                    directory: fixture.directory.clone(),
                },
            );
            drain_directory_bridge(&fixture).await;
            let reconnected = query_live_directory(&provider, &fixture.chrome, &doc, &root);
            assert!(
                reconnected.iter().any(|result| result.id.starts_with("directory\u{1f}")),
                "a reconnected directory is reloaded through the queue, never revived from stale cache"
            );
        }));
    }

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

    #[test]
    fn closed_workspace_control_session_is_terminal_for_rpc_readers() {
        let target = iroh::SecretKey::generate().public();
        let mut session = crate::network::WorkspaceControlSession::new(
            7,
            11,
            target,
            crate::network::WorkspaceControlRoute::Direct { target },
        );
        assert!(closed_workspace_control_event(&session).is_none());
        session.disconnect();
        assert_eq!(
            closed_workspace_control_event(&session),
            Some(runtime_collab::WorkspaceControlEvent::Closed(
                runtime_collab::WorkspaceControlCloseReason::LocalDisconnect,
            ))
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
        let workspace = &mut tree.desktop.workspaces[0];
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
        let mut workspace =
            crate::shell::state::Workspace::new(crate::shell::state::WorkspaceId(1), "Member room");
        let route = CrdtRoute::cluster(workspace.room.to_string());
        workspace.grant_capability(crate::shell::state::CapOverride::membership(
            crate::shell::state::Subject::person(&principal),
            workspace.room.to_string(),
            crate::shell::state::Verb::Write,
        ));
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(workspace);

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
        let mut workspace =
            crate::shell::state::Workspace::new(crate::shell::state::WorkspaceId(44), "Agent room");
        workspace.owner = Some(principal);
        let route = CrdtRoute::cluster(workspace.room.to_string());
        let agent_membership = workspace.grant_capability(CapOverride::membership(
            Subject::agent("scribe"),
            workspace.room.to_string(),
            Verb::Comment,
        ));
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(workspace);

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
        assert!(membership.write().desktop.workspaces[0].revoke_capability(&agent_membership));
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
        let mut workspace = crate::shell::state::Workspace::new(
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
        tree.desktop.workspaces.push(workspace);
        let mut admitted = admit_verified_join(&tree, &route, verified_device(&member))
            .expect("the person is a member");
        assert!(admit_agent_delegation(&tree, &route, &mut admitted, "operator").is_err());
    }

    #[test]
    fn certified_join_admission_rejects_wrong_or_removed_root() {
        let member = SigningKey::from_bytes(&[21; 32]);
        let stranger = SigningKey::from_bytes(&[22; 32]);
        let member_id = principal_id_for_root(member.verifying_key().to_bytes());
        let mut workspace = crate::shell::state::Workspace::new(
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
        tree.desktop.workspaces.push(workspace);

        assert!(admit_verified_join(&tree, &route, verified_device(&stranger)).is_err());
        let admitted = admit_verified_join(&tree, &route, verified_device(&member))
            .expect("member starts admitted");
        let membership_tree = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(Arc::clone(&membership_tree)),
            None,
        );
        assert!(membership_tree.write().desktop.workspaces[0].revoke_capability(&membership));
        assert!(
            state.revalidate_admission(&route, &admitted).is_err(),
            "removing membership rejects the certified session's next operation"
        );
    }

    #[test]
    fn certified_join_admission_rejects_cross_workspace_replay() {
        let root = SigningKey::from_bytes(&[23; 32]);
        let principal = principal_id_for_root(root.verifying_key().to_bytes());
        let mut member_workspace =
            crate::shell::state::Workspace::new(crate::shell::state::WorkspaceId(3), "Member room");
        member_workspace.set_owner(&principal);
        let other_workspace =
            crate::shell::state::Workspace::new(crate::shell::state::WorkspaceId(4), "Other room");
        let replay_route = CrdtRoute::cluster(other_workspace.room.to_string());
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(member_workspace);
        tree.desktop.workspaces.push(other_workspace);

        assert!(admit_verified_join(&tree, &replay_route, verified_device(&root)).is_err());
    }

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
        assert_eq!(app_id, "so.tilo.notes");
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
        let mut workspace = crate::shell::state::Workspace::new(
            crate::shell::state::WorkspaceId(5),
            "Typed document room",
        );
        workspace.set_owner(&principal);
        let route = CrdtRoute::record("so.tilo.notes", workspace.room.to_string(), "meeting-notes");
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(workspace);
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
        let (state, _bridge_rx) = CapnpBrokerState::new(
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
        assert!(matches!(
            state
                .authorize_certified_document(&route, &admitted, &encrypted)
                .expect("broker authorizes signed clear metadata without decrypting"),
            collab_types::RoutedDocumentPayload::Encrypted { channel, epoch: 7 }
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

        let target = CrdtRoute::record("so.tilo.notes", "room-1", "note-a");
        let other_record = CrdtRoute::record("so.tilo.notes", "room-1", "note-b");
        let other_app = CrdtRoute::record("so.tilo.files", "room-1", "note-a");
        let other_room = CrdtRoute::record("so.tilo.notes", "room-2", "note-a");
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

    #[allow(
        clippy::too_many_lines,
        clippy::arithmetic_side_effects,
        clippy::unchecked_time_subtraction,
        reason = "the directed-action test keeps capability setup, delivery, and denial assertions in one scenario"
    )]
    #[test]
    fn live_action_invocation_is_fresh_directed_and_permission_checked() {
        let mut workspace = crate::shell::state::Workspace::new(
            crate::shell::state::WorkspaceId(31),
            "Action room",
        );
        workspace.set_owner("owner");
        let route = CrdtRoute::app("so.tilo.files", workspace.room.to_string());
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(workspace);
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
                app_id: "so.tilo.files".into(),
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
                        view: None,
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
                        view: None,
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
                        view: None,
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
        let owner_catalogue = catalogue_text(
            &state,
            &tilo_actions::remote::Caller::local_person("owner").in_room(&action_cluster),
        );
        assert!(owner_catalogue.contains("files.entry:/tmp/report"));
        let denied_catalogue = catalogue_text(
            &state,
            &tilo_actions::remote::Caller::local_person("stranger").in_room(&action_cluster),
        );
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
            app_id: "so.tilo.files".into(),
            route: CrdtRoute::app(
                "so.tilo.files",
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
                view: None,
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
            !catalogue_text(
                &state,
                &tilo_actions::remote::Caller::local_person("owner")
                    .in_room(&archived[0].route.cluster_id),
            )
            .contains("new-window"),
            "archived launchable descriptors are not directly invokable through this protocol"
        );
    }

    /// The catalogue `caller` is offered, as searchable text.
    fn catalogue_text(
        state: &Arc<CapnpBrokerState>,
        caller: &tilo_actions::remote::Caller,
    ) -> String {
        let mut page = state.action_query(
            caller,
            &tilo_actions::remote::CatalogueQuery {
                max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
                ..tilo_actions::remote::CatalogueQuery::default()
            },
        );
        let tilo_actions::remote::PendingPoll::Ready(Ok(page)) = page.poll_ready() else {
            panic!("the eager catalogue answers at once");
        };
        serde_json::to_string(&page).expect("catalogue encodes")
    }

    /// A workspace whose `agent` may save and assign permission profiles.
    fn profile_governed_workspace(agent: &str) -> crate::shell::state::Workspace {
        let mut workspace = crate::shell::state::Workspace::new(
            crate::shell::state::WorkspaceId(32),
            "Governance room",
        );
        workspace.set_owner("owner");
        for action in [
            crate::shell::state::PROFILE_SAVE_ACTION_ID,
            crate::shell::state::PROFILE_ASSIGN_ACTION_ID,
        ] {
            workspace.grant_capability(collab_types::CapOverride::grant(
                collab_types::Subject::agent(agent),
                collab_types::Scope::Action {
                    app: crate::shell::state::WORKSPACE_GOVERNANCE_APP_ID.into(),
                    action: action.into(),
                },
                collab_types::Verb::Admin,
            ));
        }
        workspace
    }

    #[test]
    fn broker_catalogue_agent_can_create_and_assign_a_permission_profile() {
        let workspace = profile_governed_workspace("architect");
        let cluster = workspace.room.to_string();
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(workspace);
        let membership = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(membership.clone()),
            None,
        );
        let actor = BrokerActor {
            id: "agent:architect".into(),
            is_agent: true,
        };
        let catalogue = catalogue_text(
            &state,
            &tilo_actions::remote::Caller::local_agent("architect").in_room(&cluster),
        );
        assert!(catalogue.contains(crate::shell::state::PROFILE_SAVE_ACTION_ID));
        assert!(catalogue.contains(crate::shell::state::WORKSPACE_GOVERNANCE_APP_ID));

        let rules = serde_json::to_value(vec![collab_types::PermissionProfileRule::grant(
            collab_types::Scope::Action {
                app: "so.tilo.notes".into(),
                action: "note.open".into(),
            },
            collab_types::Verb::Read,
        )])
        .unwrap();
        let save_arguments = format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::json!({
                "name": "Notes reader",
                "description": "Read one Notes action",
                "rules": rules,
            })
        );
        let root_id = format!("workspace.permissions:{cluster}");
        let (_, saved) = state
            .invoke_workspace_profile_action(
                &cluster,
                &actor,
                &root_id,
                crate::shell::state::PROFILE_SAVE_ACTION_ID,
                Some(&save_arguments),
                &serde_json::json!({"confirm": true}),
            )
            .expect("broker-owned save succeeds");
        assert!(matches!(
            saved,
            tilo_actions::InvocationUpdate::Succeeded { .. }
        ));

        let assign_arguments = format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::json!({
                "profile": "Notes reader",
                "subject": "alice",
                "subject_kind": "person",
            })
        );
        state
            .invoke_workspace_profile_action(
                &cluster,
                &actor,
                &root_id,
                crate::shell::state::PROFILE_ASSIGN_ACTION_ID,
                Some(&assign_arguments),
                &serde_json::json!({"confirm": true}),
            )
            .expect("broker-owned assignment succeeds");

        let tree = membership.read();
        let workspace = &tree.desktop.workspaces[0];
        assert!(workspace.allows(
            &workspace.actor_for_person(&"alice".into()),
            &collab_types::ResourceRef::Action {
                app: "so.tilo.notes".into(),
                action: "note.open".into(),
            },
            collab_types::Verb::Read,
        ));
        assert_eq!(workspace.effect_receipts.len(), 2);
    }

    #[allow(
        clippy::significant_drop_tightening,
        clippy::too_many_lines,
        reason = "the receipt assertions retain the read guard for the complete consistency snapshot"
    )]
    #[test]
    fn remote_agent_effect_terminals_become_redacted_workspace_receipts() {
        let mut workspace = crate::shell::state::Workspace::new(
            crate::shell::state::WorkspaceId(32),
            "Agent effect room",
        );
        workspace.set_owner("owner");
        let cluster_id = workspace.room.to_string();
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(workspace);
        let membership = tree.shared();
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(Arc::clone(&membership)),
            None,
        );
        let (tx, _rx) = std::sync::mpsc::channel();
        let invocation = tilo_actions::InvocationId::new();
        let pending = PendingInvocation {
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
            .with_app("org.tilo.Writer", "Writer")
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
        let receipts = &tree.desktop.workspaces[0].effect_receipts;
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
    fn nearby_target_handles_reject_spoofed_routes_and_expiry() {
        let route = CrdtRoute::app(FILES_APP_ID, "room-a");
        let other_route = CrdtRoute::app(FILES_APP_ID, "room-b");
        let target = crate::shell::personal_continuity::DeviceId::new("certified-device")
            .expect("valid device");
        let now = Instant::now();
        let mut registry = NearbyTargetRegistry::default();
        let handle = registry.issue(41, &route, target.clone(), now);

        assert!(registry.consume(handle, 42, &route, now).is_none());
        assert!(registry.consume(handle, 41, &other_route, now).is_none());
        assert_eq!(registry.consume(handle, 41, &route, now), Some(target));
        assert!(registry.consume(handle, 41, &route, now).is_none());

        let stale = registry.issue(
            41,
            &route,
            crate::shell::personal_continuity::DeviceId::new("stale-device").expect("valid device"),
            now,
        );
        assert!(
            registry
                .consume(
                    stale,
                    41,
                    &route,
                    now + NEARBY_TARGET_HANDLE_TTL + Duration::from_millis(1),
                )
                .is_none()
        );

        let retained_target = crate::shell::personal_continuity::DeviceId::new("retained-device")
            .expect("valid device");
        let retained = registry.issue(41, &route, retained_target.clone(), now);
        registry.begin_snapshot(41, &route, std::slice::from_ref(&retained_target), now);
        let refreshed = registry.issue(41, &route, retained_target, now);
        assert_eq!(retained, refreshed);
        assert_eq!(registry.bindings.len(), 1);
        registry.begin_snapshot(41, &route, &[], now);
        assert!(registry.consume(retained, 41, &route, now).is_none());
        assert!(registry.bindings.is_empty());
    }

    #[test]
    fn nearby_transfer_handles_reject_cross_workspace_actions_and_disconnect() {
        let route = CrdtRoute::app(FILES_APP_ID, "room-a");
        let other_route = CrdtRoute::app(FILES_APP_ID, "room-b");
        let transfer_id = [7_u8; 16];
        let now = Instant::now();
        let mut registry = NearbyTransferRegistry::default();
        let handle = registry
            .issue(
                41,
                &route,
                transfer_id,
                runtime_collab::NearbyTransferDirection::Receiving,
                true,
                now,
            )
            .expect("owner receives opaque handle");
        assert!(
            registry
                .resolve(
                    handle,
                    42,
                    &route,
                    runtime_collab::NearbyTransferDirection::Receiving,
                    now,
                )
                .is_none()
        );
        assert!(
            registry
                .resolve(
                    handle,
                    41,
                    &other_route,
                    runtime_collab::NearbyTransferDirection::Receiving,
                    now,
                )
                .is_none()
        );
        assert!(
            registry
                .issue(
                    42,
                    &other_route,
                    transfer_id,
                    runtime_collab::NearbyTransferDirection::Receiving,
                    true,
                    now,
                )
                .is_none()
        );
        registry.remove_peer(41);
        assert!(
            registry
                .resolve(
                    handle,
                    41,
                    &route,
                    runtime_collab::NearbyTransferDirection::Receiving,
                    now,
                )
                .is_none()
        );
    }

    #[test]
    fn nearby_destination_handles_are_route_bound_and_session_remembered() {
        let root = tempfile::tempdir().expect("destination root");
        let route = CrdtRoute::app(FILES_APP_ID, "room-a");
        let other_route = CrdtRoute::app(FILES_APP_ID, "room-b");
        let now = Instant::now();
        let mut registry = NearbyDestinationRegistry::default();
        let destination = registry.issue(
            41,
            &route,
            root.path().to_path_buf(),
            "Workspace files".into(),
            now,
        );
        assert!(
            registry
                .resolve(destination.handle, 42, &route, false, now)
                .is_none()
        );
        assert!(
            registry
                .resolve(destination.handle, 41, &other_route, false, now)
                .is_none()
        );
        assert!(
            registry
                .resolve(destination.handle, 41, &route, true, now)
                .is_some()
        );
        let refreshed = registry.issue(
            41,
            &route,
            root.path().to_path_buf(),
            "Workspace files".into(),
            now,
        );
        assert_eq!(refreshed.handle, destination.handle);
        assert!(refreshed.remembered_for_session);
        registry.remove_peer(41);
        assert!(
            registry
                .resolve(destination.handle, 41, &route, false, now)
                .is_none()
        );
    }

    #[test]
    fn nearby_source_is_canonical_and_confined_to_live_files_grants() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("temp grant root");
        let source = root.path().join("report.txt");
        std::fs::write(&source, b"report").expect("write source");
        let grants = vec![Capability::Filesystem {
            path: root.path().to_path_buf(),
            mode: FsMode::Read,
            scope: GRANT_SESSION,
        }];
        let opened = nearby_source_from_grants(source.as_os_str().as_encoded_bytes(), &grants)
            .expect("granted source");
        assert_eq!(opened.name(), "report.txt");
        assert_eq!(opened.metadata().expect("source metadata").len(), 6);
        assert!(nearby_source_from_grants(b"/etc/passwd", &grants).is_err());

        let escape = root.path().join("escape");
        symlink("/etc/passwd", &escape).expect("create escape symlink");
        assert!(nearby_source_from_grants(escape.as_os_str().as_encoded_bytes(), &grants).is_err());
    }

    #[test]
    fn nearby_rpc_policy_requires_master_and_file_transfer_switches() {
        let policy = |enabled, file_transfer| {
            let document = serde_json::json!({
                "continuity": {
                    "enabled": enabled,
                    "file_transfer": file_transfer,
                }
            });
            crate::shell::continuity_policy::ContinuityPolicySnapshot::from_document(
                document.as_object().expect("policy object"),
            )
        };
        assert!(!nearby_policy_enabled(policy(false, true)));
        assert!(!nearby_policy_enabled(policy(true, false)));
        assert!(nearby_policy_enabled(policy(true, true)));
    }

    fn continuation_policy(
        enabled: bool,
    ) -> crate::shell::continuity_policy::ContinuityPolicySnapshot {
        let document = serde_json::json!({
            "continuity": {
                "enabled": true,
                "app_continuations": enabled,
            }
        });
        crate::shell::continuity_policy::ContinuityPolicySnapshot::from_document(
            document.as_object().expect("policy object"),
        )
    }

    fn continuation_metadata(url: &str) -> ui::collaboration::ContinuationMetadata {
        ui::app::compose_structural_continuation_metadata(
            ui::view::View::default(),
            Some(ui::collaboration::ContinuationMetadata {
                schema_version: 1,
                bytes: format!(r#"{{"url":"{url}"}}"#).into_bytes(),
            }),
        )
        .expect("valid browser continuation")
    }

    fn continuation_route_for(room: crate::shell::state::RoomId, instance: u64) -> CrdtRoute {
        CrdtRoute::record(
            "so.tilo.browser",
            room.to_string(),
            format!("continuation:{instance}"),
        )
    }

    #[test]
    fn disabled_app_continuation_report_never_enters_registry() {
        let room = crate::shell::state::RoomId::new();
        let route = continuation_route_for(room, 11);
        let metadata = continuation_metadata("https://disabled.test");
        let mut reports =
            crate::shell::personal_continuity_registry::ContinuationRegistry::default();

        let result = report_continuation_for_route(
            &mut reports,
            continuation_policy(false),
            &route,
            "so.tilo.browser",
            metadata.schema_version,
            metadata.bytes,
        );

        assert_eq!(result, Err("app continuation is disabled".into()));
        assert!(
            reports
                .resume_for(
                    room,
                    "so.tilo.browser",
                    crate::shell::personal_continuity::AppInstanceId(11),
                )
                .is_none()
        );
    }

    #[test]
    fn clear_continuation_affects_only_the_exact_authenticated_route() {
        let room = crate::shell::state::RoomId::new();
        let first = continuation_route_for(room, 11);
        let second = continuation_route_for(room, 22);
        let mut reports =
            crate::shell::personal_continuity_registry::ContinuationRegistry::default();
        for (route, url) in [
            (&first, "https://first.test"),
            (&second, "https://second.test"),
        ] {
            let metadata = continuation_metadata(url);
            report_continuation_for_route(
                &mut reports,
                continuation_policy(true),
                route,
                "so.tilo.browser",
                metadata.schema_version,
                metadata.bytes,
            )
            .expect("enabled exact route report");
        }

        assert!(
            clear_continuation_for_route(&mut reports, &first, "so.tilo.browser")
                .expect("exact clear")
        );
        assert!(
            reports
                .resume_for(
                    room,
                    "so.tilo.browser",
                    crate::shell::personal_continuity::AppInstanceId(11),
                )
                .is_none()
        );
        assert!(
            reports
                .resume_for(
                    room,
                    "so.tilo.browser",
                    crate::shell::personal_continuity::AppInstanceId(22),
                )
                .is_some()
        );
        assert!(clear_continuation_for_route(&mut reports, &second, "so.tilo.editor").is_err());
        assert!(
            reports
                .resume_for(
                    room,
                    "so.tilo.browser",
                    crate::shell::personal_continuity::AppInstanceId(22),
                )
                .is_some()
        );
    }

    fn test_published_view(action: &str, view_id: &str, primary: bool) -> PublishedAction {
        PublishedAction {
            resource: "note:one".into(),
            action: action.into(),
            resource_kind: "note".into(),
            resource_label: "One".into(),
            action_label: "Open".into(),
            description: None,
            operation_kind: tilo_actions::OperationKind::View,
            params: vec![
                tilo_actions::ActionParam::optional(
                    "slot_capability",
                    tilo_actions::ParamType::Text,
                    "Mount capability",
                ),
                tilo_actions::ActionParam::optional(
                    "slot_view",
                    tilo_actions::ParamType::Text,
                    "View slot",
                ),
            ],
            contract: tilo_actions::ActionContract::view(),
            view: Some(tilo_actions::ResourceViewDescriptor {
                id: view_id.into(),
                label: "Editor".into(),
                description: None,
                open_action: action.into(),
                primary,
                mount: Some(tilo_actions::ResourceViewMountSpec::new(
                    action,
                    "slot_capability",
                    "slot_view",
                )),
            }),
            input: PublishedInput::None,
        }
    }

    #[test]
    fn published_resource_views_validate_bindings_and_resource_cardinality() {
        let editor = test_published_view("open", "editor", true);
        assert!(validate_resource_view_descriptors(std::slice::from_ref(&editor)).is_ok());

        let mut wrong_owner = editor.clone();
        wrong_owner.view.as_mut().unwrap().open_action = "preview".into();
        assert!(validate_resource_view_descriptors(std::slice::from_ref(&wrong_owner)).is_err());

        let mut unsafe_open = editor.clone();
        unsafe_open.contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
        assert!(validate_resource_view_descriptors(std::slice::from_ref(&unsafe_open)).is_err());

        let mut invalid_mount = editor.clone();
        invalid_mount
            .view
            .as_mut()
            .unwrap()
            .mount
            .as_mut()
            .unwrap()
            .view_param = "slot_capability".into();
        assert!(validate_resource_view_descriptors(std::slice::from_ref(&invalid_mount)).is_err());

        let reader = test_published_view("read", "reader", true);
        assert!(validate_resource_view_descriptors(&[editor, reader]).is_err());
    }
}
