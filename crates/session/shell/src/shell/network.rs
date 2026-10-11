//! Shell-side bridge into the peer-sync [`Actor`]: owns the
//! [`WorkspaceTreeReplica`], drives the actor's `tokio` runtime, and drains
//! `NetEvent`s into replica replies re-published as `NetCmd::Publish`s.
//! Started by default in normal sessions; recovery sessions may disable it.

use crate::shell::state::WorkspaceAuthority as _;

mod boot_join;
#[cfg(test)]
mod display_input_tests;
#[cfg(test)]
mod display_observation_refresh_tests;
mod display_observations;
mod governor_actions;
mod hosted_topology;
#[cfg(test)]
mod idle_quiet_tests;
mod input_declines;
#[cfg(test)]
mod live_identity_tests;
mod native_enrollment;
mod native_ingress;
#[cfg(test)]
mod paired_continuity_tests;
mod personal_device_peers;
#[cfg(test)]
mod reachability_tests;
mod record_content;
pub use record_content::InboundRecordContent;
#[cfg(test)]
mod node_member_tests;
mod relay_rooms;
mod room_gate;
#[cfg(test)]
mod room_gate_tests;
#[cfg(test)]
mod same_person_room_tests;

use std::collections::VecDeque;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use getrandom::{SysRng, rand_core::UnwrapErr};
use iroh::address_lookup::memory::MemoryLookup;
use iroh::endpoint::presets;
use rand_core::Rng;
use serde::{Deserialize, Serialize};
use tilo_clock::unix_now_ms;
use tokio::sync::mpsc;

use crate::network::actor::{Actor, HostIdentity, NearbyTransferCheckpoint, NetCmd, NetEvent};
use crate::network::actor::{PersonalFabricPayload, WorkspaceControlRoute};
use crate::network::behaviour::load_or_create_keypair;
use crate::network::identity::PeerIdentityStore;
use crate::network::invite::accept_invite_into;
use crate::network::protocol::{MAX_PERSONAL_CONTINUITY_CHECKPOINT_BYTES, NeighbourMsg};
use crate::network::replica::WorkspaceTreeReplica;
use crate::shell::state::WorkspaceTree;
use crate::shell::state::{RoomId, WorkspaceTreeHandle};

pub use crate::network::actor::PeerDepartureCause;
use crate::shell::collab_broker::CrdtRoute;

/// Compatibility/control topic used only when a message has no room identity.
pub const DEFAULT_WS_TOPIC: u64 = 0;

/// Maximum number of recent transport departures retained for diagnostics.
///
/// A session can repeatedly lose and regain a peer, so this is a ring rather
/// than a once-per-peer map. The history is session-local and intentionally
/// contains no application payload.
const PEER_DEPARTURE_HISTORY_CAPACITY: usize = 64;
const PERSONAL_FABRIC_ACTION_QUEUE_CAPACITY: usize = 256;
/// Leave room for consent, cancellation, revocation, and teardown while a
/// producer is faster than the runtime can consume media frames.
const PERSONAL_FABRIC_DATA_QUEUE_CAPACITY: usize = 192;

/// Why a personal fabric action did not reach the transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersonalFabricSendError {
    /// The transport command queue is momentarily full. The action was not
    /// sent, but the same call can succeed once the actor drains the queue.
    Backpressure,
    /// Identity, enrollment, wire validation or a closed transport refused
    /// the action; retrying it unchanged cannot succeed.
    Rejected(String),
}

impl std::fmt::Display for PersonalFabricSendError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backpressure => formatter.write_str("personal fabric transport is busy"),
            Self::Rejected(reason) => formatter.write_str(reason),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersonalFabricQueueOutcome {
    Queued,
    Coalesced,
    Saturated,
}

fn is_personal_fabric_bulk_data(
    action: &crate::shell::device_fabric::PersonalFabricAction,
) -> bool {
    matches!(
        action,
        crate::shell::device_fabric::PersonalFabricAction::RemoteAudioFrame { .. }
            | crate::shell::device_fabric::PersonalFabricAction::RemoteCaptureChunk { .. }
    )
}

fn is_duplicate_personal_fabric_data(
    queued: &crate::shell::device_fabric::PersonalFabricActionEnvelope,
    incoming: &crate::shell::device_fabric::PersonalFabricActionEnvelope,
) -> bool {
    use crate::shell::device_fabric::PersonalFabricAction;

    if queued.source != incoming.source || queued.target != incoming.target {
        return false;
    }
    match (&queued.action, &incoming.action) {
        (
            PersonalFabricAction::RemoteAudioFrame {
                session_id,
                sequence,
                ..
            },
            PersonalFabricAction::RemoteAudioFrame {
                session_id: incoming_session,
                sequence: incoming_sequence,
                ..
            },
        ) => session_id == incoming_session && sequence == incoming_sequence,
        (
            PersonalFabricAction::RemoteCaptureChunk { chunk },
            PersonalFabricAction::RemoteCaptureChunk {
                chunk: incoming_chunk,
            },
        ) => {
            chunk.session_id == incoming_chunk.session_id
                && chunk.sequence == incoming_chunk.sequence
        }
        _ => false,
    }
}

trait PersonalFabricEnvelopeRef {
    fn envelope(&self) -> &crate::shell::device_fabric::PersonalFabricActionEnvelope;
}

impl PersonalFabricEnvelopeRef for crate::shell::device_fabric::PersonalFabricActionEnvelope {
    fn envelope(&self) -> &crate::shell::device_fabric::PersonalFabricActionEnvelope {
        self
    }
}

fn personal_fabric_terminal_matches_data(
    terminal: &impl PersonalFabricEnvelopeRef,
    queued: &impl PersonalFabricEnvelopeRef,
) -> bool {
    use crate::shell::device_fabric::PersonalFabricAction;
    use crate::shell::remote_capture::device::RemoteCaptureSignal;

    let terminal = terminal.envelope();
    let queued = queued.envelope();

    if terminal.source != queued.source || terminal.target != queued.target {
        return false;
    }
    match (&terminal.action, &queued.action) {
        (
            PersonalFabricAction::RemoteAudioStop { session_id, .. }
            | PersonalFabricAction::RemoteAudioCancel { session_id },
            PersonalFabricAction::RemoteAudioFrame {
                session_id: queued_session,
                ..
            }
            | PersonalFabricAction::RemoteAudioKeepalive {
                session_id: queued_session,
                ..
            },
        ) => session_id == queued_session,
        (
            PersonalFabricAction::RemoteCaptureSignal {
                signal: RemoteCaptureSignal::Stop { session_id, .. },
            },
            PersonalFabricAction::RemoteCaptureChunk { chunk },
        ) => session_id == &chunk.session_id,
        _ => false,
    }
}

/// Apply backpressure without allowing untrusted bulk producers to evict
/// queued authority decisions. Distinct media sequences are never collapsed:
/// a dropped intermediate sequence stays visible to the runtime's sequence
/// checks (live audio conceals it as a bounded gap) instead of presenting
/// corrupted media as contiguous.
fn queue_personal_fabric_action<T: PersonalFabricEnvelopeRef>(
    queue: &mut VecDeque<T>,
    envelope: T,
) -> PersonalFabricQueueOutcome {
    if is_personal_fabric_bulk_data(&envelope.envelope().action) {
        if queue
            .iter()
            .any(|queued| is_duplicate_personal_fabric_data(queued.envelope(), envelope.envelope()))
        {
            return PersonalFabricQueueOutcome::Coalesced;
        }
        let data_count = queue
            .iter()
            .filter(|queued| is_personal_fabric_bulk_data(&queued.envelope().action))
            .count();
        if queue.len() >= PERSONAL_FABRIC_ACTION_QUEUE_CAPACITY
            || data_count >= PERSONAL_FABRIC_DATA_QUEUE_CAPACITY
        {
            return PersonalFabricQueueOutcome::Saturated;
        }
        queue.push_back(envelope);
        return PersonalFabricQueueOutcome::Queued;
    }

    queue.retain(|queued| !personal_fabric_terminal_matches_data(&envelope, queued));
    if queue.len() >= PERSONAL_FABRIC_ACTION_QUEUE_CAPACITY {
        let Some(oldest_data) = queue
            .iter()
            .position(|queued| is_personal_fabric_bulk_data(&queued.envelope().action))
        else {
            return PersonalFabricQueueOutcome::Saturated;
        };
        queue.remove(oldest_data);
    }
    queue.push_back(envelope);
    PersonalFabricQueueOutcome::Queued
}

fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[derive(Default)]
struct PersonalFabricReplayWindow {
    seen: std::collections::HashSet<[u8; 16]>,
    order: VecDeque<[u8; 16]>,
}

impl PersonalFabricReplayWindow {
    fn accept(&mut self, request_id: [u8; 16]) -> bool {
        if !self.seen.insert(request_id) {
            return false;
        }
        self.order.push_back(request_id);
        if self.order.len() > PERSONAL_FABRIC_ACTION_QUEUE_CAPACITY
            && let Some(expired) = self.order.pop_front()
        {
            self.seen.remove(&expired);
        }
        true
    }
}

/// A transport peer leaving one collaboration route.
///
/// `endpoint_id` is the authenticated transport identity; `user_public_key`
/// is present only after the peer completed identity admission. Keeping both
/// lets diagnostics distinguish an unnamed device from an admitted person's
/// second device without retaining any message contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerDeparture {
    /// Process-local monotonic marker assigned when the departure is recorded.
    /// Consumers use it as a cursor over the bounded history; wall-clock time
    /// is diagnostic only and must not decide whether an event is new.
    pub generation: u64,
    pub endpoint_id: iroh::EndpointId,
    /// Actor-minted incarnation of this endpoint on this exact topic. `None`
    /// is reserved for relay/session events that cannot authorize the
    /// personal fabric.
    pub membership_epoch: Option<crate::network::actor::MembershipEpoch>,
    pub session_handle: u64,
    pub route: CrdtRoute,
    pub cause: PeerDepartureCause,
    pub user_public_key: Option<[u8; 32]>,
    pub participant_name: Option<String>,
    pub observed_at: SystemTime,
}

/// Process-local proof that one certified same-person endpoint completed an
/// `IdentityHello` on its current actor membership. It is deliberately not
/// serializable; wire payloads can name devices but cannot mint route
/// authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AuthenticatedPersonalRoute {
    workspace_topic: u64,
    endpoint_id: iroh::EndpointId,
    membership_epoch: crate::network::actor::MembershipEpoch,
    user_public_key: [u8; 32],
    device: crate::shell::personal_continuity::DeviceId,
}

impl AuthenticatedPersonalRoute {
    pub(crate) fn device(&self) -> &crate::shell::personal_continuity::DeviceId {
        &self.device
    }

    #[cfg(test)]
    pub(crate) fn for_test(device: crate::shell::personal_continuity::DeviceId) -> Self {
        Self::for_test_epoch(device, 1)
    }

    #[cfg(test)]
    pub(crate) fn for_test_epoch(
        device: crate::shell::personal_continuity::DeviceId,
        epoch: u64,
    ) -> Self {
        Self {
            workspace_topic: 1,
            endpoint_id: iroh::SecretKey::from_bytes(&[0xE7; 32]).public(),
            membership_epoch: crate::network::actor::MembershipEpoch::for_test(epoch),
            user_public_key: [0xE8; 32],
            device,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReceivedPersonalFabricAction {
    pub(crate) envelope: crate::shell::device_fabric::PersonalFabricActionEnvelope,
    pub(crate) route: AuthenticatedPersonalRoute,
    /// [`fabric_monotonic_us`] when the bytes came off the connection, before
    /// validation and queueing, so clock probes exclude local queueing time.
    pub(crate) received_monotonic_us: u64,
}

/// This device's `CLOCK_MONOTONIC` in microseconds: the clock the device
/// fabric exchanges for peer clock estimation, and the one `PipeWire` schedules
/// audio against.
#[must_use]
pub fn fabric_monotonic_us() -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `time` is a valid, writable timespec for the duration of the
    // call, and CLOCK_MONOTONIC is always available on Linux.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut time) };
    if status != 0 {
        return 0;
    }
    u64::try_from(time.tv_sec)
        .unwrap_or_default()
        .saturating_mul(1_000_000)
        .saturating_add(u64::try_from(time.tv_nsec).unwrap_or_default() / 1_000)
}

impl PersonalFabricEnvelopeRef for ReceivedPersonalFabricAction {
    fn envelope(&self) -> &crate::shell::device_fabric::PersonalFabricActionEnvelope {
        &self.envelope
    }
}

/// Shared, bounded departure history. It is intentionally independent of
/// the actor event channel so a reader exit can still be recorded if a later
/// roster or identity cleanup step cannot acquire its own lock.
#[derive(Default)]
struct PeerDepartureHistoryState {
    generation: u64,
    entries: VecDeque<PeerDeparture>,
}

#[derive(Default)]
pub(crate) struct PeerDepartureHistory {
    state: Mutex<PeerDepartureHistoryState>,
}

impl PeerDepartureHistory {
    fn record(&self, mut departure: PeerDeparture) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.generation = state.generation.saturating_add(1).max(1);
        departure.generation = state.generation;
        if state.entries.len() >= PEER_DEPARTURE_HISTORY_CAPACITY {
            state.entries.pop_front();
        }
        state.entries.push_back(departure);
    }

    pub(crate) fn snapshot(&self) -> Vec<PeerDeparture> {
        self.state
            .lock()
            .map(|state| state.entries.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Departures recorded so far. Equal values read at two moments mean
    /// [`Self::snapshot`] did not change in between.
    pub(crate) fn generation(&self) -> u64 {
        self.state
            .lock()
            .map(|state| state.generation)
            .unwrap_or_default()
    }
}

/// Stable transport/host shard for a durable room. Local workspace handles are
/// deliberately not used: they differ between devices.
pub fn room_topic_id(room: RoomId) -> u64 {
    if room.is_nil() {
        return DEFAULT_WS_TOPIC;
    }
    // `room.to_string()` is `Uuid::to_string()`, which `room_workspace_id`
    // can always re-parse, so the `Err` path is unreachable in practice.
    // Fall back to the "no room identity" topic rather than panic on this
    // hot path — `room_workspace_id` never returns 0 on success, so a real
    // topic id can't collide with the fallback.
    tilo_relay_protocol::room_workspace_id(&room.to_string()).unwrap_or(DEFAULT_WS_TOPIC)
}

/// Stable private shard shared only by certified devices rooted in the same
/// person identity. The full root key is also carried in and authenticated on
/// every message; this 64-bit value is routing, never authority.
pub fn personal_continuity_topic_id(user_public_key: &[u8; 32]) -> u64 {
    tilo_relay_protocol::personal_continuity_workspace_id(user_public_key)
}

/// Topics a machine dials on startup and on home-peer refresh.
///
/// Local rooms are included, and so is the person topic, so a device with no
/// rooms yet still reaches the peers enrolled for that identity.
fn relay_topics_for_home_peers(
    rooms: impl IntoIterator<Item = RoomId>,
    user: Option<[u8; 32]>,
) -> std::collections::BTreeSet<u64> {
    let mut topics = rooms
        .into_iter()
        .map(room_topic_id)
        .collect::<std::collections::BTreeSet<_>>();
    if let Some(user) = user {
        topics.insert(personal_continuity_topic_id(&user));
    }
    topics
}

/// Default on-disk path of the machine's device key, from which the
/// shell derives its Iroh identity. The FUSE mount reads the same
/// file, so one device has one identity rather than one per subsystem.
pub const DEFAULT_KEYPAIR_PATH: &str = crate::network::behaviour::DEVICE_KEY_PATH;

/// Resolve one network configuration value; an empty value counts as absent.
fn network_env(name: &'static str) -> Option<(&'static str, std::ffi::OsString)> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(|value| (name, value))
}

fn network_env_string(name: &'static str) -> Option<(&'static str, String)> {
    let (name, value) = network_env(name)?;
    if let Ok(value) = value.into_string() {
        Some((name, value))
    } else {
        tracing::warn!(config = name, "network configuration must be valid UTF-8");
        None
    }
}

/// The active collaboration identity tree unless `TILO_PEER_KEY_PATH` names
/// somewhere else.
/// Full-system sessions resolve to [`DEFAULT_KEYPAIR_PATH`];
/// host-installed sessions select an XDG-owned tree at process startup. The
/// override is what makes two sessions on one machine two
/// *peers*: the key is the shell's network identity, so two processes
/// reading one key file are one peer talking to itself.
pub fn peer_key_path() -> std::path::PathBuf {
    network_env("TILO_PEER_KEY_PATH").map_or_else(
        || crate::firstboot::collaboration_identity_paths().device_privkey,
        |(_, value)| std::path::PathBuf::from(value),
    )
}

/// Where this session keeps what peer sync learns and must remember: the
/// person's pending invitations, device fabric, workspace list and
/// continuity. It belongs to the person's session, not to the device key's
/// directory: on Tilo OS that directory is root-owned and only readable by
/// the identity group, so the session that reads the key cannot replace it.
///
/// # Errors
///
/// When the session has no per-user state directory.
pub fn peer_state_dir() -> std::io::Result<std::path::PathBuf> {
    tilo_paths::state_dir()
        .map(|state| state.join("tilo").join("peer-sync"))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no per-user state directory for peer sync",
            )
        })
}

fn parse_host_addr(value: &str) -> Option<iroh::EndpointAddr> {
    serde_json::from_str(value).ok().or_else(|| {
        value
            .parse::<iroh::EndpointId>()
            .ok()
            .map(iroh::EndpointAddr::new)
    })
}

/// Which peers a session's endpoint can reach.
#[derive(Clone, Copy)]
enum Reach {
    /// Any, through the public relays and discovery. The relays wait until
    /// something needs this device reachable
    /// ([`crate::network::reachability`]).
    Internet,
    /// Only this machine's (tests).
    #[cfg(test)]
    Loopback,
    /// Only this machine's, holding relays nobody listens on back the way
    /// [`Self::Internet`] holds back the public ones (tests of the
    /// deferral).
    #[cfg(test)]
    LoopbackHoldingRelays,
}

/// Why someone could need to dial this device as its session starts, from
/// what is already recorded: a room it shares (one it joined, a peer it
/// named or admitted, a relay-governed or continuation room), an invite
/// still open, or another of the person's devices in its fabric (they
/// continue, sync and disk-unlock through it). Hosts, enrolled home peers
/// and the person's paired devices are dialled at start, which wakes the
/// relays in the actor, as does every relationship that begins later.
fn needs_reachability(
    tree: &WorkspaceTree,
    local_endpoint: &str,
    open_invitations: bool,
    other_devices: bool,
) -> Option<&'static str> {
    // The inviter names and certifies itself in its own rooms, so only an
    // endpoint other than this one is a peer.
    let someone_else = |peer: &String| peer != local_endpoint;
    if tree
        .room_authority
        .values()
        .any(|authority| *authority != crate::shell::state::RoomAuthority::Local)
    {
        Some("a room this device joined")
    } else if tree
        .room_peer_names
        .values()
        .any(|peers| peers.keys().any(someone_else))
        || tree
            .room_peer_identities
            .values()
            .any(|peers| peers.keys().any(someone_else))
    {
        Some("a room shared with a peer")
    } else if !tree.native_authority.is_empty() {
        Some("a relay-governed room")
    } else if !tree.continuation_rooms.is_empty() {
        Some("a room continued from another device")
    } else if open_invitations {
        Some("an open invitation")
    } else if other_devices {
        Some("another of the person's devices")
    } else {
        None
    }
}

fn configured_host_addrs() -> Vec<iroh::EndpointAddr> {
    let mut hosts = Vec::new();
    if let Some((name, value)) = network_env_string("TILO_COLLAB_HOSTS") {
        match serde_json::from_str::<Vec<iroh::EndpointAddr>>(&value) {
            Ok(parsed) => hosts.extend(parsed),
            Err(error) => {
                tracing::warn!(config = name, %error, "collaboration hosts must be a JSON array of Iroh endpoint addresses");
            }
        }
    }
    if let Some((name, value)) = network_env_string("TILO_COLLAB_HOST") {
        if let Some(addr) = parse_host_addr(&value) {
            hosts.push(addr);
        } else {
            tracing::warn!(
                config = name,
                "collaboration host is not a valid Iroh endpoint address or id"
            );
        }
    }
    hosts.sort_by_key(|addr| addr.id.to_string());
    hosts.dedup_by_key(|addr| addr.id);
    hosts
}

/// Whether `TILO_COLLAB_HOST` or `TILO_COLLAB_HOSTS` names a workspace host.
pub fn collab_host_configured() -> bool {
    !configured_host_addrs().is_empty()
}

const MAX_HOME_PEER_REGISTRY_BYTES: u64 = 1024 * 1024;
const MAX_HOME_PEERS: usize = 128;
const MAX_HOME_PEER_LABEL_BYTES: usize = 80;
const MAX_HOME_PEER_WORKSPACES: usize = 4096;

fn home_peer_registry_path() -> PathBuf {
    network_env("TILO_HOME_PEERS_PATH").map_or_else(
        || {
            let paths = crate::firstboot::collaboration_identity_paths();
            tilo_user_identity::home_peer_registry_path(
                paths
                    .user_pubkey
                    .parent()
                    .and_then(std::path::Path::parent)
                    .unwrap_or_else(|| std::path::Path::new("/var/lib/tilo")),
            )
        },
        |(_, path)| PathBuf::from(path),
    )
}

fn decode_lower_hex_32(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return None;
    }
    hex::decode(value).ok()?.try_into().ok()
}

/// Read home peers only from the locally-rooted enrollment registry. Entries
/// are additionally constrained to this user's certified root and one of the
/// shell's current relay workspace shards; a copied registry must not make a
/// shell dial another person's host. The relay service key is intentionally
/// distinct from the device key, preventing a service address from claiming
/// ordinary peer identity.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RegisteredHomePeer {
    addr: iroh::EndpointAddr,
    expectation: crate::network::actor::HomePeerExpectation,
}

fn registered_home_peer_addrs(
    local_user: Option<[u8; 32]>,
    relay_workspace_ids: &std::collections::BTreeSet<u64>,
) -> Result<Vec<RegisteredHomePeer>, String> {
    let Some(local_user) = local_user else {
        return Ok(Vec::new());
    };
    let path = home_peer_registry_path();
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("could not inspect home-peer registry: {error}")),
    };
    if metadata.len() > MAX_HOME_PEER_REGISTRY_BYTES {
        tracing::warn!(path = %path.display(), "home-peer registry exceeds its bounded schema");
        return Err("home-peer registry exceeds its bounded schema".into());
    }
    let registrations: Vec<tilo_relay_protocol::HomePeerRegistration> =
        match std::fs::read(&path).ok().and_then(|bytes| {
            serde_json::from_slice::<Vec<tilo_relay_protocol::HomePeerRegistration>>(&bytes).ok()
        }) {
            Some(registrations) if registrations.len() <= MAX_HOME_PEERS => registrations,
            _ => {
                tracing::warn!(path = %path.display(), "home-peer registry is invalid");
                return Err("home-peer registry is invalid".into());
            }
        };
    let mut addrs = Vec::new();
    for registration in registrations {
        if registration.label.is_empty()
            || registration.label.len() > MAX_HOME_PEER_LABEL_BYTES
            || registration.relay_workspace_ids.len() > MAX_HOME_PEER_WORKSPACES
            || !registration
                .relay_workspace_ids
                .iter()
                .any(|workspace| relay_workspace_ids.contains(workspace))
        {
            continue;
        }
        // Validate the enrolled device id too, even though it is not dialled:
        // it binds replacement/upsert identity in the signed registry.
        if decode_lower_hex_32(&registration.device_node_id).is_none()
            || decode_lower_hex_32(&registration.user_public_key) != Some(local_user)
        {
            continue;
        }
        let Some(relay_id) = decode_lower_hex_32(&registration.relay_service_node_id)
            .and_then(|bytes| iroh::EndpointId::from_bytes(&bytes).ok())
        else {
            continue;
        };
        let Some(device_public_key) = decode_lower_hex_32(&registration.device_node_id) else {
            continue;
        };
        let addr = registration
            .relay_endpoint
            .filter(|endpoint| endpoint.id == relay_id && !endpoint.is_empty())
            // Legacy registrations contain only a service id. Keep the
            // address-lookup fallback for them, but prefer the persisted
            // direct/relay hint when one was transferred during enrollment.
            .unwrap_or_else(|| iroh::EndpointAddr::new(relay_id));
        addrs.push(RegisteredHomePeer {
            addr,
            expectation: crate::network::actor::HomePeerExpectation {
                user_public_key: local_user,
                device_public_key,
            },
        });
    }
    addrs.sort_by_key(|host| host.addr.id.to_string());
    addrs.dedup_by_key(|host| host.addr.id);
    Ok(addrs)
}

/// Device endpoint ids this machine should dial directly.
///
/// These are ordinary peers of the same person, not relays. A relay handshake
/// rejects a certificate that cannot act as a host, so a setup laptop is
/// reached on its device endpoint instead.
fn personal_device_peer_addrs(
    path: &Path,
    local_device: Option<[u8; 32]>,
) -> Vec<iroh::EndpointAddr> {
    enrolled_endpoints(&read_personal_device_peers(path), local_device)
}

/// The dialable endpoints among `devices`, other than `local_device`.
fn enrolled_endpoints(
    devices: &[[u8; 32]],
    local_device: Option<[u8; 32]>,
) -> Vec<iroh::EndpointAddr> {
    devices
        .iter()
        .filter(|device| local_device != Some(**device))
        .filter_map(|device| iroh::EndpointId::from_bytes(device).ok())
        .map(iroh::EndpointAddr::new)
        .collect()
}

/// Every valid endpoint key in the bounded enrolment file at `path`. Reads
/// the disk: hot paths go through [`ShellNetwork::enrolled_personal_devices`].
fn read_personal_device_peers(path: &Path) -> Vec<[u8; 32]> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    if bytes.len() > 64 * 1024 {
        tracing::warn!(path = %path.display(), "personal device list exceeds its bound");
        return Vec::new();
    }
    let Ok(ids) = serde_json::from_slice::<Vec<String>>(&bytes) else {
        tracing::warn!(path = %path.display(), "personal device list is invalid");
        return Vec::new();
    };
    if ids.len() > MAX_HOME_PEERS {
        tracing::warn!(path = %path.display(), "personal device list exceeds its bound");
        return Vec::new();
    }
    ids.into_iter()
        .filter_map(|id| {
            let bytes = decode_lower_hex_32(&id)?;
            iroh::EndpointId::from_bytes(&bytes).ok()?;
            Some(bytes)
        })
        .collect()
}

fn personal_device_peer_path() -> PathBuf {
    home_peer_registry_path().with_file_name("personal-device-peers.json")
}

/// The roots, other than its own, that certified this device for a workspace
/// of theirs: each one's person room is routed to that root's devices.
fn workspace_person_roots(identity: &HostIdentity) -> Vec<[u8; 32]> {
    let mut roots: Vec<[u8; 32]> = identity
        .workspace_device_certs
        .values()
        .map(|cert| cert.user_public_key)
        .filter(|root| *root != identity.device_cert.user_public_key)
        .collect();
    roots.sort_unstable();
    roots.dedup();
    roots
}

/// The role a device of the person acts with in their own room: its owner,
/// unless it is a foreign device, which the foreign-device cap holds below
/// governing even there.
fn person_room_role(foreign: bool) -> collab_types::Role {
    if foreign {
        collab_types::foreign_device::foreign_device_role(collab_types::Role::Owner, &[], 0)
    } else {
        collab_types::Role::Owner
    }
}

fn personal_room_ids(path: &Path) -> Vec<RoomId> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    if bytes.len() > 64 * 1024 {
        return Vec::new();
    }
    let Ok(ids) = serde_json::from_slice::<Vec<String>>(&bytes) else {
        return Vec::new();
    };
    if ids.len() > 256 {
        return Vec::new();
    }
    ids.into_iter()
        .filter_map(|id| RoomId::parse(&id))
        .collect()
}

/// How often a waking shell re-derives who its authenticated peers are (so a
/// revoked or expired device drops out) and checks its own continuity
/// snapshot for a change to publish. Local work; nothing goes out unless
/// something changed.
const SYNC_KICK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
/// How long after a room changed, here or by a peer's batch, a shell asks
/// its rooms for anything it is missing. Local changes are pushed the tick
/// they land ([`ShellNetwork::push_room_changes`]); this pull, made once a
/// burst of activity settles, is the repair path for a push a peer missed:
/// a peer whose request names changes it lacks asks back
/// ([`crate::network::replica::WorkspaceTreeReplica`]). A peer that comes
/// back is pulled when it joins.
const SYNC_REPAIR_DELAY: std::time::Duration = std::time::Duration::from_secs(2);
/// The longest a shell goes without that pull while nothing changes: the
/// safety net under the repair pull and the join pull, not a heartbeat. An
/// idle, converged desktop asks once per this long.
const SYNC_SAFETY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);
const HOME_PEER_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
/// Upper bound between full projections of every room while nothing the
/// push trigger watches has moved. Most local edits move the desktop
/// document's heads and are pushed the same tick; this catches the few that
/// only reach a room document through projection (a record index, say).
const ROOM_PUSH_FALLBACK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// What [`ShellNetwork::push_room_changes`] last saw and last announced.
#[derive(Default)]
struct RoomPush {
    /// Desktop-document heads, peer-merge epoch and external revision at
    /// the last full check: when none moved, no room can have changed
    /// locally, so a tick costs one heads read.
    trigger: Option<(Vec<automerge::ChangeHash>, u64, u64)>,
    /// When every room was last projected.
    last_full: Option<std::time::Instant>,
    /// Each room's document heads as last announced (or first seen, which
    /// announces nothing: a peer that lacks older history pulls it).
    announced: std::collections::HashMap<RoomId, Vec<automerge::ChangeHash>>,
}
/// Keep transport bursts from monopolizing the compositor thread. The actor's
/// channel is bounded, so leaving events queued simply applies backpressure
/// until the next frame instead of losing ordering or data.
const MAX_NET_EVENTS_PER_TICK: usize = 8;

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct PendingInvitation {
    pub id: [u8; 32],
    pub room: crate::shell::state::RoomId,
    pub name: String,
    pub role: crate::network::invite::InviteRole,
    /// Original signed bearer, kept in the private mode-0600 inventory so the
    /// owner can resend the same invitation without minting another grant.
    pub token: String,
    /// When present, redemption is limited to this exact person identity.
    #[serde(default)]
    pub recipient_user_public_key: Option<[u8; 32]>,
    pub expiry_unix_seconds: u64,
}

impl PendingInvitation {
    #[must_use]
    pub fn is_expired(&self, now_unix_seconds: u64) -> bool {
        self.expiry_unix_seconds < now_unix_seconds
    }

    #[must_use]
    pub fn id_hex(&self) -> String {
        hex::encode(self.id)
    }
}

/// Nanoseconds since the Unix epoch, failing closed to `u64::MAX` (every
/// certificate expired) when the clock precedes the epoch.
fn now_unix_nanos() -> u64 {
    tilo_clock::try_unix_now_ns().unwrap_or(u64::MAX)
}

fn pending_invites_path(state_dir: &Path) -> PathBuf {
    state_dir.join("pending-invitations.json")
}

fn personal_continuity_path(state_dir: &Path) -> PathBuf {
    state_dir.join("personal-continuity.postcard")
}

/// The governing endpoints of rooms `tree` holds now but did not hold as
/// `before`, for rooms someone else governs.
fn governors_before_and_after(
    tree: &WorkspaceTreeHandle,
    before: &[RoomId],
) -> Vec<iroh::EndpointId> {
    let tree = tree.read();
    tree.desktop
        .rooms()
        .into_iter()
        .filter(|room| !before.contains(room))
        .filter_map(|room| match tree.room_authority(room) {
            crate::shell::state::RoomAuthority::Peer(origin) => origin
                .as_str()
                .strip_prefix("iroh:")
                .and_then(|endpoint| endpoint.parse().ok()),
            _ => None,
        })
        .collect()
}

/// Tell the workspace menus which of the person's devices each of their
/// workspaces shows on.
fn refresh_device_choices(
    tree: &mut crate::shell::state::WorkspaceTree,
    workspaces: &crate::shell::personal_workspaces::PersonalWorkspaces,
) {
    let choices: std::collections::HashMap<_, _> = tree
        .desktop
        .workspaces
        .iter()
        .map(|workspace| (workspace.room, workspaces.device_choices(workspace.room)))
        .collect();
    if tree.personal_device_choices != choices {
        tree.personal_device_choices = choices;
        tree.desktop.external_revision = tree.desktop.external_revision.wrapping_add(1);
    }
}

fn personal_workspaces_path(state_dir: &Path) -> PathBuf {
    state_dir.join("personal-workspaces.postcard")
}

/// How often a device restates its workspace list when nothing changed, so
/// a snapshot lost in transit is repaired without waiting for the next edit.
/// A change goes out when it lands and the list goes to each of the
/// person's devices that joins, so this is the safety net alone, at the pace
/// of room repair ([`SYNC_SAFETY_INTERVAL`]): an idle desktop is quiet.
const PERSONAL_WORKSPACES_RESTATE_INTERVAL: std::time::Duration = SYNC_SAFETY_INTERVAL;

fn device_fabric_path(state_dir: &Path) -> PathBuf {
    state_dir.join("personal-device-fabric.postcard")
}

fn device_fabric_backup_path(path: &Path) -> PathBuf {
    path.with_extension("postcard.backup")
}

fn load_device_fabric(path: &Path) -> crate::shell::device_fabric::DeviceFabric {
    [path.to_path_buf(), device_fabric_backup_path(path)]
        .into_iter()
        .find_map(|candidate| {
            let bytes = std::fs::read(candidate).ok()?;
            crate::shell::device_fabric::DeviceFabric::decode_checkpoint(&bytes).ok()
        })
        .unwrap_or_default()
}

fn persist_device_fabric(
    path: &Path,
    fabric: &crate::shell::device_fabric::DeviceFabric,
) -> Result<(), String> {
    let bytes = fabric
        .encode_checkpoint()
        .map_err(|error| format!("encode checkpoint: {error}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let prior = std::fs::read(path)
        .ok()
        .filter(|prior| crate::shell::device_fabric::DeviceFabric::decode_checkpoint(prior).is_ok())
        .unwrap_or_else(|| bytes.clone());
    let backup = device_fabric_backup_path(path);
    tilo_files::native::atomic::write_with_mode(&backup, &prior, 0o600)
        .map_err(|error| format!("write {}: {error}", backup.display()))?;
    tilo_files::native::atomic::write_with_mode(path, &bytes, 0o600)
        .map_err(|error| format!("write {}: {error}", path.display()))
}

#[derive(Serialize, Deserialize)]
struct PersistedPersonalContinuity {
    counter: u64,
    continuity: crate::shell::personal_continuity::PersonalContinuity,
}

fn decode_persisted_personal_continuity(bytes: &[u8]) -> Option<PersistedPersonalContinuity> {
    let mut current = postcard::from_bytes::<PersistedPersonalContinuity>(bytes).ok()?;
    current.counter = current.continuity.recover_counter(current.counter);
    Some(current)
}

fn personal_continuity_backup_path(path: &Path) -> PathBuf {
    path.with_extension("postcard.backup")
}

fn purge_personal_continuity_store(path: &Path) {
    for candidate in [path.to_path_buf(), personal_continuity_backup_path(path)] {
        match std::fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(%error, path = %candidate.display(), "personal continuity purge failed");
            }
        }
    }
}

fn load_personal_continuity(path: &Path) -> PersistedPersonalContinuity {
    for (index, candidate) in [path.to_path_buf(), personal_continuity_backup_path(path)]
        .into_iter()
        .enumerate()
    {
        if let Ok(bytes) = std::fs::read(&candidate)
            && let Some(mut value) = decode_persisted_personal_continuity(&bytes)
        {
            if index != 0 {
                // The primary may have been published before it was damaged,
                // while this backup is one revision behind. Reserve another
                // Lamport value so the next capture cannot reuse a counter a
                // peer already associates with different bytes.
                value.counter = value.counter.saturating_add(1);
            }
            return value;
        }
    }
    PersistedPersonalContinuity {
        counter: 0,
        continuity: crate::shell::personal_continuity::PersonalContinuity::default(),
    }
}

fn persist_personal_continuity(path: &Path, value: &PersistedPersonalContinuity) -> bool {
    let Ok(bytes) = postcard::to_allocvec(value) else {
        return false;
    };
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(%error, path = %path.display(), "personal continuity directory unavailable");
        return false;
    }
    let current = std::fs::read(path).ok();
    // A quiet tree asks to persist the same state on every sync round. The
    // primary already holds it, and rewriting it costs four fsyncs (file and
    // directory, for backup and primary) on the compositor thread.
    if current.as_deref() == Some(bytes.as_slice()) {
        return true;
    }
    // Establish the recovery point before replacing the primary. Refusing to
    // advance when this fails keeps the backup exactly one committed primary
    // behind (or equal on first write), which makes the reserved-counter
    // recovery rule above sound even across repeated failures.
    let prior = current
        .filter(|prior| decode_persisted_personal_continuity(prior).is_some())
        .unwrap_or_else(|| bytes.clone());
    if let Err(error) = tilo_files::native::atomic::write_with_mode(
        &personal_continuity_backup_path(path),
        &prior,
        0o600,
    ) {
        tracing::warn!(%error, path = %path.display(), "personal continuity backup failed");
        return false;
    }
    if tilo_files::native::atomic::write_with_mode(path, &bytes, 0o600).is_err() {
        tracing::warn!(path = %path.display(), "personal continuity persistence failed");
        return false;
    }
    true
}

fn load_pending_invitations(path: &Path) -> Vec<PendingInvitation> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        tracing::warn!(%error, path = %path.display(), "ignoring invalid pending invitation store");
        Vec::new()
    })
}

fn persist_pending_invitations(
    path: &Path,
    invitations: &std::collections::HashMap<[u8; 32], PendingInvitation>,
) {
    let Some(parent) = path.parent() else {
        return;
    };
    if let Err(error) = std::fs::create_dir_all(parent) {
        tracing::warn!(%error, path = %path.display(), "could not create pending invitation directory");
        return;
    }
    let mut rows: Vec<_> = invitations.values().cloned().collect();
    rows.sort_by_key(|invite| (invite.expiry_unix_seconds, invite.id));
    let Ok(bytes) = serde_json::to_vec_pretty(&rows) else {
        return;
    };
    if let Err(error) = tilo_files::native::atomic::write_with_mode(path, &bytes, 0o600) {
        tracing::warn!(%error, path = %path.display(), "could not persist pending invitations");
    }
}

/// Non-secret metadata shown in collaboration settings for a pending invite.
/// `invite_id` is a one-way BLAKE3 digest, never the redeemable bearer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingInviteSummary {
    pub invite_id: String,
    pub invitee: String,
    pub role: crate::network::invite::InviteRole,
    pub recipient_bound: bool,
    pub expiry_unix_seconds: u64,
}

/// Shell-side handle for the peer-sync subsystem. Owns the tokio
/// runtime hosting the [`Actor`], the per-workspace
/// [`WorkspaceTreeReplica`], and the actor↔replica event channel.
pub struct ShellNetwork {
    native_enrollment: native_enrollment::State,
    hosted_topology: hosted_topology::State,
    relay_rooms: relay_rooms::State,
    record_content: record_content::State,
    /// Send commands into the actor (e.g. publish a `NeighbourMsg`).
    pub cmd_tx: mpsc::Sender<NetCmd>,
    /// Shared replica owning the tree-side reactions to `NetEvent::Msg`;
    /// `Arc<Mutex<_>>` so other tasks can share a clone.
    pub replica: Arc<Mutex<WorkspaceTreeReplica>>,
    /// Sender half of the actor→replica event channel, cloned into
    /// `NetCmd::JoinWorkspace`.
    pub event_tx: mpsc::Sender<NetEvent>,
    /// Receiver half of the actor→replica event channel, drained on
    /// each [`Self::tick`]. `Mutex`-wrapped so `tick` can take `&self`
    /// while shared behind `Arc<ShellNetwork>`.
    pub event_rx: Mutex<mpsc::Receiver<NetEvent>>,
    /// Wakes the session's event loop when an event arrives, so an idle
    /// compositor still folds in what peers and the room's host send.
    event_wake: Arc<std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>>,
    /// Topics already subscribed in the actor. Avoids sending a Join command on
    /// every app delta while still allowing rooms accepted after startup.
    joined_topics: Mutex<std::collections::HashSet<u64>>,
    /// Transport-authenticated direct neighbours, scoped by workspace.
    /// Populated only after `IdentityHello` admission succeeds.
    workspace_control_direct: Mutex<std::collections::HashSet<(u64, iroh::EndpointId)>>,
    /// Authenticated host sessions whose ephemeral presence plane is ready.
    workspace_control_relays: Mutex<std::collections::HashSet<(u64, iroh::EndpointId)>>,
    /// Tokio runtime hosting the actor. `Option` so `drop` can `take`
    /// it and shut down without blocking the test thread.
    runtime: Option<tokio::runtime::Runtime>,
    /// Cached hash of the local iroh endpoint id, stamped on every
    /// outbound `NeighbourMsg::PresenceMoved`. Computed once at spawn.
    local_peer_hash: u64,
    /// This device's iroh identity. An invite is a signed statement
    /// by the inviting owner — [`Self::issue_invite`] signs with it.
    keypair: iroh::SecretKey,
    /// Root-signed certificate matching `keypair`, when the machine has been
    /// provisioned through the identity layer.
    host_identity: Option<HostIdentity>,
    /// Endpoint id derived from [`Self::keypair`].
    local_peer_id: iroh::EndpointId,
    /// Current direct/relay dialing addresses reported by iroh.
    listen_addrs: Arc<Mutex<Vec<iroh::EndpointAddr>>>,
    /// The endpoint's relays, held back until something needs this device
    /// reachable ([`crate::network::reachability`]).
    reachability: crate::network::Reachability,
    /// Always-on hosts included in newly issued invites.
    host_addrs: Mutex<Vec<iroh::EndpointAddr>>,
    /// Home-peer ids sourced specifically from the mutable enrollment
    /// registry. This lets refresh remove revoked/decommissioned entries
    /// without disturbing hosts carried by workspace invitations.
    registered_home_peer_ids: Mutex<std::collections::HashSet<iroh::EndpointId>>,
    /// Direct peers already seen on a live topic. Startup and refresh keep
    /// dialing the others, so a peer that is still offline when Wi-Fi comes up
    /// is tried again.
    dialed_direct_peers: Mutex<std::collections::HashSet<iroh::EndpointId>>,
    /// Topics on which a direct peer has actually appeared. A remote action
    /// sent before that is dropped by the transport, so it is not attempted.
    live_room_peers: Mutex<std::collections::HashSet<(u64, iroh::EndpointId)>>,
    /// Certified device keys eligible to decrypt personal secrets. Routing
    /// uses the deliberately distinct relay service ids above.
    registered_home_device_ids: Mutex<std::collections::HashSet<[u8; 32]>>,
    /// `personal-device-peers.json` and the certified recipients derived from
    /// it, re-validated at most once per recheck interval instead of read per
    /// outgoing message. `registered_home_device_ids` may be locked while
    /// holding this, never the reverse.
    personal_device_peers: Mutex<personal_device_peers::PersonalDevicePeers>,
    /// Last bounded scan of the enrollment registry. A paired home peer may
    /// be approved after this shell started, so startup-only loading is not
    /// sufficient.
    last_home_peer_refresh: Mutex<Option<std::time::Instant>>,
    /// When [`Self::kick_periodic_sync`] last did its local upkeep. `None`
    /// until the first tick.
    last_sync_kick: Mutex<Option<std::time::Instant>>,
    /// When [`Self::kick_periodic_sync`] next asks the rooms for what this
    /// shell is missing. `None` is now: the first frame after startup asks.
    next_sync_pull: Mutex<Option<std::time::Instant>>,
    /// See [`Self::peers_joined_epoch`].
    peers_joined: std::sync::atomic::AtomicU64,
    /// Push side of room replication: see [`Self::push_room_changes`].
    room_push: Mutex<RoomPush>,
    /// Private, same-person continuation state. It is intentionally separate
    /// from `replica`, whose documents are visible to room collaborators.
    personal_continuity: Mutex<crate::shell::personal_continuity::PersonalContinuity>,
    personal_continuity_user: Option<[u8; 32]>,
    /// The other roots that certified this device for a workspace of theirs
    /// (its workspace identities): each has a person room of its own, routed
    /// to that root's other devices only.
    workspace_person_roots: Vec<[u8; 32]>,
    personal_continuity_device: Option<crate::shell::personal_continuity::DeviceId>,
    personal_continuity_counter: Mutex<u64>,
    personal_continuity_fingerprint: Mutex<Option<[u8; 32]>>,
    personal_continuity_path: Option<PathBuf>,
    /// The person's workspace list, shared by all of their devices. `None`
    /// on a device the identity layer has not certified for a person.
    personal_workspaces: Option<Mutex<crate::shell::personal_workspaces::PersonalWorkspaces>>,
    /// When this device last sent its workspace list unprompted.
    last_personal_workspaces_snapshot: Mutex<Option<std::time::Instant>>,
    /// When each of this person's other devices last moved focus, for the
    /// "continue from another device" offer.
    personal_focus_recency: Mutex<crate::shell::continuity_suggestion::FocusRecency>,
    #[cfg(test)]
    continuity_policy_override: Option<crate::shell::continuity_policy::ContinuityPolicySnapshot>,
    /// Convergent same-person device state, persisted separately from app
    /// continuation so transport upgrades cannot regress existing handoff.
    device_fabric: Mutex<crate::shell::device_fabric::DeviceFabric>,
    /// How often [`Self::device_fabric`] cloned and [`Self::with_device_fabric`]
    /// borrowed the fabric, so tests can hold per-frame paths to borrowing.
    #[cfg(test)]
    device_fabric_reads: DeviceFabricReads,
    device_fabric_path: Option<PathBuf>,
    /// Validated, ephemeral exact-target actions awaiting the shell runtime.
    personal_fabric_actions: Mutex<VecDeque<ReceivedPersonalFabricAction>>,
    /// Exact current same-person routes admitted by a certificate-bearing
    /// `IdentityHello`. Membership edges alone never populate this map.
    authenticated_personal_routes:
        Mutex<std::collections::HashMap<(u64, iroh::EndpointId), AuthenticatedPersonalRoute>>,
    /// Bounded session-local replay filter for action request ids.
    personal_fabric_action_replays: Mutex<PersonalFabricReplayWindow>,
    /// Verified transport-device chains received on workspace topics.
    peer_identities: PeerIdentityStore,
    /// Production is fail-closed until a peer presents `IdentityHello`.
    /// The in-process test bridge disables this because it has no Iroh actor.
    require_peer_identity: bool,
    /// Exact signed invitations awaiting a certificate-bearing person.
    /// Keying by the invite hash permits concurrent invites without letting
    /// one bearer receive another invite's name or role.
    pending_identity_admissions: Mutex<std::collections::HashMap<[u8; 32], PendingInvitation>>,
    pending_invitations_path: Option<PathBuf>,
    /// Invite ids this guest must attach to anti-entropy requests until the
    /// authority's first governance response proves redemption completed.
    pending_redemptions: Mutex<std::collections::HashMap<crate::shell::state::RoomId, [u8; 32]>>,
    /// Inbound presence updates pushed by [`Self::tick`], drained by the
    /// shell's frame-tick into `PresenceCtx::remote_cursors`.
    inbound_presence: Arc<Mutex<Vec<InboundPresence>>>,
    /// Inbound signed document envelopes pushed by [`Self::tick`]. The
    /// broker validates and decodes each before local fan-out.
    inbound_documents: Arc<Mutex<Vec<InboundDocument>>>,
    /// Wakes the broker's drain thread. An empty queue parks there.
    pub(crate) drain_wake: std::sync::Arc<crate::idle_wake::DrainPark>,
    /// Inbound channel governance pushed by [`Self::tick`], drained by the
    /// channel encryption service.
    inbound_channel_governance: Arc<Mutex<Vec<InboundChannelGovernance>>>,
    /// Local record ids behind the sealed wire names this device publishes
    /// and accepts; path-bearing record ids never leave the device.
    wire_records: Mutex<crate::network::wire_record::WireRecordIds>,
    /// Artifacts of records in the person's own room this session has held.
    /// Their history is answered to nobody on a room topic.
    person_artifacts: Mutex<std::collections::HashSet<collab_types::ArtifactId>>,
    /// Another of this person's devices (re)appeared on an authenticated
    /// route: the local apps holding person-room records owe it their
    /// snapshots, since it missed whatever was written while it was away.
    person_snapshots_wanted: Mutex<std::collections::BTreeSet<[u8; 32]>>,
    /// Received app-presence frames awaiting fan-out to local apps.
    inbound_app_presence: Arc<Mutex<Vec<InboundAppPresence>>>,
    /// This device's presence per room as workspace-host live frames.
    live_presence: Mutex<crate::shell::live_presence::Outbound>,
    /// Names live members asked to be drawn with, per room and handle:
    /// browsers and agents that are no peers of the room document.
    live_labels: Mutex<std::collections::HashMap<String, std::collections::BTreeMap<u64, String>>>,
    /// The colour each peer session said its person wears, by handle: the
    /// latest any presence plane carried ([`neighbour_wire::PresenceColour`],
    /// a live frame's participant colour).
    peer_colours: Mutex<std::collections::HashMap<u64, ui::tokens::PresenceHue>>,
    /// Advanced whenever [`Self::peer_colours`] changes, so the presence
    /// pump re-applies colours only after one did.
    peer_colours_epoch: std::sync::atomic::AtomicU64,
    /// Inbound cooperative-scrollback *requests*; the broker's drainer
    /// answers any it holds matching ops for.
    inbound_scrollback_request: Arc<Mutex<Vec<InboundScrollbackRequest>>>,
    /// Inbound cooperative-scrollback *responses*, forwarded by the
    /// broker's drainer to the local app that asked.
    inbound_scrollback_response: Arc<Mutex<Vec<InboundScrollbackResponse>>>,
    /// Remote-action requests from authenticated devices, answered by the
    /// broker's drainer.
    inbound_remote_actions: Arc<Mutex<Vec<crate::shell::remote_action_link::InboundRemoteAction>>>,
    /// Remote-action requests that arrived before their sender's identity
    /// hello. Delivered once that identity is accepted, dropped if it is
    /// refused or the peer leaves.
    held_remote_actions: Mutex<std::collections::VecDeque<HeldRemoteAction>>,
    /// Local callers waiting for a remote-action answer from a device.
    remote_action_replies: crate::shell::remote_action_link::ReplyWaiters,
    /// The suspend lease actions from the person's own devices renew.
    remote_use: Mutex<crate::shell::remote_use::RemoteUseLease>,
    /// Authenticated legacy-surface control, frame, and input messages.
    /// Frames are coalesced per surface when the render loop falls behind,
    /// keeping network jitter from turning into unbounded compositor memory.
    inbound_legacy: Arc<Mutex<std::collections::VecDeque<InboundLegacyEvent>>>,
    workspace_control_observers:
        Mutex<Vec<tokio::sync::mpsc::UnboundedSender<WorkspaceControlNetworkEvent>>>,
    /// Recent reader/transport exits, retained independently of mutable room
    /// state so cleanup cannot erase the evidence it needs to diagnose.
    peer_departures: Arc<PeerDepartureHistory>,
}

/// One remote-action request kept until its sender's identity is accepted.
struct HeldRemoteAction {
    workspace_topic: u64,
    peer: iroh::EndpointId,
    membership_epoch: Option<crate::network::actor::MembershipEpoch>,
    replayed: bool,
    frame: Vec<u8>,
}

const MAX_HELD_REMOTE_ACTIONS: usize = 8;

/// A live legacy-app message after transport identity and room capability
/// checks have passed. `workspace_topic` is retained so a viewer's input is
/// always sent back to the room the stream arrived on, even after a local
/// workspace switch.
#[derive(Debug, Clone)]
pub struct InboundLegacyEvent {
    pub workspace_topic: u64,
    /// Full authenticated sender identity. `None` only in the in-process test
    /// bridge, which has no Iroh transport.
    pub transport_peer: Option<iroh::EndpointId>,
    pub message: NeighbourMsg,
}

/// Authenticated network edges observed by workspace-control sessions. This
/// fan-out tap leaves the compositor's consuming legacy queue untouched.
#[derive(Debug, Clone)]
pub enum WorkspaceControlNetworkEvent {
    Message(InboundLegacyEvent),
    PeerLeft {
        workspace_topic: u64,
        peer: iroh::EndpointId,
    },
    PeerRevoked {
        workspace_topic: u64,
        peer: iroh::EndpointId,
    },
}

const LEGACY_INBOX_CAP: usize = 256;

/// One received app-presence frame, fanned to local apps by the
/// `collab_broker` drainer. Carries no record id: presence is fanned
/// at cluster scope.
#[derive(Debug, Clone)]
pub struct InboundAppPresence {
    /// Hashed Iroh peer id of the publisher. Unlike the CRDT queues
    /// this one is load-bearing: it becomes the `user_id` the
    /// receiving app attributes the caret and its colour to.
    pub peer: u64,
    pub app_id: String,
    pub cluster_id: String,
    pub presence: collab_types::PeerPresence,
}

/// One received signed, typed document envelope.
#[derive(Debug, Clone)]
pub struct InboundDocument {
    pub peer: u64,
    /// Empty (a peer too old to stamp one) addresses nobody.
    pub app_id: String,
    pub cluster_id: String,
    pub record_id: String,
    /// Live workspace role established from the authenticated peer's user
    /// certificate before this entry was queued.
    pub role: collab_types::Role,
    /// A workspace host replayed it from retained, host-ordered history, so
    /// its age says nothing about freshness.
    pub replayed: bool,
    /// Boxed to mirror the wire shape and bound the queue entry size —
    /// `SignedOp` is large (32-byte pubkey + 64-byte sig + payload + UUIDs).
    pub envelope: Box<collab_types::SignedOp>,
}

pub use tilo_action_broker::channel_encryption::InboundChannelGovernance;

/// One received cooperative-scrollback request. The broker's drainer
/// may answer with a `ScrollbackResponse` if its op log holds the range.
#[derive(Debug, Clone)]
pub struct InboundScrollbackRequest {
    /// The room topic it was asked on; only that room's history answers it.
    pub workspace_topic: u64,
    /// Hashed peer id of the asker; used to address the reply.
    pub requester: u64,
    pub artifact: collab_types::ArtifactId,
    /// Exclusive upper bound on `seq`.
    pub before_seq: u64,
    pub max: u16,
}

/// One received cooperative-scrollback response, translated by the
/// broker's drainer into a `CollabEvent::ScrollbackBatch`.
#[derive(Debug, Clone)]
pub struct InboundScrollbackResponse {
    pub responder: u64,
    pub artifact: collab_types::ArtifactId,
    pub ops: Vec<collab_types::SignedOp>,
}

/// One received cursor-presence update, merged by the shell's
/// frame drainer into `PresenceCtx`.
#[derive(Debug, Clone)]
pub struct InboundPresence {
    pub peer: u64,
    /// Who the publishing session says is sitting at it. Empty when it
    /// did not say; the drainer falls back to the session handle.
    pub user: String,
    /// Presence channel key (workspace, tab, widget path, …), JSON so
    /// the wire format stays opaque to the bridge.
    pub key: String,
    /// Pointer position in the space `key` names — for a person's desktop
    /// pointer, logical pixels from the corner of a tab's tile
    /// ([`crate::shell::peer_presence::PointerAnchor`]).
    pub point: (f64, f64),
    /// Wall-clock millisecond timestamp from the publisher.
    pub ts: u64,
}

fn lan_discovery_requested() -> bool {
    network_env("TILO_COLLAB_LAN_DISCOVERY").is_some_and(|(_, value)| value != "0")
}

/// Start mDNS LAN peer discovery when `TILO_COLLAB_LAN_DISCOVERY` requests
/// it, registering it with `endpoint`'s address lookup. Returns `None`
/// when discovery is disabled, disallowed by the env var, or the mDNS
/// service or address-lookup registry is unavailable — the caller then
/// simply skips [`spawn_lan_discovery_forwarding`].
fn init_lan_discovery(
    endpoint: &iroh::Endpoint,
) -> Option<iroh_mdns_address_lookup::MdnsAddressLookup> {
    if !lan_discovery_requested() {
        return None;
    }
    let mdns = match iroh_mdns_address_lookup::MdnsAddressLookup::builder()
        .service_name("tilo-collab-v1")
        .build(endpoint.id())
    {
        Ok(mdns) => mdns,
        Err(error) => {
            tracing::warn!(%error, "LAN discovery requested but could not start");
            return None;
        }
    };
    let Ok(address_lookup) = endpoint.address_lookup() else {
        tracing::warn!("LAN discovery requested but endpoint has no address lookup registry");
        return None;
    };
    address_lookup.add(mdns.clone());
    Some(mdns)
}

/// Forward every mDNS-discovered peer into `cmd_tx` as a
/// `NetCmd::Connect`, on a task spawned onto `runtime`.
fn spawn_lan_discovery_forwarding(
    runtime: &tokio::runtime::Runtime,
    mdns: iroh_mdns_address_lookup::MdnsAddressLookup,
    cmd_tx: &mpsc::Sender<NetCmd>,
) {
    let discovery_tx = cmd_tx.clone();
    runtime.spawn(async move {
        use futures_util::StreamExt;
        let mut events = mdns.subscribe().await;
        while let Some(event) = events.next().await {
            if let iroh_mdns_address_lookup::DiscoveryEvent::Discovered { endpoint_info, .. } =
                event
            {
                let _ = discovery_tx
                    .send(NetCmd::Connect {
                        addr: endpoint_info.into_endpoint_addr(),
                    })
                    .await;
            }
        }
    });
    tracing::info!(
        "peer-sync: privacy-sensitive LAN discovery enabled; advertising on _tilo-collab-v1._udp.local"
    );
}

impl ShellNetwork {
    /// Retained cryptographic custody for the same native broker's private
    /// account setup. No key or certificate is created by this projection.
    pub(crate) fn messaging_host_identity(&self) -> Option<HostIdentity> {
        self.host_identity.clone()
    }
    /// Build a tokio runtime, spawn the iroh [`Actor`], wrap the
    /// shared `WorkspaceTree` in a [`WorkspaceTreeReplica`], and
    /// return a handle the shell can use to drive sync. The keypair is
    /// loaded if it already exists, otherwise a fresh ed25519 key is
    /// generated and persisted.
    ///
    /// # Errors
    ///
    /// Returns an error if the tokio runtime fails to build, `state_dir`
    /// can't be created, the device keypair can't be loaded or generated at
    /// `keypair_path`, or the Iroh endpoint fails to bind.
    pub fn spawn(
        tree: WorkspaceTreeHandle,
        keypair_path: &std::path::Path,
        state_dir: &std::path::Path,
    ) -> std::io::Result<Self> {
        Self::spawn_with(
            tree,
            keypair_path,
            state_dir,
            Reach::Internet,
            host_identity_for_key,
        )
    }

    /// [`Self::spawn`] with this session's device key ([`peer_key_path`])
    /// and peer-sync state ([`peer_state_dir`]).
    ///
    /// # Errors
    ///
    /// As [`Self::spawn`], and when the session has no state directory.
    pub fn spawn_for_session(tree: WorkspaceTreeHandle) -> std::io::Result<Self> {
        Self::spawn(tree, &peer_key_path(), &peer_state_dir()?)
    }

    /// [`Self::spawn`] reaching peers as `reach` says, authenticating to
    /// hosts with the identity `identity` finds for the key.
    #[allow(
        clippy::too_many_lines,
        reason = "network startup keeps endpoint construction, identity recovery, and task wiring in a single failure-atomic boundary"
    )]
    fn spawn_with(
        tree: WorkspaceTreeHandle,
        keypair_path: &std::path::Path,
        state_dir: &std::path::Path,
        reach: Reach,
        identity: impl FnOnce(&iroh::SecretKey, &std::path::Path) -> Option<HostIdentity>,
    ) -> std::io::Result<Self> {
        // Peer sync owns its runtime so that dropping the network ends every
        // task it started. Two workers: the actor waits on sockets.
        let runtime = tilo_rt::daemon("tilo-net").workers(2).build()?;
        let _enter = runtime.enter();

        ensure_private_dir(state_dir).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("peer-sync state {}: {error}", state_dir.display()),
            )
        })?;
        let keypair = load_or_create_keypair(keypair_path)?;
        let local_peer_id = keypair.public();
        // Must use the same hash function `tick`'s receive path uses,
        // so a peer's own publishes hash to the same `peer` u64 on
        // both sides of the wire.
        let local_peer_hash = crate::network::behaviour::peer_liveness_handle(&keypair.public());
        let lookup = MemoryLookup::new();
        // The relay transport binds with an empty map: no home relay, net
        // report or port mapping until something needs this device
        // reachable, when `reachability` adds the held-back relays.
        let held_back = iroh::RelayMode::Custom(iroh::RelayMap::empty());
        let internet = matches!(reach, Reach::Internet);
        let (builder, deferred_relays) = match reach {
            Reach::Internet => (
                iroh::Endpoint::builder(presets::N0).relay_mode(held_back),
                iroh::endpoint::default_relay_mode().relay_map(),
            ),
            #[cfg(test)]
            Reach::Loopback => (
                iroh::Endpoint::builder(presets::Minimal)
                    .relay_mode(iroh::RelayMode::Disabled)
                    .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
                    .map_err(|e| std::io::Error::other(format!("iroh bind: {e}")))?,
                iroh::RelayMap::empty(),
            ),
            #[cfg(test)]
            Reach::LoopbackHoldingRelays => (
                iroh::Endpoint::builder(presets::Minimal)
                    .relay_mode(held_back)
                    .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
                    .map_err(|e| std::io::Error::other(format!("iroh bind: {e}")))?,
                reachability_tests::unused_relays(),
            ),
        };
        let endpoint = runtime
            .block_on(
                builder
                    .secret_key(keypair.clone())
                    .address_lookup(lookup.clone())
                    .transport_config(crate::network::actor::transport_config())
                    .bind(),
            )
            .map_err(|e| std::io::Error::other(format!("iroh bind: {e}")))?;
        let reachability = crate::network::Reachability::defer(
            &endpoint,
            &deferred_relays,
            runtime.handle().clone(),
        );
        let mdns = if internet {
            init_lan_discovery(&endpoint)
        } else {
            None
        };
        tracing::info!(
            local_peer_id = %local_peer_id,
            addr = ?endpoint.addr(),
            "peer-sync: Iroh endpoint started"
        );
        let listen_addrs: Arc<Mutex<Vec<iroh::EndpointAddr>>> = Arc::new(Mutex::new(Vec::new()));
        let host_identity = identity(&keypair, keypair_path);
        let personal_continuity_user = host_identity
            .as_ref()
            .map(|identity| identity.device_cert.user_public_key);
        let workspace_person_roots = host_identity
            .as_ref()
            .map(workspace_person_roots)
            .unwrap_or_default();
        let personal_continuity_device = host_identity.as_ref().and_then(|identity| {
            crate::shell::personal_continuity::DeviceId::new(hex::encode(
                identity.device_cert.device_public_key,
            ))
            .ok()
        });
        tree.write().local_person = personal_continuity_user;
        let personal_workspaces = host_identity.as_ref().map(|identity| {
            Mutex::new(
                crate::shell::personal_workspaces::PersonalWorkspaces::open(
                    identity.device_cert.user_public_key,
                    identity.device_cert.device_public_key,
                    personal_workspaces_path(state_dir),
                )
                .with_label(identity.device_cert.device_label.clone()),
            )
        });
        let (cmd_tx, _join) = Actor::spawn_deferring_relays(
            endpoint,
            lookup,
            host_identity.clone(),
            Arc::clone(&listen_addrs),
            reachability.clone(),
        );
        if let Some(mdns) = mdns {
            spawn_lan_discovery_forwarding(&runtime, mdns, &cmd_tx);
        }

        let relay_workspace_ids =
            relay_topics_for_home_peers(tree.read().rooms(), personal_continuity_user);
        let home_peers = registered_home_peer_addrs(personal_continuity_user, &relay_workspace_ids)
            .unwrap_or_default();
        let home_peer_ids = home_peers
            .iter()
            .map(|host| host.addr.id)
            .collect::<std::collections::HashSet<_>>();
        let home_device_ids = home_peers
            .iter()
            .map(|host| host.expectation.device_public_key)
            .collect::<std::collections::HashSet<_>>();
        // Enrollment-bound home peers must never race an unpinned connection.
        // The actor treats the first `(workspace, endpoint)` dial as the active
        // attempt, and an unpinned attempt deliberately cannot discover Files.
        let mut host_addrs = configured_host_addrs()
            .into_iter()
            .filter(|addr| !home_peer_ids.contains(&addr.id))
            .collect::<Vec<_>>();
        host_addrs.sort_by_key(|addr| addr.id.to_string());
        host_addrs.dedup_by_key(|addr| addr.id);
        for addr in host_addrs.iter().cloned() {
            let _ = cmd_tx.try_send(NetCmd::ConnectHost {
                addr,
                expectation: None,
            });
        }
        for host in home_peers {
            host_addrs.push(host.addr.clone());
            let _ = cmd_tx.try_send(NetCmd::ConnectHost {
                addr: host.addr,
                expectation: Some(host.expectation),
            });
        }
        let dialed_direct_peers = std::collections::HashSet::new();
        for addr in personal_device_peer_addrs(
            &home_peer_registry_path().with_file_name("personal-device-peers.json"),
            Some(*local_peer_id.as_bytes()),
        ) {
            let _ = cmd_tx.try_send(NetCmd::Connect { addr });
        }
        host_addrs.sort_by_key(|addr| addr.id.to_string());
        host_addrs.dedup_by_key(|addr| addr.id);

        // Peer id `1` is the replica-side identity for
        // `WorkspaceTreeReplica::handle`'s `_sender` arg; the actor's
        // Iroh `EndpointId` is transport-level and hashed separately.
        let tree_for_reachability = tree.clone();
        let replica = Arc::new(Mutex::new(WorkspaceTreeReplica::new(1, tree)));

        // Capacity matches `Actor::spawn`'s command-channel default.
        let (event_tx, mut arriving) = mpsc::channel(64);
        let (delivered, event_rx) = mpsc::channel(64);
        let event_wake: Arc<std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>> = Arc::default();
        let wake = Arc::clone(&event_wake);
        runtime.spawn(async move {
            while let Some(event) = arriving.recv().await {
                if delivered.send(event).await.is_err() {
                    break;
                }
                if let Some(wake) = wake.get() {
                    wake();
                }
            }
        });

        let pending_invitations_path = pending_invites_path(state_dir);
        let personal_continuity_path = personal_continuity_path(state_dir);
        let app_continuations_permitted =
            crate::shell::continuity_policy::ContinuityPolicySnapshot::load()
                .permits(crate::shell::continuity_policy::ContinuityCapability::AppContinuations);
        let persisted_continuity = if app_continuations_permitted {
            load_personal_continuity(&personal_continuity_path)
        } else {
            purge_personal_continuity_store(&personal_continuity_path);
            PersistedPersonalContinuity {
                counter: 0,
                continuity: crate::shell::personal_continuity::PersonalContinuity::default(),
            }
        };
        let device_fabric_path = device_fabric_path(state_dir);
        let device_fabric = load_device_fabric(&device_fabric_path);
        let now = tilo_clock::unix_now();
        let pending_identity_admissions: std::collections::HashMap<_, _> =
            load_pending_invitations(&pending_invitations_path)
                .into_iter()
                .filter(|invite| !invite.is_expired(now))
                .map(|invite| (invite.id, invite))
                .collect();
        persist_pending_invitations(&pending_invitations_path, &pending_identity_admissions);
        let other_devices = device_fabric
            .snapshots()
            .keys()
            .any(|device| Some(device) != personal_continuity_device.as_ref());
        if let Some(why) = needs_reachability(
            &tree_for_reachability.read(),
            &local_peer_id.to_string(),
            !pending_identity_admissions.is_empty(),
            other_devices,
        ) {
            reachability.wake(why);
        }

        let network = Self {
            cmd_tx,
            replica,
            event_tx,
            event_rx: Mutex::new(event_rx),
            event_wake,
            joined_topics: Mutex::new(std::collections::HashSet::new()),
            workspace_control_direct: Mutex::new(std::collections::HashSet::new()),
            workspace_control_relays: Mutex::new(std::collections::HashSet::new()),
            runtime: Some(runtime),
            local_peer_hash,
            keypair,
            host_identity,
            local_peer_id,
            listen_addrs,
            reachability,
            host_addrs: Mutex::new(host_addrs),
            registered_home_peer_ids: Mutex::new(home_peer_ids),
            dialed_direct_peers: Mutex::new(dialed_direct_peers),
            live_room_peers: Mutex::new(std::collections::HashSet::new()),
            registered_home_device_ids: Mutex::new(home_device_ids),
            personal_device_peers: Mutex::default(),
            last_home_peer_refresh: Mutex::new(None),
            last_sync_kick: Mutex::new(None),
            next_sync_pull: Mutex::new(None),
            peers_joined: std::sync::atomic::AtomicU64::new(0),
            room_push: Mutex::new(RoomPush::default()),
            personal_focus_recency: Mutex::new(
                crate::shell::continuity_suggestion::FocusRecency::seeded(
                    &persisted_continuity.continuity,
                ),
            ),
            personal_continuity: Mutex::new(persisted_continuity.continuity),
            personal_continuity_user,
            workspace_person_roots,
            personal_continuity_device,
            personal_continuity_counter: Mutex::new(persisted_continuity.counter),
            personal_continuity_fingerprint: Mutex::new(None),
            personal_continuity_path: Some(personal_continuity_path),
            personal_workspaces,
            last_personal_workspaces_snapshot: Mutex::new(None),
            #[cfg(test)]
            continuity_policy_override: None,
            device_fabric: Mutex::new(device_fabric),
            #[cfg(test)]
            device_fabric_reads: DeviceFabricReads::default(),
            device_fabric_path: Some(device_fabric_path),
            personal_fabric_actions: Mutex::new(VecDeque::new()),
            authenticated_personal_routes: Mutex::new(std::collections::HashMap::new()),
            personal_fabric_action_replays: Mutex::new(PersonalFabricReplayWindow::default()),
            peer_identities: PeerIdentityStore::new(),
            require_peer_identity: true,
            pending_identity_admissions: Mutex::new(pending_identity_admissions),
            pending_invitations_path: Some(pending_invitations_path),
            pending_redemptions: Mutex::new(std::collections::HashMap::new()),
            native_enrollment: native_enrollment::State::default(),
            hosted_topology: hosted_topology::State::default(),
            relay_rooms: relay_rooms::State::default(),
            record_content: record_content::State::default(),
            inbound_presence: Arc::new(Mutex::new(Vec::new())),
            inbound_documents: Arc::new(Mutex::new(Vec::new())),
            drain_wake: std::sync::Arc::new(crate::idle_wake::DrainPark::default()),
            inbound_channel_governance: Arc::new(Mutex::new(Vec::new())),
            wire_records: Mutex::new(crate::network::wire_record::WireRecordIds::default()),
            person_artifacts: Mutex::default(),
            person_snapshots_wanted: Mutex::default(),
            inbound_app_presence: Arc::new(Mutex::new(Vec::new())),
            live_presence: Mutex::default(),
            live_labels: Mutex::default(),
            peer_colours: Mutex::default(),
            peer_colours_epoch: std::sync::atomic::AtomicU64::new(0),
            inbound_scrollback_request: Arc::new(Mutex::new(Vec::new())),
            inbound_scrollback_response: Arc::new(Mutex::new(Vec::new())),
            inbound_remote_actions: Arc::new(Mutex::new(Vec::new())),
            held_remote_actions: Mutex::new(std::collections::VecDeque::new()),
            remote_action_replies: crate::shell::remote_action_link::ReplyWaiters::default(),
            remote_use: Mutex::default(),
            inbound_legacy: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            workspace_control_observers: Mutex::new(Vec::new()),
            peer_departures: Arc::new(PeerDepartureHistory::default()),
        };
        if !app_continuations_permitted {
            network.enforce_app_continuations_policy();
        }
        network.restore_display_observations();
        Ok(network)
    }

    /// Pre-computed hash of the local iroh endpoint id. Stamped on every
    /// outbound presence move so peers can attribute the cursor to its
    /// publisher.
    pub fn local_peer_id_hash(&self) -> u64 {
        self.local_peer_hash
    }

    /// Certified local identity used to author personal-fabric state.
    pub fn local_device_fabric_identity(
        &self,
    ) -> Option<(crate::shell::personal_continuity::DeviceId, String)> {
        Some((
            self.personal_continuity_device.clone()?,
            self.host_identity
                .as_ref()?
                .device_cert
                .device_label
                .clone(),
        ))
    }

    /// Resolve and open a Tilo workspace-control session over an authenticated
    /// exact-target device route. A successful route is transport reachability
    /// only: the returned session still waits for the target's authenticated
    /// `SurfaceStreamStart` before allowing input.
    ///
    /// # Errors
    ///
    /// Returns an error when the target is local, workspace transport is
    /// unavailable, or no authenticated direct route exists.
    pub fn open_workspace_control(
        &self,
        workspace: u64,
        target: iroh::EndpointId,
    ) -> Result<crate::network::WorkspaceControlSession, String> {
        if target == self.local_peer_id {
            return Err("cannot control this device through its remote route".into());
        }
        if !self.join_topic(workspace) {
            return Err("workspace transport is unavailable".into());
        }
        let direct_reachable = self
            .workspace_control_direct
            .lock()
            .is_ok_and(|peers| peers.contains(&(workspace, target)));
        // Exact-target relayed control does not exist yet: a relay only fans
        // out to the whole workspace, so it can explain the refusal but never
        // carry input. A relay that is this device or the target is no relay.
        let fanout_relay_online = self.workspace_control_relays.lock().is_ok_and(|relays| {
            relays.iter().any(|(relay_ws, relay)| {
                *relay_ws == workspace && *relay != self.local_peer_id && *relay != target
            })
        });
        let route = if direct_reachable {
            WorkspaceControlRoute::Direct { target }
        } else if fanout_relay_online {
            return Err(
                "workspace control needs an exact-target route; this relay only supports workspace-wide fan-out"
                    .into(),
            );
        } else {
            return Err("no authenticated direct workspace-control path is online".into());
        };
        Ok(crate::network::WorkspaceControlSession::new(
            workspace,
            self.local_peer_hash,
            target,
            route,
        ))
    }

    /// Publish one input through a session's selected route. The session gate
    /// constructs no message before consent or after revocation/disconnect.
    ///
    /// # Errors
    ///
    /// Returns an error when the session refuses input or the actor transport
    /// is unavailable.
    pub fn publish_workspace_control_input(
        &self,
        session: &crate::network::WorkspaceControlSession,
        event: &crate::network::InputEvent,
    ) -> Result<(), String> {
        let msg = session.input_message(event).map_err(str::to_owned)?;
        self.cmd_tx
            .try_send(NetCmd::PublishWorkspaceControl {
                ws: session.workspace(),
                msg,
                route: session.route(),
            })
            .map_err(|error| format!("workspace-control transport is unavailable: {error}"))
    }

    pub fn subscribe_workspace_control(
        &self,
    ) -> tokio::sync::mpsc::UnboundedReceiver<WorkspaceControlNetworkEvent> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        if let Ok(mut observers) = self.workspace_control_observers.lock() {
            observers.push(tx);
        }
        rx
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "each observer requires an owned event clone and the value is consumed by the broadcast fanout"
    )]
    fn notify_workspace_control(&self, event: WorkspaceControlNetworkEvent) {
        if let Ok(mut observers) = self.workspace_control_observers.lock() {
            observers.retain(|observer| observer.send(event.clone()).is_ok());
        }
    }

    /// What the peers in `cluster_id` are called, keyed by the peer
    /// handle presence frames carry. `cluster_id` is a room id, so this
    /// is the room document's answer and nobody else's — names are
    /// written there by the room's authority and refused from anyone
    /// else. Empty for a cluster that is not a room this machine is in,
    /// or a room where nobody has been named; both leave every peer
    /// labelled with its own handle.
    /// The other devices `room` names, which may keep its records.
    pub fn room_devices(&self, room: crate::shell::state::RoomId) -> Vec<iroh::EndpointId> {
        let Ok(replica) = self.replica.lock() else {
            return Vec::new();
        };
        let local = self.local_peer_id_hash();
        replica
            .tree
            .read()
            .room_peer_endpoints(room)
            .into_iter()
            .filter(|device| crate::network::behaviour::peer_liveness_handle(device) != local)
            .collect()
    }

    /// This device's copy of the workspace that is `room`, when it hosts it.
    pub fn workspace_of_room(
        &self,
        room: crate::shell::state::RoomId,
    ) -> Option<crate::shell::state::Workspace> {
        self.replica
            .lock()
            .ok()?
            .tree
            .read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room == room)
            .cloned()
    }

    pub fn peer_labels(&self, cluster_id: &str) -> std::collections::BTreeMap<u64, String> {
        let empty = std::collections::BTreeMap::new();
        let Some(room) = crate::shell::state::RoomId::parse(cluster_id) else {
            return empty;
        };
        if room.is_nil() {
            return empty;
        }
        let Ok(replica) = self.replica.lock() else {
            return empty;
        };
        let mut labels = replica.tree.read().room_peer_labels(room);
        drop(replica);
        if let Ok(live) = self.live_labels.lock()
            && let Some(live) = live.get(cluster_id)
        {
            for (handle, label) in live {
                labels.entry(*handle).or_insert_with(|| label.clone());
            }
        }
        labels
    }

    /// Live session handles and their admitted display names for one room.
    /// Unlike [`Self::peer_labels`], this projection is deliberately
    /// ephemeral: `WorkspaceTree::retire_session` removes a device as soon
    /// as the transport reports departure. Presence cleanup uses it to avoid
    /// erasing a person's status when another device of theirs is still here.
    pub fn active_session_labels(
        &self,
        cluster_id: &str,
    ) -> std::collections::BTreeMap<u64, String> {
        let Some(room) = crate::shell::state::RoomId::parse(cluster_id) else {
            return std::collections::BTreeMap::new();
        };
        let Ok(replica) = self.replica.lock() else {
            return std::collections::BTreeMap::new();
        };
        replica
            .tree
            .read()
            .desktop
            .workspaces
            .iter()
            .filter(|workspace| workspace.room == room)
            .flat_map(|workspace| &workspace.collaborators)
            .filter_map(|participant| {
                participant
                    .session
                    .map(|session| (session, participant.name.clone()))
            })
            .collect()
    }

    /// Snapshot recent transport departures, oldest first. This is bounded
    /// session diagnostics, not a durable participant roster.
    pub fn peer_departures(&self) -> Vec<PeerDeparture> {
        self.peer_departures.snapshot()
    }

    pub(crate) fn peer_departure_history(&self) -> Arc<PeerDepartureHistory> {
        Arc::clone(&self.peer_departures)
    }

    /// Workspace topic for session traffic associated with the active room.
    /// Peer handles of the workspace hosts this session reaches `workspace`
    /// through (the relays that answered ready for it).
    pub fn workspace_hosts(&self, workspace: u64) -> std::collections::HashSet<u64> {
        self.workspace_control_relays
            .lock()
            .map(|relays| {
                relays
                    .iter()
                    .filter(|(relay_ws, _)| *relay_ws == workspace)
                    .map(|(_, relay)| crate::network::behaviour::peer_liveness_handle(relay))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Wake the compositor loop whenever a network event arrives for
    /// [`Self::tick`], and whenever legacy-window traffic (a stream, or a
    /// viewer's input) arrives, so an idle headless session answers at once.
    pub fn set_wake(&self, wake: crate::session_runtime::Wake) {
        let events = Arc::clone(&wake);
        let _ = self.event_wake.set(Box::new(move || events()));
        let _ = self
            .cmd_tx
            .try_send(NetCmd::SetWake(crate::network::actor::LoopWake(wake)));
    }

    /// Wake the compositor loop from any thread, once [`Self::set_wake`]
    /// has supplied the loop's wake; a no-op before that.
    pub(crate) fn loop_waker(&self) -> crate::session_runtime::Wake {
        let wake = Arc::clone(&self.event_wake);
        Arc::new(move || {
            if let Some(wake) = wake.get() {
                wake();
            }
        })
    }

    pub fn active_ws_for_publish(&self) -> u64 {
        let Ok(replica) = self.replica.lock() else {
            return DEFAULT_WS_TOPIC;
        };
        replica
            .tree
            .read()
            .desktop
            .active_workspace()
            .map_or(DEFAULT_WS_TOPIC, |workspace| room_topic_id(workspace.room))
    }

    fn explicit_topic_for_message(msg: &NeighbourMsg) -> Option<u64> {
        if let NeighbourMsg::PersonalContinuityRequest {
            user_public_key, ..
        }
        | NeighbourMsg::PersonalContinuityCheckpoint {
            user_public_key, ..
        } = msg
        {
            return Some(personal_continuity_topic_id(user_public_key));
        }
        if let NeighbourMsg::PersonalWorkspaces {
            user_public_key, ..
        } = msg
        {
            return Some(personal_continuity_topic_id(user_public_key));
        }
        let room = match msg {
            NeighbourMsg::IdentityAnnouncement { room, .. }
            | NeighbourMsg::IdentityAnnouncementV2 { room, .. }
            | NeighbourMsg::SyncReq { room, .. }
            | NeighbourMsg::SyncResp { room, .. }
            | NeighbourMsg::MlsGovernance { room, .. }
            | NeighbourMsg::MlsAccessRequest { room, .. }
            | NeighbourMsg::MlsGovernanceAck { room, .. } => Some(room.as_str()),
            NeighbourMsg::AppDocument { cluster_id, .. }
            | NeighbourMsg::AppPresenceV2 { cluster_id, .. }
            | NeighbourMsg::RecordContentWant { cluster_id, .. }
            | NeighbourMsg::RecordContentPiece { cluster_id, .. } => Some(cluster_id.as_str()),
            _ => None,
        };
        room.and_then(RoomId::parse)
            .filter(|room| !room.is_nil())
            .map(room_topic_id)
    }

    fn topic_for_message(&self, msg: &NeighbourMsg) -> u64 {
        Self::explicit_topic_for_message(msg).unwrap_or_else(|| self.active_ws_for_publish())
    }

    fn join_topic(&self, ws: u64) -> bool {
        let Ok(mut joined) = self.joined_topics.lock() else {
            return false;
        };
        if !joined.insert(ws) {
            return true;
        }
        if self
            .cmd_tx
            .try_send(NetCmd::JoinWorkspace {
                ws,
                on_event: self.event_tx.clone(),
            })
            .is_err()
        {
            joined.remove(&ws);
            return false;
        }
        true
    }

    fn publish_message(&self, mut msg: NeighbourMsg) -> bool {
        if matches!(
            &msg,
            NeighbourMsg::PersonalContinuityRequest { .. }
                | NeighbourMsg::PersonalContinuityCheckpoint { .. }
        ) && !self.enforce_app_continuations_policy()
        {
            return false;
        }
        if let NeighbourMsg::SyncReq {
            room, invite_id, ..
        } = &mut msg
            && invite_id.is_none()
            && let Some(room_id) = RoomId::parse(room)
            && let Some(redemption) = self
                .pending_redemptions
                .lock()
                .ok()
                .and_then(|pending| pending.get(&room_id).copied())
        {
            *invite_id = Some(redemption);
        }
        let ws = self.topic_for_message(&msg);
        if self.native_topic_required(ws)
            && matches!(
                &msg,
                NeighbourMsg::SyncReq { .. }
                    | NeighbourMsg::SyncResp { .. }
                    | NeighbourMsg::IdentityAnnouncement { .. }
                    | NeighbourMsg::IdentityAnnouncementV2 { .. }
            )
        {
            return false;
        }
        if self.native_topic_required(ws)
            && matches!(&msg, NeighbourMsg::AppDocument { .. })
            && !self.native_document_committed(&msg)
            && !self.sealed_record_write(&msg)
        {
            return false;
        }
        self.join_topic(ws) && self.cmd_tx.try_send(NetCmd::Publish { ws, msg }).is_ok()
    }

    /// Publish live legacy-app traffic on an explicit authenticated room
    /// route. Unlike ordinary shell messages this must not follow a later
    /// active-workspace switch: remote input belongs to the stream's room.
    pub fn publish_legacy_message(&self, workspace_topic: u64, msg: NeighbourMsg) -> bool {
        if !matches!(
            msg,
            NeighbourMsg::SurfaceStreamStart { .. }
                | NeighbourMsg::SurfaceFrame { .. }
                | NeighbourMsg::SurfaceStreamEnd { .. }
                | NeighbourMsg::RemoteInput { .. }
                | NeighbourMsg::PermissionGrant { .. }
        ) {
            return false;
        }
        self.join_topic(workspace_topic)
            && self
                .cmd_tx
                .try_send(NetCmd::Publish {
                    ws: workspace_topic,
                    msg,
                })
                .is_ok()
    }

    /// Send one window's stream traffic on `workspace_topic` to exactly
    /// `viewers` (peer handles) over the direct media route, and to nobody
    /// else in the room.
    pub fn publish_legacy_to(
        &self,
        workspace_topic: u64,
        msg: NeighbourMsg,
        viewers: Vec<u64>,
    ) -> bool {
        if !matches!(
            msg,
            NeighbourMsg::SurfaceStreamStart { .. }
                | NeighbourMsg::SurfaceFrame { .. }
                | NeighbourMsg::SurfaceStreamEnd { .. }
                | NeighbourMsg::PermissionGrant { .. }
        ) {
            return false;
        }
        self.join_topic(workspace_topic)
            && self
                .cmd_tx
                .try_send(NetCmd::PublishSurfaceTo {
                    ws: workspace_topic,
                    msg,
                    viewers,
                })
                .is_ok()
    }

    /// Publish owner-side stream traffic to the room currently being viewed.
    /// The receiver retains that concrete topic and uses
    /// [`Self::publish_legacy_message`] for all input replies.
    pub fn publish_legacy_active(&self, msg: NeighbourMsg) -> bool {
        let workspace_topic = self.active_ws_for_publish();
        self.publish_legacy_message(workspace_topic, msg)
    }

    /// Drain authenticated live legacy-app traffic without holding the queue
    /// while decoders or the compositor do work.
    pub fn take_inbound_legacy(&self) -> Vec<InboundLegacyEvent> {
        let Ok(mut queue) = self.inbound_legacy.lock() else {
            return Vec::new();
        };
        queue.drain(..).collect()
    }

    #[cfg(test)]
    fn queue_legacy_message(&self, workspace_topic: u64, message: NeighbourMsg) {
        self.queue_legacy_message_from(workspace_topic, None, message);
    }

    fn queue_legacy_message_from(
        &self,
        workspace_topic: u64,
        transport_peer: Option<iroh::EndpointId>,
        message: NeighbourMsg,
    ) {
        // Never reflect a host's own gossip echo into a decoder or seat.
        if Self::claimed_publisher(&message) == Some(self.local_peer_hash) {
            return;
        }
        if let NeighbourMsg::RemoteInput { peer, surface, .. } = &message {
            tracing::debug!(workspace_topic, peer, surface, "remote input queued");
        }
        self.notify_workspace_control(WorkspaceControlNetworkEvent::Message(InboundLegacyEvent {
            workspace_topic,
            transport_peer,
            message: message.clone(),
        }));
        if matches!(message, NeighbourMsg::PermissionGrant { .. }) {
            return;
        }
        let Ok(mut queue) = self.inbound_legacy.lock() else {
            return;
        };
        if let NeighbourMsg::SurfaceFrame {
            surface,
            source_peer,
            ts,
            ..
        } = &message
        {
            // Video is drop-oldest/latest-wins. Control and input retain
            // ordering; only an older frame for this exact stream is removed.
            if let Some((index, queued_ts)) =
                queue
                    .iter()
                    .enumerate()
                    .find_map(|(index, event)| match &event.message {
                        NeighbourMsg::SurfaceFrame {
                            surface: queued_surface,
                            source_peer: queued_peer,
                            ts: queued_ts,
                            ..
                        } if queued_surface == surface && queued_peer == source_peer => {
                            Some((index, *queued_ts))
                        }
                        _ => None,
                    })
            {
                if queued_ts >= *ts {
                    return;
                }
                queue.remove(index);
            }
        }
        while queue.len() >= LEGACY_INBOX_CAP {
            let frame_index = queue
                .iter()
                .position(|event| matches!(event.message, NeighbourMsg::SurfaceFrame { .. }));
            if let Some(index) = frame_index {
                queue.remove(index);
            } else {
                queue.pop_front();
            }
        }
        queue.push_back(InboundLegacyEvent {
            workspace_topic,
            transport_peer,
            message,
        });
    }

    fn publish_shell_checkpoint(&self, msg: NeighbourMsg) -> bool {
        if matches!(&msg, NeighbourMsg::PersonalContinuityCheckpoint { .. })
            && !self.enforce_app_continuations_policy()
        {
            return false;
        }
        let ws = self.topic_for_message(&msg);
        self.join_topic(ws)
            && self
                .cmd_tx
                .try_send(NetCmd::PublishCheckpoint { ws, msg })
                .is_ok()
    }

    /// Shared handle to the inbound-presence buffer. The shell's
    /// per-frame drainer swaps the `Vec` out under the lock and writes
    /// each entry into `PresenceCtx::remote_cursors`.
    pub fn inbound_presence_handle(&self) -> Arc<Mutex<Vec<InboundPresence>>> {
        Arc::clone(&self.inbound_presence)
    }

    /// Forward a local-cursor presence move to the active workspace's
    /// peers. Throttling lives upstream (`pointer_broadcast_allowed`).
    /// `try_send` failures are swallowed — presence is best-effort and
    /// a momentary backlog should never block the render loop.
    pub fn publish_presence(&self, key: String, point: (f64, f64)) {
        let ts = tilo_clock::unix_now_ms();
        let msg = NeighbourMsg::PresenceMoved {
            peer: self.local_peer_id_hash(),
            user: crate::shell::state::local_user_id(),
            key,
            point,
            ts,
            colour: Some(crate::shell::state::local_presence_hue().id().to_owned()),
        };
        let _ = self.publish_message(msg);
    }

    /// Remember the colour peer session `peer` said its person wears. A
    /// name this build does not know, or none, leaves what was heard before.
    pub fn note_peer_colour(&self, peer: u64, colour: Option<&str>) {
        let Some(hue) = colour.and_then(ui::tokens::PresenceHue::from_id) else {
            return;
        };
        if peer == self.local_peer_hash {
            return;
        }
        if let Ok(mut colours) = self.peer_colours.lock()
            && colours.insert(peer, hue) != Some(hue)
        {
            self.peer_colours_epoch
                .fetch_add(1, std::sync::atomic::Ordering::Release);
        }
    }

    /// A counter that advances whenever a peer joins one of this session's
    /// topics or is first authenticated: what such a peer has not heard yet
    /// (where a resting pointer is) is said again for it, and nothing is
    /// restated on a timer for peers that already heard it.
    pub fn peers_joined_epoch(&self) -> u64 {
        self.peers_joined.load(std::sync::atomic::Ordering::Acquire)
    }

    fn note_peer_joined(&self) {
        self.peers_joined
            .fetch_add(1, std::sync::atomic::Ordering::Release);
    }

    /// A counter that advances whenever a peer's colour changes. Equal
    /// values read at two moments mean [`Self::peer_colours`] did not
    /// change in between.
    pub fn peer_colours_epoch(&self) -> u64 {
        self.peer_colours_epoch
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// The colour peer session `peer` said its person wears, if it said.
    pub fn peer_colour(&self, peer: u64) -> Option<ui::tokens::PresenceHue> {
        self.peer_colours.lock().ok()?.get(&peer).copied()
    }

    /// Every colour a peer session said its person wears, by handle.
    pub fn peer_colours(&self) -> Vec<(u64, ui::tokens::PresenceHue)> {
        self.peer_colours
            .lock()
            .map(|colours| colours.iter().map(|(peer, hue)| (*peer, *hue)).collect())
            .unwrap_or_default()
    }

    /// Queue a received presence move; public so tests can inject one
    /// without going through gossip.
    pub fn handle_presence_message(
        &self,
        peer: u64,
        user: String,
        key: String,
        point: (f64, f64),
        ts: u64,
    ) {
        // A session does not paint its own cursor from the network: the
        // pointer it would draw is the one already under the user's hand.
        if peer == self.local_peer_hash {
            return;
        }
        if let Ok(mut buf) = self.inbound_presence.lock() {
            buf.push(InboundPresence {
                peer,
                user,
                key,
                point,
                ts,
            });
        }
        self.drain_wake.poke();
    }

    /// Is `cluster_id` a room this machine is in? An app's cluster is
    /// the room's uuid, so this is the same question `apply_room_changes`
    /// asks before storing anything. A `cluster_id` that fails to parse
    /// as a uuid is not a room, and refusing it is fail-closed.
    fn is_in_room(&self, cluster_id: &str) -> bool {
        let Some(room) = crate::shell::state::RoomId::parse(cluster_id) else {
            tracing::info!(
                target: "tilo_collab_probe",
                cluster = %cluster_id,
                "PROBE is_in_room=false (not a uuid)",
            );
            return false;
        };
        if room.is_nil() {
            tracing::info!(
                target: "tilo_collab_probe",
                cluster = %cluster_id,
                "PROBE is_in_room=false (nil)",
            );
            return false;
        }
        // The person's own room is no room a peer shares: its records arrive
        // only over the personal fabric, never as room traffic.
        if self.person_root_of_room(room).is_some() {
            return false;
        }
        let Ok(replica) = self.replica.lock() else {
            return false;
        };
        let known = replica.tree.read().rooms();
        let in_room = known.contains(&room);
        drop(replica);
        tracing::info!(
            target: "tilo_collab_probe",
            cluster = %cluster_id,
            in_room,
            known = ?known.iter().map(std::string::ToString::to_string).collect::<Vec<_>>(),
            "PROBE is_in_room",
        );
        in_room
    }

    pub(crate) fn private_message_identities(&self) -> PeerIdentityStore {
        self.peer_identities.clone()
    }

    /// Certificates of `user`'s devices this session authenticated (directly
    /// or through a workspace host), valid and not revoked now.
    pub fn authenticated_devices_of(&self, user: &[u8; 32]) -> Vec<tilo_user_identity::DeviceCert> {
        self.peer_identities
            .devices_of(user, tilo_clock::unix_now_ns())
    }

    /// When [`Self::authenticated_devices_of`] next answers differently by
    /// time alone, after `now_ns`: a certificate expiring or a revocation
    /// dated ahead taking effect. Its writes are announced
    /// ([`tilo_identity_host::policy_change`]).
    pub fn authenticated_devices_next_change(&self, now_ns: u64) -> Option<u64> {
        self.peer_identities.next_change_after(now_ns)
    }

    /// Shared handle to inbound signed document envelopes.
    pub fn inbound_documents_handle(&self) -> Arc<Mutex<Vec<InboundDocument>>> {
        Arc::clone(&self.inbound_documents)
    }

    /// Shared handle to inbound channel governance.
    pub fn inbound_channel_governance_handle(&self) -> Arc<Mutex<Vec<InboundChannelGovernance>>> {
        Arc::clone(&self.inbound_channel_governance)
    }

    /// Publish one piece of channel governance to `room`. Durable workspace
    /// hosts retain it (the frame chain per channel, the newest request and
    /// acknowledgement per device), so devices that are offline now still
    /// receive it. Returns whether it was handed to the transport.
    pub fn publish_channel_governance(&self, message: NeighbourMsg) -> bool {
        if !matches!(
            message,
            NeighbourMsg::MlsGovernance { .. }
                | NeighbourMsg::MlsAccessRequest { .. }
                | NeighbourMsg::MlsGovernanceAck { .. }
        ) {
            return false;
        }
        self.publish_message(message)
    }

    fn queue_channel_governance(&self, entry: InboundChannelGovernance) {
        if let Ok(mut inbound) = self.inbound_channel_governance.lock() {
            inbound.push(entry);
        }
        self.drain_wake.poke();
    }

    /// Publish one validated signed document mutation. A record id that
    /// embeds a local path is replaced by its sealed, room-scoped wire name.
    pub fn publish_document(
        &self,
        app_id: String,
        cluster_id: String,
        record_id: &str,
        envelope: collab_types::SignedOp,
    ) {
        if self.is_local_person_room(&cluster_id) {
            self.publish_person_document(app_id, &cluster_id, record_id, envelope);
            return;
        }
        if let Some(room) = RoomId::parse(&cluster_id)
            && self.native_topic_required(room_topic_id(room))
            && !(envelope.encryption.is_some() && self.native_room_seals_records(room))
        {
            let _ = self.cmd_tx.try_send(NetCmd::NativePublish {
                ws: room_topic_id(room),
            });
            return;
        }
        let record_id = self.register_local_record(&app_id, &cluster_id, record_id);
        let msg = NeighbourMsg::AppDocument {
            peer: self.local_peer_id_hash(),
            app_id,
            cluster_id,
            record_id,
            envelope: Box::new(envelope),
        };
        let _ = self.publish_message(msg);
    }

    /// Promptly drive the already-durable native owner's outgoing custody,
    /// including signed governance checkpoints. Queue admission is not a
    /// commit acknowledgement; heartbeat/reconnect retries a full queue.
    pub(crate) fn schedule_native_owner_publication(&self, room: RoomId) {
        let _ = self.cmd_tx.try_send(NetCmd::NativePublish {
            ws: room_topic_id(room),
        });
    }

    /// Remember that a local app holds `record_id`, so peers' traffic for
    /// its sealed wire name can be routed to it. Returns the wire name.
    pub fn register_local_record(&self, app_id: &str, cluster_id: &str, record_id: &str) -> String {
        if self.is_local_person_room(cluster_id) {
            self.remember_person_artifact(collab_types::document_artifact_id(
                app_id, cluster_id, record_id,
            ));
        }
        match self.wire_records.lock() {
            Ok(mut records) => records.register(cluster_id, app_id, record_id),
            Err(_) => crate::network::wire_record::wire_record_id(cluster_id, app_id, record_id),
        }
    }

    /// The person's own room: the room derived from the root that certified
    /// this device. Its records travel only between that root's devices.
    pub fn local_person_room(&self) -> Option<RoomId> {
        self.personal_continuity_user
            .map(|user| RoomId::from(collab_types::person_room(&user)))
    }

    /// Every root this device acts for: the one that certified it, then its
    /// workspace identities. Each has its own person room.
    fn person_roots(&self) -> impl Iterator<Item = [u8; 32]> + '_ {
        self.personal_continuity_user
            .into_iter()
            .chain(self.workspace_person_roots.iter().copied())
    }

    /// The root of this device whose person room `room` is.
    fn person_root_of_room(&self, room: RoomId) -> Option<[u8; 32]> {
        self.person_roots()
            .find(|root| RoomId::from(collab_types::person_room(root)) == room)
    }

    /// The root of this device whose person room `cluster_id` names.
    fn person_root_of(&self, cluster_id: &str) -> Option<[u8; 32]> {
        RoomId::parse(cluster_id).and_then(|room| self.person_root_of_room(room))
    }

    fn is_local_person_room(&self, cluster_id: &str) -> bool {
        self.person_root_of(cluster_id).is_some()
    }

    /// A person room, once after another device of its root appeared on an
    /// authenticated route: the broker then asks each local app holding one
    /// of its records for a snapshot to send that device. One room per call.
    pub fn take_person_snapshot_request(&self) -> Option<String> {
        let root = self.person_snapshots_wanted.lock().ok()?.pop_first()?;
        Some(collab_types::person_room(&root).to_string())
    }

    fn remember_person_artifact(&self, artifact: collab_types::ArtifactId) {
        if let Ok(mut artifacts) = self.person_artifacts.lock() {
            artifacts.insert(artifact);
        }
    }

    fn is_person_artifact(&self, artifact: &collab_types::ArtifactId) -> bool {
        self.person_artifacts
            .lock()
            .is_ok_and(|artifacts| artifacts.contains(artifact))
    }

    /// Send a person-room document to each of this person's other devices on
    /// its authenticated personal route, and nowhere else: no room topic, no
    /// workspace host, no collaborator.
    fn publish_person_document(
        &self,
        app_id: String,
        cluster_id: &str,
        record_id: &str,
        envelope: collab_types::SignedOp,
    ) {
        // The room's own root: the device's, or one of its workspace
        // identities', each with its own continuity topic and devices.
        let Some(user_public_key) = self.person_root_of(cluster_id) else {
            return;
        };
        let record_id = self.register_local_record(&app_id, cluster_id, record_id);
        let ws = personal_continuity_topic_id(&user_public_key);
        // The person's always-on hosts keep it for their devices that are
        // away: on the continuity topic, whose only member on a host is the
        // person, and to host sessions only, never to gossip neighbours.
        let _ = self.cmd_tx.try_send(NetCmd::PublishToHosts {
            ws,
            msg: NeighbourMsg::AppDocument {
                peer: self.local_peer_id_hash(),
                app_id: app_id.clone(),
                cluster_id: cluster_id.to_owned(),
                record_id: record_id.clone(),
                envelope: Box::new(envelope.clone()),
            },
        });
        let payload = PersonalFabricPayload::Document {
            app_id,
            record_id,
            envelope: Box::new(envelope),
        };
        // A whole mutation fits a frame (larger ones travel as segments); one
        // that does not would be refused by every receiver.
        let size = postcard::to_allocvec(&payload).map_or(usize::MAX, |bytes| bytes.len());
        if size > crate::shell::device_fabric::MAX_FABRIC_CHECKPOINT_BYTES {
            tracing::warn!(
                size,
                "person room document exceeds the personal fabric frame limit; dropping"
            );
            return;
        }
        if !self.personal_fabric_allowed() {
            return;
        }
        let targets = self.personal_route_targets(ws, &user_public_key);
        if targets.is_empty() || !self.join_topic(ws) {
            return;
        }
        for target in targets {
            let _ = self.cmd_tx.try_send(NetCmd::PublishPersonalFabricTarget {
                ws,
                user_public_key,
                target,
                payload: payload.clone(),
            });
        }
    }

    /// The person's other devices with an authenticated route on their
    /// continuity topic `ws`.
    fn personal_route_targets(&self, ws: u64, user_public_key: &[u8; 32]) -> Vec<iroh::EndpointId> {
        self.authenticated_personal_routes
            .lock()
            .map(|routes| {
                routes
                    .values()
                    .filter(|route| {
                        route.workspace_topic == ws && route.user_public_key == *user_public_key
                    })
                    .map(|route| route.endpoint_id)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A person-room document an always-on host kept on the person's
    /// continuity topic, written by `author`: taken in only when that topic is
    /// this person's and the author is a device their root certified.
    fn dispatch_hosted_person_document(
        &self,
        ws: u64,
        author: iroh::EndpointId,
        msg: NeighbourMsg,
    ) {
        let NeighbourMsg::AppDocument {
            app_id,
            cluster_id,
            record_id,
            envelope,
            ..
        } = msg
        else {
            return;
        };
        let Some(user) = self.person_root_of(&cluster_id) else {
            return;
        };
        if ws != personal_continuity_topic_id(&user) {
            tracing::warn!(
                ws,
                "person room document outside the continuity topic; dropping"
            );
            return;
        }
        let origin = crate::shell::state::PeerOrigin::peer(&author);
        let Some(identity) = self.peer_identities.get(&origin) else {
            tracing::debug!(author = %author, "person room document from an unknown device; dropping");
            return;
        };
        if !identity.is_same_user(&user) {
            tracing::warn!(author = %author, "person room document from another person's device; dropping");
            return;
        }
        self.dispatch_person_document(author, &identity, app_id, &record_id, *envelope);
    }

    /// A person-room document from another device of one of this device's
    /// roots, authenticated by its personal route: queued, in the person room
    /// of the sender's root, for the local apps holding the record, as
    /// written by the room's owner — or, from a device on an operating system
    /// Tilo does not control (`foreign`), with the foreign-device cap's role.
    fn dispatch_person_document(
        &self,
        peer: iroh::EndpointId,
        sender: &crate::network::identity::PeerIdentity,
        app_id: String,
        wire: &str,
        envelope: collab_types::SignedOp,
    ) {
        let room = RoomId::from(collab_types::person_room(&sender.user_pubkey));
        if self.person_root_of_room(room).is_none() {
            tracing::warn!(peer = %peer, "person room document of a root this device does not act for; dropping");
            return;
        }
        if *envelope.author.as_bytes() != sender.device_pubkey {
            tracing::warn!(peer = %peer, "person room document is not signed by its sending device; dropping");
            return;
        }
        let cluster_id = room.to_string();
        let Some(record_id) = self.local_record_for_wire(&app_id, &cluster_id, wire) else {
            tracing::debug!(
                app_id,
                "person room document for a record no local app holds; dropping"
            );
            return;
        };
        if collab_types::validate_document_payload_route(
            &app_id,
            &cluster_id,
            &record_id,
            &envelope,
        )
        .is_err()
        {
            tracing::warn!(peer = %peer, "person room document names another route; dropping");
            return;
        }
        self.remember_person_artifact(envelope.artifact);
        self.handle_document_message(
            crate::network::behaviour::peer_liveness_handle(&peer),
            app_id,
            cluster_id,
            record_id,
            person_room_role(sender.capabilities.foreign),
            false,
            envelope,
        );
    }

    /// The local record id an inbound wire record id names, if this device
    /// holds that record.
    fn local_record_for_wire(&self, app_id: &str, cluster_id: &str, wire: &str) -> Option<String> {
        if crate::network::wire_record::embeds_local_path(wire) {
            // A current peer never sends a path; refuse one that does rather
            // than route by a leaked name.
            tracing::warn!(app_id, "peer document names a local path; dropping");
            return None;
        }
        self.wire_records
            .lock()
            .ok()?
            .resolve(cluster_id, app_id, wire)
    }

    /// A document this device published earlier that its workspace host
    /// replayed ([`NetEvent::OwnReplay`]): queued for the broker as this
    /// device's own editor op. The broker still checks the signature, and its
    /// per-author sequence drops what the device already holds.
    fn dispatch_own_replayed_document(&self, msg: NeighbourMsg) {
        let NeighbourMsg::AppDocument {
            peer,
            app_id,
            cluster_id,
            record_id,
            envelope,
        } = msg
        else {
            return;
        };
        let Some(record_id) = self.local_record_for_wire(&app_id, &cluster_id, &record_id) else {
            return;
        };
        // The person's own records come back from their always-on hosts as
        // their owner wrote them.
        let role = if self.is_local_person_room(&cluster_id) {
            collab_types::Role::Owner
        } else if self.is_in_room(&cluster_id) {
            collab_types::Role::Editor
        } else {
            return;
        };
        self.handle_document_message(peer, app_id, cluster_id, record_id, role, true, *envelope);
    }

    /// Queue a received signed document for the broker's drainer.
    pub fn handle_document_message(
        &self,
        peer: u64,
        app_id: String,
        cluster_id: String,
        record_id: String,
        role: collab_types::Role,
        replayed: bool,
        envelope: collab_types::SignedOp,
    ) {
        if let Ok(mut buf) = self.inbound_documents.lock() {
            buf.push(InboundDocument {
                peer,
                app_id,
                cluster_id,
                record_id,
                role,
                replayed,
                envelope: Box::new(envelope),
            });
        }
        self.drain_wake.poke();
    }

    /// Publish an app's presence frame to the peers in its room —
    /// without this a peer's caret never reaches another machine, since
    /// the broker's local fan-out only reaches apps on *this* host.
    pub fn publish_app_presence(
        &self,
        app_id: String,
        cluster_id: String,
        presence: collab_types::PeerPresence,
    ) {
        if self.is_local_person_room(&cluster_id) {
            return;
        }
        let msg = NeighbourMsg::AppPresenceV2 {
            peer: self.local_peer_id_hash(),
            app_id,
            cluster_id,
            presence,
            colour: Some(crate::shell::state::local_presence_hue().id().to_owned()),
        };
        let _ = self.publish_message(msg);
    }

    /// Publish an app's presence on the room's workspace-host live plane,
    /// where browsers and agents draw it: the pointer and caret in the
    /// record, under this person's name.
    pub fn publish_live_presence(
        &self,
        app_id: &str,
        cluster_id: &str,
        record_id: &str,
        presence: &collab_types::PeerPresence,
    ) {
        let Some(room) = RoomId::parse(cluster_id)
            .filter(|room| !room.is_nil() && self.person_root_of_room(*room).is_none())
        else {
            return;
        };
        let ws = room_topic_id(room);
        let focus = tilo_relay_protocol::live::LiveRecord {
            app: app_id.to_owned(),
            record: crate::network::wire_record::wire_record_id(cluster_id, app_id, record_id),
        };
        let frame = self.live_presence.lock().ok().and_then(|mut live| {
            live.update(
                ws,
                focus,
                self.live_participant(),
                presence,
                std::time::Instant::now(),
            )
        });
        if let Some(frame) = frame.and_then(|frame| Self::seal_live(room, &frame)) {
            let _ = self.cmd_tx.try_send(NetCmd::PublishLive { ws, frame });
        }
    }

    /// Repeat quiet live presence so other devices keep drawing it.
    fn live_keepalive(&self) {
        let due = self
            .live_presence
            .lock()
            .map(|mut live| live.keepalives(std::time::Instant::now()))
            .unwrap_or_default();
        for (ws, frame) in due {
            if let Some(frame) = self
                .room_of_topic(ws)
                .and_then(|room| Self::seal_live(room, &frame))
            {
                let _ = self.cmd_tx.try_send(NetCmd::PublishLive { ws, frame });
            }
        }
    }

    /// The content keys of `channel` of `room`, when this device is admitted.
    fn live_channel_keys(
        room: RoomId,
        channel: collab_types::ChannelId,
    ) -> Option<channel_crypto::ChannelContentKeys> {
        let access = crate::shell::channel_encryption::ChannelKeyProvider::session().get()?;
        match access.access(room, channel).0 {
            crate::shell::channel_encryption::ChannelAccess::Ready(keys) => Some(keys),
            _ => None,
        }
    }

    /// Seal live `frame` of `room` with its record's channel keys; a frame
    /// this device cannot seal is never published.
    fn seal_live(room: RoomId, frame: &[u8]) -> Option<Vec<u8>> {
        crate::shell::live_presence::seal(&room.to_string(), frame, |channel| {
            Self::live_channel_keys(room, channel)
        })
    }

    /// The room whose workspace topic is `ws`.
    fn room_of_topic(&self, ws: u64) -> Option<RoomId> {
        self.replica.lock().ok().and_then(|replica| {
            replica
                .tree
                .read()
                .rooms()
                .into_iter()
                .find(|room| room_topic_id(*room) == ws)
        })
    }

    /// How this device asks live members to draw it: the person's name,
    /// else the device's certified label.
    fn live_participant(&self) -> tilo_relay_protocol::live::LiveParticipant {
        let label = ui::local_identity::load()
            .map(|identity| identity.display_name)
            .filter(|name| !name.is_empty())
            .or_else(|| {
                self.host_identity
                    .as_ref()
                    .map(|identity| identity.device_cert.device_label.clone())
            })
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "Tilo".into());
        crate::shell::live_presence::participant(label, crate::shell::state::local_presence_hue())
    }

    /// A live member's frame: its pointer and caret reach local apps as
    /// presence of the sender's handle, and its name labels that handle.
    fn handle_live_frame(&self, ws: u64, from: [u8; 32], frame: &[u8]) {
        let Some(room) = self.room_of_topic(ws) else {
            return;
        };
        let Some(frame) = crate::shell::live_presence::open(frame, |channel| {
            Self::live_channel_keys(room, channel)
        }) else {
            return;
        };
        let Some(inbound) = crate::shell::live_presence::inbound(&frame) else {
            return;
        };
        let cluster_id = room.to_string();
        let peer = neighbour_wire::peer_handle(&from);
        self.note_peer_colour(peer, inbound.colour.map(ui::tokens::PresenceHue::id));
        tracing::debug!(
            room = %cluster_id,
            from = %hex::encode(&from[..4]),
            label = inbound.label.as_deref().unwrap_or(""),
            app = %inbound.app_id,
            updates = ?inbound.updates.iter().map(|update| &update.payload).collect::<Vec<_>>(),
            "live presence from a workspace member"
        );
        if let Some(label) = inbound.label
            && let Ok(mut labels) = self.live_labels.lock()
        {
            let previous = labels
                .entry(cluster_id.clone())
                .or_default()
                .insert(peer, label.clone());
            if previous.as_ref() != Some(&label) {
                tracing::info!(
                    room = %cluster_id,
                    from = %hex::encode(&from[..4]),
                    label,
                    app = %inbound.app_id,
                    "a live member's cursor reaches this device"
                );
            }
        }
        for presence in inbound.updates {
            self.handle_typed_app_presence_message(
                peer,
                inbound.app_id.clone(),
                cluster_id.clone(),
                presence,
            );
        }
    }

    /// Ask the room's workspace hosts to replay the room, for a local app
    /// that just joined one of its records: what they replayed or fanned out
    /// before was dropped here while nothing held the record, and the app
    /// would otherwise miss the room's history.
    pub fn request_room_replay(&self, cluster_id: &str) {
        // The person's own room is kept by their always-on hosts on the
        // continuity topic.
        if let Some(user) = self.person_root_of(cluster_id) {
            let _ = self.cmd_tx.try_send(NetCmd::Replay {
                ws: personal_continuity_topic_id(&user),
            });
            return;
        }
        if let Some(room) = RoomId::parse(cluster_id).filter(|room| !room.is_nil()) {
            let _ = self.cmd_tx.try_send(NetCmd::Replay {
                ws: room_topic_id(room),
            });
        }
    }

    /// Shared handle to the inbound app-presence buffer, drained by the
    /// broker and fanned to local apps as `RemotePresenceUpdate`.
    pub fn inbound_app_presence_handle(&self) -> Arc<Mutex<Vec<InboundAppPresence>>> {
        Arc::clone(&self.inbound_app_presence)
    }

    pub fn handle_typed_app_presence_message(
        &self,
        peer: u64,
        app_id: String,
        cluster_id: String,
        presence: collab_types::PeerPresence,
    ) {
        if let Ok(mut buf) = self.inbound_app_presence.lock() {
            buf.push(InboundAppPresence {
                peer,
                app_id,
                cluster_id,
                presence,
            });
        }
        self.drain_wake.poke();
    }

    /// Shared handle to the cooperative-scrollback request buffer,
    /// answered by the broker's drainer on each tick.
    pub fn inbound_scrollback_request_handle(&self) -> Arc<Mutex<Vec<InboundScrollbackRequest>>> {
        Arc::clone(&self.inbound_scrollback_request)
    }

    /// Shared handle to the remote-action request buffer, answered by the
    /// broker's drainer.
    pub fn inbound_remote_actions_handle(
        &self,
    ) -> Arc<Mutex<Vec<crate::shell::remote_action_link::InboundRemoteAction>>> {
        Arc::clone(&self.inbound_remote_actions)
    }

    /// Send one remote-action message to exactly `target` on the workspace
    /// topic. Returns whether it was handed to the transport.
    pub fn send_remote_action(
        &self,
        workspace_topic: u64,
        target: iroh::EndpointId,
        message: tilo_actions::remote::RemoteActionMessage,
    ) -> bool {
        let Ok(frame) = tilo_actions::remote::RemoteActionFrame::new(message).encode() else {
            return false;
        };
        self.cmd_tx
            .try_send(NetCmd::PublishRemoteAction {
                ws: workspace_topic,
                target,
                msg: NeighbourMsg::RemoteAction { frame },
            })
            .is_ok()
    }

    /// Send one native action to an enrolled personal device, on a workspace
    /// both machines joined. A device that is not in the enrolled peer list
    /// is refused, so this cannot address an arbitrary endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error when `device_hex` is not an enrolled peer, the two
    /// machines share no workspace, or the frame cannot be handed to the
    /// transport.
    pub fn invoke_enrolled_device(
        &self,
        device_hex: &str,
        target: tilo_actions::remote::RemoteActionTarget,
        argument: tilo_actions::remote::InvokeArgument,
    ) -> Result<(), String> {
        let device = decode_lower_hex_32(device_hex)
            .ok_or_else(|| "device is not an enrolled device id".to_string())?;
        let local = self
            .personal_continuity_device
            .as_ref()
            .and_then(|device| decode_lower_hex_32(device.as_str()));
        let enrolled = self.enrolled_personal_devices(local);
        if !enrolled.iter().any(|addr| *addr.id.as_bytes() == device) {
            return Err("device is not an enrolled device".into());
        }
        let endpoint = iroh::EndpointId::from_bytes(&device)
            .map_err(|_| "device is not an enrolled device id".to_string())?;
        // A named workspace is sent on that room's topic: the device confines
        // the caller to the room a request arrives on. Otherwise use the
        // first shared room where the device is actually present.
        let named = target
            .provider
            .strip_prefix("workspace:")
            .and_then(RoomId::parse);
        let shared = self.online_enrolled_room(endpoint, named)?;
        let topic = room_topic_id(shared);
        if !self.join_topic(topic) {
            return Err("the shared workspace is not joined".into());
        }
        let message = tilo_actions::remote::RemoteActionMessage::Invoke(
            tilo_actions::remote::InvokeRequest {
                on_behalf_of: None,
                invocation: tilo_actions::InvocationId::new(),
                target,
                argument,
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
            },
        );
        if self.send_remote_action(topic, endpoint, message) {
            Ok(())
        } else {
            Err("the action could not be handed to the enrolled device".into())
        }
    }

    fn room_for_topic(&self, topic: u64) -> Option<RoomId> {
        let replica = self.replica.lock().ok()?;
        replica
            .tree
            .read()
            .rooms()
            .into_iter()
            .find(|room| room_topic_id(*room) == topic)
    }

    fn enrolled_room_candidates(&self) -> Result<Vec<RoomId>, String> {
        let mut enrolled =
            personal_room_ids(&personal_device_peer_path().with_file_name("personal-rooms.json"));
        // The setup session publishes its rooms under the user state
        // directory. The copy on the target lives beside the peer list.
        if let Some(exported) = tilo_identity_host::provisioning::personal_rooms_export_path() {
            for room in personal_room_ids(&exported) {
                if !enrolled.contains(&room) {
                    enrolled.push(room);
                }
            }
        }
        let local = {
            let replica = self
                .replica
                .lock()
                .map_err(|_| "workspace state is unavailable".to_string())?;
            replica.tree.read().rooms()
        };
        Ok(enrolled
            .into_iter()
            .filter(|room| local.contains(room))
            .collect())
    }

    /// The shared room to reach `endpoint` on: `named` when given, else the
    /// first shared room where the device is present.
    fn online_enrolled_room(
        &self,
        endpoint: iroh::EndpointId,
        named: Option<RoomId>,
    ) -> Result<RoomId, String> {
        let candidates = self.enrolled_room_candidates()?;
        let live = self
            .live_room_peers
            .lock()
            .map_err(|_| "workspace state is unavailable".to_string())?;
        let online = |room: RoomId| live.contains(&(room_topic_id(room), endpoint));
        let room = match named {
            Some(room) if candidates.contains(&room) => room,
            Some(_) => return Err("the enrolled device does not share that workspace".into()),
            None => {
                let first = candidates
                    .first()
                    .copied()
                    .ok_or_else(|| "the enrolled device shares no workspace".to_string())?;
                candidates
                    .iter()
                    .copied()
                    .find(|room| online(*room))
                    .unwrap_or(first)
            }
        };
        if online(room) {
            Ok(room)
        } else {
            Err("the enrolled device is not online yet".into())
        }
    }

    /// Sort one authenticated remote-action frame: requests queue for the
    /// broker with the caller the transport proved, answers wake the local
    /// caller waiting on that device. An unauthenticated device reaches
    /// neither, whatever the compatibility mode.
    fn dispatch_remote_action(
        &self,
        workspace_topic: u64,
        origin: &crate::shell::state::PeerOrigin,
        peer: iroh::EndpointId,
        frame: &[u8],
    ) {
        let Some(identity) = self
            .peer_identities
            .get(origin)
            .or_else(|| self.governed_identity(workspace_topic, peer))
        else {
            tracing::warn!(peer = %peer, "dropping remote action from an unauthenticated device");
            return;
        };
        let Ok(frame) = tilo_actions::remote::RemoteActionFrame::decode(frame) else {
            tracing::warn!(peer = %peer, "dropping malformed remote action frame");
            return;
        };
        if frame.message.is_request() {
            if let tilo_actions::remote::RemoteActionMessage::Invoke(request) = &frame.message
                && !identity.capabilities.foreign
                && self
                    .personal_continuity_user
                    .is_some_and(|user| identity.is_same_user(&user))
            {
                self.note_remote_use(&identity.device_cert.device_label, request);
            }
            let mut caller = tilo_actions::remote::Caller::authenticated(
                identity.user_pubkey,
                identity.device_pubkey,
            )
            .from_foreign_device(identity.capabilities.foreign);
            // Opening or closing an app is refused unless the transport
            // confined the caller to the room the frame arrived on.
            if let Some(room) = self.room_for_topic(workspace_topic) {
                caller = caller.in_room(room.to_string());
            }
            if let Ok(mut inbound) = self.inbound_remote_actions.lock() {
                inbound.push(crate::shell::remote_action_link::InboundRemoteAction {
                    workspace_topic,
                    from: peer,
                    caller,
                    message: frame.message,
                });
            }
            self.drain_wake.poke();
        } else if !self.remote_action_replies.deliver(peer, &frame.message) {
            tracing::debug!(peer = %peer, "remote action answer nobody was waiting for");
        }
    }

    /// Keep a request that arrived before its sender's identity hello. A
    /// device that has just joined sends both at once, and the request can
    /// win. Answers are not held: nobody is waiting on an unknown device.
    fn hold_remote_action(&self, held_action: HeldRemoteAction) {
        let request = tilo_actions::remote::RemoteActionFrame::decode(&held_action.frame)
            .is_ok_and(|decoded| decoded.message.is_request());
        if !request {
            tracing::warn!(peer = %held_action.peer, "dropping remote action from an unauthenticated device");
            return;
        }
        let Ok(mut held) = self.held_remote_actions.lock() else {
            return;
        };
        if held.len() >= MAX_HELD_REMOTE_ACTIONS {
            held.pop_front();
        }
        held.push_back(held_action);
    }

    fn drop_held_remote_actions(&self, peer: iroh::EndpointId) {
        if let Ok(mut held) = self.held_remote_actions.lock() {
            held.retain(|item| item.peer != peer);
        }
    }

    /// Dispatch, in arrival order, the requests held for `peer`. Each goes
    /// through the full message path again, so room authority is checked
    /// with the identity that was just accepted.
    fn release_held_remote_actions(&self, peer: iroh::EndpointId) {
        let ready: std::collections::VecDeque<HeldRemoteAction> = {
            let Ok(mut held) = self.held_remote_actions.lock() else {
                return;
            };
            let (ready, rest): (std::collections::VecDeque<_>, std::collections::VecDeque<_>) =
                std::mem::take(&mut *held)
                    .into_iter()
                    .partition(|item| item.peer == peer);
            *held = rest;
            ready
        };
        for item in ready {
            self.dispatch_neighbour_msg(
                item.workspace_topic,
                item.peer,
                item.membership_epoch,
                item.replayed,
                NeighbourMsg::RemoteAction { frame: item.frame },
            );
        }
    }

    /// Hold the session's idle-suspend off while the person works on this
    /// machine from another of their devices; see
    /// [`crate::shell::remote_use`].
    pub fn attach_session_inhibitors(&self, registry: tilo_power_mgmt::InhibitRegistry) {
        if let Ok(mut lease) = self.remote_use.lock() {
            lease.attach(registry);
        }
    }

    /// One of the person's own devices invoked an action here.
    fn note_remote_use(&self, label: &str, request: &tilo_actions::remote::InvokeRequest) {
        let renewal = self
            .remote_use
            .lock()
            .map(|mut lease| lease.renew(label, std::time::Instant::now()));
        // An audit line per invocation: who acted on this machine, and which
        // action, never its resource or argument.
        tracing::info!(
            target: "tilo::remote_actions",
            device = label,
            provider = request.target.provider.as_str(),
            action = request.target.address.action.as_str(),
            "remote action from another of your devices"
        );
        if matches!(renewal, Ok(crate::shell::remote_use::Renewal::Taken)) {
            tracing::info!(
                target: "tilo::power",
                device = label,
                minutes = crate::shell::remote_use::REMOTE_USE_LEASE.as_secs() / 60,
                "remote use keeps this device from idle-suspending"
            );
        }
    }

    /// Shared handle to the cooperative-scrollback response buffer.
    pub fn inbound_scrollback_response_handle(&self) -> Arc<Mutex<Vec<InboundScrollbackResponse>>> {
        Arc::clone(&self.inbound_scrollback_response)
    }

    /// Publish a `NeighbourMsg::ScrollbackRequest`. Called when a local
    /// app's `FetchScrollback` returned `exhausted=true` with an empty
    /// batch — the broker has nothing more in-memory, but the app still
    /// wants the range.
    /// Asked on `cluster_id`'s own topic: only that room's members hold, and
    /// may answer with, its history.
    pub fn publish_scrollback_request(
        &self,
        cluster_id: &str,
        artifact: collab_types::ArtifactId,
        before_seq: u64,
        max: u16,
    ) {
        let Some(room) = RoomId::parse(cluster_id).filter(|room| !room.is_nil()) else {
            return;
        };
        if self.person_root_of_room(room).is_some() || self.is_person_artifact(&artifact) {
            return;
        }
        let msg = NeighbourMsg::ScrollbackRequest {
            artifact,
            before_seq,
            max,
            requester: self.local_peer_id_hash(),
        };
        let ws = room_topic_id(room);
        let _ = self.join_topic(ws) && self.cmd_tx.try_send(NetCmd::Publish { ws, msg }).is_ok();
    }

    /// Publish a `NeighbourMsg::ScrollbackResponse` for an artifact
    /// whose log has matching ops, on the topic the request came from.
    pub fn publish_scrollback_response(
        &self,
        workspace_topic: u64,
        artifact: collab_types::ArtifactId,
        ops: Vec<Box<collab_types::SignedOp>>,
    ) {
        if self.is_person_artifact(&artifact) {
            return;
        }
        let msg = NeighbourMsg::ScrollbackResponse {
            artifact,
            ops,
            responder: self.local_peer_id_hash(),
        };
        let _ = self.join_topic(workspace_topic)
            && self
                .cmd_tx
                .try_send(NetCmd::Publish {
                    ws: workspace_topic,
                    msg,
                })
                .is_ok();
    }

    /// Whether history of an artifact bound to `cluster_id` may be answered
    /// on `workspace_topic`: only on that room's own topic, and never for
    /// the person's own room, which no topic carries.
    pub fn scrollback_answer_allowed(&self, cluster_id: &str, workspace_topic: u64) -> bool {
        RoomId::parse(cluster_id)
            .filter(|room| !room.is_nil() && self.person_root_of_room(*room).is_none())
            .is_some_and(|room| room_topic_id(room) == workspace_topic)
    }

    /// Queue a received `ScrollbackRequest` for the broker's drainer.
    pub fn handle_scrollback_request_message(
        &self,
        workspace_topic: u64,
        requester: u64,
        artifact: collab_types::ArtifactId,
        before_seq: u64,
        max: u16,
    ) {
        if let Ok(mut buf) = self.inbound_scrollback_request.lock() {
            buf.push(InboundScrollbackRequest {
                workspace_topic,
                requester,
                artifact,
                before_seq,
                max,
            });
        }
        self.drain_wake.poke();
    }

    /// Queue a received `ScrollbackResponse` for the broker's drainer.
    pub fn handle_scrollback_response_message(
        &self,
        responder: u64,
        artifact: collab_types::ArtifactId,
        ops: Vec<collab_types::SignedOp>,
    ) {
        if let Ok(mut buf) = self.inbound_scrollback_response.lock() {
            buf.push(InboundScrollbackResponse {
                responder,
                artifact,
                ops,
            });
        }
        self.drain_wake.poke();
    }

    /// Subscribe the actor to every durable room currently in the tree, plus
    /// the compatibility/control topic. Rooms accepted later are joined by
    /// `Self::publish_message`.
    pub fn join_default_workspace(&self) -> bool {
        self.adopt_enrolled_rooms();
        self.export_personal_rooms();
        let mut topics = vec![DEFAULT_WS_TOPIC];
        if let Ok(replica) = self.replica.lock() {
            topics.extend(replica.tree.read().rooms().into_iter().map(room_topic_id));
        }
        topics.extend(
            self.person_roots()
                .map(|root| personal_continuity_topic_id(&root)),
        );
        topics.into_iter().all(|topic| self.join_topic(topic)) && {
            self.publish_sync_requests();
            true
        }
    }

    fn adopt_enrolled_rooms(&self) {
        let path = personal_device_peer_path().with_file_name("personal-rooms.json");
        // Read the file before taking the tree's write lock: the frame
        // painter reads the same tree.
        let rooms = personal_room_ids(&path);
        if rooms.is_empty() {
            return;
        }
        let Ok(replica) = self.replica.lock() else {
            return;
        };
        let mut tree = replica.tree.write();
        for room in rooms {
            tree.join_enrolled_room(room);
        }
    }

    fn export_personal_rooms(&self) {
        let Some(path) = tilo_identity_host::provisioning::personal_rooms_export_path() else {
            return;
        };
        let Ok(replica) = self.replica.lock() else {
            return;
        };
        let ids = replica
            .tree
            .read()
            .rooms()
            .into_iter()
            .map(|room| room.to_string())
            .filter(|id| RoomId::parse(id).is_some())
            .collect::<Vec<_>>();
        drop(replica);
        let Ok(bytes) = serde_json::to_vec(&ids) else {
            return;
        };
        // Runs on every home-peer refresh; the room list rarely changes, and
        // an unchanged rewrite still costs a file and a directory fsync.
        if std::fs::read(&path).is_ok_and(|current| current == bytes) {
            return;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = tilo_files::native::atomic::write(&path, &bytes);
    }

    /// Attach an always-on home or hosted workspace server. The actor keeps
    /// its direct Iroh gossip path and additionally uses the authenticated
    /// host session for offline store-and-forward.
    pub fn connect_host(&self, addr: iroh::EndpointAddr) -> bool {
        if let Ok(mut hosts) = self.host_addrs.lock()
            && !hosts.iter().any(|known| known.id == addr.id)
        {
            hosts.push(addr.clone());
        }
        self.cmd_tx
            .try_send(NetCmd::ConnectHost {
                addr,
                expectation: None,
            })
            .is_ok()
    }

    /// Reload newly approved same-identity home peers. This is safe to call
    /// from the frame loop: it reads a bounded local registry, queues dials
    /// with `try_send`, and never waits for relay discovery or a connection.
    pub fn refresh_home_peer_hosts(&self) -> bool {
        // An enrollment can deliver rooms after this session started. Adopt
        // them and join their topics, or a request sent to this device on
        // such a room never arrives.
        self.adopt_enrolled_rooms();
        let mut changed = self.join_room_topics();
        // Direct peers are how a setup machine reaches the device it
        // enrolled, with or without a relay. A missing or malformed relay
        // registry must not skip them.
        changed |= self.dial_direct_peers();
        let mut relay_workspace_ids = self
            .replica
            .lock()
            .ok()
            .map(|replica| relay_topics_for_home_peers(replica.tree.read().rooms(), None))
            .unwrap_or_default();
        if let Some(user) = self.personal_continuity_user {
            relay_workspace_ids.insert(personal_continuity_topic_id(&user));
        }
        let Ok(desired) =
            registered_home_peer_addrs(self.personal_continuity_user, &relay_workspace_ids)
        else {
            // Preserve existing routes for availability, but fail closed for
            // secrets when current enrollment authority cannot be proven.
            self.update_registered_home_device_ids(std::collections::HashSet::clear);
            // Preserve the last known-good routing set across a partial or
            // malformed registry write. A subsequent bounded refresh retries.
            self.export_personal_rooms();
            return changed;
        };
        let desired_device_ids = desired
            .iter()
            .map(|host| host.expectation.device_public_key)
            .collect::<std::collections::HashSet<_>>();
        self.update_registered_home_device_ids(|devices| *devices = desired_device_ids);
        let desired_ids: std::collections::HashSet<_> =
            desired.iter().map(|host| host.addr.id).collect();
        let previous_ids = self
            .registered_home_peer_ids
            .lock()
            .map(|ids| ids.clone())
            .unwrap_or_default();
        let mut applied_ids = previous_ids.clone();
        for removed in previous_ids.difference(&desired_ids).copied() {
            if self
                .cmd_tx
                .try_send(NetCmd::DisconnectHost { id: removed })
                .is_ok()
            {
                if let Ok(mut hosts) = self.host_addrs.lock() {
                    hosts.retain(|host| host.id != removed);
                }
                applied_ids.remove(&removed);
                changed = true;
            }
        }
        for host in desired {
            let addr = host.addr;
            let current = self
                .host_addrs
                .lock()
                .ok()
                .and_then(|hosts| hosts.iter().find(|known| known.id == addr.id).cloned());
            if current.as_ref() != Some(&addr)
                && self
                    .cmd_tx
                    .try_send(NetCmd::ConnectHost {
                        addr: addr.clone(),
                        expectation: Some(host.expectation),
                    })
                    .is_ok()
            {
                if let Ok(mut hosts) = self.host_addrs.lock() {
                    hosts.retain(|known| known.id != addr.id);
                    hosts.push(addr.clone());
                }
                applied_ids.insert(addr.id);
                changed = true;
            }
        }
        if let Ok(mut ids) = self.registered_home_peer_ids.lock() {
            *ids = applied_ids;
        }
        self.export_personal_rooms();
        changed
    }

    /// Join the topic of every room in the tree. Returns whether a join was
    /// queued.
    fn join_room_topics(&self) -> bool {
        let Some(rooms) = self
            .replica
            .lock()
            .ok()
            .map(|replica| replica.tree.read().rooms())
        else {
            return false;
        };
        let mut queued = false;
        for topic in rooms.into_iter().map(room_topic_id) {
            let new = self
                .joined_topics
                .lock()
                .is_ok_and(|joined| !joined.contains(&topic));
            queued |= new && self.join_topic(topic);
        }
        queued
    }

    /// Dial enrolled devices this session has not reached yet.
    fn dial_direct_peers(&self) -> bool {
        let direct = self.enrolled_personal_devices(Some(*self.local_peer_id.as_bytes()));
        let Ok(connected) = self.dialed_direct_peers.lock() else {
            return false;
        };
        let mut changed = false;
        for addr in direct {
            if connected.contains(&addr.id) {
                continue;
            }
            if self.cmd_tx.try_send(NetCmd::Connect { addr }).is_ok() {
                changed = true;
            }
        }
        changed
    }

    fn refresh_home_peer_hosts_periodically(&self) {
        let Ok(mut last) = self.last_home_peer_refresh.lock() else {
            return;
        };
        let now = std::time::Instant::now();
        if let Some(previous) = *last
            && now.duration_since(previous) < HOME_PEER_REFRESH_INTERVAL
        {
            return;
        }
        *last = Some(now);
        drop(last);
        let _ = self.refresh_home_peer_hosts();
    }

    /// The current iroh address published in an invite. It contains the
    /// stable endpoint id plus fresh direct/relay dialing hints.
    pub fn dial_addr(&self) -> Option<iroh::EndpointAddr> {
        let addrs = self.listen_addrs.lock().ok()?;
        addrs.first().cloned()
    }

    /// The tally of every application message this device's peers and
    /// hosts delivered to it. Blocks until the actor answers, so it must
    /// not be called from inside the network's runtime.
    pub fn wire_tally(&self) -> Option<Arc<crate::network::actor::WireTally>> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.cmd_tx.try_send(NetCmd::WireTally(reply)).ok()?;
        answer.blocking_recv().ok()
    }

    /// Invitations this authority can still resend or cancel. Expired
    /// credentials are removed from both memory and the private on-disk
    /// inventory before rows are returned to the UI.
    #[must_use]
    pub fn pending_invitations(&self, room: RoomId) -> Vec<PendingInvitation> {
        let Ok(mut pending) = self.pending_identity_admissions.lock() else {
            return Vec::new();
        };
        let before = pending.len();
        let now = tilo_clock::unix_now();
        pending.retain(|_, invite| !invite.is_expired(now));
        if pending.len() != before
            && let Some(path) = self.pending_invitations_path.as_deref()
        {
            persist_pending_invitations(path, &pending);
        }
        let mut rows: Vec<_> = pending
            .values()
            .filter(|invite| invite.room == room)
            .cloned()
            .collect();
        rows.sort_by_key(|invite| (invite.expiry_unix_seconds, invite.id));
        rows
    }

    /// Return the original signed token without extending its lifetime or
    /// minting a second grant.
    ///
    /// # Errors
    ///
    /// Returns an error when the inventory is unavailable, expired, canceled,
    /// or does not contain `id`.
    pub fn resend_invitation(&self, id: &[u8; 32]) -> Result<String, String> {
        self.pending_identity_admissions
            .lock()
            .map_err(|_| "invite admission state is unavailable".to_string())?
            .get(id)
            .filter(|invite| !invite.is_expired(tilo_clock::unix_now()))
            .map(|invite| invite.token.clone())
            .ok_or_else(|| "invitation is no longer pending".to_string())
    }

    /// Cancel one exact bearer credential. A later redemption carrying its
    /// hash has no pending authority grant and is refused. Existing members
    /// are unaffected; removing them is a separate governance action. An
    /// invitation a relay issued stays pending until that relay cancels it.
    pub fn cancel_invitation(&self, id: &[u8; 32]) -> bool {
        let room = match self.pending_identity_admissions.lock() {
            Ok(pending) => match pending.get(id) {
                Some(invite) => invite.room,
                None => return false,
            },
            Err(_) => return false,
        };
        if let Err(error) = self.cancel_relay_invitation(room, *id) {
            tracing::warn!(%error, "the relay did not cancel the invitation");
            return false;
        }
        let Ok(mut pending) = self.pending_identity_admissions.lock() else {
            return false;
        };
        if pending.remove(id).is_none() {
            return false;
        }
        if let Some(path) = self.pending_invitations_path.as_deref() {
            persist_pending_invitations(path, &pending);
        }
        true
    }

    /// Export the selected person identity as a non-secret card another Tilo
    /// user can paste into the recipient-bound invite composer.
    ///
    /// # Errors
    ///
    /// Returns an error if the workspace is unknown, replica state is
    /// unavailable, or no selected device certificate can be produced.
    pub fn person_identity_card(
        &self,
        local: crate::shell::state::WorkspaceId,
    ) -> Result<String, String> {
        let room = {
            let replica = self
                .replica
                .lock()
                .map_err(|_| "peer-sync replica lock poisoned".to_string())?;
            replica
                .tree
                .read()
                .room_of(local)
                .ok_or_else(|| format!("no workspace {}", local.raw()))?
        };
        let cert = selected_device_cert(room, &self.keypair)?;
        Ok(format!(
            "tilo-identity:{}",
            hex::encode(cert.user_public_key)
        ))
    }

    /// Mint a signed invite to the workspace `local`, valid for
    /// `expiry_seconds_from_now`. Signs with this shell's own Iroh
    /// key and embeds the address the endpoint actually bound, so the
    /// token a user copies is one a peer can act on.
    ///
    /// # Errors
    ///
    /// Returns an error (as a message) if peer-sync has no dial address
    /// yet, `local` names no known workspace, or the replica lock is
    /// poisoned.
    pub fn issue_invite(
        &self,
        local: crate::shell::state::WorkspaceId,
        expiry_seconds_from_now: u64,
    ) -> Result<String, String> {
        self.issue_invite_for(
            local,
            expiry_seconds_from_now,
            None,
            crate::network::invite::InviteRole::Editor,
            None,
        )
    }

    /// Mint a named invite carrying the exact role the owner selected.
    ///
    /// # Errors
    ///
    /// Same conditions as [`Self::issue_invite`].
    pub fn issue_invite_named_with_role(
        &self,
        local: crate::shell::state::WorkspaceId,
        expiry_seconds_from_now: u64,
        invitee: &str,
        role: crate::network::invite::InviteRole,
    ) -> Result<String, String> {
        self.issue_invite_for(local, expiry_seconds_from_now, Some(invitee), role, None)
    }

    /// Mint a recipient-bound invite. Possessing its link does not grant
    /// admission unless the joining device presents a certificate for the
    /// selected person identity. This is the safe form for offline hosted
    /// admission.
    ///
    /// # Errors
    ///
    /// Same conditions as [`Self::issue_invite`].
    pub fn issue_invite_bound_with_role(
        &self,
        local: crate::shell::state::WorkspaceId,
        expiry_seconds_from_now: u64,
        invitee: &str,
        recipient_user_public_key: [u8; 32],
        role: crate::network::invite::InviteRole,
    ) -> Result<String, String> {
        self.issue_invite_for(
            local,
            expiry_seconds_from_now,
            Some(invitee),
            role,
            Some(recipient_user_public_key),
        )
    }

    /// Mint an invitation on the terms an `invitation.issue` action stated:
    /// any role, lifetime and optional name, recipient-bound when a person
    /// key is given. Same authority checks as every other issuance path.
    ///
    /// # Errors
    ///
    /// Same conditions as [`Self::issue_invite`].
    pub fn issue_invitation(
        &self,
        local: crate::shell::state::WorkspaceId,
        terms: &crate::shell::peer_sync_overlay::InvitationTerms,
    ) -> Result<String, String> {
        self.issue_invite_for(
            local,
            terms.expires_in_seconds,
            terms.invitee_name.as_deref(),
            terms.role,
            terms.recipient_user_public_key,
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "invite issuance keeps membership authorization, recipient binding, expiry, and signing in one validated operation"
    )]
    fn issue_invite_for(
        &self,
        local: crate::shell::state::WorkspaceId,
        expiry_seconds_from_now: u64,
        invitee: Option<&str>,
        role: crate::network::invite::InviteRole,
        recipient_user_public_key: Option<[u8; 32]>,
    ) -> Result<String, String> {
        use crate::shell::invite_policy::InviteRoute;
        // Attach the native owner, when this device has one, so the policy
        // below reads the admitted governor of a relay-governed room. A
        // device without one only holds self-governed rooms.
        if let Err(error) = self.native_owner() {
            tracing::debug!(%error, "inviting without a native owner");
        }
        let tree_handle = {
            let replica = self
                .replica
                .lock()
                .map_err(|_| "peer-sync replica lock poisoned".to_string())?;
            replica.tree.clone()
        };
        let missing = || format!("no workspace {} to invite into", local.raw());
        // The room's governor is read outside the tree's lock (see
        // `invite_policy::native_governor`); the decision below is taken on
        // the workspace as it stands under one read, and issued for it.
        let (room, owner) = {
            let tree = tree_handle.read();
            let room = tree.room_of(local).ok_or_else(missing)?;
            (room, tree.installed_native_owner())
        };
        let governor = crate::shell::invite_policy::native_governor(owner, room);
        let local_user = crate::shell::state::local_user_id_for_room(&room);
        let tree = tree_handle.read();
        let ws = tree
            .desktop
            .workspaces
            .iter()
            .find(|w| w.id == local && w.room == room)
            .cloned()
            .ok_or_else(missing)?;
        // The same decision the invite composer and the palette are gated
        // on: who may invite, and which authority signs it.
        let route =
            crate::shell::invite_policy::may_issue_invitation(&tree, &ws, &local_user, governor);
        drop(tree);
        let relay_room = match route {
            InviteRoute::Relay => Some(
                self.administered_room(ws.room)
                    .ok_or("this workspace's relay is not reachable yet; try again in a moment")?,
            ),
            InviteRoute::ShareOnRelay => self.share_on_home_relay(local, ws.room, &ws.name)?,
            InviteRoute::ViaGovernor | InviteRoute::Refused(_) => {
                return Err(route.refusal().unwrap_or_default().into());
            }
        };
        if let Some(relay_room) = relay_room {
            return self.issue_relay_invitation(
                &relay_room,
                role,
                recipient_user_public_key,
                invitee,
                expiry_seconds_from_now,
            );
        }
        // The invitee will dial this device. Until its address names a home
        // relay, the invitee's dial finds the relay through address lookup.
        self.reachability.wake("an invitation was issued");
        let addr = self
            .dial_addr()
            .ok_or_else(|| "peer-sync is not listening yet; try again in a moment".to_string())?;
        let (token, room) = {
            let hosts = self
                .host_addrs
                .lock()
                .map(|hosts| hosts.clone())
                .unwrap_or_default();
            let pins: Vec<_> = self
                .personal_continuity_user
                .map(|user| {
                    registered_home_peer_addrs(
                        Some(user),
                        &std::collections::BTreeSet::from([room_topic_id(ws.room)]),
                    )
                    .unwrap_or_default()
                    .into_iter()
                    .map(|host| crate::network::invite::HomeHostPin {
                        addr: host.addr,
                        user_public_key: host.expectation.user_public_key,
                        device_public_key: host.expectation.device_public_key,
                    })
                    .collect()
                })
                .unwrap_or_default();
            let device_cert = selected_device_cert(ws.room, &self.keypair)?;
            let token = crate::network::invite::make_invite_token_with_recipient(
                &self.keypair,
                &ws,
                &addr,
                &hosts,
                &pins,
                Some(device_cert),
                crate::network::invite::InviteTerms {
                    role,
                    recipient: recipient_user_public_key.map(|user_public_key| {
                        crate::network::invite::InviteRecipient {
                            user_public_key,
                            name: invitee,
                        }
                    }),
                    expiry_seconds_from_now,
                },
            )
            .map_err(|e| e.to_string())?;
            (token, ws.room)
        };

        // Nobody invites the inviter, so without this the host would be
        // the one peer left unnamed. Self-declared, but through the same
        // authority-checked channel every other name travels on, so it
        // can't be forged. Best-effort: a non-administering machine has
        // the write refused and stays an id rather than failing the invite.
        if let Ok(replica) = self.replica.lock() {
            let me = crate::shell::state::local_user_id_for_room(&room);
            let peer = self.local_peer_id.to_string();
            replica.tree.write().name_peer(room, &me, &peer, &me);
            if let Some(identity) = &self.host_identity {
                replica.tree.write().certify_peer_identity(
                    room,
                    &peer,
                    identity.device_cert.clone(),
                );
            }
            let projection = replica.tree.read().trusted_identity_projection(room);
            crate::shell::launcher::set_trusted_identity_projection(room, &projection);
        }
        self.join_topic(room_topic_id(room));

        let invitee = invitee.map(str::trim).filter(|name| !name.is_empty());
        let parsed = crate::network::invite::validate_invite_token(&token)?;
        let invite_id = crate::network::invite::invite_id(&token);
        let mut pending = self
            .pending_identity_admissions
            .lock()
            .map_err(|_| "invite admission state is unavailable".to_string())?;
        pending.insert(
            invite_id,
            PendingInvitation {
                id: invite_id,
                room,
                name: invitee.unwrap_or("Member").to_string(),
                role,
                token: token.clone(),
                recipient_user_public_key,
                expiry_unix_seconds: parsed.expiry_unix_seconds,
            },
        );
        if let Some(path) = self.pending_invitations_path.as_deref() {
            persist_pending_invitations(path, &pending);
        }
        drop(pending);
        Ok(token)
    }

    /// List live invitations for a locally governed workspace. Only
    /// non-secret metadata is returned through this API; the private bearer
    /// remains available through the explicit resend action.
    ///
    /// # Errors
    ///
    /// Returns an error if the workspace is unknown, replica state is
    /// unavailable, or the local user may not administer the workspace.
    pub fn pending_invites(
        &self,
        local: crate::shell::state::WorkspaceId,
    ) -> Result<Vec<PendingInviteSummary>, String> {
        let room = {
            let replica = self
                .replica
                .lock()
                .map_err(|_| "peer-sync replica lock poisoned".to_string())?;
            let tree = replica.tree.read();
            let workspace = tree
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.id == local)
                .ok_or_else(|| format!("no workspace {}", local.raw()))?;
            let actor = crate::shell::state::local_user_id_for_room(&workspace.room);
            if !tree.governs_room(workspace.room) || !workspace.may_administer(&actor) {
                return Err(
                    "only the governing workspace administrator can inspect invites".into(),
                );
            }
            let room = workspace.room;
            drop(tree);
            drop(replica);
            room
        };
        Ok(self
            .pending_invitations(room)
            .into_iter()
            .map(|invite| PendingInviteSummary {
                invite_id: invite.id_hex(),
                invitee: invite.name,
                role: invite.role,
                recipient_bound: invite.recipient_user_public_key.is_some(),
                expiry_unix_seconds: invite.expiry_unix_seconds,
            })
            .collect())
    }

    /// Revoke a pending invite by the digest returned from
    /// [`Self::pending_invites`]. A digest from another room is never removed.
    ///
    /// # Errors
    ///
    /// Returns an error if the workspace or invite id is invalid, state is
    /// unavailable, or the local user may not administer the workspace.
    pub fn revoke_pending_invite(
        &self,
        local: crate::shell::state::WorkspaceId,
        invite_id: &str,
    ) -> Result<bool, String> {
        let room = {
            let replica = self
                .replica
                .lock()
                .map_err(|_| "peer-sync replica lock poisoned".to_string())?;
            let tree = replica.tree.read();
            let workspace = tree
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.id == local)
                .ok_or_else(|| format!("no workspace {}", local.raw()))?;
            let actor = crate::shell::state::local_user_id_for_room(&workspace.room);
            if !tree.governs_room(workspace.room) || !workspace.may_administer(&actor) {
                return Err("only the governing workspace administrator can revoke invites".into());
            }
            let room = workspace.room;
            drop(tree);
            drop(replica);
            room
        };
        let bytes =
            hex::decode(invite_id).map_err(|_| "invite id is not hexadecimal".to_string())?;
        let id: [u8; 32] = bytes
            .try_into()
            .map_err(|_| "invite id must encode exactly 32 bytes".to_string())?;
        let mut pending = self
            .pending_identity_admissions
            .lock()
            .map_err(|_| "invite admission state is unavailable".to_string())?;
        if pending.get(&id).is_none_or(|invite| invite.room != room) {
            return Ok(false);
        }
        pending.remove(&id);
        if let Some(path) = self.pending_invitations_path.as_deref() {
            persist_pending_invitations(path, &pending);
        }
        drop(pending);
        Ok(true)
    }

    /// Promote a transport-authenticated identity announcement through room
    /// governance and immediately publish the accepted Files projection.
    ///
    /// The endpoint binding is checked here, before governance sees the row;
    /// `certify_peer_identity_with_revocations` then checks the room governor,
    /// governed name, certificate lifetime and every root-signed revocation.
    fn handle_identity_announcement(
        &self,
        peer: &iroh::EndpointId,
        room: &str,
        device_cert: &tilo_user_identity::DeviceCert,
        revocations: &[tilo_user_identity::RevocationEntry],
    ) -> bool {
        let Some(room) = RoomId::parse(room) else {
            return false;
        };
        if device_cert.device_public_key != *peer.as_bytes()
            || tilo_user_identity::verify_device_cert(device_cert).is_err()
        {
            tracing::warn!(%peer, %room, "dropping mismatched device identity announcement");
            return false;
        }

        let projection = {
            let Ok(rep) = self.replica.lock() else {
                tracing::error!(%peer, %room, "replica state is poisoned; refusing identity announcement");
                return false;
            };
            rep.tree.write().certify_peer_identity_with_revocations(
                room,
                &peer.to_string(),
                device_cert.clone(),
                revocations.to_vec(),
            );
            rep.tree.read().trusted_identity_projection(room)
        };
        crate::shell::launcher::set_trusted_identity_projection(room, &projection);
        true
    }

    /// Accept an invite token into `local`, then immediately ask the
    /// room it named to catch us up — replication is per room, so
    /// without that request a freshly accepted invite sits idle until
    /// something else kicks a round. Takes the token *string*, not a
    /// parsed token, so validation cannot be skipped on the way in.
    ///
    /// # Errors
    ///
    /// Returns an error (as a message) if the replica lock is poisoned
    /// or `token` fails to parse/validate.
    pub fn accept_invite(
        &self,
        local: crate::shell::state::WorkspaceId,
        token: &str,
    ) -> Result<crate::shell::state::RoomId, String> {
        let replica = self
            .replica
            .lock()
            .map_err(|_| "peer-sync replica lock poisoned".to_string())?;
        let room = {
            let mut tree = replica.tree.write();
            accept_invite_into(&mut tree, local, token)?
        };
        if !self.join_topic(room_topic_id(room)) {
            return Err("peer-sync command queue is full; try accepting the invite again".into());
        }
        self.publish_identity_announcement(room);
        // Dial the authenticated address carried by the invite.
        // `validate_invite_token` already ran inside `accept_invite_into`,
        // so re-reading the token here cannot let an unverified address through.
        if let Ok(parsed) = crate::network::invite::validate_invite_token(token) {
            if let Ok(mut pending) = self.pending_redemptions.lock() {
                pending.insert(room, crate::network::invite::invite_id(token));
            }
            let _ = self.cmd_tx.try_send(NetCmd::Connect {
                addr: parsed.owner_addr,
            });
            // Connect identity-pinned home hosts first.  Do not also enqueue
            // the same endpoint through the legacy, unpinned host list: that
            // would let an unauthenticated session win the race before the
            // actor has a chance to verify the enrolled device/root pair.
            for pin in &parsed.home_host_pins {
                let _ = self.cmd_tx.try_send(NetCmd::ConnectHost {
                    addr: pin.addr.clone(),
                    expectation: Some(crate::network::actor::HomePeerExpectation {
                        user_public_key: pin.user_public_key,
                        device_public_key: pin.device_public_key,
                    }),
                });
            }
            for host in parsed.host_addrs {
                if !parsed.home_host_pins.iter().any(|pin| pin.addr == host) {
                    let _ = self.connect_host(host);
                }
            }
        }
        replica.request_sync();
        let outgoing = replica.drain_outgoing();
        drop(replica);
        for msg in outgoing {
            let _ = self.publish_message(msg);
        }
        Ok(room)
    }

    fn publish_identity_announcement(&self, room: crate::shell::state::RoomId) {
        let Some(identity) = &self.host_identity else {
            return;
        };
        let (device_cert, revocations) = identity.material_for(room_topic_id(room));
        let _ = self.publish_message(NeighbourMsg::IdentityAnnouncementV2 {
            room: room.to_string(),
            device_cert: Box::new(device_cert),
            revocations,
        });
    }

    /// Ask every room this machine is in for whatever it is missing,
    /// and publish the requests.
    pub fn publish_sync_requests(&self) {
        let Ok(replica) = self.replica.lock() else {
            return;
        };
        let rooms = replica.tree.read().rooms();
        replica.request_sync();
        let outgoing = replica.drain_outgoing();
        drop(replica);
        for room in rooms {
            self.publish_identity_announcement(room);
        }
        for msg in outgoing {
            let _ = self.publish_message(msg);
        }
        self.publish_personal_continuity_request();
        self.publish_personal_continuity_if_changed();
        self.publish_device_fabric_request();
    }

    #[allow(
        clippy::unused_self,
        reason = "tests inject a per-network policy snapshot while production loads the device-local store"
    )]
    fn continuity_policy(&self) -> crate::shell::continuity_policy::ContinuityPolicySnapshot {
        #[cfg(test)]
        if let Some(policy) = self.continuity_policy_override {
            return policy;
        }
        crate::shell::continuity_policy::ContinuityPolicySnapshot::load()
    }

    fn permits_continuity(
        &self,
        capability: crate::shell::continuity_policy::ContinuityCapability,
    ) -> bool {
        self.continuity_policy().permits(capability)
    }

    /// Enforce revocation before every app-continuation boundary. Besides
    /// refusing new wire and persistence work, this removes already staged
    /// remote offers so turning the switch off has immediate local effect.
    fn enforce_app_continuations_policy(&self) -> bool {
        use crate::shell::continuity_policy::ContinuityCapability;

        if self.permits_continuity(ContinuityCapability::AppContinuations) {
            return true;
        }
        if let Ok(mut continuity) = self.personal_continuity.lock() {
            *continuity = crate::shell::personal_continuity::PersonalContinuity::default();
        }
        if let Ok(mut recency) = self.personal_focus_recency.lock() {
            recency.clear();
        }
        if let Ok(mut fingerprint) = self.personal_continuity_fingerprint.lock() {
            *fingerprint = None;
        }
        if let Ok(mut registry) =
            crate::shell::personal_continuity_registry::global_registry().lock()
        {
            registry.clear_remote();
        }
        if let Some(path) = self.personal_continuity_path.as_deref() {
            purge_personal_continuity_store(path);
        }
        if let Some(tree) = self.replica.lock().ok().map(|replica| replica.tree.clone()) {
            let mut tree = tree.write();
            tree.reconcile_continuation_tabs(&std::collections::BTreeSet::new());
            tree.reconcile_continuation_rooms(&std::collections::BTreeSet::new());
        }
        false
    }

    fn local_continuity_snapshot_bytes(&self) -> Option<Vec<u8>> {
        if !self.enforce_app_continuations_policy() {
            return None;
        }
        let device = self.personal_continuity_device.clone()?;
        let tree_handle = self.replica.lock().ok()?.tree.clone();
        let pending_closed_tabs = tree_handle.read().continuation_closed_tabs.clone();

        // Hash a counter-neutral capture so a quiet tree stays quiet. The
        // device counter advances only for a real topology/app change.
        let reports = crate::shell::personal_continuity_registry::global_registry()
            .lock()
            .ok()?
            .clone();
        let neutral =
            crate::shell::personal_continuity::DeviceContinuationSnapshot::capture_with_registry(
                &tree_handle.read(),
                device.clone(),
                0,
                &reports,
            )
            .snapshot;
        let neutral_bytes = postcard::to_allocvec(&neutral).ok()?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(&neutral_bytes);
        hasher.update(&reports.owner_claim_revision().to_le_bytes());
        let fingerprint = *hasher.finalize().as_bytes();

        let mut previous = self.personal_continuity_fingerprint.lock().ok()?;
        if previous.as_ref() == Some(&fingerprint) {
            let continuity = self.personal_continuity.lock().ok()?;
            let bytes = postcard::to_allocvec(&*continuity).ok()?;
            if bytes.len() > MAX_PERSONAL_CONTINUITY_CHECKPOINT_BYTES {
                // Do not let a malformed/stale in-memory value bypass the
                // transport limit merely because the captured tree is quiet.
                tracing::warn!(
                    bytes = bytes.len(),
                    "cached personal continuity checkpoint exceeds transport limit"
                );
                return None;
            }
            drop(continuity);
            // Retry a prior failed durable write even while the captured tree
            // is quiet.
            drop(previous);
            if !self.enforce_app_continuations_policy() {
                return None;
            }
            self.persist_and_acknowledge_owner_claims(reports.owner_claim_revision());
            return Some(bytes);
        }
        let mut counter = self.personal_continuity_counter.lock().ok()?;
        let prior = {
            let continuity = self.personal_continuity.lock().ok()?;
            *counter = continuity.recover_counter(*counter);
            continuity.snapshot_for(&device).cloned()
        };
        *counter = counter.saturating_add(1);
        let mut report =
            crate::shell::personal_continuity::DeviceContinuationSnapshot::capture_with_registry(
                &tree_handle.read(),
                device,
                *counter,
                &reports,
            );
        if let Some(prior) = prior.as_ref() {
            report
                .snapshot
                .preserve_owner_view_revisions(prior, &reports);
        }
        if let Some(prior) = prior.as_ref()
            && let Err(error) = report.snapshot.record_closures_from(prior)
        {
            tracing::warn!(%error, "personal continuity could not record local closures");
            return None;
        }
        if report.redacted_foreign_tabs > 0 {
            tracing::debug!(
                redacted = report.redacted_foreign_tabs,
                "personal continuity omitted non-portable foreign surfaces"
            );
        }
        let mut continuity = self.personal_continuity.lock().ok()?;
        continuity.upsert(report.snapshot).ok()?;
        let bytes = postcard::to_allocvec(&*continuity).ok()?;
        if bytes.len() > MAX_PERSONAL_CONTINUITY_CHECKPOINT_BYTES {
            tracing::warn!(
                bytes = bytes.len(),
                "personal continuity checkpoint exceeds transport limit"
            );
            return None;
        }
        *previous = Some(fingerprint);
        drop(continuity);
        drop(counter);
        drop(previous);
        if !self.enforce_app_continuations_policy() {
            return None;
        }
        let durable = self.persist_and_acknowledge_owner_claims(reports.owner_claim_revision());
        if durable && !pending_closed_tabs.is_empty() {
            let mut tree = tree_handle.write();
            for (tab, room) in pending_closed_tabs {
                if tree.continuation_closed_tabs.get(&tab) == Some(&room) {
                    tree.continuation_closed_tabs.remove(&tab);
                }
            }
        }
        Some(bytes)
    }

    fn persist_and_acknowledge_owner_claims(&self, revision: u64) -> bool {
        self.persist_personal_continuity_with_claims(Some(revision))
    }

    fn persist_personal_continuity(&self) -> bool {
        self.persist_personal_continuity_with_claims(None)
    }

    fn persist_personal_continuity_with_claims(&self, claims: Option<u64>) -> bool {
        if !self.enforce_app_continuations_policy() {
            return false;
        }
        let Some(path) = self.personal_continuity_path.as_deref() else {
            return false;
        };
        let (Ok(counter), Ok(continuity)) = (
            self.personal_continuity_counter.lock(),
            self.personal_continuity.lock(),
        ) else {
            return false;
        };
        let durable = persist_personal_continuity(
            path,
            &PersistedPersonalContinuity {
                counter: *counter,
                continuity: continuity.clone(),
            },
        );
        // The acknowledgement refers to the exact value durably written
        // while these ledger/counter locks remain held, never a later capture.
        if durable
            && let Some(revision) = claims
            && let Some(snapshot) = self
                .personal_continuity_device
                .as_ref()
                .and_then(|device| continuity.snapshot_for(device))
            && let Ok(mut registry) =
                crate::shell::personal_continuity_registry::global_registry().lock()
        {
            registry.acknowledge_owner_claims(revision, snapshot);
        }
        durable
    }

    /// What chrome needs to offer continuing another device's focused tab:
    /// each device's focus and when it moved, plus the certified devices of
    /// this identity with their display labels.
    pub fn continuity_feed(&self) -> crate::shell::continuity_suggestion::ContinuityFeed {
        let observations = self
            .personal_focus_recency
            .lock()
            .map(|recency| recency.observations())
            .unwrap_or_default();
        let certified = self.certified_secret_recipients();
        let fabric = self.device_fabric.lock().ok();
        let devices = certified
            .iter()
            .filter(|device| self.personal_continuity_device.as_ref() != Some(*device))
            .cloned()
            .map(|device| {
                let label = fabric
                    .as_ref()
                    .and_then(|fabric| fabric.snapshots().get(&device))
                    .map_or_else(
                        || crate::shell::i18n::t("continuity-other-device"),
                        |snapshot| snapshot.presence.label.clone(),
                    );
                (device, label)
            })
            .collect();
        drop(fabric);
        crate::shell::continuity_suggestion::ContinuityFeed {
            local_device: self.personal_continuity_device.clone(),
            observations,
            devices,
        }
    }

    pub(crate) fn pump_owner_views(
        &self,
    ) -> Vec<(
        ui::collaboration::owner_view::OwnerView,
        Option<AuthenticatedPersonalRoute>,
    )> {
        let permitted = self.enforce_app_continuations_policy();
        let enrolled: std::collections::HashSet<_> = self
            .enrolled_personal_devices(Some(*self.local_peer_id.as_bytes()))
            .into_iter()
            .map(|peer| peer.id)
            .collect();
        let persons = self
            .peer_identities
            .authenticated_persons(tilo_clock::unix_now_ns());
        let current: std::collections::BTreeMap<_, _> = self
            .authenticated_personal_routes
            .lock()
            .map(|routes| {
                routes
                    .values()
                    .filter(|route| {
                        enrolled.contains(&route.endpoint_id)
                            && persons
                                .get(&crate::shell::state::PeerOrigin::peer(&route.endpoint_id))
                                .is_some_and(|root| {
                                    Some(root) == self.personal_continuity_user.as_ref()
                                })
                    })
                    .map(|route| (route.device.clone(), route.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let continuity = self
            .personal_continuity
            .lock()
            .map(|state| state.clone())
            .unwrap_or_default();
        let registry = crate::shell::personal_continuity_registry::global_registry();
        let Ok(mut registry) = registry.lock() else {
            return Vec::new();
        };
        registry.stop_owner_views_unless(|route| {
            permitted && current.get(&route.device) == Some(route)
        });
        if permitted {
            let candidates: std::collections::BTreeSet<_> = continuity
                .snapshots()
                .values()
                .flat_map(|snapshot| &snapshot.workspaces)
                .flat_map(|workspace| workspace.apps.iter().map(|app| (workspace.room, app.id)))
                .collect();
            let live_keys = candidates
                .iter()
                .filter_map(|(room, id)| {
                    let app = continuity.newest_app_state_in_room(*room, *id)?;
                    registry.owner_view_key(*room, app)
                })
                .collect();
            registry.retain_owner_view_candidates(&live_keys);
            for (room, id) in candidates {
                let Some(app) = continuity.newest_app_state_in_room(room, id) else {
                    continue;
                };
                if self.personal_continuity_device.as_ref() == Some(&app.stamp.device) {
                    if registry.owner_claimed(room, app) {
                        continue;
                    }
                    registry.offer_owner_view(room, app, None);
                } else if let Some(route) = current.get(&app.stamp.device) {
                    registry.offer_owner_view(room, app, Some(route.clone()));
                }
            }
        }
        registry.drain_owner_views()
    }

    /// Return the locally retained, convergent view of this person's devices.
    ///
    /// A deep clone, clipboard histories included: per-frame readers use
    /// [`Self::with_device_fabric`] or [`Self::online_device_ids`] instead.
    pub fn device_fabric(&self) -> crate::shell::device_fabric::DeviceFabric {
        #[cfg(test)]
        self.device_fabric_reads
            .cloned
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.device_fabric.lock().map_or_else(
            |_| crate::shell::device_fabric::DeviceFabric::default(),
            |state| state.clone(),
        )
    }

    /// Read the device fabric in place, without cloning it. `None` when the
    /// lock is poisoned. `f` must not call back into the fabric.
    pub fn with_device_fabric<R>(
        &self,
        f: impl FnOnce(&crate::shell::device_fabric::DeviceFabric) -> R,
    ) -> Option<R> {
        #[cfg(test)]
        self.device_fabric_reads
            .borrowed
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.device_fabric.lock().ok().map(|state| f(&state))
    }

    /// The devices of this person the fabric currently sees online.
    pub fn online_device_ids(
        &self,
    ) -> std::collections::BTreeSet<crate::shell::personal_continuity::DeviceId> {
        self.with_device_fabric(|fabric| {
            fabric
                .online_devices()
                .map(|presence| presence.device.clone())
                .collect()
        })
        .unwrap_or_default()
    }

    /// `(clones, borrows)` of the device fabric so far.
    #[cfg(test)]
    pub(crate) fn device_fabric_reads(&self) -> (usize, usize) {
        use std::sync::atomic::Ordering::Relaxed;
        (
            self.device_fabric_reads.cloned.load(Relaxed),
            self.device_fabric_reads.borrowed.load(Relaxed),
        )
    }

    /// Stop only the outgoing transfer stream bound to this exact certified
    /// target/id. The receiver keeps its last durable prefix.
    ///
    /// # Errors
    ///
    /// Returns an error for a malformed target or a full command queue.
    pub(crate) fn cancel_nearby_transfer_stream(
        &self,
        target: &crate::shell::personal_continuity::DeviceId,
        transfer_id: [u8; 16],
    ) -> Result<(), String> {
        let target_bytes: [u8; 32] = hex::decode(target.as_str())
            .map_err(|_| "invalid transfer target")?
            .try_into()
            .map_err(|_| "invalid transfer target")?;
        let target =
            iroh::EndpointId::from_bytes(&target_bytes).map_err(|_| "invalid transfer target")?;
        self.cmd_tx
            .try_send(NetCmd::CancelNearbyTransfer {
                target,
                transfer_id,
            })
            .map_err(|error| error.to_string())
    }

    /// Stream an immutable Nearby spool object from a receiver-confirmed
    /// checkpoint to one exact certified transport peer.
    pub(crate) fn start_nearby_transfer(
        &self,
        target: &crate::shell::personal_continuity::DeviceId,
        transfer: NearbyTransferCheckpoint,
        path: &Path,
    ) -> Result<(), String> {
        let user_public_key = self
            .personal_continuity_user
            .ok_or_else(|| "this device has no certified personal identity".to_string())?;
        let source = self
            .personal_continuity_device
            .as_ref()
            .ok_or_else(|| "this device has no personal fabric identity".to_string())?;
        if source == target || !self.certified_secret_recipients().contains(target) {
            return Err("target is not another enrolled personal device".into());
        }
        let target_bytes: [u8; 32] = hex::decode(target.as_str())
            .map_err(|_| "invalid transfer target")?
            .try_into()
            .map_err(|_| "invalid transfer target")?;
        let transport_target =
            iroh::EndpointId::from_bytes(&target_bytes).map_err(|_| "invalid transfer target")?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|error| error.to_string())?;
        if !file
            .metadata()
            .map_err(|error| error.to_string())?
            .is_file()
        {
            return Err("nearby spool object is unsafe".into());
        }
        let ws = personal_continuity_topic_id(&user_public_key);
        if !self.join_topic(ws) {
            return Err("personal fabric transport is unavailable".into());
        }
        self.cmd_tx
            .try_send(NetCmd::PublishNearbyTransfer {
                ws,
                user_public_key,
                target: transport_target,
                transfer,
                file,
            })
            .map_err(|error| error.to_string())
    }

    /// Send one non-durable command to exactly one certified personal device.
    ///
    /// # Errors
    ///
    /// Returns an error when personal identity is unavailable, the target is
    /// malformed/local, the action fails the bounded wire contract, or the
    /// transport cannot take it right now.
    pub fn send_personal_fabric_action(
        &self,
        target: &crate::shell::personal_continuity::DeviceId,
        action: crate::shell::device_fabric::PersonalFabricAction,
    ) -> Result<[u8; 16], String> {
        self.try_send_personal_fabric_action(target, action)
            .map_err(|error| error.to_string())
    }

    /// [`Self::send_personal_fabric_action`] that tells a momentarily full
    /// transport queue apart from a failure retrying cannot fix, so
    /// loss-tolerant media can drop one message and keep its route.
    ///
    /// # Errors
    ///
    /// Returns [`PersonalFabricSendError::Backpressure`] when the transport
    /// command queue is full, and [`PersonalFabricSendError::Rejected`] for
    /// every other failure.
    pub fn try_send_personal_fabric_action(
        &self,
        target: &crate::shell::personal_continuity::DeviceId,
        action: crate::shell::device_fabric::PersonalFabricAction,
    ) -> Result<[u8; 16], PersonalFabricSendError> {
        let (request_id, command) = self
            .personal_fabric_command(target, action)
            .map_err(PersonalFabricSendError::Rejected)?;
        self.cmd_tx.try_send(command).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => PersonalFabricSendError::Backpressure,
            mpsc::error::TrySendError::Closed(_) => {
                PersonalFabricSendError::Rejected("personal fabric transport is closed".into())
            }
        })?;
        Ok(request_id)
    }

    fn personal_fabric_command(
        &self,
        target: &crate::shell::personal_continuity::DeviceId,
        action: crate::shell::device_fabric::PersonalFabricAction,
    ) -> Result<([u8; 16], NetCmd), String> {
        let user_public_key = self
            .personal_continuity_user
            .ok_or_else(|| "this device has no certified personal identity".to_string())?;
        let source = self
            .personal_continuity_device
            .clone()
            .ok_or_else(|| "this device has no personal fabric identity".to_string())?;
        if &source == target {
            return Err("personal fabric actions require another device".into());
        }
        if !self.certified_secret_recipients().contains(target) {
            return Err("target is not an enrolled personal device".into());
        }
        let target_bytes = hex::decode(target.as_str())
            .map_err(|_| "target is not a certified transport device id".to_string())?;
        let target_bytes: [u8; 32] = target_bytes
            .try_into()
            .map_err(|_| "target is not a certified transport device id".to_string())?;
        let transport_target = iroh::EndpointId::from_bytes(&target_bytes)
            .map_err(|_| "target is not a certified transport device id".to_string())?;

        let mut request_id = [0; 16];
        while request_id == [0; 16] {
            UnwrapErr(SysRng).fill_bytes(&mut request_id);
        }
        let envelope = crate::shell::device_fabric::PersonalFabricActionEnvelope {
            request_id,
            source,
            target: target.clone(),
            issued_unix_ms: unix_now_ms(),
            action,
        };
        envelope
            .validate_at(&envelope.source, target, envelope.issued_unix_ms)
            .map_err(|error| error.to_string())?;
        let bytes = postcard::to_allocvec(&envelope).map_err(|error| error.to_string())?;
        let ws = personal_continuity_topic_id(&user_public_key);
        if !self.join_topic(ws) {
            return Err("personal fabric transport is unavailable".into());
        }
        Ok((
            request_id,
            NetCmd::PublishPersonalFabricTarget {
                ws,
                user_public_key,
                target: transport_target,
                payload: PersonalFabricPayload::Action(bytes),
            },
        ))
    }

    /// Send only while the exact actor membership authenticated by the caller
    /// is still current. Holding the route registry through command enqueue
    /// gives departure a defined ordering: either enqueue wins, or departure
    /// removes authority first and this call fails without sending.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the route registry guard is held through enqueue on purpose: it orders this \
                  send against departure, which must remove authority before or after it"
    )]
    pub(crate) fn send_personal_fabric_action_on_route(
        &self,
        route: &AuthenticatedPersonalRoute,
        action: crate::shell::device_fabric::PersonalFabricAction,
    ) -> Result<[u8; 16], String> {
        let routes = self
            .authenticated_personal_routes
            .lock()
            .map_err(|_| "personal route authority is unavailable".to_string())?;
        if routes.get(&(route.workspace_topic, route.endpoint_id)) != Some(route) {
            return Err("personal route is no longer current".into());
        }
        let sent = self.send_personal_fabric_action(&route.device, action);
        drop(routes);
        sent
    }

    pub(crate) fn current_personal_route(
        &self,
        device: &crate::shell::personal_continuity::DeviceId,
    ) -> Option<AuthenticatedPersonalRoute> {
        self.authenticated_personal_routes
            .lock()
            .ok()?
            .values()
            .find(|route| &route.device == device)
            .cloned()
    }

    pub(crate) fn personal_route_is_current(&self, route: &AuthenticatedPersonalRoute) -> bool {
        self.authenticated_personal_routes
            .lock()
            .is_ok_and(|routes| {
                routes.get(&(route.workspace_topic, route.endpoint_id)) == Some(route)
            })
    }

    pub(crate) fn with_current_personal_route<T>(
        &self,
        route: &AuthenticatedPersonalRoute,
        operation: impl FnOnce() -> T,
    ) -> Option<T> {
        let routes = self.authenticated_personal_routes.lock().ok()?;
        (routes.get(&(route.workspace_topic, route.endpoint_id)) == Some(route)).then(operation)
    }

    #[cfg(test)]
    pub(crate) fn install_personal_route_for_test(
        &self,
        workspace_topic: u64,
        endpoint_id: iroh::EndpointId,
        membership_epoch: crate::network::actor::MembershipEpoch,
        user_public_key: [u8; 32],
    ) -> AuthenticatedPersonalRoute {
        let route = AuthenticatedPersonalRoute {
            workspace_topic,
            endpoint_id,
            membership_epoch,
            user_public_key,
            device: crate::shell::personal_continuity::DeviceId::new(hex::encode(
                endpoint_id.as_bytes(),
            ))
            .expect("endpoint id is a valid test device"),
        };
        self.authenticated_personal_routes
            .lock()
            .expect("test personal routes")
            .insert((workspace_topic, endpoint_id), route.clone());
        route
    }

    /// Drain validated actions. They are never persisted or included in the
    /// convergent snapshot, so a stale command cannot execute after restart.
    pub(crate) fn take_personal_fabric_actions(&self) -> Vec<ReceivedPersonalFabricAction> {
        self.personal_fabric_actions
            .lock()
            .map_or_else(|_| Vec::new(), |mut actions| actions.drain(..).collect())
    }

    fn display_observations_path(path: &Path) -> PathBuf {
        path.with_file_name("personal-display-observations.json")
    }

    fn persist_device_fabric(
        &self,
        path: &Path,
        fabric: &crate::shell::device_fabric::DeviceFabric,
    ) -> Result<(), String> {
        persist_device_fabric(path, fabric)?;
        self.persist_display_observations(path, fabric)
    }

    fn persist_display_observations(
        &self,
        path: &Path,
        fabric: &crate::shell::device_fabric::DeviceFabric,
    ) -> Result<(), String> {
        if let Some(user) = self.personal_continuity_user {
            display_observations::save(&Self::display_observations_path(path), fabric, user)?;
        }
        Ok(())
    }

    pub(crate) fn restore_display_observations(&self) {
        let (Some(path), Some(user)) = (
            self.device_fabric_path.as_deref(),
            self.personal_continuity_user,
        ) else {
            return;
        };
        let enrolled = self.certified_secret_recipients();
        if let Ok(mut fabric) = self.device_fabric.lock()
            && let Err(error) = display_observations::restore(
                &Self::display_observations_path(path),
                &mut fabric,
                user,
                &enrolled,
            )
        {
            tracing::warn!(%error, "cached display observations were not admitted");
        }
    }

    pub(crate) fn personal_device_is_enrolled(
        &self,
        device: &crate::shell::personal_continuity::DeviceId,
    ) -> bool {
        self.personal_continuity_user.is_some()
            && self.certified_secret_recipients().contains(device)
    }

    /// Retain offline facts without treating a checkpoint as live reachability.
    pub(crate) fn person_display_devices(
        &self,
        local: &crate::shell::personal_continuity::DeviceId,
    ) -> Vec<settings_store::person_displays::PersonDevice> {
        if self.personal_continuity_user.is_none()
            || self.personal_continuity_device.as_ref() != Some(local)
        {
            return Vec::new();
        }
        let enrolled = self.certified_secret_recipients();
        self.device_fabric()
            .person_devices(local)
            .into_iter()
            .filter(|device| {
                crate::shell::personal_continuity::DeviceId::new(device.device.clone())
                    .is_ok_and(|owner| enrolled.contains(&owner))
            })
            .map(|mut device| {
                device.online = device.local
                    || (device.online
                        && crate::shell::personal_continuity::DeviceId::new(device.device.clone())
                            .is_ok_and(|owner| self.current_personal_route(&owner).is_some()));
                device
            })
            .collect()
    }

    fn observe_display_outputs(
        &self,
        owner: &crate::shell::personal_continuity::DeviceId,
        stamp: &crate::shell::device_fabric::FabricStamp,
        outputs: Vec<crate::shell::device_fabric::DisplayOutputObservation>,
    ) -> Result<bool, crate::shell::device_fabric::FabricError> {
        let mut fabric = self
            .device_fabric
            .lock()
            .map_err(|_| crate::shell::device_fabric::FabricError::InvalidCheckpoint)?;
        let changed = fabric.observe_display_outputs(owner, stamp, outputs)?;
        if changed
            && let Some(path) = self.device_fabric_path.as_deref()
            && let Err(error) = self.persist_display_observations(path, &fabric)
        {
            tracing::warn!(%error, "display observation persistence failed");
        }
        drop(fabric);
        Ok(changed)
    }

    /// Replace this machine's self-authored fabric snapshot and advertise it
    /// to currently connected, certified devices of the same person.
    ///
    /// # Errors
    ///
    /// Returns an error when the snapshot is invalid or names another device.
    pub fn upsert_local_device_fabric(
        &self,
        snapshot: crate::shell::device_fabric::DeviceFabricSnapshot,
    ) -> Result<bool, crate::shell::device_fabric::FabricError> {
        if self
            .personal_continuity_device
            .as_ref()
            .is_none_or(|device| device != &snapshot.owner)
        {
            return Err(crate::shell::device_fabric::FabricError::ForeignAuthorship);
        }
        let changed = {
            let mut fabric = self
                .device_fabric
                .lock()
                .map_err(|_| crate::shell::device_fabric::FabricError::InvalidCheckpoint)?;
            let changed = fabric.upsert(snapshot)?;
            if changed
                && let Some(path) = self.device_fabric_path.as_deref()
                && let Err(error) = self.persist_device_fabric(path, &fabric)
            {
                tracing::warn!(%error, "device fabric persistence failed");
            }
            drop(fabric);
            changed
        };
        if changed {
            self.publish_device_fabric_snapshot();
        }
        Ok(changed)
    }

    /// Advertise this compositor's rich hardware observations for its exact
    /// display revision. These facts confer no input or placement authority.
    /// # Errors
    /// Rejects metadata without the local owner identity or current revision.
    pub fn publish_local_display_outputs(
        &self,
        stamp: &crate::shell::device_fabric::FabricStamp,
        outputs: Vec<crate::shell::device_fabric::DisplayOutputObservation>,
    ) -> Result<bool, crate::shell::device_fabric::FabricError> {
        use crate::shell::device_fabric::FabricError;
        if !self
            .permits_continuity(crate::shell::continuity_policy::ContinuityCapability::DisplayInput)
            || self.personal_continuity_device.as_ref() != Some(&stamp.device)
        {
            return Err(FabricError::ForeignAuthorship);
        }
        let changed = self.observe_display_outputs(&stamp.device, stamp, outputs.clone())?;
        if changed {
            self.publish_device_fabric_payload(PersonalFabricPayload::DisplayOutputs {
                stamp: stamp.clone(),
                outputs,
            });
        }
        Ok(changed)
    }

    fn display_input_declines_path(&self) -> Result<(PathBuf, [u8; 32]), String> {
        let user = self
            .personal_continuity_user
            .ok_or("display input has no selected identity")?;
        let path = self
            .device_fabric_path
            .as_ref()
            .ok_or("display input has no private state directory")?;
        let name = format!(
            "personal-display-input-declines-{}.json",
            blake3::hash(&user).to_hex()
        );
        Ok((path.with_file_name(name), user))
    }

    pub(crate) fn load_display_input_declines(
        &self,
    ) -> Result<std::collections::BTreeSet<crate::shell::personal_continuity::DeviceId>, String>
    {
        let (path, user) = self.display_input_declines_path()?;
        input_declines::load(&path, user)
    }

    pub(crate) fn save_display_input_declines(
        &self,
        devices: &std::collections::BTreeSet<crate::shell::personal_continuity::DeviceId>,
    ) -> Result<(), String> {
        let (path, user) = self.display_input_declines_path()?;
        input_declines::save(&path, user, devices)
    }

    /// The person whose devices form this device's personal fabric.
    pub fn personal_user_public_key(&self) -> Option<[u8; 32]> {
        self.personal_continuity_user
    }

    /// Delete everything `device` published to the personal fabric, here and
    /// in the persisted checkpoint, once it has left the person's fleet.
    pub fn forget_device_fabric(&self, device: &crate::shell::personal_continuity::DeviceId) {
        let Ok(mut fabric) = self.device_fabric.lock() else {
            return;
        };
        if fabric.remove_device(device)
            && let Some(path) = self.device_fabric_path.as_deref()
            && let Err(error) = self.persist_device_fabric(path, &fabric)
        {
            tracing::warn!(%error, "device fabric persistence failed");
        }
    }

    /// Change this device's disk-unlock state in its own snapshot and
    /// publish it. `update` receives the stamp the change is published under.
    ///
    /// # Errors
    ///
    /// Returns an error when this device has no fabric identity or presence
    /// yet, or the changed snapshot is invalid.
    pub fn update_local_unlock_state(
        &self,
        update: impl FnOnce(
            &mut crate::shell::device_unlock::UnlockFabricState,
            &crate::shell::device_fabric::FabricStamp,
        ),
    ) -> Result<(), String> {
        let local = self
            .personal_continuity_device
            .clone()
            .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-no-identity"))?;
        let fabric = self.device_fabric();
        let mut snapshot = fabric
            .snapshots()
            .get(&local)
            .cloned()
            .ok_or_else(|| crate::shell::i18n::t("wifi-share-error-no-presence"))?;
        let stamp = crate::shell::device_fabric::FabricStamp {
            counter: fabric.max_counter().saturating_add(1).max(1),
            device: local,
        };
        update(&mut snapshot.unlock, &stamp);
        snapshot.presence.stamp = stamp;
        snapshot.presence.last_seen_unix_ms = tilo_clock::unix_now().saturating_mul(1000);
        self.upsert_local_device_fabric(snapshot)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Encrypt and publish a newly learned Wi-Fi passphrase to the local
    /// device plus every non-revoked home device in the enrollment registry.
    ///
    /// # Errors
    ///
    /// Returns an error when identity/fabric state is unavailable or sealing
    /// and validation fail. The passphrase is never logged or persisted in
    /// plaintext by this path.
    pub fn share_wifi_credential(
        &self,
        ssid: &str,
        security: os_actions::WifiSecurity,
        password: &str,
    ) -> Result<(), String> {
        if !self
            .permits_continuity(crate::shell::continuity_policy::ContinuityCapability::WifiSettings)
        {
            return Err(crate::shell::i18n::t("tethering-error-continuity-off"));
        }
        if !security.supports_portable_passphrase() {
            return Err(crate::shell::i18n::t(match security {
                os_actions::WifiSecurity::Enterprise => "wifi-share-error-enterprise",
                os_actions::WifiSecurity::Open | os_actions::WifiSecurity::Owe => {
                    "wifi-share-error-no-passphrase"
                }
                os_actions::WifiSecurity::Unknown => "wifi-share-error-unknown-security",
                os_actions::WifiSecurity::Wpa2 | os_actions::WifiSecurity::Wpa3 => {
                    "wifi-share-error-unvalidated"
                }
            }));
        }
        let local = self
            .personal_continuity_device
            .clone()
            .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-no-identity"))?;
        let fabric = self.device_fabric();
        let mut snapshot = fabric
            .snapshots()
            .get(&local)
            .cloned()
            .ok_or_else(|| crate::shell::i18n::t("wifi-share-error-no-presence"))?;
        let recipients = self.certified_secret_recipients();
        let stamp = crate::shell::device_fabric::FabricStamp {
            counter: fabric.max_counter().saturating_add(1).max(1),
            device: local,
        };
        let credential = crate::shell::device_fabric_secrets::seal_wifi_credential(
            ssid,
            security,
            password,
            &snapshot.owner,
            recipients.iter().cloned(),
            stamp.clone(),
        )
        .map_err(|error| error.to_string())?;
        snapshot
            .wifi
            .retain(|saved| saved.network_id != credential.network_id);
        snapshot.wifi.push(credential);
        snapshot.presence.stamp = stamp;
        snapshot.presence.last_seen_unix_ms = tilo_clock::unix_now().saturating_mul(1000);
        self.upsert_local_device_fabric(snapshot)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Decrypt a retained credential only for this certified device.
    /// The returned string is intended for an immediate `NetworkManager` call.
    pub fn shared_wifi_password(
        &self,
        ssid: &str,
        security: os_actions::WifiSecurity,
    ) -> Option<String> {
        if !self
            .permits_continuity(crate::shell::continuity_policy::ContinuityCapability::WifiSettings)
        {
            return None;
        }
        if !security.supports_portable_passphrase() {
            return None;
        }
        let recipient_key = &self.host_identity.as_ref()?.device_signing_key;
        self.device_fabric()
            .merged_wifi()
            .into_values()
            .filter(|credential| credential.ssid == ssid && credential.security == security)
            .find_map(|credential| {
                crate::shell::device_fabric_secrets::open_wifi_credential(
                    &credential,
                    recipient_key,
                )
                .ok()
            })
    }

    /// Seal one ephemeral tethering credential to exactly one certified
    /// requester. Unlike roaming Wi-Fi continuity this does not mutate or
    /// persist the local fabric snapshot.
    pub(crate) fn seal_tethering_credential(
        &self,
        requester: &crate::shell::personal_continuity::DeviceId,
        ssid: &str,
        password: &str,
    ) -> Result<crate::shell::device_fabric::WifiCredential, String> {
        let author = self
            .personal_continuity_device
            .clone()
            .ok_or_else(|| "this device has no certified personal identity".to_string())?;
        if !self.certified_secret_recipients().contains(requester) {
            return Err("tethering requester is not an enrolled personal device".into());
        }
        crate::shell::device_fabric_secrets::seal_wifi_credential(
            ssid,
            os_actions::WifiSecurity::Wpa2,
            password,
            &author,
            std::collections::BTreeSet::from([requester.clone()]),
            crate::shell::device_fabric::FabricStamp {
                counter: self.device_fabric().max_counter().saturating_add(1).max(1),
                device: author.clone(),
            },
        )
        .map_err(|error| error.to_string())
    }

    /// Open one already route-validated tethering envelope for an immediate
    /// `NetworkManager` call. The plaintext is never retained by `ShellNetwork`.
    pub(crate) fn open_tethering_credential(
        &self,
        credential: &crate::shell::device_fabric::WifiCredential,
    ) -> Result<String, String> {
        let key = &self
            .host_identity
            .as_ref()
            .ok_or_else(|| "this device has no certified personal identity".to_string())?
            .device_signing_key;
        crate::shell::device_fabric_secrets::open_wifi_credential(credential, key)
            .map_err(|error| error.to_string())
    }

    /// Rewrap credentials this device can open whenever certified device
    /// membership changes. This is decentralized: any online recipient can
    /// onboard a new device without uploading a universal key to a service.
    ///
    /// # Errors
    ///
    /// Returns an error if a retained envelope is malformed or resealing the
    /// credential into the local authored snapshot fails.
    pub fn refresh_wifi_credential_recipients(&self) -> Result<bool, String> {
        let Some(recipient_key) = self
            .host_identity
            .as_ref()
            .map(|identity| &identity.device_signing_key)
        else {
            return Ok(false);
        };
        let desired = self.certified_secret_recipients();
        let credentials = self.device_fabric().merged_wifi().into_values();
        let mut changed = false;
        for credential in credentials {
            if !credential.security.supports_portable_passphrase() {
                continue;
            }
            let current =
                crate::shell::device_fabric_secrets::wifi_envelope_recipients(&credential)
                    .map_err(|error| error.to_string())?;
            if current == *desired {
                continue;
            }
            let Ok(password) = crate::shell::device_fabric_secrets::open_wifi_credential(
                &credential,
                recipient_key,
            ) else {
                continue;
            };
            self.share_wifi_credential(&credential.ssid, credential.security, &password)?;
            changed = true;
        }
        Ok(changed)
    }

    /// This device, the registered home devices and the enrolled personal
    /// devices: who may receive personal secrets and fabric actions.
    ///
    /// Consulted per outgoing message, so it is served from
    /// [`Self::personal_device_peers`]: the enrolment file is re-validated at
    /// most once per recheck interval, and the set is rebuilt only when that
    /// file or the registered home-device ids change.
    fn certified_secret_recipients(
        &self,
    ) -> Arc<std::collections::BTreeSet<crate::shell::personal_continuity::DeviceId>> {
        let local = self.personal_continuity_device.as_ref();
        let build = |enrolled: &[[u8; 32]]| {
            let mut recipients = local
                .cloned()
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>();
            if let Ok(devices) = self.registered_home_device_ids.lock() {
                recipients.extend(devices.iter().filter_map(|device| {
                    crate::shell::personal_continuity::DeviceId::new(hex::encode(device)).ok()
                }));
            }
            recipients.extend(enrolled.iter().filter_map(|device| {
                crate::shell::personal_continuity::DeviceId::new(hex::encode(device)).ok()
            }));
            recipients
        };
        let Ok(mut peers) = self.personal_device_peers.lock() else {
            // Fail closed for secrets: nobody but this device.
            return Arc::new(local.cloned().into_iter().collect());
        };
        peers.recipients_at(
            personal_device_peer_path,
            std::time::Instant::now(),
            local,
            build,
        )
    }

    /// Enrolled personal-device endpoints other than `local_device`, from the
    /// bounded-staleness cache of the enrolment file.
    fn enrolled_personal_devices(&self, local_device: Option<[u8; 32]>) -> Vec<iroh::EndpointAddr> {
        let devices = self.personal_device_peers.lock().map_or_else(
            |_| Arc::from([]),
            |mut peers| peers.devices_at(personal_device_peer_path, std::time::Instant::now()),
        );
        enrolled_endpoints(&devices, local_device)
    }

    /// Re-validate the enrolment file on the next query, as a due recheck
    /// would: for tests that rewrite it mid-session.
    #[cfg(test)]
    fn recheck_personal_device_peers(&self) {
        if let Ok(mut peers) = self.personal_device_peers.lock() {
            peers.expire();
        }
    }

    /// Change the registered home-device ids, and drop the recipient set
    /// derived from them.
    fn update_registered_home_device_ids(
        &self,
        update: impl FnOnce(&mut std::collections::HashSet<[u8; 32]>),
    ) {
        if let Ok(mut devices) = self.registered_home_device_ids.lock() {
            update(&mut devices);
        }
        if let Ok(mut peers) = self.personal_device_peers.lock() {
            peers.invalidate_recipients();
        }
    }

    fn local_device_fabric_checkpoint(&self) -> Option<Vec<u8>> {
        let local = self.personal_continuity_device.as_ref()?;
        let snapshot = self
            .device_fabric
            .lock()
            .ok()?
            .snapshots()
            .get(local)
            .cloned()?;
        let mut single = crate::shell::device_fabric::DeviceFabric::default();
        single.upsert(snapshot).ok()?;
        single.encode_checkpoint().ok()
    }

    fn publish_device_fabric_payload(&self, payload: PersonalFabricPayload) {
        // The fabric has capability-specific gates at each producer, but the
        // master switch is also a strict transport kill switch: not even an
        // otherwise content-free anti-entropy request may leave this device.
        // Enrolled devices are an explicit grant made by the setup machine.
        // Presence and actions between those devices stay available when the
        // optional continuity features are switched off.
        if !self.personal_fabric_allowed() {
            return;
        }
        let Some(user_public_key) = self.personal_continuity_user else {
            return;
        };
        let ws = personal_continuity_topic_id(&user_public_key);
        if self.join_topic(ws) {
            let _ = self.cmd_tx.try_send(NetCmd::PublishPersonalFabric {
                ws,
                user_public_key,
                payload,
            });
        }
    }

    /// The continuity master switch, unless the setup machine enrolled
    /// personal devices: that enrollment is itself the grant.
    fn personal_fabric_allowed(&self) -> bool {
        self.continuity_policy().enabled()
            || !self
                .enrolled_personal_devices(Some(*self.local_peer_id.as_bytes()))
                .is_empty()
    }

    fn publish_device_fabric_request(&self) {
        self.publish_device_fabric_payload(PersonalFabricPayload::Request);
    }

    fn publish_device_fabric_snapshot(&self) {
        if let Some(snapshot) = self.local_device_fabric_checkpoint() {
            self.publish_device_fabric_payload(PersonalFabricPayload::Snapshot(snapshot));
        }
        if self
            .permits_continuity(crate::shell::continuity_policy::ContinuityCapability::DisplayInput)
            && let Some(local) = self.personal_continuity_device.as_ref()
            && let Some(display) = self
                .device_fabric()
                .snapshots()
                .get(local)
                .and_then(|snapshot| snapshot.display.clone())
        {
            let rich_outputs = self
                .device_fabric()
                .display_outputs(local)
                .map(<[_]>::to_vec);
            self.publish_device_fabric_payload(PersonalFabricPayload::DisplayConnectors {
                stamp: display.stamp.clone(),
                outputs: display.outputs,
            });
            if let Some(outputs) = rich_outputs {
                self.publish_device_fabric_payload(PersonalFabricPayload::DisplayOutputs {
                    stamp: display.stamp,
                    outputs,
                });
            }
        }
    }

    fn publish_personal_continuity_request(&self) {
        if !self.enforce_app_continuations_policy() {
            return;
        }
        let Some(user_public_key) = self.personal_continuity_user else {
            return;
        };
        let _ = self.publish_message(NeighbourMsg::PersonalContinuityRequest {
            user_public_key,
            requester: self.local_peer_id_hash(),
        });
    }

    fn publish_personal_continuity_if_changed(&self) {
        if !self.enforce_app_continuations_policy() {
            return;
        }
        let Some(user_public_key) = self.personal_continuity_user else {
            return;
        };
        let before = self
            .personal_continuity_fingerprint
            .lock()
            .ok()
            .and_then(|fingerprint| *fingerprint);
        let Some(snapshot) = self.local_continuity_snapshot_bytes() else {
            return;
        };
        let after = self
            .personal_continuity_fingerprint
            .lock()
            .ok()
            .and_then(|fingerprint| *fingerprint);
        if before == after && before.is_some() {
            return;
        }
        let msg = NeighbourMsg::PersonalContinuityCheckpoint {
            user_public_key,
            publisher: self.local_peer_id_hash(),
            snapshot,
        };
        let ws = personal_continuity_topic_id(&user_public_key);
        if self.enforce_app_continuations_policy() && self.join_topic(ws) {
            let _ = self.cmd_tx.try_send(NetCmd::PublishCheckpoint { ws, msg });
        }
    }

    /// Tell the tree which person stands behind each authenticated peer, so
    /// room admission can let any device of a room's owner govern it. Rebuilt
    /// from the identity store, so a revoked or expired device drops out on
    /// the next refresh.
    fn refresh_authenticated_persons(&self) {
        let persons = self.peer_identities.authenticated_persons(now_unix_nanos());
        let Some(tree) = self.replica.lock().ok().map(|replica| replica.tree.clone()) else {
            return;
        };
        if tree.read().authenticated_persons != persons {
            tree.write().authenticated_persons = persons;
        }
    }

    /// The person this device acts as in the owner field of their own
    /// workspaces; `None` on a device certified for no person.
    fn personal_person(&self) -> Option<crate::shell::state::UserId> {
        let user = self.personal_continuity_user?;
        match collab_types::caps::Principal::person_from_key(
            collab_types::crypto::Ed25519PublicKey::from_bytes(user),
        ) {
            collab_types::caps::Principal::Person(person) => Some(person),
            _ => None,
        }
    }

    /// List this device's own workspaces for the person's other devices, and
    /// send them the list when it changed (or, rarely, when it did not, to
    /// repair a lost snapshot). Called every frame tick; when nothing
    /// changed it is one walk over the tree's workspaces.
    fn reconcile_personal_workspaces(&self) {
        let (Some(workspaces), Some(person)) =
            (self.personal_workspaces.as_ref(), self.personal_person())
        else {
            return;
        };
        let Some(tree) = self.replica.lock().ok().map(|replica| replica.tree.clone()) else {
            return;
        };
        let (mut changed, settle) = {
            let tree = tree.read();
            let changed = workspaces
                .lock()
                .is_ok_and(|mut workspaces| workspaces.publish_local(&tree, &person));
            let settle = !tree.personal_device_requests.is_empty()
                || (tree.personal_device_choices.is_empty() && !tree.desktop.workspaces.is_empty());
            drop(tree);
            (changed, settle)
        };
        if changed || settle {
            let mut tree = tree.write();
            if let Ok(mut workspaces) = workspaces.lock() {
                for (room, device, shown) in std::mem::take(&mut tree.personal_device_requests) {
                    changed |= workspaces.set_shown(room, device, shown);
                }
                // Keeping a workspace off this device takes it off at once.
                workspaces.adopt_into(&mut tree, &person);
                refresh_device_choices(&mut tree, &workspaces);
            }
            drop(tree);
        }
        let due = self
            .last_personal_workspaces_snapshot
            .lock()
            .is_ok_and(|last| {
                last.is_none_or(|at| at.elapsed() >= PERSONAL_WORKSPACES_RESTATE_INTERVAL)
            });
        if changed || due {
            self.publish_personal_workspaces();
        }
    }

    /// Send this device's whole workspace list, signed with its device key,
    /// on the person's own topic.
    fn publish_personal_workspaces(&self) {
        let (Some(workspaces), Some(user), Some(identity)) = (
            self.personal_workspaces.as_ref(),
            self.personal_continuity_user,
            self.host_identity.as_ref(),
        ) else {
            return;
        };
        let Some(update) = workspaces.lock().ok().and_then(|mut workspaces| {
            workspaces.snapshot(&identity.device_signing_key, now_unix_nanos())
        }) else {
            return;
        };
        if let Ok(mut last) = self.last_personal_workspaces_snapshot.lock() {
            *last = Some(std::time::Instant::now());
        }
        let _ = self.publish_shell_checkpoint(NeighbourMsg::PersonalWorkspaces {
            user_public_key: user,
            update: Box::new(update),
        });
    }

    /// Merge another of this person's devices' workspace list and join the
    /// workspaces it brings. Returns `true` for every workspace-list message,
    /// merged or refused, so none falls through to room authorization.
    fn dispatch_personal_workspaces(
        &self,
        ws: u64,
        origin: &crate::shell::state::PeerOrigin,
        msg: &NeighbourMsg,
    ) -> bool {
        let NeighbourMsg::PersonalWorkspaces {
            user_public_key,
            update,
        } = msg
        else {
            return false;
        };
        let (Some(local_user), Some(workspaces), Some(person)) = (
            self.personal_continuity_user,
            self.personal_workspaces.as_ref(),
            self.personal_person(),
        ) else {
            return true;
        };
        if ws != personal_continuity_topic_id(user_public_key)
            || user_public_key != &local_user
            || self
                .peer_identities
                .get(origin)
                .is_none_or(|peer| !peer.is_same_user(&local_user))
        {
            tracing::warn!(peer = %origin, "dropping a workspace list from a different identity");
            return true;
        }
        // The author may differ from the transport peer when a host replays a
        // device's retained list; it must still be a certified device of this
        // person that this device has authenticated.
        let now = now_unix_nanos();
        let Some(author) = self
            .peer_identities
            .devices_of(&local_user, now)
            .into_iter()
            .find(|cert| cert.device_public_key == update.author_device_public_key)
        else {
            tracing::debug!(peer = %origin, "workspace list from a device not yet authenticated");
            return true;
        };
        if self.peer_identities.is_revoked(
            &local_user,
            &author.device_public_key,
            update.authored_at_ns,
        ) {
            return true;
        }
        let Some(tree) = self.replica.lock().ok().map(|replica| replica.tree.clone()) else {
            return true;
        };
        let rooms_before = tree.read().desktop.rooms();
        let (adopted, answer) = {
            let mut tree = tree.write();
            let Ok(mut workspaces) = workspaces.lock() else {
                return true;
            };
            // What changed here since the last agreement goes into the
            // directory first, so the merge sees both sides.
            let local = workspaces.publish_local(&tree, &person);
            if let Err(error) = workspaces.merge(update, &author, now) {
                tracing::warn!(%error, peer = %origin, "refusing a workspace list");
                return true;
            }
            let adopted = workspaces.adopt_into(&mut tree, &person);
            refresh_device_choices(&mut tree, &workspaces);
            drop(tree);
            let answer = local || !workspaces.covered_by(update);
            drop(workspaces);
            (adopted, answer)
        };
        if adopted {
            // Reach the governor of each room someone else owns directly, so
            // its answer does not depend on another device relaying it.
            for governor in governors_before_and_after(&tree, &rooms_before) {
                let _ = self.cmd_tx.try_send(NetCmd::Connect {
                    addr: iroh::EndpointAddr::new(governor),
                });
            }
            // Join each new workspace's room and ask for its content now
            // rather than at the next periodic round.
            self.publish_sync_requests();
        }
        if answer {
            self.publish_personal_workspaces();
        }
        true
    }

    /// Called from every frame tick, which runs far more often than a room
    /// changes. Every [`SYNC_KICK_INTERVAL`] it does the local upkeep that
    /// keeps revocation prompt and continuity current, which sends nothing
    /// unless something changed; [`Self::publish_sync_requests`] goes out
    /// only when [`Self::schedule_repair_pull`] or [`SYNC_SAFETY_INTERVAL`]
    /// made it due.
    fn kick_periodic_sync(&self) {
        let now = std::time::Instant::now();
        let upkeep = self.last_sync_kick.lock().is_ok_and(|mut last| {
            let due = last.is_none_or(|prev| now.duration_since(prev) >= SYNC_KICK_INTERVAL);
            if due {
                *last = Some(now);
            }
            due
        });
        if upkeep {
            self.refresh_authenticated_persons();
            self.publish_personal_continuity_if_changed();
        }
        // A guest still redeeming an invite keeps asking at the repair pace
        // until the authority answers: its request is what redeems.
        let redeeming = self
            .pending_redemptions
            .lock()
            .is_ok_and(|pending| !pending.is_empty());
        let pull = self.next_sync_pull.lock().is_ok_and(|mut next| {
            let due = next.is_none_or(|at| now >= at);
            if due {
                let wait = if redeeming {
                    SYNC_REPAIR_DELAY
                } else {
                    SYNC_SAFETY_INTERVAL
                };
                *next = Some(now.checked_add(wait).unwrap_or(now));
            }
            due
        });
        if pull {
            self.publish_sync_requests();
        }
    }

    /// A room changed: pull [`SYNC_REPAIR_DELAY`] from now, unless a pull is
    /// due sooner.
    fn schedule_repair_pull(&self) {
        let now = std::time::Instant::now();
        let due = now.checked_add(SYNC_REPAIR_DELAY).unwrap_or(now);
        if let Ok(mut next) = self.next_sync_pull.lock()
            && let Some(at) = *next
            && at > due
        {
            *next = Some(due);
        }
    }

    /// Announce every room document change since the last announcement as a
    /// `SyncResp`, so peers see an edit, an opened, moved or closed tab, a
    /// rename within one network round trip instead of at their next pull.
    /// At most one message per room per tick. Cheap when idle: rooms are
    /// projected only when the desktop document, the peer-merge epoch or the
    /// external revision moved, or [`ROOM_PUSH_FALLBACK_INTERVAL`] passed.
    /// A batch that landed from a peer is re-announced once, which reaches
    /// a member that missed the original; the echo changes nothing for
    /// those that already hold it, so it does not repeat.
    fn push_room_changes(&self) {
        let Some(handle) = self.replica.lock().ok().map(|replica| replica.tree.clone()) else {
            return;
        };
        let Ok(mut push) = self.room_push.lock() else {
            return;
        };
        let now = std::time::Instant::now();
        let mut outgoing = Vec::new();
        let mut tree = handle.write();
        let trigger = (
            tree.current_heads(),
            tree.desktop.room_epoch,
            tree.desktop.external_revision,
        );
        let due = push
            .last_full
            .is_none_or(|last| now.duration_since(last) >= ROOM_PUSH_FALLBACK_INTERVAL);
        let moved = push.trigger.as_ref() != Some(&trigger);
        if !moved && !due {
            return;
        }
        if moved {
            // Once this activity settles, ask the rooms for what this shell
            // or a peer missed of it.
            self.schedule_repair_pull();
        }
        push.trigger = Some(trigger);
        push.last_full = Some(now);
        let rooms = tree.rooms();
        push.announced.retain(|room, _| rooms.contains(room));
        for room in rooms {
            if tree.requires_native_owner(room) {
                continue;
            }
            tree.sync_room_doc(room);
            let heads = tree.room_heads(room);
            if heads.is_empty() {
                continue;
            }
            let Some(announced) = push.announced.get(&room) else {
                push.announced.insert(room, heads);
                continue;
            };
            if *announced == heads {
                continue;
            }
            let changes = tree
                .room_docs
                .get_mut(&room)
                .map(|doc| {
                    doc.get_changes(announced)
                        .into_iter()
                        .map(|change| change.raw_bytes().to_vec())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            push.announced.insert(room, heads);
            if !changes.is_empty() {
                outgoing.push(NeighbourMsg::SyncResp {
                    changes,
                    room: room.to_string(),
                });
            }
        }
        drop(tree);
        drop(push);
        for msg in outgoing {
            let _ = self.publish_message(msg);
        }
    }

    /// Drain a bounded batch of pending [`NetEvent`]s from the actor, forward `Msg`
    /// variants into the replica's `handle`, and publish each resulting
    /// outbound message via `cmd_tx`. Non-blocking — never stalls the
    /// render loop even on a starved channel.
    pub fn tick(&self) {
        self.remote_action_replies.expire(std::time::Instant::now());
        // Local room changes go out the tick they land; the periodic pull
        // repairs whatever a peer missed.
        self.push_room_changes();
        self.kick_periodic_sync();
        self.reconcile_personal_workspaces();
        self.refresh_home_peer_hosts_periodically();
        self.finish_shared_rooms();
        self.live_keepalive();
        // Hold the lock for the whole drain so a concurrent frame-tick
        // never sees half-merged events. A poisoned mutex (a prior
        // holder panicked mid-drain) just skips this tick rather than
        // panicking this one too.
        let Ok(mut event_rx) = self.event_rx.lock() else {
            return;
        };
        for _ in 0..MAX_NET_EVENTS_PER_TICK {
            let Ok(event) = event_rx.try_recv() else {
                break;
            };
            self.dispatch_net_event(event);
        }
    }

    /// Route one drained [`NetEvent`] to its handler. Split out of
    /// [`Self::tick`], which owns only the drain loop and the
    /// `event_rx` lock.
    #[expect(
        clippy::too_many_lines,
        reason = "one exhaustive dispatcher routes each network event variant"
    )]
    fn dispatch_net_event(&self, event: NetEvent) {
        match event {
            NetEvent::NativeInput(input) => self.dispatch_native_input(&input),
            NetEvent::Live { ws, from, frame } => self.handle_live_frame(ws, from, &frame),
            NetEvent::HostReady { ws, peer } => {
                if let Ok(mut relays) = self.workspace_control_relays.lock() {
                    relays.insert((ws, peer));
                }
                self.handle_host_ready(ws, peer);
            }
            NetEvent::PeerJoined { ws, peer, .. } => {
                self.note_peer_joined();
                if let Ok(mut connected) = self.dialed_direct_peers.lock() {
                    connected.insert(peer);
                }
                if let Ok(mut live) = self.live_room_peers.lock() {
                    live.insert((ws, peer));
                }
                self.publish_sync_requests();
                if self
                    .personal_continuity_user
                    .is_some_and(|user| ws == personal_continuity_topic_id(&user))
                {
                    self.publish_device_fabric_request();
                    self.publish_personal_workspaces();
                }
            }
            // Without this the roster only ever grows: nothing else
            // tells it a session went quiet.
            NetEvent::PeerLeft {
                ws,
                peer,
                cause,
                membership_epoch,
            } => {
                if let Ok(mut connected) = self.dialed_direct_peers.lock() {
                    connected.remove(&peer);
                }
                self.drop_held_remote_actions(peer);
                if let Ok(mut live) = self.live_room_peers.lock() {
                    live.remove(&(ws, peer));
                }
                if let Ok(mut direct) = self.workspace_control_direct.lock() {
                    direct.remove(&(ws, peer));
                }
                if let Ok(mut relays) = self.workspace_control_relays.lock() {
                    relays.remove(&(ws, peer));
                }
                self.notify_workspace_control(WorkspaceControlNetworkEvent::PeerLeft {
                    workspace_topic: ws,
                    peer,
                });
                self.record_peer_departure(ws, peer, cause, membership_epoch);
                if let Some(epoch) = membership_epoch
                    && let Ok(mut routes) = self.authenticated_personal_routes.lock()
                    && routes
                        .get(&(ws, peer))
                        .is_some_and(|route| route.membership_epoch == epoch)
                {
                    routes.remove(&(ws, peer));
                }
                let has_live_route =
                    self.authenticated_personal_routes
                        .lock()
                        .is_ok_and(|routes| routes.values().any(|route| route.endpoint_id == peer))
                        || self.workspace_control_direct.lock().is_ok_and(|direct| {
                            direct.iter().any(|(_, endpoint)| endpoint == &peer)
                        });
                if !has_live_route {
                    let handle = crate::network::behaviour::peer_liveness_handle(&peer);
                    // Adverts are not restated on a timer, so they do not
                    // go stale by themselves: a departure forgets them.
                    crate::network::media_caps::forget(handle);
                    self.peer_identities
                        .remove(&crate::shell::state::PeerOrigin::peer(&peer));
                    if let Ok(replica) = self.replica.lock() {
                        replica.tree.write().retire_session(handle);
                    }
                }
            }
            NetEvent::Msg {
                ws,
                peer,
                membership_epoch,
                replayed,
                msg,
            } => self.dispatch_neighbour_msg(ws, peer, membership_epoch, replayed, msg),
            NetEvent::OwnReplay { msg, .. } => self.dispatch_own_replayed_document(msg),
            NetEvent::PersonalFabric {
                ws,
                peer,
                membership_epoch,
                user_public_key,
                payload,
            } => {
                self.dispatch_personal_fabric(ws, peer, membership_epoch, user_public_key, payload);
            }
            NetEvent::NearbyTransferProgress {
                target,
                transfer_id,
                sent_bytes,
                total_bytes,
                status,
            } => {
                if let Ok(target) =
                    crate::shell::personal_continuity::DeviceId::new(hex::encode(target.as_bytes()))
                {
                    crate::shell::device_fabric_runtime::record_nearby_transfer_progress(
                        target,
                        transfer_id,
                        sent_bytes.min(total_bytes),
                        total_bytes,
                        status,
                    );
                }
            }
        }
    }

    fn dispatch_personal_fabric(
        &self,
        ws: u64,
        peer: iroh::EndpointId,
        membership_epoch: crate::network::actor::MembershipEpoch,
        user_public_key: [u8; 32],
        payload: PersonalFabricPayload,
    ) {
        let origin = crate::shell::state::PeerOrigin::peer(&peer);
        let Some(identity) = self.peer_identities.get(&origin) else {
            tracing::warn!(peer = %peer, "dropping unauthenticated personal fabric frame");
            return;
        };
        // The device's own root carries the whole fabric; a workspace
        // identity's root carries only its person room's records.
        let own_root = self.personal_continuity_user == Some(user_public_key)
            || (matches!(payload, PersonalFabricPayload::Document { .. })
                && self.workspace_person_roots.contains(&user_public_key));
        if !own_root
            || ws != personal_continuity_topic_id(&user_public_key)
            || !identity.is_same_user(&user_public_key)
        {
            tracing::warn!(peer = %peer, "dropping cross-identity personal fabric frame");
            return;
        }
        let route = self
            .authenticated_personal_routes
            .lock()
            .ok()
            .and_then(|routes| routes.get(&(ws, peer)).cloned());
        let Some(route) = route.filter(|route| {
            route.membership_epoch == membership_epoch
                && route.user_public_key == user_public_key
                && route.endpoint_id == peer
                && route.device.as_str() == hex::encode(identity.device_pubkey)
        }) else {
            tracing::warn!(peer = %peer, "dropping personal fabric frame without current authenticated route");
            return;
        };
        match payload {
            PersonalFabricPayload::Request => self.publish_device_fabric_snapshot(),
            PersonalFabricPayload::Snapshot(bytes) => {
                self.dispatch_personal_fabric_checkpoint(peer, identity.device_pubkey, &bytes);
            }
            PersonalFabricPayload::DisplayConnectors { stamp, outputs } => {
                if self.permits_continuity(
                    crate::shell::continuity_policy::ContinuityCapability::DisplayInput,
                ) && let Ok(mut fabric) = self.device_fabric.lock()
                    && let Err(error) =
                        fabric.observe_display_connectors(&route.device, &stamp, outputs)
                {
                    tracing::debug!(%error, peer = %peer, "dropping invalid connector observation");
                }
            }
            PersonalFabricPayload::DisplayOutputs { stamp, outputs } => {
                if self.permits_continuity(
                    crate::shell::continuity_policy::ContinuityCapability::DisplayInput,
                ) && let Err(error) =
                    self.observe_display_outputs(&route.device, &stamp, outputs)
                {
                    tracing::debug!(%error, peer = %peer, "dropping invalid rich display observation");
                }
            }
            PersonalFabricPayload::Action(bytes) => {
                self.dispatch_personal_fabric_action(peer, identity.device_pubkey, route, &bytes);
            }
            PersonalFabricPayload::MediaTransfer(_) => {
                // The retired media lane: a device that predates the unified
                // transfer lane. Its bytes are never accepted; the handoff
                // fails on the sender's timeout like any other unanswered one.
                tracing::debug!(peer = %peer, "dropping legacy personal media transfer frame");
            }
            PersonalFabricPayload::NearbyTransfer(frame) => {
                let Ok(source) = crate::shell::personal_continuity::DeviceId::new(hex::encode(
                    identity.device_pubkey,
                )) else {
                    return;
                };
                crate::shell::device_fabric_runtime::handle_nearby_transfer_frame(
                    self, &source, frame,
                );
            }
            PersonalFabricPayload::Document {
                app_id,
                record_id,
                envelope,
            } => {
                self.dispatch_person_document(peer, &identity, app_id, &record_id, *envelope);
            }
            payload @ (PersonalFabricPayload::RecordContentWant { .. }
            | PersonalFabricPayload::RecordContentPiece { .. }) => {
                self.dispatch_person_content(ws, peer, payload);
            }
        }
    }

    fn dispatch_personal_fabric_checkpoint(
        &self,
        peer: iroh::EndpointId,
        device_public_key: [u8; 32],
        bytes: &[u8],
    ) {
        let Ok(incoming) = crate::shell::device_fabric::DeviceFabric::decode_checkpoint(bytes)
        else {
            tracing::warn!(peer = %peer, "dropping invalid personal fabric checkpoint");
            return;
        };
        if incoming.snapshots().len() != 1 {
            tracing::warn!(peer = %peer, "dropping relayed personal fabric authorship");
            return;
        }
        let expected = hex::encode(device_public_key);
        let Some(snapshot) = incoming.snapshots().values().next().cloned() else {
            return;
        };
        if snapshot.owner.as_str() != expected {
            tracing::warn!(peer = %peer, "personal fabric owner does not match device certificate");
            return;
        }
        let Ok(mut fabric) = self.device_fabric.lock() else {
            return;
        };
        match fabric.upsert(snapshot) {
            Ok(true) => {
                if let Some(path) = self.device_fabric_path.as_deref()
                    && let Err(error) = self.persist_device_fabric(path, &fabric)
                {
                    tracing::warn!(%error, "device fabric merge persistence failed");
                }
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(%error, peer = %peer, "dropping conflicting personal fabric state");
            }
        }
    }

    fn dispatch_personal_fabric_action(
        &self,
        peer: iroh::EndpointId,
        authenticated_device: [u8; 32],
        route: AuthenticatedPersonalRoute,
        bytes: &[u8],
    ) {
        let received_monotonic_us = fabric_monotonic_us();
        if bytes.len() > crate::shell::device_fabric::MAX_FABRIC_ACTION_BYTES {
            tracing::warn!(peer = %peer, "dropping oversized personal fabric action");
            return;
        }
        let Ok((envelope, trailing)) = postcard::take_from_bytes::<
            crate::shell::device_fabric::PersonalFabricActionEnvelope,
        >(bytes) else {
            tracing::warn!(peer = %peer, "dropping invalid personal fabric action");
            return;
        };
        if !trailing.is_empty() {
            tracing::warn!(peer = %peer, "personal fabric action has trailing data");
            return;
        }
        let Some(local) = self.personal_continuity_device.as_ref() else {
            return;
        };
        let Ok(source) =
            crate::shell::personal_continuity::DeviceId::new(hex::encode(authenticated_device))
        else {
            return;
        };
        if envelope.validate_at(&source, local, unix_now_ms()).is_err() {
            tracing::warn!(peer = %peer, "dropping untrusted personal fabric action");
            return;
        }
        let Ok(mut replays) = self.personal_fabric_action_replays.lock() else {
            return;
        };
        if !replays.accept(envelope.request_id) {
            tracing::warn!(peer = %peer, "dropping replayed personal fabric action");
            return;
        }
        drop(replays);
        let Ok(mut actions) = self.personal_fabric_actions.lock() else {
            return;
        };
        let outcome = queue_personal_fabric_action(
            &mut actions,
            ReceivedPersonalFabricAction {
                envelope,
                route,
                received_monotonic_us,
            },
        );
        drop(actions);
        if outcome == PersonalFabricQueueOutcome::Saturated {
            tracing::warn!(
                peer = %peer,
                "personal fabric action queue saturated; rejecting action without evicting control"
            );
        }
    }

    /// Capture departure evidence before identity and roster cleanup. Every
    /// lock is best-effort: a poisoned diagnostic/replica lock must never turn
    /// a reader exit into a shell panic or keep a stale session alive.
    fn record_peer_departure(
        &self,
        ws: u64,
        peer: iroh::EndpointId,
        cause: PeerDepartureCause,
        membership_epoch: Option<crate::network::actor::MembershipEpoch>,
    ) {
        let session_handle = crate::network::behaviour::peer_liveness_handle(&peer);
        let route = self
            .replica
            .lock()
            .ok()
            .and_then(|replica| {
                replica
                    .tree
                    .read()
                    .rooms()
                    .into_iter()
                    .find(|room| room_topic_id(*room) == ws)
            })
            .map_or_else(
                || CrdtRoute::cluster(""),
                |room| CrdtRoute::cluster(room.to_string()),
            );
        let participant_name = (!route.cluster_id.is_empty())
            .then(|| {
                self.peer_labels(&route.cluster_id)
                    .get(&session_handle)
                    .cloned()
            })
            .flatten();
        let user_public_key = self
            .peer_identities
            .get(&crate::shell::state::PeerOrigin::peer(&peer))
            .map(|identity| identity.user_pubkey);
        self.peer_departures.record(PeerDeparture {
            generation: 0,
            endpoint_id: peer,
            membership_epoch,
            session_handle,
            route,
            cause,
            user_public_key,
            participant_name,
            observed_at: SystemTime::now(),
        });
    }

    /// Seed a durable host's checkpoint once it finishes subscribing to
    /// `ws` — see [`NetEvent::HostReady`]. Unlike `SyncReq`, this does
    /// not need another device online to answer it.
    fn handle_host_ready(&self, ws: u64, peer: iroh::EndpointId) {
        if self.native_topic_required(ws) {
            return;
        }
        if self
            .personal_continuity_user
            .is_some_and(|user| personal_continuity_topic_id(&user) == ws)
        {
            if !self.enforce_app_continuations_policy() {
                return;
            }
            // PublishCheckpoint gives a newly enrolled/offline device a
            // retained baseline even when no interactive device is online.
            if let (Some(user_public_key), Some(snapshot)) = (
                self.personal_continuity_user,
                self.local_continuity_snapshot_bytes(),
            ) && self.enforce_app_continuations_policy()
            {
                let _ = self.cmd_tx.try_send(NetCmd::PublishCheckpoint {
                    ws,
                    msg: NeighbourMsg::PersonalContinuityCheckpoint {
                        user_public_key,
                        publisher: self.local_peer_id_hash(),
                        snapshot,
                    },
                });
            }
            return;
        }
        let room = self.replica.lock().ok().and_then(|replica| {
            replica
                .tree
                .read()
                .rooms()
                .into_iter()
                .find(|room| room_topic_id(*room) == ws)
        });
        let Some(room) = room else {
            return;
        };
        // Seed a complete shell-room checkpoint.
        let changes = self
            .replica
            .lock()
            .ok()
            .and_then(|replica| replica.tree.write().export_room_changes_since(room, &[]))
            .unwrap_or_default();
        if !changes.is_empty() {
            let _ = self.publish_shell_checkpoint(NeighbourMsg::SyncResp {
                changes,
                room: room.to_string(),
            });
        }

        // Apps answer an Arrived presence event with their full
        // Automerge document. Synthesize that local event for the
        // durable host so its retained checkpoints include app
        // state that existed before the host connected.
        let host_peer = crate::network::behaviour::peer_liveness_handle(&peer);
        for payload in [
            ui::collaboration::PresencePayload::Arrived,
            ui::collaboration::PresencePayload::Left,
        ] {
            let key = ui::presence::PresenceKey::Workspace {
                id: room.to_string(),
            };
            if let Some(presence) = crate::shell::collab_presence::to_peer(&key, &payload) {
                self.handle_typed_app_presence_message(
                    host_peer,
                    String::new(),
                    room.to_string(),
                    presence,
                );
            }
        }
    }

    /// Route one gossip [`NeighbourMsg`] that arrived on topic `ws`
    /// from `peer` to its per-variant handler, then flush anything the
    /// replica queued in response. Split out of [`Self::tick`] via
    /// [`Self::dispatch_net_event`].
    #[allow(
        clippy::too_many_lines,
        reason = "one security gate followed by an exhaustive wire-variant dispatcher; splitting the match would obscure the shared authentication order"
    )]
    fn dispatch_neighbour_msg(
        &self,
        ws: u64,
        peer: iroh::EndpointId,
        membership_epoch: Option<crate::network::actor::MembershipEpoch>,
        replayed: bool,
        msg: NeighbourMsg,
    ) {
        // The one boundary for retired wire generations: no build produces
        // them, so nothing past this point handles or authorizes them.
        if msg.is_retired() {
            tracing::debug!(%peer, ws, "dropping a retired collaboration message");
            return;
        }
        // A record of the person's own room arrives from another of their
        // devices over the personal fabric, or kept by their always-on host
        // on their continuity topic; never as room traffic.
        if let NeighbourMsg::AppDocument { cluster_id, .. } = &msg
            && self.is_local_person_room(cluster_id)
        {
            self.dispatch_hosted_person_document(ws, peer, msg);
            return;
        }
        if let Some(expected) = Self::explicit_topic_for_message(&msg)
            && expected != ws
        {
            tracing::warn!(
                ws,
                expected,
                "dropping collaboration message on the wrong room topic"
            );
            return;
        }
        if self.native_topic_required(ws)
            && matches!(
                &msg,
                NeighbourMsg::SyncReq { .. }
                    | NeighbourMsg::SyncResp { .. }
                    | NeighbourMsg::AppDocument { .. }
                    | NeighbourMsg::IdentityAnnouncementV2 { .. }
            )
        {
            tracing::warn!(
                ws,
                "enrolled room state requires authenticated native relay admission"
            );
            return;
        }
        let origin = crate::shell::state::PeerOrigin::peer(&peer);
        if let NeighbourMsg::IdentityHello { cert, revocations } = &msg {
            // A new hello is a full replacement attempt for this exact route.
            // Purge first so a wrong-user/revoked replacement can never leave
            // the old same-user authority alive.
            if let Ok(mut routes) = self.authenticated_personal_routes.lock() {
                routes.remove(&(ws, peer));
            }
            self.accept_identity_hello(ws, &origin, &peer, cert, revocations);
            self.refresh_authenticated_persons();
            if self.peer_identities.get(&origin).is_some()
                && let Ok(mut direct) = self.workspace_control_direct.lock()
            {
                direct.insert((ws, peer));
            }
            if self.peer_identities.get(&origin).is_none() {
                self.notify_workspace_control(WorkspaceControlNetworkEvent::PeerRevoked {
                    workspace_topic: ws,
                    peer,
                });
            }
            if let (Some(epoch), Some(user), Some(identity)) = (
                membership_epoch,
                self.person_roots()
                    .find(|root| ws == personal_continuity_topic_id(root)),
                self.peer_identities.get(&origin),
            ) && identity.is_same_user(&user)
                && let Ok(device) = crate::shell::personal_continuity::DeviceId::new(hex::encode(
                    identity.device_pubkey,
                ))
                && let Ok(mut routes) = self.authenticated_personal_routes.lock()
            {
                routes.insert(
                    (ws, peer),
                    AuthenticatedPersonalRoute {
                        workspace_topic: ws,
                        endpoint_id: peer,
                        membership_epoch: epoch,
                        user_public_key: user,
                        device,
                    },
                );
                drop(routes);
                if let Ok(mut wanted) = self.person_snapshots_wanted.lock() {
                    wanted.insert(user);
                }
                self.drain_wake.poke();
            }
            return;
        }
        if self.native_topic_required(ws)
            && let NeighbourMsg::AppPresenceV2 {
                peer: publisher,
                app_id,
                cluster_id,
                presence,
                colour,
            } = &msg
        {
            // An admitted native member need not also send a legacy hello.
            // Presence still binds the publisher to the authenticated endpoint
            // and checks this exact room's current signed read authority.
            if *publisher == crate::network::behaviour::peer_liveness_handle(&peer)
                && self.governed_presence_allows(ws, peer, app_id, cluster_id)
            {
                self.dispatch_app_presence_v2(
                    *publisher,
                    app_id.clone(),
                    cluster_id.clone(),
                    presence.clone(),
                    colour.as_deref(),
                );
            } else {
                tracing::warn!(%peer, ws, "native app presence lacks current room admission");
            }
            return;
        }
        // A member of an enrolled room that announced nothing to this device
        // (a browser) is known from the room's governance, which also says it
        // may take part in the room: its actions are answered as its own.
        let governed_action = matches!(msg, NeighbourMsg::RemoteAction { .. })
            && self.peer_identities.get(&origin).is_none()
            && self.governed_identity(ws, peer).is_some();
        if self.require_peer_identity
            && self.peer_identities.get(&origin).is_none()
            && !governed_action
        {
            if let NeighbourMsg::RemoteAction { frame } = msg {
                self.hold_remote_action(HeldRemoteAction {
                    workspace_topic: ws,
                    peer,
                    membership_epoch,
                    replayed,
                    frame,
                });
            } else {
                tracing::warn!(peer = %peer, "dropping collaboration message from an unauthenticated device");
            }
            return;
        }
        if self.require_peer_identity
            && let Some(claimed_peer) = Self::claimed_publisher(&msg)
            && claimed_peer != crate::network::behaviour::peer_liveness_handle(&peer)
        {
            tracing::warn!(
                peer = %peer,
                claimed_peer,
                "dropping collaboration message whose session identity does not match its authenticated transport"
            );
            return;
        }
        // Codec adverts ride the heartbeat on every joined topic, including
        // the default topic and the person's own lane. They describe the
        // sender's own device, already bound to its transport above, not a
        // room, so no room authority applies. Two devices of one person
        // that share no room still learn what the other can decode.
        if let NeighbourMsg::MediaCapabilities {
            peer: advertiser,
            hardware_decode,
            software_decode,
            hardware_encode,
            software_encode,
        } = &msg
        {
            crate::network::media_caps::record(
                *advertiser,
                hardware_decode,
                software_decode,
                hardware_encode,
                software_encode,
            );
            return;
        }
        if self.dispatch_personal_continuity(ws, &origin, &msg)
            || self.dispatch_personal_workspaces(ws, &origin, &msg)
        {
            return;
        }
        if self.require_peer_identity
            && let NeighbourMsg::SyncReq {
                invite_id: Some(invite_id),
                ..
            } = &msg
            && let Some(identity) = self.peer_identities.get(&origin)
        {
            self.redeem_invited_identity(ws, *invite_id, &peer, &identity.device_cert);
        }
        if self.require_peer_identity && !governed_action {
            let required = Self::required_room_verb(&msg);
            let pinned_authority_may_bootstrap = matches!(&msg, NeighbourMsg::SyncResp { .. });
            if !required.is_some_and(|required| {
                self.peer_room_allows(&origin, ws, required, pinned_authority_may_bootstrap)
            }) {
                tracing::warn!(
                    peer = %peer,
                    workspace_topic = ws,
                    ?required,
                    "dropping collaboration message without workspace authority"
                );
                return;
            }
        }
        if let NeighbourMsg::SyncResp { room, .. } = &msg
            && let Some(room) = RoomId::parse(room)
            && let Ok(mut pending) = self.pending_redemptions.lock()
        {
            pending.remove(&room);
        }
        match msg {
            NeighbourMsg::IdentityAnnouncementV2 {
                room,
                device_cert,
                revocations,
            } => {
                self.handle_identity_announcement(&peer, &room, &device_cert, &revocations);
                self.refresh_authenticated_persons();
            }
            // Ephemeral overlay state, not CRDT state — skips the replica entirely.
            NeighbourMsg::PresenceMoved {
                peer: peer_hash,
                user,
                key,
                point,
                ts,
                colour,
            } => {
                self.note_peer_colour(peer_hash, colour.as_deref());
                self.handle_presence_message(peer_hash, user, key, point, ts);
            }
            NeighbourMsg::AppDocument {
                peer: peer_hash,
                app_id,
                cluster_id,
                record_id,
                envelope,
            } => {
                let Some(record_id) = self.local_record_for_wire(&app_id, &cluster_id, &record_id)
                else {
                    tracing::debug!(
                        app_id,
                        "peer document for a record no local app holds; dropping"
                    );
                    return;
                };
                let role = if self.require_peer_identity {
                    self.peer_document_role(&origin, &cluster_id, &app_id, &record_id, &envelope)
                } else {
                    Some(collab_types::Role::Editor)
                };
                if peer_hash != self.local_peer_hash
                    && self.is_in_room(&cluster_id)
                    && let Some(role) = role
                {
                    self.handle_document_message(
                        peer_hash, app_id, cluster_id, record_id, role, replayed, *envelope,
                    );
                }
            }
            NeighbourMsg::AppPresenceV2 {
                peer: peer_hash,
                app_id,
                cluster_id,
                presence,
                colour,
            } => self.dispatch_app_presence_v2(
                peer_hash,
                app_id,
                cluster_id,
                presence,
                colour.as_deref(),
            ),
            // Cooperative scrollback: skip self-echoes.
            NeighbourMsg::ScrollbackRequest {
                artifact,
                before_seq,
                max,
                requester,
            } => self.dispatch_scrollback_request(ws, requester, artifact, before_seq, max),
            NeighbourMsg::ScrollbackResponse {
                artifact,
                ops,
                responder,
            } => self.dispatch_scrollback_response(
                responder,
                artifact,
                // Unbox at the wire boundary: nothing downstream needs
                // per-op indirection once it's off the wire.
                ops.into_iter().map(|op| *op).collect(),
            ),
            NeighbourMsg::RemoteAction { frame } => {
                self.dispatch_remote_action(ws, &origin, peer, &frame);
            }
            message @ (NeighbourMsg::SurfaceStreamStart { .. }
            | NeighbourMsg::SurfaceFrame { .. }
            | NeighbourMsg::SurfaceStreamEnd { .. }
            | NeighbourMsg::RemoteInput { .. }
            | NeighbourMsg::PermissionGrant { .. }) => {
                self.queue_legacy_message_from(ws, Some(peer), message);
            }
            NeighbourMsg::MlsGovernance { room, frame, .. } => {
                if let Some(room) = RoomId::parse(&room) {
                    self.queue_channel_governance(InboundChannelGovernance::Frame { room, frame });
                }
            }
            NeighbourMsg::MlsAccessRequest { room, request, .. } => {
                if let Some(room) = RoomId::parse(&room) {
                    self.queue_channel_governance(InboundChannelGovernance::AccessRequest {
                        room,
                        request,
                    });
                }
            }
            NeighbourMsg::MlsGovernanceAck { room, ack, .. } => {
                if let Some(room) = RoomId::parse(&room) {
                    self.queue_channel_governance(InboundChannelGovernance::Ack { room, ack });
                }
            }
            message @ (NeighbourMsg::RecordContentWant { .. }
            | NeighbourMsg::RecordContentPiece { .. }) => {
                self.dispatch_record_content(ws, peer, message);
            }
            other => self.dispatch_to_replica(peer, &other),
        }
    }

    /// Handle the private same-person lane before room authorization. Returns
    /// `true` for both accepted and rejected personal messages so neither can
    /// fall through into the collaboration-room replica.
    #[allow(
        clippy::too_many_lines,
        reason = "personal-continuity dispatch validates the private lane and applies its room projection as one authorization decision"
    )]
    fn dispatch_personal_continuity(
        &self,
        ws: u64,
        origin: &crate::shell::state::PeerOrigin,
        msg: &NeighbourMsg,
    ) -> bool {
        let (user_public_key, checkpoint) = match msg {
            NeighbourMsg::PersonalContinuityRequest {
                user_public_key, ..
            } => (user_public_key, None),
            NeighbourMsg::PersonalContinuityCheckpoint {
                user_public_key,
                snapshot,
                ..
            } => (user_public_key, Some(snapshot.as_slice())),
            _ => return false,
        };
        if !self.enforce_app_continuations_policy() {
            return true;
        }
        let Some(local_user) = self.personal_continuity_user else {
            return true;
        };
        let identity = self.peer_identities.get(origin);
        if ws != personal_continuity_topic_id(user_public_key)
            || user_public_key != &local_user
            || identity
                .as_ref()
                .is_none_or(|peer| !peer.is_same_user(&local_user))
        {
            tracing::warn!(
                peer = %origin,
                "dropping personal continuity traffic from a different identity"
            );
            return true;
        }

        let Some(snapshot_bytes) = checkpoint else {
            let Some(snapshot) = self.local_continuity_snapshot_bytes() else {
                return true;
            };
            let msg = NeighbourMsg::PersonalContinuityCheckpoint {
                user_public_key: local_user,
                publisher: self.local_peer_id_hash(),
                snapshot,
            };
            if self.enforce_app_continuations_policy() && self.join_topic(ws) {
                let _ = self.cmd_tx.try_send(NetCmd::PublishCheckpoint { ws, msg });
            }
            return true;
        };
        if snapshot_bytes.len() > MAX_PERSONAL_CONTINUITY_CHECKPOINT_BYTES {
            tracing::warn!(
                bytes = snapshot_bytes.len(),
                "dropping oversized continuity state"
            );
            return true;
        }
        let Ok(incoming) = crate::shell::personal_continuity::decode_checkpoint(snapshot_bytes)
        else {
            tracing::warn!("dropping invalid continuity checkpoint");
            return true;
        };
        if !self.enforce_app_continuations_policy() {
            return true;
        }
        if let Ok(mut counter) = self.personal_continuity_counter.lock() {
            *counter = (*counter).max(incoming.observed_counter());
        }
        let mut changed = false;
        let mut merged = None;
        if let Ok(mut continuity) = self.personal_continuity.lock() {
            for snapshot in incoming.snapshots().values() {
                match continuity.upsert(snapshot.clone()) {
                    Ok(snapshot_changed) => changed |= snapshot_changed,
                    Err(error) => {
                        tracing::warn!(%error, "dropping conflicting continuity checkpoint");
                    }
                }
            }
            merged = Some(continuity.clone());
        }
        if changed
            && let Some(merged) = merged.as_ref()
            && let Ok(mut recency) = self.personal_focus_recency.lock()
        {
            recency.observe(merged, std::time::Instant::now());
        }
        if changed {
            if !self.enforce_app_continuations_policy() {
                return true;
            }
            let _ = self.persist_personal_continuity();
            let Some(merged) = merged else {
                return true;
            };
            // Reconcile from the accepted merged set, not from the wire
            // payload. A checkpoint can contain stale entries alongside one
            // newer entry; arrival order must never choose an app state.
            let mut candidate_apps = std::collections::BTreeSet::new();
            let mut rooms = std::collections::BTreeMap::new();
            for snapshot in merged.snapshots().values() {
                for workspace in &snapshot.workspaces {
                    rooms
                        .entry(workspace.room)
                        .or_insert_with(|| snapshot.device.as_str().to_string());
                    for app in &workspace.apps {
                        candidate_apps.insert((workspace.room, app.id));
                    }
                }
            }
            let mut winners_by_room = std::collections::BTreeMap::<_, Vec<_>>::new();
            for (room, app_id) in candidate_apps {
                if let Some(app) = merged.newest_app_state_in_room(room, app_id) {
                    winners_by_room.entry(room).or_default().push(app.clone());
                }
            }
            let materializations: Vec<_> = winners_by_room
                .into_iter()
                .map(|(room, apps)| {
                    let source = apps
                        .first()
                        .map(|app| app.stamp.device.as_str().to_string())
                        .unwrap_or_default();
                    (
                        source,
                        crate::shell::personal_continuity::WorkspaceContinuation { room, apps },
                    )
                })
                .collect();
            let desired_tabs = materializations
                .iter()
                .flat_map(|(_, workspace)| workspace.apps.iter().map(|app| app.id.0))
                .collect::<std::collections::BTreeSet<_>>();
            let tree_handle = self.replica.lock().ok().map(|replica| replica.tree.clone());
            #[allow(
                clippy::significant_drop_tightening,
                reason = "all continuation room and tab mutations share one workspace write lock to preserve their common reconciliation snapshot"
            )]
            let adopted = tree_handle.is_some_and(|tree| {
                if !self.enforce_app_continuations_policy() {
                    return false;
                }
                let mut tree = tree.write();
                let desired_rooms = rooms
                    .keys()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>();
                let adopted_rooms =
                    rooms
                        .into_iter()
                        .fold(false, |changed, (room, source_device)| {
                            tree.adopt_continuation_room(room, &source_device) || changed
                        });
                // Materialized tabs are idempotent by durable AppInstanceId.
                // The frame loop opens them locally without changing the
                // active workspace or focus.
                let materialized =
                    materializations
                        .into_iter()
                        .fold(false, |changed, (source, workspace)| {
                            tree.materialize_continuation_workspace(&source, &workspace) || changed
                        });
                let reconciled_tabs = tree.reconcile_continuation_tabs(&desired_tabs);
                let reconciled_rooms = tree.reconcile_continuation_rooms(&desired_rooms);
                reconciled_tabs || reconciled_rooms || adopted_rooms || materialized
            });
            if adopted {
                // Joining the room topics is demand-driven by these requests;
                // the active workspace remains untouched.
                self.publish_sync_requests();
            }
        }
        true
    }

    /// Return the session handle a message claims as its publisher.
    ///
    /// These fields are routing conveniences, not identities. Production
    /// receive paths bind them to the authenticated Iroh endpoint before any
    /// cursor, input, stream, app-presence, or scrollback state is accepted.
    /// `PermissionGrant.peer` is deliberately excluded because it names the
    /// grant's recipient rather than its publisher.
    fn claimed_publisher(msg: &NeighbourMsg) -> Option<u64> {
        match msg {
            NeighbourMsg::PresenceMoved { peer, .. }
            | NeighbourMsg::PresenceCleared { peer, .. }
            | NeighbourMsg::RemoteInput { peer, .. }
            | NeighbourMsg::PermissionRequest { peer, .. }
            | NeighbourMsg::Heartbeat { peer, .. }
            | NeighbourMsg::MediaCapabilities { peer, .. }
            | NeighbourMsg::AppDocument { peer, .. }
            | NeighbourMsg::AppPresenceV2 { peer, .. } => Some(*peer),
            NeighbourMsg::SurfaceStreamStart { source_peer, .. }
            | NeighbourMsg::SurfaceStreamEnd { source_peer, .. }
            | NeighbourMsg::SurfaceFrame { source_peer, .. } => Some(*source_peer),
            NeighbourMsg::ScrollbackRequest { requester, .. }
            | NeighbourMsg::PersonalContinuityRequest { requester, .. }
            | NeighbourMsg::RecordContentWant { requester, .. } => Some(*requester),
            NeighbourMsg::ScrollbackResponse { responder, .. }
            | NeighbourMsg::RecordContentPiece { responder, .. } => Some(*responder),
            NeighbourMsg::PersonalContinuityCheckpoint { publisher, .. } => Some(*publisher),
            NeighbourMsg::SyncReq { .. }
            | NeighbourMsg::SyncResp { .. }
            | NeighbourMsg::PermissionGrant { .. }
            | NeighbourMsg::IdentityHello { .. }
            | NeighbourMsg::IdentityAnnouncement { .. }
            | NeighbourMsg::IdentityAnnouncementV2 { .. }
            | NeighbourMsg::RemoteAction { .. }
            | NeighbourMsg::MlsGovernance { .. }
            | NeighbourMsg::MlsAccessRequest { .. }
            | NeighbourMsg::MlsGovernanceAck { .. }
            | NeighbourMsg::PersonalWorkspaces { .. } => None,
        }
    }

    /// Resolve a transport shard to exactly one local room. Although a room
    /// topic is derived from a UUID, it is only 64 bits; an ambiguous collision
    /// must not let membership in one room authorize traffic for another.
    fn unique_room_for_topic(&self, workspace_topic: u64) -> Option<RoomId> {
        let tree_handle = self.replica.lock().ok()?.tree.clone();
        let tree = tree_handle.read();
        let mut matching = tree
            .rooms()
            .into_iter()
            .filter(|room| room_topic_id(*room) == workspace_topic);
        let Some(room) = matching.next() else {
            drop(matching);
            drop(tree);
            return None;
        };
        let result = matching.next().is_none().then_some(room);
        drop(tree);
        result
    }

    /// Authenticate the transport device as a person and then authorize that
    /// person against the room capability model. Device authentication alone
    /// is deliberately insufficient: any valid Tilo identity can possess a
    /// certificate without being a member of this workspace.
    fn peer_room_allows(
        &self,
        origin: &crate::shell::state::PeerOrigin,
        workspace_topic: u64,
        required: crate::shell::state::Verb,
        pinned_authority_may_bootstrap: bool,
    ) -> bool {
        let Some(identity) = self.peer_identities.get(origin) else {
            return false;
        };
        let Some(room) = self.unique_room_for_topic(workspace_topic) else {
            return false;
        };
        let Some(tree_handle) = self.replica.lock().ok().map(|replica| replica.tree.clone()) else {
            return false;
        };
        let tree = tree_handle.read();
        // A newly joined guest has not received the owner's governance yet,
        // so it cannot derive the owner's membership locally. The invite pins
        // that exact transport peer as authority; allow only its SyncResp to
        // bootstrap the signed-invite room. A room pinned to a person (one
        // they own, or one their own device offered for continuation) is
        // bootstrapped the same way by any of that person's devices. The
        // replica still runs the shared room admission, which requires the
        // pinned governance writer; an unpinned room accepts no governance.
        let authority = tree.room_authority(room);
        let pinned_writer = pinned_authority_may_bootstrap
            && match &authority {
                crate::shell::state::RoomAuthority::Peer(pinned) => pinned == origin,
                // Only a device currently authenticated as that person, so a
                // revoked or expired device of theirs is not a pinned writer.
                crate::shell::state::RoomAuthority::Person(person) => {
                    tree.authenticated_persons.get(origin) == Some(person)
                }
                crate::shell::state::RoomAuthority::Local
                | crate::shell::state::RoomAuthority::Unpinned => false,
            };
        let allowed = pinned_writer || {
            let person = collab_types::caps::Principal::person_from_key(
                collab_types::crypto::Ed25519PublicKey::from_bytes(identity.user_pubkey),
            )
            .id()
            .to_string();
            tree.desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room == room)
                .is_some_and(|workspace| {
                    workspace.allows(
                        &workspace.actor_for_person(&person),
                        &crate::shell::state::ResourceRef::Workspace,
                        required,
                    )
                })
        };
        drop(tree);
        allowed
    }

    /// `AppPresenceV2`: app-level presence crossing the host boundary.
    /// Split out of [`Self::dispatch_neighbour_msg`].
    fn dispatch_app_presence_v2(
        &self,
        peer_hash: u64,
        app_id: String,
        cluster_id: String,
        presence: collab_types::PeerPresence,
        colour: Option<&str>,
    ) {
        if peer_hash != self.local_peer_hash && self.is_in_room(&cluster_id) {
            self.note_peer_colour(peer_hash, colour);
            self.handle_typed_app_presence_message(peer_hash, app_id, cluster_id, presence);
        }
    }

    /// `ScrollbackRequest`: skip self-echoes, otherwise queue for the
    /// broker's drainer. Split out of [`Self::dispatch_neighbour_msg`].
    fn dispatch_scrollback_request(
        &self,
        workspace_topic: u64,
        requester: u64,
        artifact: collab_types::ArtifactId,
        before_seq: u64,
        max: u16,
    ) {
        // A collaborator who guesses a person-room record's artifact gets no
        // page of it.
        if requester != self.local_peer_hash && !self.is_person_artifact(&artifact) {
            self.handle_scrollback_request_message(
                workspace_topic,
                requester,
                artifact,
                before_seq,
                max,
            );
        }
    }

    /// `ScrollbackResponse`: skip self-echoes, otherwise queue for the
    /// broker's drainer. Split out of [`Self::dispatch_neighbour_msg`].
    fn dispatch_scrollback_response(
        &self,
        responder: u64,
        artifact: collab_types::ArtifactId,
        ops: Vec<collab_types::SignedOp>,
    ) {
        if responder != self.local_peer_hash {
            self.handle_scrollback_response_message(responder, artifact, ops);
        }
    }

    /// Every [`NeighbourMsg`] variant not special-cased in
    /// [`Self::dispatch_neighbour_msg`] (`SyncReq`, `SyncResp`, ...)
    /// replicates through the shell-tree replica. `SyncReq`
    /// additionally carries an invite id which is redeemed before the
    /// replica answers, so roster and reply land together.
    fn dispatch_to_replica(&self, peer: iroh::EndpointId, msg: &NeighbourMsg) {
        // A poisoned replica lock here just drops this one message
        // rather than panicking the whole compositor.
        let Ok(mut rep) = self.replica.lock() else {
            return;
        };
        // The peer's own identity, not the liveness-handle
        // digest: this decides whether the payload may rewrite
        // a room's governance.
        rep.handle_from(&crate::shell::state::PeerOrigin::peer(&peer), msg);
        let refreshed_room = match msg {
            NeighbourMsg::SyncReq { room, .. } | NeighbourMsg::SyncResp { room, .. } => {
                RoomId::parse(room)
            }
            _ => None,
        };
        let refreshed_projection =
            refreshed_room.map(|room| (room, rep.tree.read().trusted_identity_projection(room)));
        let outgoing = rep.drain_outgoing();
        drop(rep);
        if let Some((room, projection)) = refreshed_projection {
            crate::shell::launcher::set_trusted_identity_projection(room, &projection);
        }
        for out_msg in outgoing {
            let _ = self.publish_message(out_msg);
        }
    }

    fn accept_identity_hello(
        &self,
        workspace_topic: u64,
        origin: &crate::shell::state::PeerOrigin,
        peer: &iroh::EndpointId,
        cert: &tilo_user_identity::DeviceCert,
        revocations: &[tilo_user_identity::RevocationEntry],
    ) {
        if cert.device_public_key != *peer.as_bytes() || revocations.len() > 4096 {
            tracing::warn!(peer = %peer, "rejecting invalid peer identity handshake");
            self.peer_identities.remove(origin);
            self.drop_held_remote_actions(*peer);
            return;
        }
        let mut list = tilo_user_identity::RevocationList::new();
        for entry in revocations {
            if entry.user_public_key != cert.user_public_key || list.insert(entry.clone()).is_err()
            {
                tracing::warn!(peer = %peer, "rejecting peer identity with invalid revocation set");
                self.peer_identities.remove(origin);
                self.drop_held_remote_actions(*peer);
                return;
            }
        }
        self.peer_identities
            .set_revocations(cert.user_public_key, &list);
        let was_known = self.peer_identities.get(origin).is_some();
        let now_ns = now_unix_nanos();
        if self
            .peer_identities
            .insert(origin.clone(), cert.clone(), now_ns)
            .is_err()
            || self.peer_identities.is_revoked(
                &cert.user_public_key,
                &cert.device_public_key,
                now_ns,
            )
        {
            tracing::warn!(peer = %peer, "rejecting expired, invalid, or revoked peer device");
            self.peer_identities.remove(origin);
            self.drop_held_remote_actions(*peer);
            return;
        }
        tracing::debug!(
            peer = %peer,
            workspace_topic,
            user = %hex::encode(cert.user_public_key),
            "accepted collaboration device identity"
        );
        if let Some(room) = self.replica.lock().ok().and_then(|replica| {
            replica
                .tree
                .read()
                .rooms()
                .into_iter()
                .find(|room| room_topic_id(*room) == workspace_topic)
        }) {
            self.name_own_device(room, peer, cert);
            self.handle_identity_announcement(peer, &room.to_string(), cert, revocations);
        }
        // The initial SyncReq can race ahead of the identity hello and is
        // intentionally dropped. Re-kick anti-entropy after trust is
        // established so joining never waits for another topology event.
        // Presence sent before trust was dropped the same way: say it again.
        if !was_known {
            self.note_peer_joined();
            self.publish_sync_requests();
        }
        self.release_held_remote_actions(*peer);
    }

    /// Give another of this person's devices its certified label in a room
    /// this device governs, so the person's presence there reads as that
    /// device ("Orange Pi") rather than an anonymous session. Naming stays
    /// the governor's act; the certificate was authenticated by the hello
    /// and belongs to this same person.
    fn name_own_device(
        &self,
        room: RoomId,
        peer: &iroh::EndpointId,
        cert: &tilo_user_identity::DeviceCert,
    ) {
        if self.personal_continuity_user != Some(cert.user_public_key) {
            return;
        }
        let Some(person) = self.personal_person() else {
            return;
        };
        let Ok(replica) = self.replica.lock() else {
            return;
        };
        let mut tree = replica.tree.write();
        if tree.governs_room(room) {
            let _ = tree.name_peer(room, &person, &peer.to_string(), &cert.device_label);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "invite redemption keeps room/expiry/recipient checks, membership persistence and post-admission certification in order"
    )]
    fn redeem_invited_identity(
        &self,
        workspace_topic: u64,
        invite_id: [u8; 32],
        peer: &iroh::EndpointId,
        cert: &tilo_user_identity::DeviceCert,
    ) {
        let room = self.replica.lock().ok().and_then(|replica| {
            replica
                .tree
                .read()
                .rooms()
                .into_iter()
                .find(|room| room_topic_id(*room) == workspace_topic)
        });
        let Some(room) = room else { return };
        let admission = self
            .pending_identity_admissions
            .lock()
            .ok()
            .and_then(|pending| pending.get(&invite_id).cloned());
        let Some(admission) = admission else { return };
        let now = tilo_clock::unix_now();
        if admission.room != room || admission.expiry_unix_seconds < now {
            if let Ok(mut pending) = self.pending_identity_admissions.lock() {
                pending.remove(&invite_id);
                if let Some(path) = self.pending_invitations_path.as_deref() {
                    persist_pending_invitations(path, &pending);
                }
            }
            return;
        }
        if admission
            .recipient_user_public_key
            .is_some_and(|recipient| recipient != cert.user_public_key)
        {
            tracing::warn!(
                room = %room,
                peer = %peer,
                "refusing invite redemption by a different person identity"
            );
            return;
        }
        let collab_types::caps::Principal::Person(person) =
            collab_types::caps::Principal::person_from_key(
                collab_types::crypto::Ed25519PublicKey::from_bytes(cert.user_public_key),
            )
        else {
            return;
        };
        let Ok(replica) = self.replica.lock() else {
            return;
        };
        let mut tree = replica.tree.write();
        let local = crate::shell::state::local_user_id_for_room(&room);
        let (granted, owner) = {
            let Some(workspace) = tree
                .desktop
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.room == room)
            else {
                return;
            };
            let room_scope = workspace.room.to_string();
            let granted = workspace
                .grant_as(
                    &local,
                    crate::shell::state::CapOverride::membership(
                        crate::shell::state::Subject::person(person.clone()),
                        room_scope,
                        admission.role.verb(),
                    ),
                )
                .is_some();
            (granted, workspace.owner.clone())
        };
        if !granted {
            tracing::warn!(
                room = %room,
                grantor = %local,
                ?owner,
                "workspace owner could not grant invited identity membership"
            );
            return;
        }
        let session = crate::network::behaviour::peer_liveness_handle(peer);
        tree.admit_collaborator_in_room(
            room,
            crate::shell::state::Collaborator::new(person, admission.name.clone())
                .with_session(session),
        );
        let _ = tree.name_peer(room, &local, &peer.to_string(), &admission.name);
        // The peer may already have been named by its first SyncReq. In
        // that case `name_peer` is a no-op, so project the membership grant
        // explicitly instead of relying on the naming path to do it.
        tree.sync_room_doc(room);
        tree.queue_save();
        drop(tree);
        drop(replica);
        if let Ok(mut pending) = self.pending_identity_admissions.lock() {
            pending.remove(&invite_id);
            if let Some(path) = self.pending_invitations_path.as_deref() {
                persist_pending_invitations(path, &pending);
            }
        }
        // The guest's first announcement can arrive before exact-token
        // redemption and is therefore refused. Retry certification now that
        // governance has named and admitted this endpoint; the periodic V2
        // announcement will subsequently project any revocations too.
        self.handle_identity_announcement(peer, &room.to_string(), cert, &[]);
        tracing::info!(
            room = %room,
            user = %hex::encode(cert.user_public_key),
            role = admission.role.label(),
            "admitted invited collaboration identity"
        );
    }

    /// Authenticate a signed document envelope against the transport device,
    /// exact document route, and current workspace policy. Replay and
    /// signature-state checks remain in the broker drainer, which owns the
    /// channel log.
    fn peer_document_role(
        &self,
        origin: &crate::shell::state::PeerOrigin,
        cluster_id: &str,
        app_id: &str,
        record_id: &str,
        envelope: &collab_types::SignedOp,
    ) -> Option<collab_types::Role> {
        let identity = self.peer_identities.get(origin)?;
        if *envelope.author.as_bytes() != identity.device_pubkey {
            return None;
        }
        let person = collab_types::caps::Principal::person_from_key(
            collab_types::Ed25519PublicKey::from_bytes(identity.user_pubkey),
        )
        .id()
        .to_string();
        let room = RoomId::parse(cluster_id)?;
        let replica = self.replica.lock().ok()?;
        let tree_handle = replica.tree.clone();
        drop(replica);
        let tree = tree_handle.read();
        let workspace = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room == room)?;
        let role = workspace.role_of(&person)?;
        if collab_types::validate_document_payload_route(app_id, cluster_id, record_id, envelope)
            .is_err()
        {
            return None;
        }
        let artifact = envelope.artifact;
        let actor = collab_types::Actor::person(Some(role), person)
            .with_principal(collab_types::Principal::App(app_id.to_string()))
            .in_room(cluster_id.to_string());
        let resource = collab_types::ResourceRef::Artifact {
            app: Some(app_id.to_string()),
            kind: collab_types::ArtifactKind::Note,
            instance: artifact,
        };
        let required = collab_types::caps::verb_for_opclass(envelope.op_class);
        let allowed =
            collab_types::role_permits(role, collab_types::ArtifactKind::Note, envelope.op_class)
                && workspace.allows(&actor, &resource, required);
        drop(tree);
        allowed.then_some(role)
    }
}

fn selected_device_cert(
    room: RoomId,
    keypair: &iroh::SecretKey,
) -> Result<tilo_user_identity::DeviceCert, String> {
    let profile = crate::identity_registry::selected_identity_profile(&room.to_string())
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "no identity is selected for this workspace".to_string())?;
    let paths = crate::firstboot::collaboration_identity_paths();
    let cert = tilo_user_identity::persist::read_device_cert(
        &paths.device_cert_for_user(&profile.user_public_key),
    )
    .map_err(|error| error.to_string())?;
    tilo_user_identity::verify_device_cert(&cert).map_err(|error| error.to_string())?;
    if cert.user_public_key != profile.user_public_key
        || cert.device_public_key != *keypair.public().as_bytes()
    {
        return Err("workspace identity certificate does not match this device".into());
    }
    if crate::device_fleet::DeviceFleetStore::from_env()
        .and_then(|store| store.load(&profile.id))
        .is_ok_and(|fleet| fleet.is_revoked(&cert.device_public_key))
    {
        return Err("this device has been revoked for the workspace identity".into());
    }
    Ok(cert)
}

fn host_identity_for_key(
    keypair: &iroh::SecretKey,
    keypair_path: &std::path::Path,
) -> Option<HostIdentity> {
    let identity_root = keypair_path.parent()?.parent()?;
    let paths = tilo_user_identity::IdentityPaths::under(identity_root);
    let user_public_key = tilo_user_identity::persist::read_public_key(&paths.user_pubkey).ok()?;
    let device_cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert).ok()?;
    if tilo_user_identity::verify_device_cert(&device_cert).is_err()
        || device_cert.user_public_key != user_public_key
        || device_cert.device_public_key != *keypair.public().as_bytes()
    {
        tracing::warn!("ignoring invalid collaboration device certificate");
        return None;
    }
    let mut workspace_device_certs = std::collections::HashMap::new();
    let mut workspace_revocations = std::collections::HashMap::new();
    if let Ok(store) = crate::identity_registry::IdentityRegistryStore::from_env()
        && let Ok(registry) = store.load_reconciled()
    {
        for (room, identity_id) in &registry.workspace_identities {
            let Some(profile) = registry.identities.get(identity_id) else {
                continue;
            };
            let Some(room_id) = RoomId::parse(room) else {
                continue;
            };
            let cert_path = paths.device_cert_for_user(&profile.user_public_key);
            let Ok(cert) = tilo_user_identity::persist::read_device_cert(&cert_path) else {
                tracing::warn!(
                    room = %room_id,
                    identity = %identity_id,
                    "workspace identity has no device certificate"
                );
                continue;
            };
            if tilo_user_identity::verify_device_cert(&cert).is_ok()
                && cert.user_public_key == profile.user_public_key
                && cert.device_public_key == *keypair.public().as_bytes()
                && !crate::device_fleet::DeviceFleetStore::from_env()
                    .and_then(|store| store.load(identity_id))
                    .is_ok_and(|fleet| fleet.is_revoked(&cert.device_public_key))
            {
                // The identity's continuity topic, where its other devices
                // meet this one for its person room, presents the same cert.
                let continuity = personal_continuity_topic_id(&cert.user_public_key);
                workspace_device_certs.insert(continuity, cert.clone());
                workspace_device_certs.insert(room_topic_id(room_id), cert);
                if let Ok(fleet) = crate::device_fleet::DeviceFleetStore::from_env()
                    .and_then(|store| store.load(identity_id))
                {
                    workspace_revocations.insert(continuity, fleet.revocations.clone());
                    workspace_revocations.insert(room_topic_id(room_id), fleet.revocations);
                }
            }
        }
    }
    Some(HostIdentity {
        device_cert,
        device_signing_key: ed25519_dalek::SigningKey::from_bytes(&keypair.to_bytes()),
        workspace_device_certs,
        workspace_revocations,
        identity_paths: Some(paths),
    })
}

#[cfg(test)]
fn load_identity_revocations(
    path: &std::path::Path,
    user_public_key: [u8; 32],
) -> Result<Vec<tilo_user_identity::RevocationEntry>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let entries: Vec<tilo_user_identity::RevocationEntry> =
        postcard::from_bytes(&bytes).map_err(|error| error.to_string())?;
    if entries.len() > tilo_user_identity::MAX_TRUSTED_IDENTITY_PROJECTION_DEVICES
        || entries.iter().any(|entry| {
            entry.user_public_key != user_public_key
                || tilo_user_identity::verify_revocation_entry(entry).is_err()
        })
    {
        return Err("revocation entries are invalid, cross-root, or over capacity".into());
    }
    Ok(entries)
}

/// See [`ShellNetwork::device_fabric_reads`].
#[cfg(test)]
#[derive(Default)]
struct DeviceFabricReads {
    cloned: std::sync::atomic::AtomicUsize,
    borrowed: std::sync::atomic::AtomicUsize,
}

impl Drop for ShellNetwork {
    fn drop(&mut self) {
        // `take` + explicit shutdown avoids the noisy "runtime dropped
        // while still running tasks" warning from the implicit path.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// Test-only constructor that wires `ShellNetwork`'s command sender to
/// a caller-owned receiver, bypassing the iroh actor. `pub(crate)` so
/// the broker's own tests can drive the inbound side of the peer bridge too.
#[cfg(test)]
#[allow(
    clippy::too_many_lines,
    reason = "the fixture spells out every field of `ShellNetwork` in one literal, as `spawn_with` does"
)]
pub(crate) fn shell_network_for_test(
    tree: WorkspaceTreeHandle,
    local_peer_hash: u64,
) -> (ShellNetwork, mpsc::Receiver<NetCmd>) {
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let (event_tx, event_rx) = mpsc::channel(64);
    let keypair = iroh::SecretKey::generate();
    let local_peer_id = keypair.public();
    let mut joined_topics = std::collections::HashSet::from([DEFAULT_WS_TOPIC]);
    joined_topics.extend(tree.read().rooms().into_iter().map(room_topic_id));
    let net = ShellNetwork {
        cmd_tx,
        replica: Arc::new(Mutex::new(WorkspaceTreeReplica::new(1, tree))),
        event_tx,
        event_rx: Mutex::new(event_rx),
        event_wake: Arc::default(),
        joined_topics: Mutex::new(joined_topics),
        workspace_control_direct: Mutex::new(std::collections::HashSet::new()),
        workspace_control_relays: Mutex::new(std::collections::HashSet::new()),
        runtime: None,
        local_peer_hash,
        keypair,
        host_identity: None,
        local_peer_id,
        listen_addrs: Arc::new(Mutex::new(Vec::new())),
        reachability: crate::network::Reachability::default(),
        host_addrs: Mutex::new(Vec::new()),
        registered_home_peer_ids: Mutex::new(std::collections::HashSet::new()),
        dialed_direct_peers: Mutex::new(std::collections::HashSet::new()),
        live_room_peers: Mutex::new(std::collections::HashSet::new()),
        registered_home_device_ids: Mutex::new(std::collections::HashSet::new()),
        personal_device_peers: Mutex::default(),
        last_home_peer_refresh: Mutex::new(None),
        last_sync_kick: Mutex::new(None),
        next_sync_pull: Mutex::new(None),
        peers_joined: std::sync::atomic::AtomicU64::new(0),
        room_push: Mutex::new(RoomPush::default()),
        personal_continuity: Mutex::default(),
        personal_continuity_user: None,
        workspace_person_roots: Vec::new(),
        personal_continuity_device: None,
        personal_continuity_counter: Mutex::new(0),
        personal_continuity_fingerprint: Mutex::new(None),
        personal_continuity_path: None,
        personal_workspaces: None,
        last_personal_workspaces_snapshot: Mutex::new(None),
        personal_focus_recency: Mutex::default(),
        continuity_policy_override: Some(
            crate::shell::continuity_policy::ContinuityPolicySnapshot::from_document(
                serde_json::json!({
                    "continuity": {
                        "enabled": true,
                        "clipboard": true,
                        "notifications": true,
                        "wifi_settings": true,
                        "media": true,
                        "display_input": true,
                        "file_transfer": true,
                        "realtime_sessions": true,
                        "typed_gamepad": true,
                        "app_continuations": true
                    }
                })
                .as_object()
                .expect("test continuity policy object"),
            ),
        ),
        device_fabric: Mutex::new(crate::shell::device_fabric::DeviceFabric::default()),
        device_fabric_reads: DeviceFabricReads::default(),
        device_fabric_path: None,
        personal_fabric_actions: Mutex::new(VecDeque::new()),
        authenticated_personal_routes: Mutex::new(std::collections::HashMap::new()),
        personal_fabric_action_replays: Mutex::new(PersonalFabricReplayWindow::default()),
        peer_identities: PeerIdentityStore::new(),
        require_peer_identity: false,
        pending_identity_admissions: Mutex::new(std::collections::HashMap::new()),
        pending_invitations_path: None,
        pending_redemptions: Mutex::new(std::collections::HashMap::new()),
        native_enrollment: native_enrollment::State::default(),
        hosted_topology: hosted_topology::State::default(),
        relay_rooms: relay_rooms::State::default(),
        record_content: record_content::State::default(),
        inbound_presence: Arc::new(Mutex::new(Vec::new())),
        inbound_documents: Arc::new(Mutex::new(Vec::new())),
        drain_wake: std::sync::Arc::new(crate::idle_wake::DrainPark::default()),
        inbound_channel_governance: Arc::new(Mutex::new(Vec::new())),
        wire_records: Mutex::new(crate::network::wire_record::WireRecordIds::default()),
        person_artifacts: Mutex::default(),
        person_snapshots_wanted: Mutex::default(),
        inbound_app_presence: Arc::new(Mutex::new(Vec::new())),
        live_presence: Mutex::default(),
        live_labels: Mutex::default(),
        peer_colours: Mutex::default(),
        peer_colours_epoch: std::sync::atomic::AtomicU64::new(0),
        inbound_scrollback_request: Arc::new(Mutex::new(Vec::new())),
        inbound_scrollback_response: Arc::new(Mutex::new(Vec::new())),
        inbound_remote_actions: Arc::new(Mutex::new(Vec::new())),
        held_remote_actions: Mutex::new(std::collections::VecDeque::new()),
        remote_action_replies: crate::shell::remote_action_link::ReplyWaiters::default(),
        remote_use: Mutex::default(),
        inbound_legacy: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        workspace_control_observers: Mutex::new(Vec::new()),
        peer_departures: Arc::new(PeerDepartureHistory::default()),
    };
    (net, cmd_rx)
}

/// Read-service custody fixture; transport is proved by the real native actor separately.
#[cfg(test)]
pub(crate) fn native_read_network_for_test(
    tree: WorkspaceTreeHandle,
    identity: HostIdentity,
) -> (ShellNetwork, mpsc::Receiver<NetCmd>) {
    let (mut network, commands) = shell_network_for_test(tree, 1);
    network.keypair = iroh::SecretKey::from_bytes(&identity.device_signing_key.to_bytes());
    network.local_peer_id = network.keypair.public();
    network.host_identity = Some(identity);
    (network, commands)
}

/// Deliver a fixture response through production peer authentication and RPC matching.
#[cfg(test)]
pub(crate) fn native_remote_answer_for_test(
    network: &ShellNetwork,
    workspace: u64,
    from: iroh::EndpointId,
    message: tilo_actions::remote::RemoteActionMessage,
) {
    let frame = tilo_actions::remote::RemoteActionFrame::new(message)
        .encode()
        .unwrap();
    network.dispatch_neighbour_msg(
        workspace,
        from,
        None,
        false,
        NeighbourMsg::RemoteAction { frame },
    );
}

/// Give a fixture network the worker runtime a spawned one owns, so governor
/// invocations are answered instead of refused as "the network is not running".
#[cfg(test)]
pub(crate) fn with_test_runtime(network: &mut ShellNetwork) {
    network.runtime = Some(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("fixture network runtime"),
    );
}

impl crate::shell::remote_action_link::RemoteActionLink for ShellNetwork {
    fn request(
        &self,
        workspace_topic: u64,
        peer: iroh::EndpointId,
        message: tilo_actions::remote::RemoteActionMessage,
    ) -> Result<tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage>, String>
    {
        let key = crate::shell::remote_action_link::ReplyKey::for_request(&message)
            .ok_or_else(|| "only requests are sent to a remote action host".to_string())?;
        let answer = self.remote_action_replies.register(peer, key);
        if self.send_remote_action(workspace_topic, peer, message) {
            Ok(answer)
        } else {
            Err("the remote action could not be handed to the network".into())
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
mod tests {
    use super::*;

    #[test]
    fn a_local_tab_is_pushed_on_the_next_tick_and_an_idle_tick_sends_nothing() {
        const SALT: u16 = 0xCAFE;
        let tree = WorkspaceTree::bootstrap_demo().shared();
        tree.write().set_salt_override(SALT);
        let room = tree.read().rooms()[0];
        let topic = room_topic_id(room);
        let (net, mut cmd_rx) = shell_network_for_test(tree.clone(), 0xCAFE);
        let drain_pushes = |cmd_rx: &mut mpsc::Receiver<NetCmd>| {
            let mut pushes = Vec::new();
            while let Ok(cmd) = cmd_rx.try_recv() {
                if let NetCmd::Publish {
                    ws,
                    msg: NeighbourMsg::SyncResp { room, changes },
                } = cmd
                {
                    pushes.push((ws, room, changes));
                }
            }
            pushes
        };
        // The first tick learns where every room stands and announces none
        // of its history; a peer that lacks it pulls it.
        net.tick();
        assert!(drain_pushes(&mut cmd_rx).is_empty());
        net.tick();
        assert!(
            drain_pushes(&mut cmd_rx).is_empty(),
            "an idle tick sends nothing"
        );
        let before = tree
            .read()
            .room_docs
            .get(&room)
            .cloned()
            .expect("room document");

        let tab = tilo_desktop_model::compose_id(SALT, 77);
        {
            let mut tree = tree.write();
            let workspace = tree
                .desktop
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.room == room)
                .expect("room workspace");
            let group = workspace
                .strips
                .iter_mut()
                .flat_map(|strip| strip.window_groups.iter_mut())
                .next()
                .expect("a window group");
            group.tabs.push(crate::shell::state::Tab {
                id: tab,
                title: "Pushed".into(),
                tile: crate::shell::state::TileContent::Stub {
                    kind: crate::shell::state::AppKind::Other("so.tilo.notes".into()),
                    label: "Notes".into(),
                },
            });
            tree.seed_doc_from_cache();
        }
        net.tick();
        let pushes = drain_pushes(&mut cmd_rx);
        let [(ws, pushed_room, changes)] = pushes.as_slice() else {
            panic!("one push for the changed room, got {pushes:?}");
        };
        assert_eq!(
            (*ws, pushed_room.as_str()),
            (topic, room.to_string().as_str())
        );

        let mut peer = before;
        for change in changes {
            tilo_automerge::ApplyIncoming::apply_incoming(
                &mut peer,
                [automerge::Change::from_bytes(change.clone()).expect("change")],
            )
            .expect("push applies on top of what was announced");
        }
        assert!(
            tilo_desktop_model::projection::contents_of(&peer)
                .iter()
                .flat_map(|strip| &strip.groups)
                .flat_map(|group| &group.tabs)
                .any(|candidate| candidate.id == tab),
            "the pushed change carries the new tab"
        );

        net.tick();
        assert!(
            drain_pushes(&mut cmd_rx).is_empty(),
            "nothing new, nothing sent"
        );
    }
    use crate::shell::state::FreshId as _;

    #[test]
    fn personal_device_peers_are_dialable_endpoints_except_this_device() {
        let dir = tempfile::tempdir().expect("temp");
        let path = dir.path().join("personal-device-peers.json");
        let own = [0x11; 32];
        let peer = [0x22; 32];
        std::fs::write(
            &path,
            serde_json::to_vec(&vec![hex::encode(own), hex::encode(peer)]).unwrap(),
        )
        .unwrap();
        let addrs = personal_device_peer_addrs(&path, Some(own));
        assert_eq!(addrs.len(), 1);
        assert_eq!(*addrs[0].id.as_bytes(), peer);
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn enrolled_device_from_the_imager_is_an_action_recipient() {
        let dir = tempfile::tempdir().expect("temp");
        let peers = dir.path().join("home-peers.json");
        std::fs::write(&peers, b"[]").unwrap();
        let device = [0x22_u8; 32];
        std::fs::write(
            dir.path().join("personal-device-peers.json"),
            serde_json::to_vec(&vec![hex::encode(device)]).unwrap(),
        )
        .unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_HOME_PEERS_PATH", &peers);
        let user = [0x44_u8; 32];
        let (mut net, mut commands) = personal_continuity_network_for_test(user);
        net.continuity_policy_override = Some(test_continuity_policy(false, false, false));
        let id = crate::shell::personal_continuity::DeviceId::new(hex::encode(device)).unwrap();
        assert!(net.certified_secret_recipients().contains(&id));
        let request_id = net
            .send_personal_fabric_action(
                &id,
                crate::shell::device_fabric::PersonalFabricAction::MediaAccepted {
                    request_id: [0x7; 16],
                    session_id: "player:one".into(),
                },
            )
            .expect("enrolled device accepts a personal action");
        assert_ne!(request_id, [0; 16]);
        let mut found = false;
        while let Ok(command) = commands.try_recv() {
            if matches!(
                command,
                NetCmd::PublishPersonalFabricTarget {
                    payload: PersonalFabricPayload::Action(_),
                    ..
                }
            ) {
                found = true;
            }
        }
        assert!(found, "the action must be handed to the personal transport");
    }

    /// Enrol `devices` the way the identity host does: an atomic replace.
    fn write_personal_device_peers(dir: &Path, devices: &[[u8; 32]]) {
        let ids = devices.iter().map(hex::encode).collect::<Vec<_>>();
        let staged = dir.join("personal-device-peers.json.staged");
        std::fs::write(&staged, serde_json::to_vec(&ids).unwrap()).unwrap();
        std::fs::rename(&staged, dir.join("personal-device-peers.json")).unwrap();
    }

    fn remote_audio_frame() -> crate::shell::device_fabric::PersonalFabricAction {
        crate::shell::device_fabric::PersonalFabricAction::RemoteAudioFrame {
            session_id: [0x5; 16],
            authorization: [0x6; 16],
            sequence: 1,
            media_ts: 48_000,
            presentation_ns: 1_000_000_000
                + crate::shell::device_fabric::REMOTE_AUDIO_GROUP_LATENCY_NS,
            pcm_s16le: vec![0; crate::shell::device_fabric::MAX_REMOTE_AUDIO_FRAME_BYTES],
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn certified_recipients_cache_rereads_only_on_change() {
        let dir = tempfile::tempdir().expect("temp");
        let peers = dir.path().join("home-peers.json");
        std::fs::write(&peers, b"[]").unwrap();
        let kept_key = *iroh::SecretKey::generate().public().as_bytes();
        let removed_key = *iroh::SecretKey::generate().public().as_bytes();
        write_personal_device_peers(dir.path(), &[kept_key, removed_key]);
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_HOME_PEERS_PATH", &peers);
        let (net, mut commands) = personal_continuity_network_for_test([0x44; 32]);
        let kept = crate::shell::personal_continuity::DeviceId::new(hex::encode(kept_key)).unwrap();
        let removed =
            crate::shell::personal_continuity::DeviceId::new(hex::encode(removed_key)).unwrap();
        let reads = || net.personal_device_peers.lock().unwrap().reads();
        // One second of audio to two speakers, on the production send path.
        for _ in 0..50 {
            for target in [&kept, &removed] {
                net.try_send_personal_fabric_action(target, remote_audio_frame())
                    .expect("enrolled device takes an audio frame");
                while commands.try_recv().is_ok() {}
            }
        }
        for _ in 0..1_000 {
            assert!(net.certified_secret_recipients().contains(&removed));
        }
        assert_eq!(
            reads(),
            1,
            "the enrolment file is read once, not per message"
        );

        write_personal_device_peers(dir.path(), &[kept_key]);
        net.recheck_personal_device_peers();
        let recipients = net.certified_secret_recipients();
        assert!(recipients.contains(&kept) && !recipients.contains(&removed));
        assert_eq!(reads(), 2);
        net.recheck_personal_device_peers();
        assert!(!net.certified_secret_recipients().contains(&removed));
        assert_eq!(
            reads(),
            2,
            "a recheck of an unchanged file does not read it"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn removed_device_is_refused_within_recheck_window() {
        let dir = tempfile::tempdir().expect("temp");
        let peers = dir.path().join("home-peers.json");
        std::fs::write(&peers, b"[]").unwrap();
        let kept = *iroh::SecretKey::generate().public().as_bytes();
        let removed = *iroh::SecretKey::generate().public().as_bytes();
        write_personal_device_peers(dir.path(), &[kept, removed]);
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_HOME_PEERS_PATH", &peers);
        let (net, mut commands) = personal_continuity_network_for_test([0x44; 32]);
        let removed =
            crate::shell::personal_continuity::DeviceId::new(hex::encode(removed)).unwrap();
        net.try_send_personal_fabric_action(&removed, remote_audio_frame())
            .expect("an enrolled device takes an audio frame");
        while commands.try_recv().is_ok() {}

        write_personal_device_peers(dir.path(), &[kept]);
        // Real time, no test hook: revocation must land by the recheck bound.
        std::thread::sleep(personal_device_peers::RECHECK);
        assert!(matches!(
            net.try_send_personal_fabric_action(&removed, remote_audio_frame()),
            Err(PersonalFabricSendError::Rejected(_))
        ));
        assert!(
            commands.try_recv().is_err(),
            "nothing reaches the transport for a removed device"
        );
    }

    #[test]
    fn a_codec_advert_outside_a_room_is_recorded() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[0x45; 32]);
        let user = root.verifying_key().to_bytes();
        let (net, mut commands) = personal_continuity_network_for_test(user);
        let device = iroh::SecretKey::from_bytes(&[0x23; 32]);
        authenticated_personal_peer(&net, &root, &device);
        while commands.try_recv().is_ok() {}
        let advertiser = crate::network::behaviour::peer_liveness_handle(&device.public());
        for ws in [DEFAULT_WS_TOPIC, personal_continuity_topic_id(&user)] {
            net.dispatch_neighbour_msg(
                ws,
                device.public(),
                None,
                false,
                NeighbourMsg::MediaCapabilities {
                    peer: advertiser,
                    hardware_decode: vec!["h264".into()],
                    software_decode: Vec::new(),
                    hardware_encode: Vec::new(),
                    software_encode: Vec::new(),
                },
            );
        }
        assert!(
            crate::network::media_caps::capabilities(advertiser)
                .hardware_decode
                .iter()
                .any(|codec| codec.wire_label() == "h264"),
            "a codec advert from an authenticated device is recorded without a shared room"
        );
        assert!(commands.try_recv().is_err());

        let stranger = iroh::SecretKey::from_bytes(&[0x24; 32]);
        let unproven = crate::network::behaviour::peer_liveness_handle(&stranger.public());
        net.dispatch_neighbour_msg(
            DEFAULT_WS_TOPIC,
            stranger.public(),
            None,
            false,
            NeighbourMsg::MediaCapabilities {
                peer: unproven,
                hardware_decode: vec!["h264".into()],
                software_decode: Vec::new(),
                hardware_encode: Vec::new(),
                software_encode: Vec::new(),
            },
        );
        assert!(
            crate::network::media_caps::capabilities(unproven)
                .hardware_decode
                .is_empty(),
            "an unauthenticated device's advert is not recorded"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn direct_peers_are_dialed_when_the_relay_registry_is_invalid() {
        let dir = tempfile::tempdir().expect("temp");
        let peers = dir.path().join("home-peers.json");
        std::fs::write(&peers, b"not-json").unwrap();
        let device = [0x22_u8; 32];
        std::fs::write(
            dir.path().join("personal-device-peers.json"),
            serde_json::to_vec(&vec![hex::encode(device)]).unwrap(),
        )
        .unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_HOME_PEERS_PATH", &peers);
        env.set("XDG_STATE_HOME", dir.path().join("state"));
        let (net, mut commands) = personal_continuity_network_for_test([0x44; 32]);
        assert!(net.refresh_home_peer_hosts());
        assert!(
            std::iter::from_fn(|| commands.try_recv().ok()).any(
                |command| matches!(command, NetCmd::Connect { addr } if *addr.id.as_bytes() == device)
            ),
            "an invalid relay registry must not skip the enrolled device"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn a_room_enrolled_after_startup_is_joined() {
        let dir = tempfile::tempdir().expect("temp");
        let peers = dir.path().join("home-peers.json");
        std::fs::write(&peers, b"[]").unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_HOME_PEERS_PATH", &peers);
        env.set("XDG_STATE_HOME", dir.path().join("state"));
        let (net, mut commands) = personal_continuity_network_for_test([0x44; 32]);
        assert!(net.join_default_workspace());
        while commands.try_recv().is_ok() {}
        let room = "22222222-2222-4222-8222-222222222222";
        std::fs::write(
            dir.path().join("personal-rooms.json"),
            format!("[\"{room}\"]"),
        )
        .unwrap();
        assert!(net.refresh_home_peer_hosts());
        let topic = room_topic_id(RoomId::parse(room).expect("room"));
        assert!(
            std::iter::from_fn(|| commands.try_recv().ok())
                .any(|command| matches!(command, NetCmd::JoinWorkspace { ws, .. } if ws == topic)),
            "a room that appears after startup must be joined"
        );
        let _ = net.refresh_home_peer_hosts();
        assert!(
            std::iter::from_fn(|| commands.try_recv().ok())
                .all(|command| !matches!(command, NetCmd::JoinWorkspace { ws, .. } if ws == topic)),
            "a joined room is not joined again"
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial(process_env)]
    fn an_unchanged_room_export_is_not_rewritten() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = tempfile::tempdir().expect("temp");
        let peers = dir.path().join("home-peers.json");
        std::fs::write(&peers, b"[]").unwrap();
        let state = dir.path().join("state");
        let export = state.join("tilo/personal-rooms.json");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_HOME_PEERS_PATH", &peers);
        env.set("XDG_STATE_HOME", &state);
        let (net, _commands) = personal_continuity_network_for_test([0x44; 32]);
        assert!(net.join_default_workspace());
        let published = std::fs::metadata(&export).expect("export").ino();
        let _ = net.refresh_home_peer_hosts();
        assert_eq!(
            std::fs::metadata(&export).expect("export").ino(),
            published,
            "an unchanged room list must not be republished on every refresh"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unchanged_personal_continuity_is_not_rewritten() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("personal-continuity.postcard");
        let value = |counter| PersistedPersonalContinuity {
            counter,
            continuity: crate::shell::personal_continuity::PersonalContinuity::default(),
        };
        assert!(persist_personal_continuity(&path, &value(7)));
        let written = std::fs::metadata(&path).unwrap().ino();
        assert!(persist_personal_continuity(&path, &value(7)));
        assert_eq!(
            std::fs::metadata(&path).unwrap().ino(),
            written,
            "the same state must not be rewritten"
        );
        assert!(persist_personal_continuity(&path, &value(8)));
        assert_ne!(std::fs::metadata(&path).unwrap().ino(), written);
        assert_eq!(load_personal_continuity(&path).counter, 8);
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn an_enrolled_action_goes_to_the_named_room_or_the_one_the_device_is_in() {
        let dir = tempfile::tempdir().expect("temp");
        let peers = dir.path().join("home-peers.json");
        std::fs::write(&peers, b"[]").unwrap();
        let device = [0x22_u8; 32];
        std::fs::write(
            dir.path().join("personal-device-peers.json"),
            serde_json::to_vec(&vec![hex::encode(device)]).unwrap(),
        )
        .unwrap();
        let absent = "11111111-1111-4111-8111-111111111111";
        let present = "22222222-2222-4222-8222-222222222222";
        std::fs::write(
            dir.path().join("personal-rooms.json"),
            format!("[\"{absent}\", \"{present}\"]"),
        )
        .unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_HOME_PEERS_PATH", &peers);
        env.set("XDG_STATE_HOME", dir.path().join("state"));
        let (net, mut commands) = personal_continuity_network_for_test([0x44; 32]);
        assert!(net.join_default_workspace());
        while commands.try_recv().is_ok() {}
        let endpoint = iroh::EndpointId::from_bytes(&device).expect("endpoint");
        let present_topic = room_topic_id(RoomId::parse(present).expect("room"));
        net.live_room_peers
            .lock()
            .expect("live peers")
            .insert((present_topic, endpoint));
        let invoke = |provider: String| {
            net.invoke_enrolled_device(
                &hex::encode(device),
                tilo_actions::remote::RemoteActionTarget {
                    provider,
                    address: tilo_actions::ActionAddress::new("app.so.tilo.terminal", "open"),
                },
                tilo_actions::remote::InvokeArgument::None,
            )
        };
        assert!(
            invoke(format!("workspace:{absent}")).is_err(),
            "a named room the device is not in is not silently swapped"
        );
        assert!(invoke("workspace:33333333-3333-4333-8333-333333333333".into()).is_err());
        invoke(format!("workspace:{present}")).expect("named room");
        invoke("9".into()).expect("first room the device is in");
        let topics: Vec<u64> = std::iter::from_fn(|| commands.try_recv().ok())
            .filter_map(|command| match command {
                NetCmd::PublishRemoteAction { ws, .. } => Some(ws),
                _ => None,
            })
            .collect();
        assert_eq!(topics, vec![present_topic, present_topic]);
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn enrolled_device_receives_a_native_action_on_the_shared_room() {
        let dir = tempfile::tempdir().expect("temp");
        let peers = dir.path().join("home-peers.json");
        std::fs::write(&peers, b"[]").unwrap();
        let device = [0x22_u8; 32];
        std::fs::write(
            dir.path().join("personal-device-peers.json"),
            serde_json::to_vec(&vec![hex::encode(device)]).unwrap(),
        )
        .unwrap();
        let room = "11111111-1111-4111-8111-111111111111";
        std::fs::write(
            dir.path().join("personal-rooms.json"),
            format!("[\"{room}\"]"),
        )
        .unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_HOME_PEERS_PATH", &peers);
        let (net, mut commands) = personal_continuity_network_for_test([0x44; 32]);
        assert!(net.join_default_workspace());
        while commands.try_recv().is_ok() {}
        let address = tilo_actions::ActionAddress::new("app.files", "open");
        let offline = net.invoke_enrolled_device(
            &hex::encode(device),
            tilo_actions::remote::RemoteActionTarget {
                provider: format!("workspace:{room}"),
                address: address.clone(),
            },
            tilo_actions::remote::InvokeArgument::None,
        );
        assert!(
            offline.is_err(),
            "an offline device must not be told the action was sent"
        );
        let endpoint = iroh::EndpointId::from_bytes(&device).expect("endpoint");
        net.live_room_peers.lock().expect("live peers").insert((
            room_topic_id(crate::shell::state::RoomId::parse(room).expect("room")),
            endpoint,
        ));
        net.invoke_enrolled_device(
            &hex::encode(device),
            tilo_actions::remote::RemoteActionTarget {
                provider: format!("workspace:{room}"),
                address: address.clone(),
            },
            tilo_actions::remote::InvokeArgument::None,
        )
        .expect("enrolled device");
        let (ws, target, msg) = std::iter::from_fn(|| commands.try_recv().ok())
            .find_map(|command| match command {
                NetCmd::PublishRemoteAction { ws, target, msg } => Some((ws, target, msg)),
                _ => None,
            })
            .expect("native action");
        assert_eq!(*target.as_bytes(), device);
        assert_eq!(
            ws,
            room_topic_id(crate::shell::state::RoomId::parse(room).expect("room"))
        );
        let NeighbourMsg::RemoteAction { frame } = msg else {
            panic!("expected a remote action frame");
        };
        let decoded = tilo_actions::remote::RemoteActionFrame::decode(&frame).expect("frame");
        let tilo_actions::remote::RemoteActionMessage::Invoke(request) = decoded.message else {
            panic!("expected an invocation");
        };
        assert_eq!(request.target.address, address);
        std::fs::remove_file(dir.path().join("personal-rooms.json")).unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir_all(state.join("tilo")).unwrap();
        std::fs::write(
            state.join("tilo").join("personal-rooms.json"),
            format!("[\"{room}\"]"),
        )
        .unwrap();
        env.set("XDG_STATE_HOME", &state);
        net.invoke_enrolled_device(
            &hex::encode(device),
            tilo_actions::remote::RemoteActionTarget {
                provider: format!("workspace:{room}"),
                address,
            },
            tilo_actions::remote::InvokeArgument::None,
        )
        .expect("exported room list");
        assert!(
            std::iter::from_fn(|| commands.try_recv().ok()).any(|command| {
                matches!(
                    command,
                    NetCmd::PublishRemoteAction { target, .. } if *target.as_bytes() == device
                )
            })
        );
        let stranger = net.invoke_enrolled_device(
            &hex::encode([0x33; 32]),
            request.target,
            tilo_actions::remote::InvokeArgument::None,
        );
        assert!(stranger.is_err());
        assert!(
            std::iter::from_fn(|| commands.try_recv().ok())
                .all(|command| !matches!(command, NetCmd::PublishRemoteAction { .. }))
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn enrolled_rooms_stay_after_startup_when_continuations_are_off() {
        let dir = tempfile::tempdir().expect("temp");
        let peers = dir.path().join("home-peers.json");
        std::fs::write(&peers, b"[]").unwrap();
        let room = "11111111-1111-4111-8111-111111111111";
        std::fs::write(
            dir.path().join("personal-rooms.json"),
            format!("[\"{room}\"]"),
        )
        .unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_HOME_PEERS_PATH", &peers);
        let (mut net, _commands) = personal_continuity_network_for_test([0x44; 32]);
        net.continuity_policy_override = Some(test_continuity_policy(false, false, false));
        assert!(net.join_default_workspace());
        let rooms = net.replica.lock().expect("replica").tree.read().rooms();
        assert!(
            rooms.iter().any(|candidate| candidate.to_string() == room),
            "startup must keep the enrolled room"
        );
        let owner = net
            .replica
            .lock()
            .expect("replica")
            .tree
            .read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == room)
            .and_then(|workspace| workspace.owner.clone());
        assert_eq!(
            owner.as_deref(),
            Some(crate::shell::state::local_user_id().as_str())
        );
    }

    #[test]
    fn personal_rooms_keep_only_real_room_ids() {
        let dir = tempfile::tempdir().expect("temp");
        let path = dir.path().join("personal-rooms.json");
        std::fs::write(
            &path,
            br#"["11111111-1111-4111-8111-111111111111","not-a-room"]"#,
        )
        .unwrap();
        let rooms = personal_room_ids(&path);
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].to_string(), "11111111-1111-4111-8111-111111111111");
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn network_config_reads_tilo_names() {
        const NAMES: [&str; 5] = [
            "TILO_PEER_KEY_PATH",
            "TILO_COLLAB_HOSTS",
            "TILO_COLLAB_HOST",
            "TILO_HOME_PEERS_PATH",
            "TILO_COLLAB_LAN_DISCOVERY",
        ];
        let mut env = tilo_test_env::ScopedEnv::new();
        for name in NAMES {
            env.remove(name);
        }

        let temp = tempfile::tempdir().expect("temp config root");
        let tilo_key_path = temp.path().join("tilo-peer-key");
        let tilo_home_path = temp.path().join("tilo-home-peers.json");
        let tilo_list_host = iroh::SecretKey::from_bytes(&[0x33; 32]).public();
        let tilo_single_host = iroh::SecretKey::from_bytes(&[0x34; 32]).public();
        let tilo_hosts = serde_json::to_string(&vec![iroh::EndpointAddr::new(tilo_list_host)])
            .expect("Tilo host list");

        env.set("TILO_COLLAB_LAN_DISCOVERY", "1");
        assert!(lan_discovery_requested());

        env.set("TILO_PEER_KEY_PATH", &tilo_key_path)
            .set("TILO_COLLAB_HOSTS", tilo_hosts)
            .set("TILO_COLLAB_HOST", tilo_single_host.to_string())
            .set("TILO_HOME_PEERS_PATH", &tilo_home_path)
            .set("TILO_COLLAB_LAN_DISCOVERY", "0");
        assert_eq!(peer_key_path(), tilo_key_path);
        assert_eq!(home_peer_registry_path(), tilo_home_path);
        assert!(!lan_discovery_requested());
        assert_eq!(
            configured_host_addrs()
                .into_iter()
                .map(|host| host.id)
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from([tilo_list_host, tilo_single_host]),
        );

        drop(env);
    }

    fn test_device(byte: u8) -> crate::shell::personal_continuity::DeviceId {
        crate::shell::personal_continuity::DeviceId::new(hex::encode([byte; 32]))
            .expect("valid test device")
    }

    fn fabric_envelope(
        request_byte: u8,
        source: &crate::shell::personal_continuity::DeviceId,
        target: &crate::shell::personal_continuity::DeviceId,
        action: crate::shell::device_fabric::PersonalFabricAction,
    ) -> crate::shell::device_fabric::PersonalFabricActionEnvelope {
        crate::shell::device_fabric::PersonalFabricActionEnvelope {
            request_id: [request_byte; 16],
            source: source.clone(),
            target: target.clone(),
            issued_unix_ms: 1,
            action,
        }
    }

    fn test_capture_grant(byte: u8) -> crate::shell::remote_capture::device::RemoteCaptureGrant {
        postcard::from_bytes(&postcard::to_allocvec(&[byte; 32]).expect("serialize capture grant"))
            .expect("deserialize capture grant")
    }

    #[test]
    fn nearby_stream_cancel_targets_only_the_exact_peer_and_transfer() {
        let (net, mut commands) = shell_network_for_test(WorkspaceTree::default().shared(), 0xCA15);
        let target_endpoint = iroh::SecretKey::from_bytes(&[0xC1; 32]).public();
        let target = crate::shell::personal_continuity::DeviceId::new(hex::encode(
            target_endpoint.as_bytes(),
        ))
        .expect("target device");
        let transfer_id = [0xC2; 16];
        net.cancel_nearby_transfer_stream(&target, transfer_id)
            .expect("cancel command");
        assert!(matches!(
            commands.try_recv(),
            Ok(NetCmd::CancelNearbyTransfer {
                target: cancelled_target,
                transfer_id: cancelled_id,
            }) if cancelled_target == target_endpoint && cancelled_id == transfer_id
        ));
        let malformed = crate::shell::personal_continuity::DeviceId::new("not-a-transport-id")
            .expect("device id");
        assert!(
            net.cancel_nearby_transfer_stream(&malformed, transfer_id)
                .is_err()
        );
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn remote_action_from_an_authenticated_peer_is_confined_to_its_room() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[0xA1; 32]);
        let device = iroh::SecretKey::from_bytes(&[0xA2; 32]);
        let person = crate::shell::collab_broker_capnp::principal_id_for_root(
            root.verifying_key().to_bytes(),
        );
        let (workspace, enrolled) = remote_action_room(&person, &person);
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(workspace);
        let (mut net, _commands) = shell_network_for_test(tree.shared(), 1);
        net.require_peer_identity = true;
        let topic = room_topic_id(enrolled);
        authenticated_personal_peer_on_topic(
            &net,
            &root,
            &device,
            crate::network::actor::MembershipEpoch::for_test(1),
            topic,
        );
        let frame = tilo_actions::remote::RemoteActionFrame::new(
            tilo_actions::remote::RemoteActionMessage::Invoke(
                tilo_actions::remote::InvokeRequest {
                    on_behalf_of: None,
                    invocation: tilo_actions::InvocationId::new(),
                    target: tilo_actions::remote::RemoteActionTarget {
                        provider: format!("workspace:{enrolled}"),
                        address: tilo_actions::ActionAddress::new("app.so.tilo.files", "open"),
                    },
                    argument: tilo_actions::remote::InvokeArgument::None,
                    context: None,
                    confirmation: None,
                    revision: None,
                    wait_ms: None,
                },
            ),
        )
        .encode()
        .expect("frame");
        net.dispatch_neighbour_msg(
            topic,
            device.public(),
            Some(crate::network::actor::MembershipEpoch::for_test(1)),
            false,
            NeighbourMsg::RemoteAction { frame },
        );
        let inbound = net
            .inbound_remote_actions_handle()
            .lock()
            .expect("inbound")
            .clone();
        assert_eq!(inbound.len(), 1);
        assert_eq!(
            inbound[0].caller.room(),
            Some(enrolled.to_string().as_str())
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn remote_actions_reach_only_exact_authenticated_targets() {
        #[allow(
            clippy::too_many_lines,
            reason = "two peers, their identities, the serving broker and both round trips form one scenario"
        )]
        fn a_peer_invokes_an_app_action_on_another_peer_and_gets_the_receipt() {
            use crate::shell::collab_broker::CrdtRoute;
            use crate::shell::collab_broker_capnp::{
                PublishedAction, PublishedInput, PublishedSurface,
            };
            use crate::shell::remote_action_link::RemoteActionLink as _;
            use tilo_actions::remote::{
                InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionMessage,
                RemoteActionTarget,
            };

            let root_a = ed25519_dalek::SigningKey::from_bytes(&[0xA1; 32]);
            let root_b = ed25519_dalek::SigningKey::from_bytes(&[0xB1; 32]);
            let device_a = iroh::SecretKey::from_bytes(&[0xA2; 32]);
            let device_b = iroh::SecretKey::from_bytes(&[0xB2; 32]);
            let person_a = crate::shell::collab_broker_capnp::principal_id_for_root(
                root_a.verifying_key().to_bytes(),
            );
            let person_b = crate::shell::collab_broker_capnp::principal_id_for_root(
                root_b.verifying_key().to_bytes(),
            );
            let (room_workspace, room) = remote_action_room(&person_a, &person_b);
            let topic = room_topic_id(room);
            let tree_for = || {
                let mut tree = WorkspaceTree::default();
                tree.desktop.workspaces.push(room_workspace.clone());
                tree.shared()
            };

            let (mut net_a, mut commands_a) = shell_network_for_test(tree_for(), 0xA);
            net_a.require_peer_identity = true;
            let tree_b = tree_for();
            let (mut net_b, mut commands_b) = shell_network_for_test(tree_b.clone(), 0xB);
            net_b.require_peer_identity = true;
            let epoch = crate::network::actor::MembershipEpoch::for_test(1);
            authenticated_personal_peer_on_topic(&net_b, &root_a, &device_a, epoch, topic);
            authenticated_personal_peer_on_topic(&net_a, &root_b, &device_b, epoch, topic);

            let broker = crate::shell::broker_services::test_broker_with_membership(tree_b);
            broker.replace_published_surfaces_for_test(vec![PublishedSurface {
                peer_id: 9,
                app_id: "so.tilo.files".into(),
                route: CrdtRoute::app("so.tilo.files", room.to_string()),
                actions: vec![PublishedAction {
                    semantic_identity: None,
                    resource: "files.entry:/tmp/report".into(),
                    action: "refresh".into(),
                    resource_kind: "files.entry".into(),
                    resource_label: "report".into(),
                    action_label: "Refresh".into(),
                    description: None,
                    operation_kind: tilo_actions::OperationKind::Effect,
                    params: Vec::new(),
                    contract: tilo_actions::ActionContract::default(),
                    input: PublishedInput::None,
                    view: None,
                }],
                live: true,
            }]);
            let mut app_invocations = broker.capture_invocations_for_test(9);
            let host_b = crate::shell::remote_actions::BrokerActionHost::new(broker);
            let net_b = Arc::new(net_b);

            let id = tilo_actions::InvocationId::new();
            let request = RemoteActionMessage::Invoke(InvokeRequest {
                on_behalf_of: None,
                invocation: id,
                target: RemoteActionTarget {
                    provider: "9".into(),
                    address: tilo_actions::ActionAddress::new("files.entry:/tmp/report", "refresh"),
                },
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
            });

            let mut round_trip = |request: RemoteActionMessage| {
                let answer = net_a
                    .request(topic, device_b.public(), request)
                    .expect("request sent");
                deliver_remote_action(&device_a, &mut commands_a, &net_b, &device_b);
                let inbound = std::mem::take(
                    &mut *net_b.inbound_remote_actions.lock().expect("inbound queue"),
                );
                assert_eq!(inbound.len(), 1, "B queued the authenticated request");
                assert_eq!(
                    inbound[0].caller,
                    tilo_actions::remote::Caller::authenticated(
                        root_a.verifying_key().to_bytes(),
                        *device_a.public().as_bytes(),
                    )
                    .in_room(room.to_string()),
                    "the caller is the authenticated device, confined to the room topic"
                );
                crate::shell::collab_broker::answer_remote_actions(&net_b, Some(&host_b), inbound);
                deliver_remote_action(&device_b, &mut commands_b, &net_a, &device_a);
                let mut answer = answer;
                match answer.poll_ready() {
                    tilo_actions::remote::PendingPoll::Ready(answer) => answer,
                    _ => panic!("A received B's answer"),
                }
            };

            let RemoteActionMessage::Receipt(receipt) = round_trip(request.clone()) else {
                panic!("an invocation is answered with a receipt");
            };
            assert_eq!(receipt.invocation, id);
            assert_eq!(receipt.status, ReceiptStatus::Accepted);
            let delivered = app_invocations
                .try_recv()
                .expect("B's app received the invocation");
            assert_eq!(delivered.resource, "files.entry:/tmp/report");
            assert_eq!(
                delivered.actor, person_a,
                "attributed to the calling person"
            );

            let RemoteActionMessage::Receipt(retried) = round_trip(request) else {
                panic!("a retry is answered with a receipt");
            };
            assert_eq!(retried, receipt, "a retry reports the original invocation");
            assert!(
                app_invocations.try_recv().is_err(),
                "a retry never reaches the app twice"
            );
        }

        fn remote_actions_from_unauthenticated_devices_are_dropped() {
            let (net, _commands) = shell_network_for_test(WorkspaceTree::default().shared(), 0xC);
            let stranger = iroh::SecretKey::from_bytes(&[0xC2; 32]);
            let frame = tilo_actions::remote::RemoteActionFrame::new(
                tilo_actions::remote::RemoteActionMessage::CatalogueRequest {
                    correlation: 1,
                    query: tilo_actions::remote::CatalogueQuery::default(),
                },
            )
            .encode()
            .expect("frame");
            // Even in the in-process compatibility mode, an action needs a proven
            // caller.
            assert!(!net.require_peer_identity);
            net.dispatch_neighbour_msg(
                DEFAULT_WS_TOPIC,
                stranger.public(),
                None,
                false,
                NeighbourMsg::RemoteAction { frame },
            );
            assert!(net.inbound_remote_actions.lock().expect("queue").is_empty());
        }

        fn a_remote_action_that_races_the_identity_hello_is_delivered_once_admitted() {
            let root_a = ed25519_dalek::SigningKey::from_bytes(&[0xA1; 32]);
            let root_b = ed25519_dalek::SigningKey::from_bytes(&[0xB1; 32]);
            let device_a = iroh::SecretKey::from_bytes(&[0xA2; 32]);
            let stranger_root = ed25519_dalek::SigningKey::from_bytes(&[0xD1; 32]);
            let stranger = iroh::SecretKey::from_bytes(&[0xD2; 32]);
            let person_a = crate::shell::collab_broker_capnp::principal_id_for_root(
                root_a.verifying_key().to_bytes(),
            );
            let person_b = crate::shell::collab_broker_capnp::principal_id_for_root(
                root_b.verifying_key().to_bytes(),
            );
            let (room_workspace, room) = remote_action_room(&person_a, &person_b);
            let topic = room_topic_id(room);
            let mut tree = WorkspaceTree::default();
            tree.desktop.workspaces.push(room_workspace);
            let (mut net, _commands) = shell_network_for_test(tree.shared(), 0xB);
            net.require_peer_identity = true;
            let request = || {
                tilo_actions::remote::RemoteActionFrame::new(
                    tilo_actions::remote::RemoteActionMessage::CatalogueRequest {
                        correlation: 1,
                        query: tilo_actions::remote::CatalogueQuery::default(),
                    },
                )
                .encode()
                .expect("frame")
            };
            let epoch = crate::network::actor::MembershipEpoch::for_test(1);
            for sender in [&device_a, &stranger] {
                net.dispatch_neighbour_msg(
                    topic,
                    sender.public(),
                    Some(epoch),
                    false,
                    NeighbourMsg::RemoteAction { frame: request() },
                );
            }
            assert!(
                net.inbound_remote_actions.lock().expect("queue").is_empty(),
                "nothing runs before the sender's identity is accepted"
            );
            assert_eq!(net.held_remote_actions.lock().expect("held").len(), 2);

            authenticated_personal_peer_on_topic(&net, &root_a, &device_a, epoch, topic);
            assert_eq!(
                net.inbound_remote_actions.lock().expect("queue").len(),
                1,
                "the held request runs once its sender is admitted"
            );

            // A certified device that is not a member of the room is still
            // refused by room authority after its hello.
            authenticated_personal_peer_on_topic(&net, &stranger_root, &stranger, epoch, topic);
            assert_eq!(net.inbound_remote_actions.lock().expect("queue").len(), 1);
            assert!(net.held_remote_actions.lock().expect("held").is_empty());

            net.dispatch_neighbour_msg(
                topic,
                iroh::SecretKey::from_bytes(&[0xE2; 32]).public(),
                Some(epoch),
                false,
                NeighbourMsg::RemoteAction { frame: request() },
            );
            net.dispatch_net_event(NetEvent::PeerLeft {
                ws: topic,
                peer: iroh::SecretKey::from_bytes(&[0xE2; 32]).public(),
                cause: PeerDepartureCause::LivenessTimedOut,
                membership_epoch: Some(epoch),
            });
            assert!(
                net.held_remote_actions.lock().expect("held").is_empty(),
                "a peer that leaves before its hello takes its requests with it"
            );
        }

        fn workspace_control_uses_exact_target_direct_and_refuses_fanout_relay() {
            let (net, mut commands) =
                shell_network_for_test(WorkspaceTree::default().shared(), 0xCA11);
            let target = iroh::SecretKey::generate().public();
            let relay = iroh::SecretKey::generate().public();

            net.workspace_control_direct
                .lock()
                .expect("direct routes")
                .insert((DEFAULT_WS_TOPIC, target));
            let mut direct = net
                .open_workspace_control(DEFAULT_WS_TOPIC, target)
                .expect("direct route");
            assert!(matches!(
                direct.route(),
                WorkspaceControlRoute::Direct { target: selected } if selected == target
            ));
            direct.observe(&NeighbourMsg::PermissionGrant {
                peer: 0xCA11,
                capability: "workspace-control:4:granted".into(),
            });
            direct.observe(&NeighbourMsg::SurfaceStreamStart {
                surface: 4,
                source_peer: crate::network::behaviour::peer_liveness_handle(&target),
                width: 64,
                height: 64,
                codec: "h264".into(),
                ts: 1,
                sender_monotonic_ns_at_send: 1,
                colorimetry: None,
                hdr_static_metadata: None,
                bit_depth: 8,
                app_id: "test.app".into(),
                tab: 0,
            });
            net.publish_workspace_control_input(
                &direct,
                &crate::network::InputEvent::PointerMotion { x: 2.0, y: 3.0 },
            )
            .expect("direct input");
            assert!(matches!(
                commands.try_recv(),
                Ok(NetCmd::PublishWorkspaceControl {
                    route: WorkspaceControlRoute::Direct { target: selected },
                    ..
                }) if selected == target
            ));

            net.workspace_control_direct
                .lock()
                .expect("direct routes")
                .clear();
            net.workspace_control_relays
                .lock()
                .expect("relay routes")
                .insert((DEFAULT_WS_TOPIC, relay));
            assert!(
                net.open_workspace_control(DEFAULT_WS_TOPIC, target)
                    .is_err(),
                "workspace-wide relay fan-out must not carry exact-target control"
            );

            net.workspace_control_relays
                .lock()
                .expect("relay routes")
                .clear();
            assert!(
                net.open_workspace_control(DEFAULT_WS_TOPIC, target)
                    .is_err()
            );
        }

        a_peer_invokes_an_app_action_on_another_peer_and_gets_the_receipt();
        remote_actions_from_unauthenticated_devices_are_dropped();
        a_remote_action_that_races_the_identity_hello_is_delivered_once_admitted();
        workspace_control_uses_exact_target_direct_and_refuses_fanout_relay();
    }

    #[test]
    fn workspace_control_with_only_relays_reports_fan_out_error() {
        let (net, _commands) = shell_network_for_test(WorkspaceTree::default().shared(), 0xCA12);
        let target = iroh::SecretKey::generate().public();
        let relay = iroh::SecretKey::generate().public();
        let open = || net.open_workspace_control(DEFAULT_WS_TOPIC, target).err();

        assert_eq!(
            open().as_deref(),
            Some("no authenticated direct workspace-control path is online"),
            "neither a direct path nor a relay"
        );

        // A relay for another workspace, or one that is the target itself,
        // offers no path to this target.
        net.workspace_control_relays
            .lock()
            .expect("relay routes")
            .extend([(DEFAULT_WS_TOPIC ^ 1, relay), (DEFAULT_WS_TOPIC, target)]);
        assert_eq!(
            open().as_deref(),
            Some("no authenticated direct workspace-control path is online"),
            "only foreign-workspace and target relays"
        );

        net.workspace_control_relays
            .lock()
            .expect("relay routes")
            .insert((DEFAULT_WS_TOPIC, relay));
        assert_eq!(
            open().as_deref(),
            Some(
                "workspace control needs an exact-target route; this relay only supports workspace-wide fan-out"
            ),
            "a workspace-wide relay without a direct path"
        );
    }

    fn personal_continuity_network_for_test(
        user_public_key: [u8; 32],
    ) -> (ShellNetwork, mpsc::Receiver<NetCmd>) {
        let (mut net, commands) =
            shell_network_for_test(WorkspaceTree::default().shared(), 0xC0_11_AB);
        net.personal_continuity_user = Some(user_public_key);
        net.personal_continuity_device = Some(
            crate::shell::personal_continuity::DeviceId::new("local-continuity-device")
                .expect("valid local device id"),
        );
        // Exercise the production fail-closed gate rather than the usual
        // in-process test bridge's unauthenticated compatibility mode.
        net.require_peer_identity = true;
        (net, commands)
    }

    /// A shell for one of `root`'s devices, `device`, holding `tree` and
    /// keeping the person's workspace list.
    pub(super) fn personal_workspaces_network_for_test(
        root: &ed25519_dalek::SigningKey,
        device: &iroh::SecretKey,
        mut tree: WorkspaceTree,
    ) -> (ShellNetwork, mpsc::Receiver<NetCmd>) {
        use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

        let user = root.verifying_key().to_bytes();
        tree.local_person = Some(user);
        let mut cert = DeviceCert {
            user_public_key: user,
            device_public_key: *device.public().as_bytes(),
            device_label: "Laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).expect("certified device");
        let (mut net, commands) =
            shell_network_for_test(tree.shared(), u64::from(device.public().as_bytes()[0]));
        net.personal_continuity_user = Some(user);
        net.require_peer_identity = true;
        net.personal_workspaces = Some(Mutex::new(
            crate::shell::personal_workspaces::PersonalWorkspaces::new(
                user,
                cert.device_public_key,
            ),
        ));
        net.host_identity = Some(HostIdentity {
            device_cert: cert,
            device_signing_key: ed25519_dalek::SigningKey::from_bytes(&device.to_bytes()),
            workspace_device_certs: std::collections::HashMap::new(),
            workspace_revocations: std::collections::HashMap::new(),
            identity_paths: None,
        });
        (net, commands)
    }

    /// `person`'s tree holding one workspace of theirs named `name`.
    fn tree_with_own_workspace(person: &str, name: &str) -> (WorkspaceTree, RoomId) {
        let mut tree = WorkspaceTree::default();
        let mut workspace =
            crate::shell::state::Workspace::new(crate::shell::state::WorkspaceId::new(), name);
        let room = RoomId::new();
        workspace.room = room;
        workspace.set_owner(person.to_string());
        tree.desktop.active = workspace.id;
        tree.desktop.workspaces.push(workspace);
        (tree, room)
    }

    pub(super) fn drain(commands: &mut mpsc::Receiver<NetCmd>) -> Vec<NetCmd> {
        std::iter::from_fn(|| commands.try_recv().ok()).collect()
    }

    fn personal_workspaces_sent(commands: &[NetCmd]) -> Vec<NeighbourMsg> {
        commands
            .iter()
            .filter_map(|command| match command {
                NetCmd::PublishCheckpoint {
                    msg: msg @ NeighbourMsg::PersonalWorkspaces { .. },
                    ..
                } => Some(msg.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "two devices, their exchange in both directions and a stranger's refusal form one scenario"
    )]
    fn a_persons_devices_share_their_workspace_list_and_join_its_rooms() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[0x3C; 32]);
        let laptop_device = iroh::SecretKey::from_bytes(&[0x3D; 32]);
        let board_device = iroh::SecretKey::from_bytes(&[0x3E; 32]);
        let collab_types::caps::Principal::Person(person) =
            collab_types::caps::Principal::person_from_key(
                collab_types::crypto::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
            )
        else {
            panic!("a root names a person");
        };
        let (laptop_tree, research) = tree_with_own_workspace(&person, "Research");
        let (board_tree, board_home) = tree_with_own_workspace(&person, "Board home");
        let (laptop, mut laptop_commands) =
            personal_workspaces_network_for_test(&root, &laptop_device, laptop_tree);
        let (board, mut board_commands) =
            personal_workspaces_network_for_test(&root, &board_device, board_tree);
        let topic = personal_continuity_topic_id(&root.verifying_key().to_bytes());

        // The laptop lists its workspace on its first tick.
        laptop.reconcile_personal_workspaces();
        let sent = personal_workspaces_sent(&drain(&mut laptop_commands));
        assert_eq!(sent.len(), 1, "one signed list per change");
        laptop.reconcile_personal_workspaces();
        assert!(
            personal_workspaces_sent(&drain(&mut laptop_commands)).is_empty(),
            "an unchanged list is not resent every frame"
        );

        // Before the laptop has introduced itself its list is not merged.
        let list = sent.into_iter().next().expect("list");
        board.dispatch_neighbour_msg(topic, laptop_device.public(), None, false, list.clone());
        assert!(
            !board
                .replica
                .lock()
                .unwrap()
                .tree
                .read()
                .desktop
                .workspaces
                .iter()
                .any(|workspace| workspace.room == research)
        );

        authenticated_personal_peer(&board, &root, &laptop_device);
        drain(&mut board_commands);
        board.dispatch_neighbour_msg(topic, laptop_device.public(), None, false, list);
        {
            let replica = board.replica.lock().unwrap();
            let tree = replica.tree.read();
            let joined = tree
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room == research)
                .expect("the laptop's workspace is on the board");
            assert_eq!(joined.name, "Research");
            assert_eq!(joined.owner.as_deref(), Some(person.as_str()));
            assert_eq!(
                tree.desktop.room_of(tree.desktop.active),
                Some(board_home),
                "the board keeps looking at its own workspace"
            );
        }
        let answered = drain(&mut board_commands);
        assert!(
            answered.iter().any(|command| matches!(
                command,
                NetCmd::Publish { ws, msg: NeighbourMsg::SyncReq { room, .. } }
                    if *ws == room_topic_id(research) && *room == research.to_string()
            )),
            "the board asks the new workspace's room for its content at once"
        );
        let reply = personal_workspaces_sent(&answered);
        assert_eq!(
            reply.len(),
            1,
            "the board answers with what the laptop lacks"
        );

        authenticated_personal_peer(&laptop, &root, &board_device);
        drain(&mut laptop_commands);
        laptop.dispatch_neighbour_msg(
            topic,
            board_device.public(),
            None,
            false,
            reply.into_iter().next().expect("reply"),
        );
        assert!(
            laptop
                .replica
                .lock()
                .unwrap()
                .tree
                .read()
                .desktop
                .workspaces
                .iter()
                .any(|workspace| workspace.room == board_home),
            "the board's workspace reaches the laptop"
        );
        assert!(
            personal_workspaces_sent(&drain(&mut laptop_commands)).is_empty(),
            "two devices that agree fall quiet"
        );

        // Another person's device cannot add workspaces to this list.
        let stranger_root = ed25519_dalek::SigningKey::from_bytes(&[0x4C; 32]);
        let stranger_device = iroh::SecretKey::from_bytes(&[0x4D; 32]);
        let collab_types::caps::Principal::Person(outsider) =
            collab_types::caps::Principal::person_from_key(
                collab_types::crypto::Ed25519PublicKey::from_bytes(
                    stranger_root.verifying_key().to_bytes(),
                ),
            )
        else {
            panic!("a root names a person");
        };
        let (stranger_tree, theirs) = tree_with_own_workspace(&outsider, "Theirs");
        let (stranger, mut stranger_commands) =
            personal_workspaces_network_for_test(&stranger_root, &stranger_device, stranger_tree);
        stranger.reconcile_personal_workspaces();
        let Some(NeighbourMsg::PersonalWorkspaces { update, .. }) =
            personal_workspaces_sent(&drain(&mut stranger_commands))
                .into_iter()
                .next()
        else {
            panic!("the stranger lists their workspace");
        };
        authenticated_personal_peer_on_topic(
            &board,
            &stranger_root,
            &stranger_device,
            crate::network::actor::MembershipEpoch::for_test(1),
            topic,
        );
        board.dispatch_neighbour_msg(
            topic,
            stranger_device.public(),
            None,
            false,
            NeighbourMsg::PersonalWorkspaces {
                user_public_key: root.verifying_key().to_bytes(),
                update,
            },
        );
        assert!(
            !board
                .replica
                .lock()
                .unwrap()
                .tree
                .read()
                .desktop
                .workspaces
                .iter()
                .any(|workspace| workspace.room == theirs)
        );
    }

    fn test_continuity_policy(
        enabled: bool,
        app_continuations: bool,
        wifi_settings: bool,
    ) -> crate::shell::continuity_policy::ContinuityPolicySnapshot {
        crate::shell::continuity_policy::ContinuityPolicySnapshot::from_document(
            serde_json::json!({
                "continuity": {
                    "enabled": enabled,
                    "app_continuations": app_continuations,
                    "wifi_settings": wifi_settings
                }
            })
            .as_object()
            .expect("test continuity policy object"),
        )
    }

    pub(super) fn authenticated_personal_peer(
        net: &ShellNetwork,
        root: &ed25519_dalek::SigningKey,
        device: &iroh::SecretKey,
    ) {
        authenticated_personal_peer_at(
            net,
            root,
            device,
            crate::network::actor::MembershipEpoch::for_test(1),
        );
    }

    fn authenticated_personal_peer_at(
        net: &ShellNetwork,
        root: &ed25519_dalek::SigningKey,
        device: &iroh::SecretKey,
        membership_epoch: crate::network::actor::MembershipEpoch,
    ) {
        authenticated_personal_peer_on_topic(
            net,
            root,
            device,
            membership_epoch,
            personal_continuity_topic_id(&root.verifying_key().to_bytes()),
        );
    }

    fn authenticated_personal_peer_on_topic(
        net: &ShellNetwork,
        root: &ed25519_dalek::SigningKey,
        device: &iroh::SecretKey,
        membership_epoch: crate::network::actor::MembershipEpoch,
        topic: u64,
    ) {
        use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: *device.public().as_bytes(),
            device_label: "Personal device".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).expect("valid personal device certificate");
        net.dispatch_neighbour_msg(
            topic,
            device.public(),
            Some(membership_epoch),
            false,
            NeighbourMsg::IdentityHello {
                cert: Box::new(cert),
                revocations: Vec::new(),
            },
        );
    }

    #[test]
    #[serial_test::serial]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn disabled_app_continuations_emit_nothing_purge_state_and_drop_ingress() {
        fn disabled_app_continuations_emit_no_request_or_checkpoint_wire() {
            let user = [0xA4; 32];
            let (mut net, mut commands) = personal_continuity_network_for_test(user);
            net.continuity_policy_override = Some(test_continuity_policy(false, true, true));

            net.publish_personal_continuity_request();
            net.publish_personal_continuity_if_changed();
            net.publish_device_fabric_request();
            assert!(
                !net.publish_message(NeighbourMsg::PersonalContinuityRequest {
                    user_public_key: user,
                    requester: net.local_peer_id_hash(),
                })
            );
            assert!(
                std::iter::from_fn(|| commands.try_recv().ok()).all(|command| !matches!(
                    command,
                    NetCmd::PublishCheckpoint { .. }
                        | NetCmd::PublishPersonalFabric { .. }
                        | NetCmd::PublishPersonalFabricTarget { .. }
                ))
            );
        }

        fn disabling_app_continuations_purges_staged_topology_and_checkpoints() {
            let user = [0xA5; 32];
            let (mut net, _commands) = personal_continuity_network_for_test(user);
            let room = RoomId::new();
            net.replica
                .lock()
                .expect("replica")
                .tree
                .write()
                .adopt_continuation_room(room, "remote-device");
            let remote = test_device(0xA6);
            let snapshot = crate::shell::personal_continuity::DeviceContinuationSnapshot::new(
                remote,
                7,
                None,
                vec![crate::shell::personal_continuity::WorkspaceContinuation {
                    room,
                    apps: Vec::new(),
                }],
            )
            .expect("valid remote snapshot");
            net.personal_continuity
                .lock()
                .expect("continuity")
                .upsert(snapshot)
                .expect("stage checkpoint");
            let tab = crate::shell::personal_continuity::AppInstanceId(0xA6A6);
            let launch = crate::shell::personal_continuity::LaunchIntent::app("so.tilo.browser")
                .expect("valid launch");
            {
                let registry = crate::shell::personal_continuity_registry::global_registry();
                let mut registry = registry.lock().expect("continuation registry");
                registry.clear_remote();
                registry.store_remote_record(room, tab, "so.tilo.browser".into(), tab);
                assert_eq!(
                    registry.record_instance_for_tab(room, tab, &launch),
                    Some(tab)
                );
                drop(registry);
            }
            let dir = tempfile::tempdir().expect("continuity store");
            let path = dir.path().join("personal-continuity.postcard");
            net.personal_continuity_path = Some(path.clone());
            assert!(net.persist_personal_continuity());
            assert!(path.exists());

            net.continuity_policy_override = Some(test_continuity_policy(true, false, true));
            assert!(!net.enforce_app_continuations_policy());

            assert!(
                net.personal_continuity
                    .lock()
                    .expect("continuity")
                    .is_empty()
            );
            assert!(!path.exists());
            assert!(!personal_continuity_backup_path(&path).exists());
            assert!(
                net.replica
                    .lock()
                    .expect("replica")
                    .tree
                    .read()
                    .continuation_rooms
                    .is_empty()
            );
            assert_eq!(
                crate::shell::personal_continuity_registry::global_registry()
                    .lock()
                    .expect("continuation registry")
                    .record_instance_for_tab(room, tab, &launch),
                None
            );
        }

        fn disabled_app_continuations_drop_authenticated_ingress_before_merge() {
            let root = ed25519_dalek::SigningKey::from_bytes(&[0xA7; 32]);
            let peer = iroh::SecretKey::from_bytes(&[0xA8; 32]);
            let user = root.verifying_key().to_bytes();
            let (mut net, _commands) = personal_continuity_network_for_test(user);
            authenticated_personal_peer(&net, &root, &peer);
            net.continuity_policy_override = Some(test_continuity_policy(true, false, true));
            let remote = test_device(0xA9);
            let snapshot = crate::shell::personal_continuity::DeviceContinuationSnapshot::new(
                remote,
                1,
                None,
                Vec::new(),
            )
            .expect("valid snapshot");
            let mut offered = crate::shell::personal_continuity::PersonalContinuity::default();
            offered.upsert(snapshot).expect("valid checkpoint");

            net.dispatch_neighbour_msg(
                personal_continuity_topic_id(&user),
                peer.public(),
                None,
                false,
                NeighbourMsg::PersonalContinuityCheckpoint {
                    user_public_key: user,
                    publisher: crate::network::behaviour::peer_liveness_handle(&peer.public()),
                    snapshot: postcard::to_allocvec(&offered).expect("encode checkpoint"),
                },
            );

            assert!(
                net.personal_continuity
                    .lock()
                    .expect("continuity")
                    .is_empty()
            );
        }

        disabled_app_continuations_emit_no_request_or_checkpoint_wire();
        disabling_app_continuations_purges_staged_topology_and_checkpoints();
        disabled_app_continuations_drop_authenticated_ingress_before_merge();
    }

    #[test]
    #[serial_test::serial(process_env)]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn wifi_secrets_recheck_the_toggle_and_seal_to_device_identity() {
        fn wifi_secret_api_rechecks_the_device_local_toggle() {
            let (mut net, _commands) = shell_network_for_test(WorkspaceTree::default().shared(), 1);
            net.continuity_policy_override = Some(test_continuity_policy(true, false, false));
            assert_eq!(
                net.share_wifi_credential("Private", os_actions::WifiSecurity::Wpa2, "secret"),
                Err("Wi-Fi Continuity is disabled on this device".to_string())
            );
            assert_eq!(
                net.shared_wifi_password("Private", os_actions::WifiSecurity::Wpa2),
                None
            );

            net.continuity_policy_override = Some(test_continuity_policy(true, false, true));
            assert_ne!(
                net.share_wifi_credential("Private", os_actions::WifiSecurity::Wpa2, "secret"),
                Err("Wi-Fi Continuity is disabled on this device".to_string()),
                "the API must sample the new policy instead of retaining a stale denial"
            );
        }

        fn wifi_registry_seals_to_device_identity_and_shrinks_after_removal() {
            use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

            let temp = tempfile::tempdir().expect("temp");
            let registry = temp.path().join("home-peers.json");
            let root = ed25519_dalek::SigningKey::from_bytes(&[0x81; 32]);
            let local_key = ed25519_dalek::SigningKey::from_bytes(&[0x82; 32]);
            let remote_key = ed25519_dalek::SigningKey::from_bytes(&[0x83; 32]);
            let relay_key = iroh::SecretKey::from_bytes(&[0x84; 32]);
            let relay_signing_key = ed25519_dalek::SigningKey::from_bytes(&relay_key.to_bytes());
            let user_public_key = root.verifying_key().to_bytes();
            let local_device = crate::shell::personal_continuity::DeviceId::new(hex::encode(
                local_key.verifying_key().to_bytes(),
            ))
            .expect("local device id");
            let remote_device = crate::shell::personal_continuity::DeviceId::new(hex::encode(
                remote_key.verifying_key().to_bytes(),
            ))
            .expect("remote device id");
            let relay_service = crate::shell::personal_continuity::DeviceId::new(hex::encode(
                relay_key.public().as_bytes(),
            ))
            .expect("relay service id");

            let mut local_cert = DeviceCert {
                user_public_key,
                device_public_key: local_key.verifying_key().to_bytes(),
                device_label: "Local device".into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut local_cert, &root).expect("local certificate");

            let (mut net, _commands) = shell_network_for_test(WorkspaceTree::default().shared(), 1);
            net.personal_continuity_user = Some(user_public_key);
            net.personal_continuity_device = Some(local_device.clone());
            net.host_identity = Some(HostIdentity {
                device_cert: local_cert,
                device_signing_key: local_key,
                workspace_device_certs: std::collections::HashMap::new(),
                workspace_revocations: std::collections::HashMap::new(),
                identity_paths: None,
            });
            net.continuity_policy_override = Some(test_continuity_policy(true, false, true));
            net.device_fabric
                .lock()
                .expect("fabric")
                .upsert(fabric_snapshot(local_device.clone(), 1))
                .expect("local presence");

            let registration = tilo_relay_protocol::HomePeerRegistration {
                schema_version: tilo_relay_protocol::HomePeerRegistration::CURRENT_SCHEMA,
                device_node_id: remote_device.as_str().to_owned(),
                relay_service_node_id: relay_service.as_str().to_owned(),
                relay_endpoint: None,
                label: "Home relay".into(),
                user_public_key: hex::encode(user_public_key),
                device_cert: None,
                relay_workspace_ids: vec![personal_continuity_topic_id(&user_public_key)],
                environments: tilo_relay_protocol::RelayEnvironments::default(),
            };
            std::fs::write(
                &registry,
                serde_json::to_vec(&vec![registration]).expect("registry"),
            )
            .expect("write registry");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_HOME_PEERS_PATH", &registry);

            assert!(net.refresh_home_peer_hosts());
            net.share_wifi_credential("Home", os_actions::WifiSecurity::Wpa2, "correct horse")
                .expect("share credential");
            let credential = net
                .device_fabric()
                .merged_wifi()
                .into_values()
                .next()
                .expect("shared credential");
            assert_eq!(
                crate::shell::device_fabric_secrets::wifi_envelope_recipients(&credential)
                    .expect("recipient projection"),
                std::collections::BTreeSet::from([local_device.clone(), remote_device]),
                "relay routing identity must never become a secret recipient"
            );
            assert_eq!(
                crate::shell::device_fabric_secrets::open_wifi_credential(&credential, &remote_key)
                    .expect("enrolled device decrypts"),
                "correct horse"
            );
            assert!(
                crate::shell::device_fabric_secrets::open_wifi_credential(
                    &credential,
                    &relay_signing_key
                )
                .is_err(),
                "the distinct relay service key must not decrypt personal secrets"
            );

            std::fs::write(&registry, b"[]").expect("remove enrollment");
            assert!(net.refresh_home_peer_hosts());
            assert!(
                net.refresh_wifi_credential_recipients()
                    .expect("rewrap after removal")
            );
            let credential = net
                .device_fabric()
                .merged_wifi()
                .into_values()
                .next()
                .expect("rewrapped credential");
            assert_eq!(
                crate::shell::device_fabric_secrets::wifi_envelope_recipients(&credential)
                    .expect("recipient projection"),
                std::collections::BTreeSet::from([local_device])
            );
            assert!(
                crate::shell::device_fabric_secrets::open_wifi_credential(&credential, &remote_key)
                    .is_err(),
                "removed devices must be excluded from the next envelope"
            );
        }

        wifi_secret_api_rechecks_the_device_local_toggle();
        wifi_registry_seals_to_device_identity_and_shrinks_after_removal();
    }

    fn fabric_snapshot(
        owner: crate::shell::personal_continuity::DeviceId,
        counter: u64,
    ) -> crate::shell::device_fabric::DeviceFabricSnapshot {
        use crate::shell::device_fabric::{
            DeviceFabricSnapshot, DeviceKind, DevicePresence, FABRIC_SCHEMA_VERSION,
            FabricCapabilities, FabricStamp,
        };
        DeviceFabricSnapshot {
            schema_version: FABRIC_SCHEMA_VERSION,
            presence: DevicePresence {
                device: owner.clone(),
                label: "Test device".into(),
                kind: DeviceKind::Laptop,
                capabilities: FabricCapabilities {
                    clipboard: true,
                    app_continuation: true,
                    ..FabricCapabilities::default()
                },
                online: true,
                last_seen_unix_ms: counter,
                wake_routes: Vec::new(),
                stamp: FabricStamp {
                    counter,
                    device: owner.clone(),
                },
            },
            owner,
            media: Vec::new(),
            clipboard: Vec::new(),
            settings: Vec::new(),
            wifi: Vec::new(),
            display: None,
            unlock: crate::shell::device_unlock::UnlockFabricState::default(),
            speakers: crate::shell::speaker_group::SpeakerFabricState::default(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn peer_sync_state_is_private_and_separate_from_the_device_key() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().unwrap();
        let key_dir = temp.path().join("device-private");
        let key_path = key_dir.join("keypair.bin");
        let state_dir = temp.path().join("session-state");
        let network = ShellNetwork::spawn_with(
            WorkspaceTree::bootstrap_demo().shared(),
            &key_path,
            &state_dir,
            Reach::Loopback,
            |_, _| None,
        )
        .expect("actual peer sync with separate session state");
        let key_before = std::fs::read(&key_path).unwrap();
        assert_eq!(
            network.pending_invitations_path.as_deref(),
            Some(state_dir.join("pending-invitations.json").as_path())
        );
        assert_eq!(
            network.personal_continuity_path.as_deref(),
            Some(state_dir.join("personal-continuity.postcard").as_path())
        );
        let fabric_path = network.device_fabric_path.as_ref().unwrap();
        assert_eq!(
            fabric_path,
            &state_dir.join("personal-device-fabric.postcard")
        );
        persist_device_fabric(fabric_path, &network.device_fabric.lock().unwrap()).unwrap();
        assert!(fabric_path.is_file());
        assert_eq!(
            std::fs::metadata(&state_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(fabric_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read(&key_path).unwrap(), key_before);
        assert_eq!(std::fs::read_dir(&key_dir).unwrap().count(), 1);
    }

    pub(super) fn rich_display_network_snapshot(
        owner: crate::shell::personal_continuity::DeviceId,
    ) -> (
        crate::shell::device_fabric::DeviceFabricSnapshot,
        crate::shell::device_fabric::DisplayOutputObservation,
    ) {
        use crate::shell::device_fabric::{
            DisplayConnectorPlacement, DisplayOutputObservation, DisplayPlacement, DisplayRect,
        };
        let mut snapshot = fabric_snapshot(owner.clone(), 1);
        let rect = DisplayRect {
            x: 0,
            y: 0,
            width: 1280,
            height: 800,
        };
        snapshot.display = Some(DisplayPlacement {
            device: owner,
            rect,
            scale_milli: 1000,
            stamp: snapshot.presence.stamp.clone(),
            outputs: vec![DisplayConnectorPlacement {
                connector: "eDP-1".into(),
                label: "Panel".into(),
                rect,
                scale_milli: 1000,
            }],
        });
        let observation = DisplayOutputObservation {
            connector: "eDP-1".into(),
            output: settings_store::person_displays::DeviceOutput {
                id: "monitor:panel".into(),
                name: "Panel".into(),
                builtin: true,
                x: 0,
                y: 0,
                width: 1280,
                height: 800,
                mode_width: 1280,
                mode_height: 800,
                refresh_mhz: 60_000,
                scale_milli: 1000,
                connected: true,
                enabled: true,
            },
        };
        (snapshot, observation)
    }

    #[test]
    fn offline_display_cache_requires_root_enrolment_and_exact_revision() {
        use crate::shell::device_fabric::DeviceFabric;
        use crate::shell::personal_continuity::DeviceId;
        use std::collections::BTreeSet;
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("facts.json");
        let owner = DeviceId::new("offline-display").unwrap();
        let (snapshot, observation) = rich_display_network_snapshot(owner.clone());
        let stamp = snapshot.display.as_ref().unwrap().stamp.clone();
        let mut saved = DeviceFabric::default();
        saved.upsert(snapshot.clone()).unwrap();
        let checkpoint = saved.encode_checkpoint().unwrap();
        saved
            .observe_display_outputs(&owner, &stamp, vec![observation.clone()])
            .unwrap();
        display_observations::save(&path, &saved, [7; 32]).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let enrolled = BTreeSet::from([owner.clone()]);
        let mut restored = DeviceFabric::decode_checkpoint(&checkpoint).unwrap();
        assert!(restored.display_outputs(&owner).is_none());
        assert!(display_observations::restore(&path, &mut restored, [8; 32], &enrolled).is_err());
        assert!(restored.display_outputs(&owner).is_none());
        assert!(
            !display_observations::restore(&path, &mut restored, [7; 32], &BTreeSet::new())
                .unwrap()
        );
        assert!(restored.display_outputs(&owner).is_none());
        assert!(display_observations::restore(&path, &mut restored, [7; 32], &enrolled).unwrap());
        assert_eq!(
            restored.display_outputs(&owner),
            Some([observation].as_slice())
        );
        assert_eq!(
            restored.encode_checkpoint().unwrap(),
            checkpoint,
            "cached facts must not alter the wire checkpoint"
        );
        let mut next = snapshot;
        next.display.as_mut().unwrap().stamp.counter += 1;
        next.presence.stamp.counter += 1;
        let mut changed = DeviceFabric::default();
        changed.upsert(next).unwrap();
        assert!(!display_observations::restore(&path, &mut changed, [7; 32], &enrolled).unwrap());
        assert!(changed.display_outputs(&owner).is_none());
        let mut forgotten = DeviceFabric::default();
        assert!(!display_observations::restore(&path, &mut forgotten, [7; 32], &enrolled).unwrap());
        assert!(forgotten.snapshots().is_empty());
    }

    #[test]
    fn offline_display_cache_rejects_corruption_without_partial_projection() {
        use crate::shell::device_fabric::DeviceFabric;
        use crate::shell::personal_continuity::DeviceId;
        use std::collections::BTreeSet;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("facts.json");
        let owner = DeviceId::new("offline-display").unwrap();
        let (snapshot, observation) = rich_display_network_snapshot(owner.clone());
        let mut saved = DeviceFabric::default();
        saved.upsert(snapshot.clone()).unwrap();
        saved
            .observe_display_outputs(
                &owner,
                &snapshot.display.as_ref().unwrap().stamp,
                vec![observation],
            )
            .unwrap();
        display_observations::save(&path, &saved, [7; 32]).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let original: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let enrolled = BTreeSet::from([owner.clone()]);
        let checkpoint = saved.encode_checkpoint().unwrap();
        let mut cases = Vec::new();
        let mut foreign = original.clone();
        foreign["observations"][0]["stamp"]["device"] = "foreign".into();
        cases.push(foreign);
        let mut geometry = original.clone();
        geometry["observations"][0]["outputs"][0]["output"]["width"] = 1.into();
        cases.push(geometry);
        let mut duplicate = original.clone();
        duplicate["observations"]
            .as_array_mut()
            .unwrap()
            .push(original["observations"][0].clone());
        cases.push(duplicate);
        let mut version = original;
        version["schema_version"] = 2.into();
        cases.push(version);
        for invalid in cases {
            std::fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
            let mut restored = DeviceFabric::decode_checkpoint(&checkpoint).unwrap();
            assert!(
                display_observations::restore(&path, &mut restored, [7; 32], &enrolled).is_err()
            );
            assert!(restored.display_outputs(&owner).is_none());
            assert_eq!(restored.encode_checkpoint().unwrap(), checkpoint);
        }
        std::fs::write(&path, vec![b' '; 2 * 1024 * 1024 + 1]).unwrap();
        let mut restored = DeviceFabric::decode_checkpoint(&checkpoint).unwrap();
        assert!(display_observations::restore(&path, &mut restored, [7; 32], &enrolled).is_err());
        assert!(restored.display_outputs(&owner).is_none());
    }

    #[test]
    fn offline_display_cache_does_not_enrol_an_authenticated_workspace_peer() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[0xE3; 32]);
        let user = root.verifying_key().to_bytes();
        let peer = iroh::SecretKey::from_bytes(&[0xE4; 32]);
        let (net, _commands) = personal_continuity_network_for_test(user);
        authenticated_personal_peer(&net, &root, &peer);
        let owner =
            crate::shell::personal_continuity::DeviceId::new(hex::encode(peer.public().as_bytes()))
                .unwrap();
        let (snapshot, observation) = rich_display_network_snapshot(owner.clone());
        let stamp = snapshot.display.as_ref().unwrap().stamp.clone();
        net.device_fabric.lock().unwrap().upsert(snapshot).unwrap();
        net.observe_display_outputs(&owner, &stamp, vec![observation])
            .unwrap();
        assert!(net.current_personal_route(&owner).is_some());
        assert!(net.device_fabric().display_outputs(&owner).is_some());
        assert!(!net.personal_device_is_enrolled(&owner));
        let local = net.personal_continuity_device.as_ref().unwrap();
        assert!(net.person_display_devices(local).is_empty());
    }

    #[test]
    fn offline_display_cache_preserves_facts_without_live_route_authority() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[0xE1; 32]);
        let user = root.verifying_key().to_bytes();
        let peer = iroh::SecretKey::from_bytes(&[0xE2; 32]);
        let (mut net, _commands) = personal_continuity_network_for_test(user);
        let dir = tempfile::tempdir().unwrap();
        let checkpoint_path = dir.path().join("personal-device-fabric.postcard");
        net.device_fabric_path = Some(checkpoint_path.clone());
        net.registered_home_device_ids
            .lock()
            .unwrap()
            .insert(*peer.public().as_bytes());
        let owner =
            crate::shell::personal_continuity::DeviceId::new(hex::encode(peer.public().as_bytes()))
                .unwrap();
        let local = net.personal_continuity_device.clone().unwrap();
        let (snapshot, observation) = rich_display_network_snapshot(owner.clone());
        let stamp = snapshot.display.as_ref().unwrap().stamp.clone();
        net.device_fabric.lock().unwrap().upsert(snapshot).unwrap();
        net.observe_display_outputs(&owner, &stamp, vec![observation.clone()])
            .unwrap();
        net.persist_device_fabric(&checkpoint_path, &net.device_fabric.lock().unwrap())
            .unwrap();
        *net.device_fabric.lock().unwrap() = load_device_fabric(&checkpoint_path);
        net.restore_display_observations();
        let devices = net.person_display_devices(&local);
        assert_eq!(devices.len(), 1);
        assert!(
            !devices[0].online,
            "a retained checkpoint is not a current certified route"
        );
        assert_eq!(devices[0].outputs, vec![observation.output]);
        assert!(net.current_personal_route(&owner).is_none());
        assert!(net.take_personal_fabric_actions().is_empty());
        authenticated_personal_peer(&net, &root, &peer);
        assert!(net.person_display_devices(&local)[0].online);
        net.dispatch_net_event(NetEvent::PeerLeft {
            ws: personal_continuity_topic_id(&user),
            peer: peer.public(),
            cause: PeerDepartureCause::GossipNeighborDown,
            membership_epoch: Some(crate::network::actor::MembershipEpoch::for_test(1)),
        });
        assert!(!net.person_display_devices(&local)[0].online);
        assert_eq!(net.person_display_devices(&local)[0].outputs.len(), 1);
        net.forget_device_fabric(&owner);
        assert!(net.person_display_devices(&local).is_empty());
        let store: serde_json::Value = serde_json::from_slice(
            &std::fs::read(ShellNetwork::display_observations_path(&checkpoint_path)).unwrap(),
        )
        .unwrap();
        assert!(store["observations"].as_array().unwrap().is_empty());
    }

    #[test]
    fn rich_display_dispatch_requires_opt_in_and_current_certified_route() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[0xE1; 32]);
        let user = root.verifying_key().to_bytes();
        let peer = iroh::SecretKey::from_bytes(&[0xE2; 32]);
        let (mut net, _commands) = personal_continuity_network_for_test(user);
        let allowed = net.continuity_policy_override;
        let epoch = crate::network::actor::MembershipEpoch::for_test(1);
        authenticated_personal_peer_at(&net, &root, &peer, epoch);
        let owner =
            crate::shell::personal_continuity::DeviceId::new(hex::encode(peer.public().as_bytes()))
                .unwrap();
        let (mut snapshot, observation) = rich_display_network_snapshot(owner.clone());
        net.device_fabric
            .lock()
            .unwrap()
            .upsert(snapshot.clone())
            .unwrap();
        let payload = PersonalFabricPayload::DisplayOutputs {
            stamp: snapshot.display.as_ref().unwrap().stamp.clone(),
            outputs: vec![observation.clone()],
        };
        let topic = personal_continuity_topic_id(&user);
        net.continuity_policy_override =
            Some(crate::shell::continuity_policy::ContinuityPolicySnapshot::default());
        net.dispatch_personal_fabric(topic, peer.public(), epoch, user, payload.clone());
        assert!(net.device_fabric().display_outputs(&owner).is_none());
        net.continuity_policy_override = allowed;
        net.dispatch_personal_fabric(
            topic,
            peer.public(),
            crate::network::actor::MembershipEpoch::for_test(2),
            user,
            payload.clone(),
        );
        assert!(net.device_fabric().display_outputs(&owner).is_none());
        net.dispatch_personal_fabric(
            topic.wrapping_add(1),
            peer.public(),
            epoch,
            user,
            payload.clone(),
        );
        assert!(net.device_fabric().display_outputs(&owner).is_none());
        net.dispatch_personal_fabric(topic, peer.public(), epoch, user, payload);
        assert_eq!(
            net.device_fabric().display_outputs(&owner),
            Some([observation.clone()].as_slice())
        );
        snapshot.presence.stamp.counter = 2;
        snapshot.display.as_mut().unwrap().stamp.counter = 2;
        net.device_fabric
            .lock()
            .unwrap()
            .upsert(snapshot.clone())
            .unwrap();
        net.peer_identities
            .remove(&crate::shell::state::PeerOrigin::peer(&peer.public()));
        net.dispatch_personal_fabric(
            topic,
            peer.public(),
            epoch,
            user,
            PersonalFabricPayload::DisplayOutputs {
                stamp: snapshot.display.unwrap().stamp,
                outputs: vec![observation],
            },
        );
        assert!(net.device_fabric().display_outputs(&owner).is_none());
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn personal_fabric_trust_and_actions_are_bound_to_exact_certified_routes() {
        fn device_fabric_store_recovers_from_valid_backup() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("personal-device-fabric.postcard");
            let owner = crate::shell::personal_continuity::DeviceId::new("laptop").unwrap();
            let mut expected = crate::shell::device_fabric::DeviceFabric::default();
            expected.upsert(fabric_snapshot(owner, 7)).unwrap();
            persist_device_fabric(&path, &expected).unwrap();
            std::fs::write(&path, b"damaged").unwrap();
            assert_eq!(load_device_fabric(&path), expected);
        }

        fn personal_fabric_merge_is_bound_to_authenticated_device_certificate() {
            let root = ed25519_dalek::SigningKey::from_bytes(&[0xF1; 32]);
            let user = root.verifying_key().to_bytes();
            let peer_device = iroh::SecretKey::generate();
            let (net, _commands) = personal_continuity_network_for_test(user);
            authenticated_personal_peer(&net, &root, &peer_device);
            let owner = crate::shell::personal_continuity::DeviceId::new(hex::encode(
                peer_device.public().as_bytes(),
            ))
            .unwrap();
            let snapshot = fabric_snapshot(owner.clone(), 8);
            let mut incoming = crate::shell::device_fabric::DeviceFabric::default();
            incoming.upsert(snapshot).unwrap();
            net.dispatch_personal_fabric(
                personal_continuity_topic_id(&user),
                peer_device.public(),
                crate::network::actor::MembershipEpoch::for_test(1),
                user,
                PersonalFabricPayload::Snapshot(incoming.encode_checkpoint().unwrap()),
            );
            assert!(net.device_fabric().snapshots().contains_key(&owner));

            let forged =
                crate::shell::personal_continuity::DeviceId::new("some-other-device").unwrap();
            let mut incoming = crate::shell::device_fabric::DeviceFabric::default();
            incoming.upsert(fabric_snapshot(forged.clone(), 9)).unwrap();
            net.dispatch_personal_fabric(
                personal_continuity_topic_id(&user),
                peer_device.public(),
                crate::network::actor::MembershipEpoch::for_test(1),
                user,
                PersonalFabricPayload::Snapshot(incoming.encode_checkpoint().unwrap()),
            );
            assert!(!net.device_fabric().snapshots().contains_key(&forged));
        }

        fn personal_fabric_action_is_exact_target_and_ephemeral() {
            let root = ed25519_dalek::SigningKey::from_bytes(&[0xD1; 32]);
            let user = root.verifying_key().to_bytes();
            let target_key = iroh::SecretKey::generate();
            let (net, mut commands) = personal_continuity_network_for_test(user);
            let target = crate::shell::personal_continuity::DeviceId::new(hex::encode(
                target_key.public().as_bytes(),
            ))
            .unwrap();
            authenticated_personal_peer(&net, &root, &target_key);
            net.update_registered_home_device_ids(|ids| {
                ids.insert(*target_key.public().as_bytes());
            });
            let request_id = net
                .send_personal_fabric_action(
                    &target,
                    crate::shell::device_fabric::PersonalFabricAction::ResumeMedia {
                        session_id: "player:one".into(),
                        app_id: "so.tilo.music".into(),
                        workspace_id: "11111111-1111-1111-1111-111111111111".into(),
                        title: "Track".into(),
                        locator: crate::shell::device_fabric::MediaSourceLocator::Workspace {
                            path: "Music/track.flac".into(),
                        },
                        position_ms: 12,
                        resume_playing: true,
                        has_video: false,
                    },
                )
                .unwrap();
            assert_ne!(request_id, [0; 16]);
            let mut found = false;
            while let Ok(command) = commands.try_recv() {
                if matches!(
                    command,
                    NetCmd::PublishPersonalFabricTarget {
                        target: selected,
                        payload: PersonalFabricPayload::Action(_),
                        ..
                    } if selected == target_key.public()
                ) {
                    found = true;
                }
            }
            assert!(found, "action must use the exact-target transport command");
            assert!(net.take_personal_fabric_actions().is_empty());
        }

        fn personal_fabric_action_replay_and_misaddress_are_rejected() {
            let root = ed25519_dalek::SigningKey::from_bytes(&[0xD2; 32]);
            let user = root.verifying_key().to_bytes();
            let peer_device = iroh::SecretKey::generate();
            let (net, _commands) = personal_continuity_network_for_test(user);
            authenticated_personal_peer(&net, &root, &peer_device);
            let source = crate::shell::personal_continuity::DeviceId::new(hex::encode(
                peer_device.public().as_bytes(),
            ))
            .unwrap();
            let target = net.personal_continuity_device.clone().unwrap();
            let envelope = crate::shell::device_fabric::PersonalFabricActionEnvelope {
                request_id: [8; 16],
                source,
                target,
                issued_unix_ms: unix_now_ms(),
                action: crate::shell::device_fabric::PersonalFabricAction::MediaAccepted {
                    request_id: [7; 16],
                    session_id: "player:one".into(),
                },
            };
            let payload = PersonalFabricPayload::Action(postcard::to_allocvec(&envelope).unwrap());
            for _ in 0..2 {
                net.dispatch_personal_fabric(
                    personal_continuity_topic_id(&user),
                    peer_device.public(),
                    crate::network::actor::MembershipEpoch::for_test(1),
                    user,
                    payload.clone(),
                );
            }
            assert_eq!(
                net.take_personal_fabric_actions()
                    .into_iter()
                    .map(|received| received.envelope)
                    .collect::<Vec<_>>(),
                vec![envelope.clone()]
            );

            let mut misaddressed = envelope;
            misaddressed.request_id = [9; 16];
            misaddressed.target =
                crate::shell::personal_continuity::DeviceId::new("another-device").unwrap();
            net.dispatch_personal_fabric(
                personal_continuity_topic_id(&user),
                peer_device.public(),
                crate::network::actor::MembershipEpoch::for_test(1),
                user,
                PersonalFabricPayload::Action(postcard::to_allocvec(&misaddressed).unwrap()),
            );
            assert!(net.take_personal_fabric_actions().is_empty());
        }

        fn personal_route_reconnect_rejects_queued_old_epoch_and_stale_departure() {
            let root = ed25519_dalek::SigningKey::from_bytes(&[0xD3; 32]);
            let user = root.verifying_key().to_bytes();
            let peer = iroh::SecretKey::generate();
            let (net, _commands) = personal_continuity_network_for_test(user);
            let old_epoch = crate::network::actor::MembershipEpoch::for_test(41);
            let new_epoch = crate::network::actor::MembershipEpoch::for_test(42);
            authenticated_personal_peer_at(&net, &root, &peer, old_epoch);
            let source = crate::shell::personal_continuity::DeviceId::new(hex::encode(
                peer.public().as_bytes(),
            ))
            .unwrap();
            let target = net.personal_continuity_device.clone().unwrap();
            let mut envelope = fabric_envelope(
                0xD4,
                &source,
                &target,
                crate::shell::device_fabric::PersonalFabricAction::MediaCancelled {
                    request_id: [0xD5; 16],
                    session_id: "player:old".into(),
                },
            );
            envelope.issued_unix_ms = unix_now_ms();
            net.dispatch_personal_fabric(
                personal_continuity_topic_id(&user),
                peer.public(),
                old_epoch,
                user,
                PersonalFabricPayload::Action(postcard::to_allocvec(&envelope).unwrap()),
            );
            let queued_actions = net.take_personal_fabric_actions();
            let [queued] = queued_actions.as_slice() else {
                panic!("old action should reach the bounded local queue");
            };
            let queued = queued.clone();

            authenticated_personal_peer_at(&net, &root, &peer, new_epoch);
            assert!(!net.personal_route_is_current(&queued.route));
            let current = net.current_personal_route(&source).unwrap();
            assert_eq!(current.membership_epoch, new_epoch);

            net.dispatch_net_event(NetEvent::PeerLeft {
                ws: personal_continuity_topic_id(&user),
                peer: peer.public(),
                cause: PeerDepartureCause::GossipNeighborDown,
                membership_epoch: Some(old_epoch),
            });
            assert_eq!(net.current_personal_route(&source), Some(current));
        }

        fn personal_route_requires_exact_direct_same_user_identity_hello() {
            let local_root = ed25519_dalek::SigningKey::from_bytes(&[0xD6; 32]);
            let other_root = ed25519_dalek::SigningKey::from_bytes(&[0xD7; 32]);
            let local_user = local_root.verifying_key().to_bytes();
            let peer = iroh::SecretKey::generate();
            let (net, _commands) = personal_continuity_network_for_test(local_user);
            let epoch = crate::network::actor::MembershipEpoch::for_test(51);
            authenticated_personal_peer_on_topic(
                &net,
                &other_root,
                &peer,
                epoch,
                personal_continuity_topic_id(&local_user),
            );
            let peer_device = crate::shell::personal_continuity::DeviceId::new(hex::encode(
                peer.public().as_bytes(),
            ))
            .unwrap();
            assert!(net.current_personal_route(&peer_device).is_none());
            net.peer_identities
                .remove(&crate::shell::state::PeerOrigin::peer(&peer.public()));

            authenticated_personal_peer_at(&net, &local_root, &peer, epoch);
            assert!(net.current_personal_route(&peer_device).is_some());
            authenticated_personal_peer_on_topic(
                &net,
                &other_root,
                &peer,
                epoch,
                personal_continuity_topic_id(&local_user),
            );
            assert!(
                net.current_personal_route(&peer_device).is_none(),
                "a rejected replacement hello must purge the prior route"
            );
            net.peer_identities
                .remove(&crate::shell::state::PeerOrigin::peer(&peer.public()));
            authenticated_personal_peer_at(&net, &local_root, &peer, epoch);
            let admitted = net.current_personal_route(&peer_device).unwrap();
            net.dispatch_net_event(NetEvent::PeerLeft {
                ws: personal_continuity_topic_id(&local_user).wrapping_add(1),
                peer: peer.public(),
                cause: PeerDepartureCause::GossipNeighborDown,
                membership_epoch: Some(epoch),
            });
            assert_eq!(net.current_personal_route(&peer_device), Some(admitted));

            if let Ok(mut routes) = net.authenticated_personal_routes.lock() {
                routes.clear();
            }
            authenticated_personal_peer_at(
                &net,
                &local_root,
                &peer,
                crate::network::actor::MembershipEpoch::for_test(52),
            );
            // Prove the actor-only token is not part of the serializable envelope:
            // changing local route authority leaves the exact wire bytes unchanged.
            let envelope = fabric_envelope(
                0xD8,
                &peer_device,
                net.personal_continuity_device.as_ref().unwrap(),
                crate::shell::device_fabric::PersonalFabricAction::MediaCancelled {
                    request_id: [0xD9; 16],
                    session_id: "wire".into(),
                },
            );
            let wire = postcard::to_allocvec(&envelope).unwrap();
            let decoded: crate::shell::device_fabric::PersonalFabricActionEnvelope =
                postcard::from_bytes(&wire).unwrap();
            assert_eq!(decoded, envelope);
            assert!(!wire.windows(8).any(|window| window == 52_u64.to_le_bytes()));
        }

        device_fabric_store_recovers_from_valid_backup();
        personal_fabric_merge_is_bound_to_authenticated_device_certificate();
        personal_fabric_action_is_exact_target_and_ephemeral();
        personal_fabric_action_replay_and_misaddress_are_rejected();
        personal_route_reconnect_rejects_queued_old_epoch_and_stale_departure();
        personal_route_requires_exact_direct_same_user_identity_hello();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn personal_fabric_saturation_reserves_control_and_purges_exact_streams() {
        fn personal_fabric_action_queue_reserves_control_during_media_saturation() {
            use crate::shell::device_fabric::PersonalFabricAction;
            use crate::shell::personal_tethering::{TETHERING_PROTOCOL_VERSION, TetheringSignal};
            use crate::shell::remote_capture::device::{RemoteCaptureChunk, RemoteCaptureSignal};

            let source = test_device(1);
            let target = test_device(2);
            let audio_session = [3; 16];
            let capture_session = [4; 16];
            let capture_grant = test_capture_grant(5);
            let mut queue = VecDeque::new();

            for sequence in 0..PERSONAL_FABRIC_DATA_QUEUE_CAPACITY as u64 {
                let action = if sequence % 2 == 0 {
                    PersonalFabricAction::RemoteAudioFrame {
                        session_id: audio_session,
                        authorization: [6; 16],
                        sequence,
                        media_ts: 0,
                        presentation_ns: 0,
                        pcm_s16le: vec![0, 0],
                    }
                } else {
                    PersonalFabricAction::RemoteCaptureChunk {
                        chunk: RemoteCaptureChunk {
                            session_id: capture_session,
                            grant: capture_grant,
                            sequence,
                            offset: 0,
                            total_bytes: 1,
                            page_index: None,
                            end_of_unit: false,
                            payload: vec![0],
                        },
                    }
                };
                assert_eq!(
                    queue_personal_fabric_action(
                        &mut queue,
                        fabric_envelope(u8::try_from(sequence).unwrap(), &source, &target, action),
                    ),
                    PersonalFabricQueueOutcome::Queued
                );
            }
            assert_eq!(queue.len(), PERSONAL_FABRIC_DATA_QUEUE_CAPACITY);

            let duplicate = fabric_envelope(
                200,
                &source,
                &target,
                PersonalFabricAction::RemoteAudioFrame {
                    session_id: audio_session,
                    authorization: [6; 16],
                    sequence: 0,
                    media_ts: 0,
                    presentation_ns: 0,
                    pcm_s16le: vec![9, 9],
                },
            );
            assert_eq!(
                queue_personal_fabric_action(&mut queue, duplicate),
                PersonalFabricQueueOutcome::Coalesced
            );
            let excess_data = fabric_envelope(
                201,
                &source,
                &target,
                PersonalFabricAction::RemoteAudioFrame {
                    session_id: audio_session,
                    authorization: [6; 16],
                    sequence: 1_000,
                    media_ts: 0,
                    presentation_ns: 0,
                    pcm_s16le: vec![0, 0],
                },
            );
            assert_eq!(
                queue_personal_fabric_action(&mut queue, excess_data),
                PersonalFabricQueueOutcome::Saturated
            );

            let controls = [
                PersonalFabricAction::RemoteAudioDecision {
                    session_id: audio_session,
                    accepted: false,
                    authorization: None,
                },
                PersonalFabricAction::RemoteCaptureSignal {
                    signal: RemoteCaptureSignal::Decision {
                        request_id: [7; 16],
                        accepted: false,
                        session_id: None,
                        grant: None,
                        profile: None,
                    },
                },
                PersonalFabricAction::Tethering {
                    signal: TetheringSignal::Stop {
                        protocol_version: TETHERING_PROTOCOL_VERSION,
                        request_id: [8; 16],
                        requester: source.clone(),
                        provider: target.clone(),
                        grant_nonce: [9; 16],
                    },
                },
                PersonalFabricAction::BluetoothHandoffFinished {
                    request_id: [10; 16],
                    address: "AA:BB:CC:DD:EE:FF".into(),
                    connected: false,
                },
                PersonalFabricAction::BluetoothHandoffCancel {
                    request_id: [11; 16],
                    address: "AA:BB:CC:DD:EE:FF".into(),
                },
                PersonalFabricAction::InputLeaseRevoked { nonce: [12; 16] },
                PersonalFabricAction::RemoteAudioStop {
                    session_id: audio_session,
                    authorization: [6; 16],
                },
                PersonalFabricAction::RemoteAudioCancel {
                    session_id: audio_session,
                },
                PersonalFabricAction::RemoteCaptureSignal {
                    signal: RemoteCaptureSignal::Stop {
                        session_id: capture_session,
                        grant: capture_grant,
                    },
                },
            ];
            for (index, action) in controls.into_iter().enumerate() {
                assert_eq!(
                    queue_personal_fabric_action(
                        &mut queue,
                        fabric_envelope(
                            210 + u8::try_from(index).unwrap(),
                            &source,
                            &target,
                            action
                        ),
                    ),
                    PersonalFabricQueueOutcome::Queued
                );
            }

            assert!(queue.len() <= PERSONAL_FABRIC_ACTION_QUEUE_CAPACITY);
            assert_eq!(
                queue
                    .iter()
                    .filter(|queued| is_personal_fabric_bulk_data(&queued.action))
                    .count(),
                0,
                "terminal controls should supersede queued data for their exact stream"
            );
            assert!(queue.iter().any(|queued| matches!(
                queued.action,
                PersonalFabricAction::Tethering {
                    signal: TetheringSignal::Stop { .. }
                }
            )));
            assert!(queue.iter().any(|queued| matches!(
                queued.action,
                PersonalFabricAction::BluetoothHandoffFinished { .. }
            )));
            assert!(queue.iter().any(|queued| matches!(
                queued.action,
                PersonalFabricAction::BluetoothHandoffCancel { .. }
            )));
            assert!(queue.iter().any(|queued| matches!(
                queued.action,
                PersonalFabricAction::InputLeaseRevoked { .. }
            )));
        }

        fn personal_fabric_terminal_purges_only_the_exact_peer_stream() {
            use crate::shell::device_fabric::PersonalFabricAction;
            use crate::shell::remote_capture::device::{RemoteCaptureChunk, RemoteCaptureSignal};

            let source = test_device(21);
            let other_source = test_device(22);
            let target = test_device(23);
            let session_id = [24; 16];
            let capture_grant = test_capture_grant(25);
            let mut queue = VecDeque::new();
            for (index, owner) in [&source, &other_source].into_iter().enumerate() {
                let audio = PersonalFabricAction::RemoteAudioFrame {
                    session_id,
                    authorization: [26; 16],
                    sequence: index as u64,
                    media_ts: 0,
                    presentation_ns: 0,
                    pcm_s16le: vec![0, 0],
                };
                let capture = PersonalFabricAction::RemoteCaptureChunk {
                    chunk: RemoteCaptureChunk {
                        session_id,
                        grant: capture_grant,
                        sequence: index as u64,
                        offset: 0,
                        total_bytes: 1,
                        page_index: None,
                        end_of_unit: false,
                        payload: vec![0],
                    },
                };
                assert_eq!(
                    queue_personal_fabric_action(
                        &mut queue,
                        fabric_envelope(30 + u8::try_from(index).unwrap(), owner, &target, audio),
                    ),
                    PersonalFabricQueueOutcome::Queued
                );
                assert_eq!(
                    queue_personal_fabric_action(
                        &mut queue,
                        fabric_envelope(40 + u8::try_from(index).unwrap(), owner, &target, capture),
                    ),
                    PersonalFabricQueueOutcome::Queued
                );
            }

            assert_eq!(
                queue_personal_fabric_action(
                    &mut queue,
                    fabric_envelope(
                        50,
                        &source,
                        &target,
                        PersonalFabricAction::RemoteAudioStop {
                            session_id,
                            authorization: [26; 16],
                        },
                    ),
                ),
                PersonalFabricQueueOutcome::Queued
            );
            assert_eq!(
                queue_personal_fabric_action(
                    &mut queue,
                    fabric_envelope(
                        51,
                        &source,
                        &target,
                        PersonalFabricAction::RemoteCaptureSignal {
                            signal: RemoteCaptureSignal::Stop {
                                session_id,
                                grant: capture_grant,
                            },
                        },
                    ),
                ),
                PersonalFabricQueueOutcome::Queued
            );

            assert!(!queue.iter().any(|queued| {
                queued.source == source && is_personal_fabric_bulk_data(&queued.action)
            }));
            assert_eq!(
                queue
                    .iter()
                    .filter(|queued| {
                        queued.source == other_source
                            && is_personal_fabric_bulk_data(&queued.action)
                    })
                    .count(),
                2,
                "a same-session terminal from another exact peer must not purge its data"
            );
        }

        fn personal_fabric_all_control_saturation_fails_closed() {
            use crate::shell::device_fabric::PersonalFabricAction;

            let source = test_device(31);
            let target = test_device(32);
            let mut queue = VecDeque::new();
            for index in 0..PERSONAL_FABRIC_ACTION_QUEUE_CAPACITY {
                let mut nonce = [0; 16];
                nonce[..8].copy_from_slice(&(index as u64).to_le_bytes());
                assert_eq!(
                    queue_personal_fabric_action(
                        &mut queue,
                        fabric_envelope(
                            u8::try_from(index).unwrap(),
                            &source,
                            &target,
                            PersonalFabricAction::InputLeaseRevoked { nonce },
                        ),
                    ),
                    PersonalFabricQueueOutcome::Queued
                );
            }
            let first = queue.front().cloned().unwrap();
            assert_eq!(
                queue_personal_fabric_action(
                    &mut queue,
                    fabric_envelope(
                        99,
                        &source,
                        &target,
                        PersonalFabricAction::BluetoothHandoffCancel {
                            request_id: [33; 16],
                            address: "11:22:33:44:55:66".into(),
                        },
                    ),
                ),
                PersonalFabricQueueOutcome::Saturated
            );
            assert_eq!(queue.len(), PERSONAL_FABRIC_ACTION_QUEUE_CAPACITY);
            assert_eq!(queue.front(), Some(&first));
            assert!(!queue.iter().any(|queued| matches!(
                queued.action,
                PersonalFabricAction::BluetoothHandoffCancel { .. }
            )));
        }

        personal_fabric_action_queue_reserves_control_during_media_saturation();
        personal_fabric_terminal_purges_only_the_exact_peer_stream();
        personal_fabric_all_control_saturation_fails_closed();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn personal_continuity_store_survives_restart_and_backup_failure() {
        fn personal_continuity_counter_and_checkpoint_survive_restart_store() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("personal-continuity.postcard");
            let device = crate::shell::personal_continuity::DeviceId::new("laptop").unwrap();
            let snapshot = crate::shell::personal_continuity::DeviceContinuationSnapshot::new(
                device.clone(),
                41,
                None,
                Vec::new(),
            )
            .unwrap();
            let mut continuity = crate::shell::personal_continuity::PersonalContinuity::default();
            continuity.upsert(snapshot.clone()).unwrap();
            let _ = persist_personal_continuity(
                &path,
                &PersistedPersonalContinuity {
                    counter: 41,
                    continuity,
                },
            );
            let restored = load_personal_continuity(&path);
            assert_eq!(restored.counter, 41);
            assert_eq!(restored.continuity.snapshot_for(&device), Some(&snapshot));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                assert_eq!(
                    std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }

            std::fs::write(&path, b"torn").unwrap();
            let recovered = load_personal_continuity(&path);
            assert_eq!(
                recovered.counter, 42,
                "backup recovery reserves a counter peers may already have seen"
            );
            assert_eq!(recovered.continuity.snapshot_for(&device), Some(&snapshot));
        }

        fn backup_failure_never_advances_continuity_primary() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("personal-continuity.postcard");
            let initial = PersistedPersonalContinuity {
                counter: 10,
                continuity: crate::shell::personal_continuity::PersonalContinuity::default(),
            };
            let _ = persist_personal_continuity(&path, &initial);
            let backup = personal_continuity_backup_path(&path);
            std::fs::remove_file(&backup).unwrap();
            std::fs::create_dir(&backup).unwrap();

            let _ = persist_personal_continuity(
                &path,
                &PersistedPersonalContinuity {
                    counter: 11,
                    continuity: crate::shell::personal_continuity::PersonalContinuity::default(),
                },
            );
            let restored = load_personal_continuity(&path);
            assert_eq!(restored.counter, 10);
        }

        fn personal_continuity_topic_is_stable_and_root_bound() {
            let root_a = [0xA1; 32];
            let root_b = [0xB2; 32];
            assert_eq!(
                personal_continuity_topic_id(&root_a),
                personal_continuity_topic_id(&root_a),
                "a user's devices must join one stable private shard"
            );
            assert_ne!(
                personal_continuity_topic_id(&root_a),
                personal_continuity_topic_id(&root_b),
                "fixed distinct roots must not share a test shard"
            );
            assert_ne!(personal_continuity_topic_id(&root_a), DEFAULT_WS_TOPIC);
        }

        personal_continuity_counter_and_checkpoint_survive_restart_store();
        backup_failure_never_advances_continuity_primary();
        personal_continuity_topic_is_stable_and_root_bound();
    }

    /// Exercises the shipped continuation boundaries as one lifecycle: an
    /// app report becomes a private Postcard checkpoint, a second shell
    /// turns it into dormant cards, the normal launcher handoff receives the
    /// exact resume record, and a source close removes only that card.  The
    /// two shells share this test process, so clear the process-local report
    /// cache between their halves just as separate shell processes do.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the lifecycle test holds test-only state guards until each restart snapshot is asserted"
    )]
    #[test]
    #[serial_test::serial]
    fn personal_continuity_app_report_checkpoint_restore_close_and_restart_lifecycle() {
        use crate::shell::personal_continuity::{AppInstanceId, DeviceId, LaunchIntent};
        use crate::shell::state::{AppKind, Tab, TileContent, WindowGroup, WindowWidth};

        let root = ed25519_dalek::SigningKey::from_bytes(&[0xE1; 32]);
        let user_public_key = root.verifying_key().to_bytes();
        let source_endpoint = iroh::SecretKey::from_bytes(&[0xE2; 32]);
        let source_device = DeviceId::new(hex::encode(source_endpoint.public().as_bytes()))
            .expect("endpoint ids are portable device ids");
        let room = RoomId::new();
        let browser_tab = 0xC011_0001;
        let ebook_tab = 0xC011_0002;
        let music_tab = 0xC011_0003;
        let file_tab = 0xC011_0004;
        let file_reference =
            LaunchIntent::file_reference("record:book-continue", Some([7; 32]), None)
                .expect("portable record reference");

        let mut source_tree = WorkspaceTree::bootstrap_default();
        let source_workspace = source_tree
            .desktop
            .workspaces
            .first_mut()
            .expect("bootstrap workspace");
        source_workspace.room = room;
        source_workspace.strips[0].window_groups.push(WindowGroup {
            id: crate::shell::state::next_id(),
            tabs: vec![
                Tab {
                    id: browser_tab,
                    title: "Browser".into(),
                    tile: TileContent::Foreign {
                        app_id: "so.tilo.browser".into(),
                        surface_id: 101,
                    },
                },
                Tab {
                    id: ebook_tab,
                    title: "Ebook".into(),
                    tile: TileContent::Foreign {
                        app_id: "so.tilo.ebook".into(),
                        surface_id: 102,
                    },
                },
                Tab {
                    id: music_tab,
                    title: "Music".into(),
                    tile: TileContent::Foreign {
                        app_id: "so.tilo.music".into(),
                        surface_id: 103,
                    },
                },
                Tab {
                    id: file_tab,
                    title: "A book".into(),
                    tile: TileContent::Stub {
                        kind: AppKind::FileExplorer,
                        label: "A book".into(),
                    },
                },
            ],
            active_tab: browser_tab,
            width: WindowWidth::Normal,
            maximized: false,
        });
        source_tree
            .continuation_file_references
            .insert(file_tab, file_reference.clone());

        // This is the shell-side endpoint of the app collaboration adapter:
        // each metadata report is keyed by its launcher-issued record id.
        let registry = crate::shell::personal_continuity_registry::global_registry();
        {
            let mut reports = registry.lock().expect("continuation registry");
            for (tab, app_id, metadata) in [
                (
                    browser_tab,
                    "so.tilo.browser",
                    br#"{"url":"https://example.test/reading"}"#.to_vec(),
                ),
                (
                    ebook_tab,
                    "so.tilo.ebook",
                    br#"{"book_id":"book-42","chapter":4,"page":9}"#.to_vec(),
                ),
                (
                    music_tab,
                    "so.tilo.music",
                    br#"{"track_id":"track-42","position_ms":12345,"paused":true}"#.to_vec(),
                ),
            ] {
                let metadata = ui::app::compose_structural_continuation_metadata(
                    ui::view::View::default(),
                    Some(ui::collaboration::ContinuationMetadata {
                        schema_version: 1,
                        bytes: metadata,
                    }),
                )
                .expect("framework presentation and app semantic checkpoint");
                reports.bind_tab(AppInstanceId(tab), AppInstanceId(tab));
                assert!(
                    reports
                        .report(
                            room,
                            app_id,
                            AppInstanceId(tab),
                            metadata.schema_version,
                            metadata.bytes
                        )
                        .expect("shipped app metadata contract accepts the report")
                );
            }
        }

        let dir = tempfile::tempdir().expect("continuity store");
        let continuity_path = dir.path().join("personal-continuity.postcard");
        let (mut source, _source_commands) = personal_continuity_network_for_test(user_public_key);
        source.replica = Arc::new(Mutex::new(WorkspaceTreeReplica::new(
            1,
            source_tree.shared(),
        )));
        source.personal_continuity_device = Some(source_device.clone());
        source.personal_continuity_path = Some(continuity_path.clone());
        let checkpoint = source
            .local_continuity_snapshot_bytes()
            .expect("report registry is captured through the shell checkpoint");
        let checkpoint_state = crate::shell::personal_continuity::decode_checkpoint(&checkpoint)
            .expect("shipped Postcard checkpoint");
        let offered = checkpoint_state
            .snapshot_for(&source_device)
            .expect("source snapshot");
        assert_eq!(offered.workspaces[0].apps.len(), 4);
        assert!(
            offered.workspaces[0]
                .apps
                .iter()
                .any(|app| { app.id == AppInstanceId(ebook_tab) && app.resume.is_some() })
        );
        assert!(
            offered.workspaces[0]
                .apps
                .iter()
                .any(|app| { app.id == AppInstanceId(music_tab) && app.resume.is_some() })
        );

        // A real second shell has a distinct process-local cache. Preserve
        // only the encoded relay payload before crossing that boundary.
        {
            let mut reports = registry.lock().expect("continuation registry");
            for tab in [browser_tab, ebook_tab, music_tab, file_tab] {
                reports.forget_local_tab(AppInstanceId(tab));
            }
        }

        let (target, _target_commands) = personal_continuity_network_for_test(user_public_key);
        authenticated_personal_peer(&target, &root, &source_endpoint);
        target.dispatch_neighbour_msg(
            personal_continuity_topic_id(&user_public_key),
            source_endpoint.public(),
            None,
            false,
            NeighbourMsg::PersonalContinuityCheckpoint {
                user_public_key,
                publisher: crate::network::behaviour::peer_liveness_handle(
                    &source_endpoint.public(),
                ),
                snapshot: checkpoint,
            },
        );
        {
            let replica = target.replica.lock().expect("target replica");
            let tree = replica.tree.read();
            for tab in [browser_tab, ebook_tab, music_tab, file_tab] {
                assert!(tree.continuation_tabs.contains(&tab));
            }
            assert_eq!(
                tree.continuation_file_references.get(&file_tab),
                Some(&file_reference)
            );
            assert!(
                tree.desktop
                    .workspaces
                    .iter()
                    .any(|workspace| workspace.room == room)
            );
        }

        // Model the exact peer-window -> launcher staging used before the
        // restored toplevel adopts its remote tab. This verifies metadata and
        // source CRDT record identity travel together without starting a GUI.
        let browser_launch = LaunchIntent::app("so.tilo.browser").expect("known app id");
        let browser_resume = registry
            .lock()
            .expect("continuation registry")
            .continuation_for_tab(room, AppInstanceId(browser_tab), &browser_launch)
            .expect("remote placeholder retains the app report");
        crate::shell::launcher::clear_pending_placements();
        crate::shell::launcher::set_pending_placement(
            "so.tilo.browser",
            crate::shell::tab_binding::Placement::IntoTab(browser_tab),
        );
        crate::shell::launcher::stage_continuation_resume(
            room,
            browser_tab,
            browser_resume.resume.schema_version(),
            browser_resume.resume.bytes().to_vec(),
            browser_resume.record_instance.0,
        );
        assert_eq!(
            crate::shell::launcher::take_staged_resume_for_package("so.tilo.browser", room),
            Some((
                Some((
                    browser_resume.resume.schema_version(),
                    browser_resume.resume.bytes().to_vec()
                )),
                browser_tab,
            )),
            "the real launcher consumption path receives exact resume metadata and record",
        );
        crate::shell::launcher::clear_pending_placements();

        // Closing on the source passes through compositor tab removal, then
        // a fresh checkpoint tombstones that durable tab. The target removes
        // it while retaining the other independent offers.
        {
            let replica = source.replica.lock().expect("source replica");
            assert!(crate::shell::tab_binding::remove_tab(
                &mut replica.tree.write(),
                browser_tab,
            ));
        }
        let closed_checkpoint = source
            .local_continuity_snapshot_bytes()
            .expect("close creates a newer checkpoint");
        let closed_state = crate::shell::personal_continuity::decode_checkpoint(&closed_checkpoint)
            .expect("closed checkpoint decodes");
        let closed_source = closed_state
            .snapshot_for(&source_device)
            .expect("updated source snapshot");
        assert!(closed_source.tombstones.iter().any(|tombstone| {
            tombstone.room == room && tombstone.id == AppInstanceId(browser_tab)
        }));
        target.dispatch_neighbour_msg(
            personal_continuity_topic_id(&user_public_key),
            source_endpoint.public(),
            None,
            false,
            NeighbourMsg::PersonalContinuityCheckpoint {
                user_public_key,
                publisher: crate::network::behaviour::peer_liveness_handle(
                    &source_endpoint.public(),
                ),
                snapshot: closed_checkpoint,
            },
        );
        let replica = target.replica.lock().expect("target replica");
        let tree = replica.tree.read();
        assert!(!tree.continuation_tabs.contains(&browser_tab));
        assert!(tree.continuation_tabs.contains(&ebook_tab));
        assert!(tree.continuation_tabs.contains(&music_tab));
        assert!(tree.continuation_tabs.contains(&file_tab));
        drop(tree);
        drop(replica);

        let restarted = load_personal_continuity(&continuity_path);
        assert!(
            restarted.counter >= closed_source.stamp.counter,
            "restart must never reuse the close checkpoint's Lamport counter"
        );
        assert_eq!(
            restarted.continuity.snapshot_for(&source_device),
            Some(closed_source),
            "the durable restart store retains the close tombstone"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn personal_continuity_checkpoints_are_root_checked_without_side_effects() {
        fn personal_continuity_rejects_a_certified_different_root() {
            let local_root = ed25519_dalek::SigningKey::from_bytes(&[0xA1; 32]);
            let foreign_root = ed25519_dalek::SigningKey::from_bytes(&[0xB2; 32]);
            let foreign_device = iroh::SecretKey::from_bytes(&[0xB3; 32]);
            let local_user = local_root.verifying_key().to_bytes();
            let (net, mut commands) = personal_continuity_network_for_test(local_user);
            authenticated_personal_peer(&net, &foreign_root, &foreign_device);
            while commands.try_recv().is_ok() {}

            let foreign_device_id = crate::shell::personal_continuity::DeviceId::new(hex::encode(
                foreign_device.public().as_bytes(),
            ))
            .expect("endpoint id is a valid device id");
            let snapshot = crate::shell::personal_continuity::DeviceContinuationSnapshot::new(
                foreign_device_id.clone(),
                1,
                None,
                Vec::new(),
            )
            .expect("valid snapshot");
            net.dispatch_neighbour_msg(
                personal_continuity_topic_id(&local_user),
                foreign_device.public(),
                None,
                false,
                NeighbourMsg::PersonalContinuityCheckpoint {
                    user_public_key: local_user,
                    publisher: crate::network::behaviour::peer_liveness_handle(
                        &foreign_device.public(),
                    ),
                    snapshot: postcard::to_allocvec(&snapshot).expect("encode snapshot"),
                },
            );

            assert!(
                net.personal_continuity
                    .lock()
                    .expect("continuity")
                    .snapshot_for(&foreign_device_id)
                    .is_none(),
                "a valid device certificate for another root cannot write this private lane"
            );
            assert!(
                commands.try_recv().is_err(),
                "rejection must not trigger restore or other transport side effects"
            );
        }

        fn personal_continuity_accepts_same_root_checkpoint_without_restore_side_effects() {
            let root = ed25519_dalek::SigningKey::from_bytes(&[0xC1; 32]);
            let peer_device = iroh::SecretKey::from_bytes(&[0xC2; 32]);
            let user_public_key = root.verifying_key().to_bytes();
            let (net, mut commands) = personal_continuity_network_for_test(user_public_key);
            authenticated_personal_peer(&net, &root, &peer_device);
            while commands.try_recv().is_ok() {}

            let peer_device_id = crate::shell::personal_continuity::DeviceId::new(hex::encode(
                peer_device.public().as_bytes(),
            ))
            .expect("endpoint id is a valid device id");
            let snapshot = crate::shell::personal_continuity::DeviceContinuationSnapshot::new(
                peer_device_id.clone(),
                1,
                None,
                Vec::new(),
            )
            .expect("valid snapshot");
            let mut offered = crate::shell::personal_continuity::PersonalContinuity::default();
            offered
                .upsert(snapshot.clone())
                .expect("store offered state");
            net.dispatch_neighbour_msg(
                personal_continuity_topic_id(&user_public_key),
                peer_device.public(),
                None,
                false,
                NeighbourMsg::PersonalContinuityCheckpoint {
                    user_public_key,
                    publisher: crate::network::behaviour::peer_liveness_handle(
                        &peer_device.public(),
                    ),
                    snapshot: postcard::to_allocvec(&offered).expect("encode checkpoint"),
                },
            );

            assert_eq!(
                net.personal_continuity
                    .lock()
                    .expect("continuity")
                    .snapshot_for(&peer_device_id),
                Some(&snapshot),
                "same-root state is stored as an offer for later UX, not applied now"
            );
            assert!(
                commands.try_recv().is_err(),
                "receiving a checkpoint neither focuses nor launches applications"
            );
        }

        #[allow(
            clippy::similar_names,
            clippy::significant_drop_tightening,
            reason = "the checkpoint test intentionally distinguishes root identity, room, and scoped continuation values"
        )]
        fn personal_continuity_accepts_populated_same_root_checkpoint_without_changing_focus() {
            let root = ed25519_dalek::SigningKey::from_bytes(&[0xD1; 32]);
            let peer_device = iroh::SecretKey::from_bytes(&[0xD2; 32]);
            let user_public_key = root.verifying_key().to_bytes();
            let (net, _commands) = personal_continuity_network_for_test(user_public_key);
            authenticated_personal_peer(&net, &root, &peer_device);

            let active_before = net
                .replica
                .lock()
                .expect("replica")
                .tree
                .read()
                .desktop
                .active_workspace()
                .map(|workspace| workspace.id);
            let peer_device_id = crate::shell::personal_continuity::DeviceId::new(hex::encode(
                peer_device.public().as_bytes(),
            ))
            .expect("endpoint id is a valid device id");
            let room = RoomId::new();
            let snapshot = crate::shell::personal_continuity::DeviceContinuationSnapshot::new(
                peer_device_id.clone(),
                1,
                None,
                vec![crate::shell::personal_continuity::WorkspaceContinuation {
                    room,
                    apps: Vec::new(),
                }],
            )
            .expect("valid populated snapshot");
            let mut offered = crate::shell::personal_continuity::PersonalContinuity::default();
            offered
                .upsert(snapshot.clone())
                .expect("store offered state");

            net.dispatch_neighbour_msg(
                personal_continuity_topic_id(&user_public_key),
                peer_device.public(),
                None,
                false,
                NeighbourMsg::PersonalContinuityCheckpoint {
                    user_public_key,
                    publisher: crate::network::behaviour::peer_liveness_handle(
                        &peer_device.public(),
                    ),
                    snapshot: postcard::to_allocvec(&offered).expect("encode populated checkpoint"),
                },
            );

            assert_eq!(
                net.personal_continuity
                    .lock()
                    .expect("continuity")
                    .snapshot_for(&peer_device_id),
                Some(&snapshot),
                "a populated continuation must traverse the production Postcard codec"
            );
            let replica = net.replica.lock().expect("replica");
            let tree = replica.tree.read();
            assert_eq!(
                tree.desktop
                    .active_workspace()
                    .map(|workspace| workspace.id),
                active_before,
                "receiving a continuation must not move focus"
            );
            let adopted = tree
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room == room)
                .expect("continuation room is available for later user-directed restore");
            assert!(
                adopted.strips.is_empty(),
                "receiving a continuation must not recreate applications"
            );
        }

        personal_continuity_rejects_a_certified_different_root();
        personal_continuity_accepts_same_root_checkpoint_without_restore_side_effects();
        personal_continuity_accepts_populated_same_root_checkpoint_without_changing_focus();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn legacy_inbox_and_tick_bound_and_order_actor_work() {
        fn legacy_inbox_coalesces_frames_but_keeps_control_order() {
            let tree = WorkspaceTree::bootstrap_demo().shared();
            let (net, _commands) = shell_network_for_test(tree, 99);
            let start = NeighbourMsg::SurfaceStreamStart {
                surface: 7,
                source_peer: 5,
                app_id: "org.example.Editor".into(),
                width: 64,
                height: 48,
                codec: "h264".into(),
                ts: 1,
                sender_monotonic_ns_at_send: 1,
                colorimetry: None,
                hdr_static_metadata: None,
                bit_depth: 8,
                tab: 0,
            };
            net.queue_legacy_message(42, start);
            for ts in 2..=20 {
                net.queue_legacy_message(
                    42,
                    NeighbourMsg::SurfaceFrame {
                        surface: 7,
                        source_peer: 5,
                        encoded_h264: vec![u8::try_from(ts).unwrap_or_default()],
                        ts,
                    },
                );
            }
            // Separate QUIC uni streams may complete out of order, and the host
            // fallback may deliver an older duplicate after the direct path.
            net.queue_legacy_message(
                42,
                NeighbourMsg::SurfaceFrame {
                    surface: 7,
                    source_peer: 5,
                    encoded_h264: vec![19],
                    ts: 19,
                },
            );
            net.queue_legacy_message(
                42,
                NeighbourMsg::SurfaceStreamEnd {
                    surface: 7,
                    source_peer: 5,
                    ts: 21,
                },
            );

            let drained = net.take_inbound_legacy();
            assert_eq!(drained.len(), 3);
            assert_eq!(drained[0].workspace_topic, 42);
            assert!(matches!(
                drained[0].message,
                NeighbourMsg::SurfaceStreamStart { .. }
            ));
            assert!(matches!(
                drained[1].message,
                NeighbourMsg::SurfaceFrame { ts: 20, .. }
            ));
            assert!(matches!(
                drained[2].message,
                NeighbourMsg::SurfaceStreamEnd { .. }
            ));
        }

        fn legacy_inbox_drops_local_gossip_echo() {
            let tree = WorkspaceTree::bootstrap_demo().shared();
            let (net, _commands) = shell_network_for_test(tree, 99);
            net.queue_legacy_message(
                42,
                NeighbourMsg::SurfaceFrame {
                    surface: 7,
                    source_peer: 99,
                    encoded_h264: vec![1],
                    ts: 1,
                },
            );
            assert!(net.take_inbound_legacy().is_empty());
        }

        fn tick_bounds_actor_event_work_per_frame() {
            let tree = WorkspaceTree::bootstrap_demo().shared();
            let (net, _commands) = shell_network_for_test(tree, 99);
            let peer = iroh::SecretKey::from_bytes(&[42; 32]).public();
            let queued = MAX_NET_EVENTS_PER_TICK + 1;
            for generation in 0..queued {
                net.event_tx
                    .try_send(NetEvent::PeerLeft {
                        ws: DEFAULT_WS_TOPIC,
                        peer,
                        cause: PeerDepartureCause::LivenessTimedOut,
                        membership_epoch: Some(crate::network::actor::MembershipEpoch::for_test(
                            u64::try_from(generation + 1).expect("small test generation"),
                        )),
                    })
                    .expect("bounded fixture fits the actor event channel");
            }

            net.tick();
            assert_eq!(net.peer_departures().len(), MAX_NET_EVENTS_PER_TICK);
            net.tick();
            assert_eq!(net.peer_departures().len(), queued);
        }

        fn tick_with_no_events_is_noop() {
            let tmp = tempfile::tempdir().unwrap();
            let key_path = tmp.path().join("peer-key");
            let tree = WorkspaceTree::bootstrap_demo().shared();
            let net =
                ShellNetwork::spawn(tree, &key_path, &tmp.path().join("peer-sync")).expect("spawn");
            net.tick();
        }

        legacy_inbox_coalesces_frames_but_keeps_control_order();
        legacy_inbox_drops_local_gossip_echo();
        tick_bounds_actor_event_work_per_frame();
        tick_with_no_events_is_noop();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn invite_credentials_persist_and_accepting_joins_the_exact_room() {
        fn pending_invites_survive_restart_and_cancel_exactly_one_credential() {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("pending-invitations.json");
            let room = RoomId::new();
            let first = PendingInvitation {
                id: [1; 32],
                room,
                name: "Ada".into(),
                role: crate::network::invite::InviteRole::Editor,
                token: "signed-one".into(),
                recipient_user_public_key: Some([0x31; 32]),
                expiry_unix_seconds: u64::MAX,
            };
            let second = PendingInvitation {
                id: [2; 32],
                room,
                name: "Lin".into(),
                role: crate::network::invite::InviteRole::Viewer,
                token: "signed-two".into(),
                recipient_user_public_key: None,
                expiry_unix_seconds: u64::MAX,
            };
            let persisted = std::collections::HashMap::from([
                (first.id, first.clone()),
                (second.id, second.clone()),
            ]);
            persist_pending_invitations(&path, &persisted);
            assert_eq!(load_pending_invitations(&path).len(), 2);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                assert_eq!(
                    std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }

            let (mut net, _commands) = shell_network_for_test(WorkspaceTree::default().shared(), 9);
            net.pending_invitations_path = Some(path.clone());
            *net.pending_identity_admissions.lock().unwrap() = persisted;
            assert_eq!(net.resend_invitation(&first.id).unwrap(), "signed-one");
            assert!(net.cancel_invitation(&first.id));
            assert!(net.resend_invitation(&first.id).is_err());
            assert_eq!(net.resend_invitation(&second.id).unwrap(), "signed-two");
            let restored = load_pending_invitations(&path);
            assert_eq!(restored, vec![second]);
        }

        fn live_session_claims_are_identified_before_dispatch() {
            assert_eq!(
                ShellNetwork::claimed_publisher(&NeighbourMsg::PresenceCleared {
                    peer: 41,
                    key: "cursor".into(),
                }),
                Some(41),
            );
            assert_eq!(
                ShellNetwork::required_room_verb(&NeighbourMsg::RemoteInput {
                    peer: 42,
                    surface: 7,
                    event: Vec::new(),
                }),
                Some(crate::shell::state::Verb::Write),
            );
            // Retired: never admitted as room traffic, not even on read.
            assert_eq!(
                ShellNetwork::required_room_verb(&NeighbourMsg::PermissionRequest {
                    peer: 42,
                    capability: "remote-input".into(),
                }),
                None,
            );
            assert_eq!(
                ShellNetwork::claimed_publisher(&NeighbourMsg::RemoteInput {
                    peer: 42,
                    surface: 7,
                    event: Vec::new(),
                }),
                Some(42),
            );
            assert_eq!(
                ShellNetwork::claimed_publisher(&NeighbourMsg::ScrollbackRequest {
                    artifact: collab_types::ArtifactId::new(),
                    before_seq: 10,
                    max: 5,
                    requester: 43,
                }),
                Some(43),
            );
            assert_eq!(
                ShellNetwork::claimed_publisher(&NeighbourMsg::PermissionGrant {
                    peer: 44,
                    capability: "remote-input".into(),
                }),
                None,
                "a grant names its recipient, so it must not be mistaken for a publisher claim",
            );
        }

        fn accepting_an_invite_joins_the_room_and_asks_it_for_a_catch_up() {
            use crate::network::invite::make_invite_token;
            use crate::shell::state::Workspace;

            let host_ws = Workspace::new(
                crate::shell::state::WorkspaceId::new(),
                "Design review".to_string(),
            );
            let host_room = host_ws.room;
            let owner_key = iroh::SecretKey::generate();
            let owner_addr = iroh::EndpointAddr::new(owner_key.public())
                .with_ip_addr("127.0.0.1:4001".parse().unwrap());
            let token = make_invite_token(&owner_key, &host_ws, &owner_addr, 3600)
                .expect("the host issues an invite");

            let mut guest = WorkspaceTree::default();
            let local = guest.append_empty_workspace("Mine");
            let before = guest.room_of(local).expect("the guest's own room");
            assert_ne!(before, host_room);
            let tree = guest.shared();
            let (net, mut cmd_rx) = shell_network_for_test(tree.clone(), 0x1234u64);

            let joined = net
                .accept_invite(local, &token)
                .expect("a well-formed invite is accepted");
            assert_eq!(
                joined, host_room,
                "the guest joins the room it was invited to"
            );
            assert_eq!(
                tree.read().room_of(local),
                Some(host_room),
                "the guest's workspace now addresses the inviter's room",
            );

            // Three commands, in order: join the stable room shard, dial the
            // address the token named, then ask that room for a catch-up.
            let subscribe = cmd_rx
                .try_recv()
                .expect("accepting an invite must join its room topic");
            match subscribe {
                NetCmd::JoinWorkspace { ws, .. } => assert_eq!(ws, room_topic_id(host_room)),
                other => panic!("expected NetCmd::JoinWorkspace, got {other:?}"),
            }
            let dial = cmd_rx
                .try_recv()
                .expect("accepting an invite must dial the address it named");
            match dial {
                NetCmd::Connect { addr } => assert_eq!(addr, owner_addr),
                other => panic!("expected NetCmd::Connect, got {other:?}"),
            }
            let cmd = cmd_rx
                .try_recv()
                .expect("accepting an invite must ask the room for a catch-up");
            match cmd {
                NetCmd::Publish { ws, msg } => {
                    assert_eq!(ws, room_topic_id(host_room));
                    match msg {
                        NeighbourMsg::SyncReq {
                            room, invite_id, ..
                        } => {
                            assert_eq!(
                                room,
                                host_room.to_string(),
                                "the request must name the room just joined",
                            );
                            assert_eq!(
                                invite_id,
                                Some(crate::network::invite::invite_id(&token)),
                                "admission must redeem this exact signed invite",
                            );
                        }
                        other => panic!("expected a SyncReq, got {other:?}"),
                    }
                }
                other => panic!("expected NetCmd::Publish, got {other:?}"),
            }
            assert!(
                cmd_rx.try_recv().is_err(),
                "one room joined is one request — the guest must not ask about \
                 anything else",
            );
        }

        fn accepting_a_pinned_home_host_invite_queues_exact_expectation_once() {
            use crate::network::invite::{
                HomeHostPin, InviteRole, InviteTerms, make_invite_token_with_recipient,
            };
            use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

            let owner_root = ed25519_dalek::SigningKey::from_bytes(&[0x51; 32]);
            let owner_device = iroh::SecretKey::from_bytes(&[0x52; 32]);
            let relay_service = iroh::SecretKey::from_bytes(&[0x53; 32]);
            let home_device = iroh::SecretKey::from_bytes(&[0x54; 32]);
            let mut owner_cert = DeviceCert {
                user_public_key: owner_root.verifying_key().to_bytes(),
                device_public_key: *owner_device.public().as_bytes(),
                device_label: "Alice laptop".into(),
                capabilities: DeviceCapabilities::primary(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut owner_cert, &owner_root).expect("owner certificate");

            let host_workspace = crate::shell::state::Workspace::new(
                crate::shell::state::WorkspaceId::new(),
                "Alice shared",
            );
            let room = host_workspace.room;
            let owner_addr = iroh::EndpointAddr::new(owner_device.public())
                .with_ip_addr("127.0.0.1:4510".parse().expect("owner address"));
            let host_addr = iroh::EndpointAddr::new(relay_service.public())
                .with_ip_addr("127.0.0.1:4511".parse().expect("relay address"));
            let pin = HomeHostPin {
                addr: host_addr.clone(),
                user_public_key: owner_cert.user_public_key,
                device_public_key: *home_device.public().as_bytes(),
            };
            let token = make_invite_token_with_recipient(
                &owner_device,
                &host_workspace,
                &owner_addr,
                std::slice::from_ref(&host_addr),
                std::slice::from_ref(&pin),
                Some(owner_cert),
                InviteTerms {
                    role: InviteRole::Editor,
                    recipient: None,
                    expiry_seconds_from_now: 3600,
                },
            )
            .expect("Alice issues Bob a pinned invite");

            let mut bob_tree = WorkspaceTree::default();
            let local = bob_tree.append_empty_workspace("Bob local");
            let (bob, mut commands) = shell_network_for_test(bob_tree.shared(), 0xB0B);
            assert_eq!(bob.accept_invite(local, &token).expect("Bob accepts"), room);

            let mut pinned = Vec::new();
            while let Ok(command) = commands.try_recv() {
                if let NetCmd::ConnectHost { addr, expectation } = command {
                    pinned.push((addr, expectation));
                }
            }
            assert_eq!(
                pinned.len(),
                1,
                "a pinned host must not also be dialed unpinned"
            );
            assert_eq!(pinned[0].0, host_addr);
            assert_eq!(
                pinned[0].1,
                Some(crate::network::actor::HomePeerExpectation {
                    user_public_key: pin.user_public_key,
                    device_public_key: pin.device_public_key,
                })
            );
        }

        pending_invites_survive_restart_and_cancel_exactly_one_credential();
        live_session_claims_are_identified_before_dispatch();
        accepting_an_invite_joins_the_room_and_asks_it_for_a_catch_up();
        accepting_a_pinned_home_host_invite_queues_exact_expectation_once();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn redeemed_invites_grant_exactly_their_signed_role_and_name() {
        #[allow(
            clippy::too_many_lines,
            reason = "one invite is redeemed, exercised and revoked end to end"
        )]
        fn exact_invite_redeems_as_editor_and_revocation_removes_trust() {
            use tilo_user_identity::{
                DeviceCapabilities, DeviceCert, RevocationEntry, RevocationReason,
                sign_device_cert, sign_revocation_entry,
            };

            // The room identity is read from the configuration directory; keep
            // tests that repoint it from doing so halfway through this one.
            let _env = tilo_test_env::lock();

            let user_root = ed25519_dalek::SigningKey::from_bytes(&[61; 32]);
            let device = iroh::SecretKey::from_bytes(&[62; 32]);
            let mut cert = DeviceCert {
                user_public_key: user_root.verifying_key().to_bytes(),
                device_public_key: *device.public().as_bytes(),
                device_label: "Guest laptop".into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut cert, &user_root).expect("certificate");

            let room = RoomId::new();
            let mut workspace =
                crate::shell::state::Workspace::new(crate::shell::state::WorkspaceId(17), "Shared");
            workspace.room = room;
            workspace.set_owner(crate::shell::state::local_user_id_for_room(&room));
            let mut tree = WorkspaceTree::default();
            tree.desktop.workspaces.push(workspace);
            let (mut net, _commands) = shell_network_for_test(tree.shared(), 99);
            net.require_peer_identity = true;
            let _ = net
                .pending_identity_admissions
                .lock()
                .expect("pending")
                .insert(
                    [1; 32],
                    PendingInvitation {
                        id: [1; 32],
                        room,
                        name: "Ada".into(),
                        role: crate::network::invite::InviteRole::Editor,
                        token: "invite-one".into(),
                        recipient_user_public_key: None,
                        expiry_unix_seconds: u64::MAX,
                    },
                );

            let attacker_root = ed25519_dalek::SigningKey::from_bytes(&[83; 32]);
            let attacker_device = iroh::SecretKey::from_bytes(&[84; 32]);
            let mut attacker_cert = DeviceCert {
                user_public_key: attacker_root.verifying_key().to_bytes(),
                device_public_key: *attacker_device.public().as_bytes(),
                device_label: "Link thief".into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut attacker_cert, &attacker_root).expect("attacker certificate");
            net.redeem_invited_identity(
                room_topic_id(room),
                [2; 32],
                &attacker_device.public(),
                &attacker_cert,
            );
            let collab_types::caps::Principal::Person(attacker_person) =
                collab_types::caps::Principal::person_from_key(
                    collab_types::crypto::Ed25519PublicKey::from_bytes(
                        attacker_cert.user_public_key,
                    ),
                )
            else {
                unreachable!()
            };
            assert!(
                net.replica
                    .lock()
                    .expect("replica")
                    .tree
                    .read()
                    .desktop
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.room == room)
                    .is_some_and(|workspace| workspace.role_of(&attacker_person).is_none()),
                "possession must not redeem an invite bound to a different person"
            );

            let origin = crate::shell::state::PeerOrigin::peer(&device.public());
            net.event_tx
                .try_send(NetEvent::Msg {
                    ws: room_topic_id(room),
                    peer: device.public(),
                    membership_epoch: None,
                    replayed: false,
                    msg: NeighbourMsg::IdentityHello {
                        cert: Box::new(cert.clone()),
                        revocations: Vec::new(),
                    },
                })
                .expect("identity hello");
            net.tick();
            assert!(
                !net.peer_room_allows(
                    &origin,
                    room_topic_id(room),
                    crate::shell::state::Verb::Read,
                    false,
                ),
                "identity authentication alone must not consume an outstanding invite",
            );
            net.event_tx
                .try_send(NetEvent::Msg {
                    ws: room_topic_id(room),
                    peer: device.public(),
                    membership_epoch: None,
                    replayed: false,
                    msg: NeighbourMsg::SyncReq {
                        since: Vec::new(),
                        room: room.to_string(),
                        invite_id: Some([99; 32]),
                        nonce: 0,
                    },
                })
                .expect("wrong invite redemption");
            net.tick();
            assert!(
                !net.peer_room_allows(
                    &origin,
                    room_topic_id(room),
                    crate::shell::state::Verb::Read,
                    false,
                ),
                "a different invite hash must not grant room membership",
            );
            net.event_tx
                .try_send(NetEvent::Msg {
                    ws: room_topic_id(room),
                    peer: device.public(),
                    membership_epoch: None,
                    replayed: false,
                    msg: NeighbourMsg::SyncReq {
                        since: Vec::new(),
                        room: room.to_string(),
                        invite_id: Some([1; 32]),
                        nonce: 0,
                    },
                })
                .expect("exact invite redemption");
            net.tick();
            let collab_types::caps::Principal::Person(person) =
                collab_types::caps::Principal::person_from_key(
                    collab_types::crypto::Ed25519PublicKey::from_bytes(cert.user_public_key),
                )
            else {
                unreachable!()
            };
            let replica = net.replica.lock().expect("replica");
            let tree = replica.tree.read();
            let joined = tree
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room == room)
                .expect("workspace");
            assert_eq!(
                joined.role_of(&person),
                Some(crate::shell::state::CapRole::Editor)
            );
            drop(tree);
            drop(replica);

            let changes = net
                .replica
                .lock()
                .expect("replica")
                .tree
                .write()
                .export_room_changes_since(room, &[])
                .expect("room export");
            let mut guest = WorkspaceTree::default();
            let guest_workspace = guest.append_empty_workspace("Guest");
            assert!(guest.set_room(guest_workspace, room));
            let host_origin = crate::shell::state::PeerOrigin::peer(&net.local_peer_id);
            assert!(guest.pin_room_authority(room, host_origin.clone()));
            guest
                .apply_room_changes(room, &changes, &host_origin)
                .expect("host room changes apply");
            assert_eq!(
                guest
                    .desktop
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.room == room)
                    .expect("guest workspace")
                    .role_of(&person),
                Some(crate::shell::state::CapRole::Editor),
                "the membership grant must be present in the replicated room document"
            );

            let route = crate::shell::collab_broker::CrdtRoute::record(
                "so.tilo.notes",
                room.to_string(),
                "meeting-notes",
            );
            let signing = ed25519_dalek::SigningKey::from_bytes(&device.to_bytes());
            let mut envelope = collab_types::SignedOp::new_unsigned(
                collab_types::DocumentMutation::Incremental(b"change".to_vec())
                    .encode()
                    .expect("typed mutation"),
                collab_types::document_artifact_id(
                    &route.app_id,
                    &route.cluster_id,
                    &route.record_id,
                ),
                collab_types::Ed25519PublicKey::from_bytes(signing.verifying_key().to_bytes()),
                collab_types::document_attestation_id(
                    &route.app_id,
                    &route.cluster_id,
                    &route.record_id,
                ),
                collab_types::OpClass::Splice,
                std::time::SystemTime::now(),
                1,
            );
            collab_types::sign_op(&mut envelope, &signing).expect("signed mutation");
            assert_eq!(
                net.peer_document_role(
                    &origin,
                    &route.cluster_id,
                    &route.app_id,
                    &route.record_id,
                    &envelope,
                ),
                Some(collab_types::Role::Editor),
            );
            assert_eq!(
                net.peer_document_role(
                    &origin,
                    &route.cluster_id,
                    &route.app_id,
                    "foreign-record",
                    &envelope,
                ),
                None,
            );
            assert!(net.peer_room_allows(
                &origin,
                room_topic_id(room),
                crate::shell::state::Verb::Read,
                false,
            ));
            assert!(net.peer_room_allows(
                &origin,
                room_topic_id(room),
                crate::shell::state::Verb::Write,
                false,
            ));

            let mut moved = envelope.clone();
            moved.op_class = collab_types::OpClass::Move;
            collab_types::sign_op(&mut moved, &signing).expect("signed move");
            assert_eq!(
                net.peer_document_role(
                    &origin,
                    &route.cluster_id,
                    &route.app_id,
                    &route.record_id,
                    &moved,
                ),
                Some(collab_types::Role::Editor),
                "document authorization must use the declared action class, not hard-code text splice",
            );

            let replica = net.replica.lock().expect("replica");
            let mut tree = replica.tree.write();
            let workspace = tree
                .desktop
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.room == room)
                .expect("workspace");
            workspace.capabilities.retain(|capability| {
                !matches!(
                    &capability.subject,
                    collab_types::Subject::Principal(collab_types::Principal::Person(member))
                        if member == &person
                )
            });
            workspace.grant_capability(collab_types::CapOverride::membership(
                collab_types::Subject::person(person.clone()),
                room.to_string(),
                collab_types::Verb::Comment,
            ));
            drop(tree);
            drop(replica);
            let mut comment = envelope.clone();
            comment.op_class = collab_types::OpClass::Comment;
            collab_types::sign_op(&mut comment, &signing).expect("signed comment");
            assert_eq!(
                net.peer_document_role(
                    &origin,
                    &route.cluster_id,
                    &route.app_id,
                    &route.record_id,
                    &comment,
                ),
                Some(collab_types::Role::Commenter),
            );
            assert_eq!(
                net.peer_document_role(
                    &origin,
                    &route.cluster_id,
                    &route.app_id,
                    &route.record_id,
                    &envelope,
                ),
                None,
                "a commenter must not smuggle a splice through the generic document route",
            );

            let replica = net.replica.lock().expect("replica");
            let mut tree = replica.tree.write();
            let workspace = tree
                .desktop
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.room == room)
                .expect("workspace");
            workspace.capabilities.retain(|capability| {
                !matches!(
                    &capability.subject,
                    collab_types::Subject::Principal(collab_types::Principal::Person(member))
                        if member == &person
                )
            });
            drop(tree);
            drop(replica);
            assert!(
                !net.peer_room_allows(
                    &origin,
                    room_topic_id(room),
                    crate::shell::state::Verb::Read,
                    false,
                ),
                "removal blocks every new room frame even while the device stays authenticated"
            );
            assert_eq!(
                net.peer_document_role(
                    &origin,
                    &route.cluster_id,
                    &route.app_id,
                    &route.record_id,
                    &comment,
                ),
                None,
                "an operation signed while online is still refused if it arrives after removal",
            );
            let replica = net.replica.lock().expect("replica");
            let mut tree = replica.tree.write();
            let workspace = tree
                .desktop
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.room == room)
                .expect("workspace");
            workspace.grant_capability(collab_types::CapOverride::membership(
                collab_types::Subject::person(person),
                room.to_string(),
                collab_types::Verb::Write,
            ));
            drop(tree);
            drop(replica);

            let mut revoked = RevocationEntry {
                user_public_key: cert.user_public_key,
                revoked_device_public_key: cert.device_public_key,
                revoked_at_ns: 1,
                reason: RevocationReason::Compromised,
                signature: [0; 64],
            };
            sign_revocation_entry(&mut revoked, &user_root).expect("revocation");
            net.accept_identity_hello(
                room_topic_id(room),
                &origin,
                &device.public(),
                &cert,
                &[revoked],
            );
            assert!(net.peer_identities.get(&origin).is_none());
            assert_eq!(
                net.peer_document_role(
                    &origin,
                    &route.cluster_id,
                    &route.app_id,
                    &route.record_id,
                    &envelope,
                ),
                None,
                "revocation removes document authority as well as app access",
            );
        }

        fn signed_viewer_invite_admits_a_viewer_not_an_editor() {
            use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

            // The room identity is read from the configuration directory; keep
            // tests that repoint it from doing so halfway through this one.
            let _env = tilo_test_env::lock();

            let user_root = ed25519_dalek::SigningKey::from_bytes(&[81; 32]);
            let device = iroh::SecretKey::from_bytes(&[82; 32]);
            let mut cert = DeviceCert {
                user_public_key: user_root.verifying_key().to_bytes(),
                device_public_key: *device.public().as_bytes(),
                device_label: "Viewer laptop".into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut cert, &user_root).expect("certificate");

            let room = RoomId::new();
            let mut workspace = crate::shell::state::Workspace::new(
                crate::shell::state::WorkspaceId(81),
                "Read only",
            );
            workspace.room = room;
            workspace.set_owner(crate::shell::state::local_user_id_for_room(&room));
            let mut tree = WorkspaceTree::default();
            tree.desktop.workspaces.push(workspace);
            let (net, _commands) = shell_network_for_test(tree.shared(), 81);
            let _ = net
                .pending_identity_admissions
                .lock()
                .expect("pending")
                .insert(
                    [2; 32],
                    PendingInvitation {
                        id: [2; 32],
                        room,
                        name: "Vi".into(),
                        role: crate::network::invite::InviteRole::Viewer,
                        token: "invite-two".into(),
                        recipient_user_public_key: Some(cert.user_public_key),
                        expiry_unix_seconds: u64::MAX,
                    },
                );

            let origin = crate::shell::state::PeerOrigin::peer(&device.public());
            net.accept_identity_hello(room_topic_id(room), &origin, &device.public(), &cert, &[]);
            net.redeem_invited_identity(room_topic_id(room), [2; 32], &device.public(), &cert);
            let collab_types::caps::Principal::Person(person) =
                collab_types::caps::Principal::person_from_key(
                    collab_types::crypto::Ed25519PublicKey::from_bytes(cert.user_public_key),
                )
            else {
                unreachable!()
            };
            let replica = net.replica.lock().expect("replica");
            let tree = replica.tree.read();
            let joined = tree
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room == room)
                .expect("workspace");
            assert_eq!(
                joined.role_of(&person),
                Some(crate::shell::state::CapRole::Viewer),
                "admission must enforce the signed invite role"
            );
            assert!(!joined.may_administer(&person));
            drop(tree);
            drop(replica);
            assert!(net.peer_room_allows(
                &origin,
                room_topic_id(room),
                crate::shell::state::Verb::Read,
                false,
            ));
            assert!(
                !net.peer_room_allows(
                    &origin,
                    room_topic_id(room),
                    crate::shell::state::Verb::Write,
                    false,
                ),
                "a viewer may request/read room state but must not publish room mutations"
            );
        }

        fn concurrent_invites_redeem_their_own_names_and_roles() {
            use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

            let (_tmp, tree, net, rooms) = net_with_rooms();
            let room = rooms[0];
            let make_guest = |seed: u8, label: &str| {
                let authority_key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
                let device = iroh::SecretKey::from_bytes(&[seed.saturating_add(1); 32]);
                let mut cert = DeviceCert {
                    user_public_key: authority_key.verifying_key().to_bytes(),
                    device_public_key: *device.public().as_bytes(),
                    device_label: label.into(),
                    capabilities: DeviceCapabilities::member(),
                    issued_at_ns: 1,
                    expires_at_ns: None,
                    signature: [0; 64],
                };
                sign_device_cert(&mut cert, &authority_key).expect("certificate");
                (device, cert)
            };
            let (viewer_device, viewer_cert) = make_guest(31, "Viewer device");
            let (editor_device, editor_cert) = make_guest(41, "Editor device");
            let mut pending = net.pending_identity_admissions.lock().unwrap();
            for (id, admission) in [
                (
                    [11; 32],
                    PendingInvitation {
                        id: [11; 32],
                        room,
                        name: "Vera".into(),
                        role: crate::network::invite::InviteRole::Viewer,
                        token: "invite-eleven".into(),
                        recipient_user_public_key: None,
                        expiry_unix_seconds: u64::MAX,
                    },
                ),
                (
                    [12; 32],
                    PendingInvitation {
                        id: [12; 32],
                        room,
                        name: "Eli".into(),
                        role: crate::network::invite::InviteRole::Editor,
                        token: "invite-twelve".into(),
                        recipient_user_public_key: None,
                        expiry_unix_seconds: u64::MAX,
                    },
                ),
            ] {
                pending.insert(id, admission);
            }
            drop(pending);

            net.redeem_invited_identity(
                room_topic_id(room),
                [12; 32],
                &editor_device.public(),
                &editor_cert,
            );
            net.redeem_invited_identity(
                room_topic_id(room),
                [11; 32],
                &viewer_device.public(),
                &viewer_cert,
            );

            let workspace = tree
                .read()
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room == room)
                .cloned()
                .expect("workspace");
            let person = |cert: &DeviceCert| {
                let collab_types::Principal::Person(person) =
                    collab_types::Principal::person_from_key(
                        collab_types::Ed25519PublicKey::from_bytes(cert.user_public_key),
                    )
                else {
                    unreachable!()
                };
                person
            };
            assert_eq!(
                workspace.role_of(&person(&viewer_cert)),
                Some(crate::shell::state::CapRole::Viewer),
            );
            assert_eq!(
                workspace.role_of(&person(&editor_cert)),
                Some(crate::shell::state::CapRole::Editor),
            );
            assert_eq!(
                name_in(&tree, room, &viewer_device.public()).as_deref(),
                Some("Vera"),
            );
            assert_eq!(
                name_in(&tree, room, &editor_device.public()).as_deref(),
                Some("Eli"),
            );
            assert!(net.pending_identity_admissions.lock().unwrap().is_empty());
        }

        exact_invite_redeems_as_editor_and_revocation_removes_trust();
        signed_viewer_invite_admits_a_viewer_not_an_editor();
        concurrent_invites_redeem_their_own_names_and_roles();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn room_governance_invites_are_bound_to_the_pinned_authority() {
        fn only_the_invite_pinned_authority_can_bootstrap_room_governance() {
            use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

            // The room identity is read from the configuration directory; keep
            // tests that repoint it from doing so halfway through this one.
            let _env = tilo_test_env::lock();

            let user_root = ed25519_dalek::SigningKey::from_bytes(&[91; 32]);
            let device = iroh::SecretKey::from_bytes(&[92; 32]);
            let mut cert = DeviceCert {
                user_public_key: user_root.verifying_key().to_bytes(),
                device_public_key: *device.public().as_bytes(),
                device_label: "Inviting device".into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut cert, &user_root).expect("certificate");

            let room = RoomId::new();
            let mut tree = WorkspaceTree::default();
            let local = tree.append_empty_workspace("Joining");
            assert!(tree.set_room(local, room));
            let origin = crate::shell::state::PeerOrigin::peer(&device.public());
            assert!(tree.pin_room_authority(room, origin.clone()));
            let (net, _commands) = shell_network_for_test(tree.shared(), 92);
            net.accept_identity_hello(room_topic_id(room), &origin, &device.public(), &cert, &[]);

            assert!(
                !net.peer_room_allows(
                    &origin,
                    room_topic_id(room),
                    crate::shell::state::Verb::Write,
                    false,
                ),
                "before governance arrives the inviter is not yet in the local roster"
            );
            assert!(net.peer_room_allows(
                &origin,
                room_topic_id(room),
                crate::shell::state::Verb::Write,
                true,
            ));

            let stranger =
                crate::shell::state::PeerOrigin::peer(&iroh::SecretKey::generate().public());
            assert!(
                !net.peer_room_allows(
                    &stranger,
                    room_topic_id(room),
                    crate::shell::state::Verb::Write,
                    true,
                ),
                "bootstrap authority is pinned to one authenticated transport peer"
            );
        }

        fn a_guest_cannot_mint_an_apparently_authoritative_room_invite() {
            let mut tree = WorkspaceTree::default();
            let local = tree.append_empty_workspace("Joined room");
            let room = tree.room_of(local).expect("room");
            tree.desktop
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.id == local)
                .expect("workspace")
                .set_owner("remote-owner");
            tree.room_authority.insert(
                room,
                crate::shell::state::RoomAuthority::Peer(crate::shell::state::PeerOrigin::peer(
                    &iroh::SecretKey::generate().public(),
                )),
            );
            let (net, _commands) = shell_network_for_test(tree.shared(), 0x1234);

            let error = net
                .issue_invite_named_with_role(
                    local,
                    3600,
                    "Mallory",
                    crate::network::invite::InviteRole::Editor,
                )
                .expect_err("a guest must not sign invites for somebody else's room");
            // The guest holds no Invite grant, so the one invitation policy
            // refuses before asking who governs the room.
            assert_eq!(
                Some(error.as_str()),
                crate::shell::invite_policy::InviteRoute::Refused(
                    crate::shell::invite_policy::InviteRefusal::NotPermitted,
                )
                .refusal(),
            );
        }

        /// A token that does not verify joins nothing and asks nothing.
        fn a_refused_invite_changes_nothing_and_asks_nothing() {
            let mut guest = WorkspaceTree::default();
            let local = guest.append_empty_workspace("Mine");
            let before = guest.room_of(local).expect("the guest's own room");
            let tree = guest.shared();
            let (net, mut cmd_rx) = shell_network_for_test(tree.clone(), 0x1234u64);

            let err = net
                .accept_invite(local, "not-a-token")
                .expect_err("garbage must be refused");
            assert!(!err.is_empty());
            assert_eq!(
                tree.read().room_of(local),
                Some(before),
                "a refused invite must leave the guest in its own room",
            );
            assert!(
                cmd_rx.try_recv().is_err(),
                "a refused invite must not put anything on the wire",
            );
        }

        only_the_invite_pinned_authority_can_bootstrap_room_governance();
        a_guest_cannot_mint_an_apparently_authoritative_room_invite();
        a_refused_invite_changes_nothing_and_asks_nothing();
    }

    #[test]
    #[serial_test::serial(process_env)]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn home_peer_registry_persists_hints_and_admits_matching_identities() {
        fn shell_network_spawns_with_default_keypair() {
            let tmp = tempfile::tempdir().unwrap();
            let key_path = tmp.path().join("peer-key");
            let tree = WorkspaceTree::bootstrap_demo().shared();
            let net =
                ShellNetwork::spawn(tree, &key_path, &tmp.path().join("peer-sync")).expect("spawn");
            // `cmd_tx` capacity is the actor's bounded-channel size; just
            // assert it's nonzero so we know the channel was constructed.
            assert!(net.cmd_tx.capacity() > 0);
            // Persistence side-effect: the keypair must have been written.
            assert!(key_path.exists(), "keypair file must exist after spawn");
        }

        fn home_peer_registry_survives_restart_and_only_admits_matching_identity_rooms() {
            let temp = tempfile::tempdir().expect("temp");
            let registry = temp.path().join("home-peers.json");
            let own_user = [0x11; 32];
            let service = iroh::SecretKey::from_bytes(&[0x22; 32]).public();
            let device = iroh::SecretKey::from_bytes(&[0x33; 32]).public();
            let other_service = iroh::SecretKey::from_bytes(&[0x44; 32]).public();
            std::fs::write(
                &registry,
                serde_json::to_vec(&serde_json::json!([
                    {
                        "schema_version": tilo_relay_protocol::HomePeerRegistration::CURRENT_SCHEMA,
                        "device_node_id": hex::encode(device.as_bytes()),
                        "relay_service_node_id": hex::encode(service.as_bytes()),
                        "label": "Home server",
                        "user_public_key": hex::encode(own_user),
                        "relay_workspace_ids": [42],
                    },
                    {
                        "schema_version": tilo_relay_protocol::HomePeerRegistration::CURRENT_SCHEMA,
                        "device_node_id": hex::encode(device.as_bytes()),
                        "relay_service_node_id": hex::encode(other_service.as_bytes()),
                        "label": "Other identity",
                        "user_public_key": hex::encode([0x99; 32]),
                        "relay_workspace_ids": [42],
                    },
                    {
                        "schema_version": tilo_relay_protocol::HomePeerRegistration::CURRENT_SCHEMA,
                        "device_node_id": hex::encode(device.as_bytes()),
                        "relay_service_node_id": hex::encode(other_service.as_bytes()),
                        "label": "Other workspace",
                        "user_public_key": hex::encode(own_user),
                        "relay_workspace_ids": [77],
                    },
                ]))
                .expect("registry"),
            )
            .expect("write registry");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_HOME_PEERS_PATH", &registry);
            let rooms = std::collections::BTreeSet::from([42]);
            let first_boot = registered_home_peer_addrs(Some(own_user), &rooms).unwrap();
            let restarted = registered_home_peer_addrs(Some(own_user), &rooms).unwrap();
            assert_eq!(first_boot, restarted, "registry is durable across restart");
            assert_eq!(first_boot.len(), 1);
            assert_eq!(first_boot[0].addr.id, service);
        }

        fn cold_boot_dials_home_peers_on_the_person_topic_without_local_rooms() {
            let temp = tempfile::tempdir().expect("temp");
            let registry = temp.path().join("home-peers.json");
            let own_user = [0x11; 32];
            let topic = personal_continuity_topic_id(&own_user);
            let service = iroh::SecretKey::from_bytes(&[0x22; 32]).public();
            let device = iroh::SecretKey::from_bytes(&[0x33; 32]).public();
            let other_service = iroh::SecretKey::from_bytes(&[0x44; 32]).public();
            std::fs::write(
                &registry,
                serde_json::to_vec(&serde_json::json!([
                    {
                        "schema_version": tilo_relay_protocol::HomePeerRegistration::CURRENT_SCHEMA,
                        "device_node_id": hex::encode(device.as_bytes()),
                        "relay_service_node_id": hex::encode(service.as_bytes()),
                        "label": "Home server",
                        "user_public_key": hex::encode(own_user),
                        "relay_workspace_ids": [topic],
                    },
                    {
                        "schema_version": tilo_relay_protocol::HomePeerRegistration::CURRENT_SCHEMA,
                        "device_node_id": hex::encode(device.as_bytes()),
                        "relay_service_node_id": hex::encode(other_service.as_bytes()),
                        "label": "Unrelated room",
                        "user_public_key": hex::encode(own_user),
                        "relay_workspace_ids": [77],
                    },
                ]))
                .expect("registry"),
            )
            .expect("write registry");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_HOME_PEERS_PATH", &registry);
            let rooms = relay_topics_for_home_peers(std::iter::empty(), Some(own_user));
            let peers = registered_home_peer_addrs(Some(own_user), &rooms).unwrap();
            assert_eq!(peers.len(), 1);
            assert_eq!(peers[0].addr.id, service);
            assert_eq!(peers[0].expectation.user_public_key, own_user);
        }

        fn home_peer_registry_prefers_persisted_endpoint_hints_and_falls_back_to_the_service_id() {
            let temp = tempfile::tempdir().expect("temp");
            let registry = temp.path().join("home-peers.json");
            let own_user = [0x71; 32];
            let hinted_service = iroh::SecretKey::from_bytes(&[0x72; 32]).public();
            let unhinted_service = iroh::SecretKey::from_bytes(&[0x73; 32]).public();
            let device = iroh::SecretKey::from_bytes(&[0x74; 32]).public();
            let hint = iroh::EndpointAddr::new(hinted_service)
                .with_ip_addr("127.0.0.1:47891".parse().expect("loopback address"));
            let registrations = vec![
                tilo_relay_protocol::HomePeerRegistration {
                    schema_version: tilo_relay_protocol::HomePeerRegistration::CURRENT_SCHEMA,
                    device_node_id: hex::encode(device.as_bytes()),
                    relay_service_node_id: hex::encode(hinted_service.as_bytes()),
                    relay_endpoint: Some(hint.clone()),
                    label: "Hinted home peer".into(),
                    user_public_key: hex::encode(own_user),
                    device_cert: None,
                    relay_workspace_ids: vec![42],
                    environments: tilo_relay_protocol::RelayEnvironments::default(),
                },
                tilo_relay_protocol::HomePeerRegistration {
                    schema_version: tilo_relay_protocol::HomePeerRegistration::CURRENT_SCHEMA,
                    device_node_id: hex::encode(device.as_bytes()),
                    relay_service_node_id: hex::encode(unhinted_service.as_bytes()),
                    relay_endpoint: None,
                    label: "Unhinted home peer".into(),
                    user_public_key: hex::encode(own_user),
                    device_cert: None,
                    relay_workspace_ids: vec![42],
                    environments: tilo_relay_protocol::RelayEnvironments::default(),
                },
            ];
            std::fs::write(
                &registry,
                serde_json::to_vec(&registrations).expect("registry"),
            )
            .expect("write registry");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_HOME_PEERS_PATH", &registry);
            let hosts =
                registered_home_peer_addrs(Some(own_user), &std::collections::BTreeSet::from([42]))
                    .expect("registry parses");
            assert!(hosts.iter().any(|host| host.addr == hint));
            assert!(
                hosts
                    .iter()
                    .any(|host| host.addr.id == unhinted_service && host.addr.is_empty())
            );
        }

        fn home_peer_registry_defaults_to_the_active_identity_root() {
            let temp = tempfile::tempdir().expect("temp");
            let identity_root = temp.path().join("user-data/tilo");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_IDENTITY_ROOT", &identity_root);
            env.remove("TILO_HOME_PEERS_PATH");
            assert_eq!(
                home_peer_registry_path(),
                tilo_user_identity::home_peer_registry_path(&identity_root),
                "the shell must read the approval registry beside its own user key, never a system copy"
            );
        }

        fn live_registry_refresh_connects_a_newly_approved_home_peer() {
            let temp = tempfile::tempdir().expect("temp");
            let registry = temp.path().join("home-peers.json");
            let own_user = [0x51; 32];
            let service = iroh::SecretKey::from_bytes(&[0x52; 32]).public();
            let device = iroh::SecretKey::from_bytes(&[0x53; 32]).public();
            let mut tree = WorkspaceTree::default();
            let workspace = tree.append_empty_workspace("Home peer workspace");
            let room = RoomId::new();
            assert!(tree.set_room(workspace, room));
            let (mut net, mut commands) = shell_network_for_test(tree.shared(), 7);
            net.personal_continuity_user = Some(own_user);
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_HOME_PEERS_PATH", &registry);
            std::fs::write(
                &registry,
                serde_json::to_vec(&serde_json::json!([{
                    "schema_version": tilo_relay_protocol::HomePeerRegistration::CURRENT_SCHEMA,
                    "device_node_id": hex::encode(device.as_bytes()),
                    "relay_service_node_id": hex::encode(service.as_bytes()),
                    "label": "Home server",
                    "user_public_key": hex::encode(own_user),
                    "relay_workspace_ids": [room_topic_id(room)],
                }]))
                .expect("registry"),
            )
            .expect("write registry");

            assert!(net.refresh_home_peer_hosts());
            let NetCmd::ConnectHost { addr, .. } = commands.try_recv().expect("queued relay dial")
            else {
                panic!("registry refresh must queue a host connection");
            };
            assert_eq!(addr.id, service);

            std::fs::write(&registry, b"{").expect("partial registry write");
            assert!(!net.refresh_home_peer_hosts());
            assert!(commands.try_recv().is_err());
            assert_eq!(net.host_addrs.lock().unwrap()[0].id, service);

            std::fs::write(&registry, b"[]").expect("remove registration");
            assert!(net.refresh_home_peer_hosts());
            let NetCmd::DisconnectHost { id } = commands.try_recv().expect("queued relay removal")
            else {
                panic!("registry removal must tear down the live host");
            };
            assert_eq!(id, service);
            assert!(net.host_addrs.lock().unwrap().is_empty());
        }

        shell_network_spawns_with_default_keypair();
        home_peer_registry_survives_restart_and_only_admits_matching_identity_rooms();
        cold_boot_dials_home_peers_on_the_person_topic_without_local_rooms();
        home_peer_registry_prefers_persisted_endpoint_hints_and_falls_back_to_the_service_id();
        home_peer_registry_defaults_to_the_active_identity_root();
        live_registry_refresh_connects_a_newly_approved_home_peer();
    }

    /// Two rooms this machine governs, and the network that holds
    /// them. Enough to ask who a name belongs to.
    fn net_with_rooms() -> (
        tempfile::TempDir,
        crate::shell::state::WorkspaceTreeHandle,
        ShellNetwork,
        Vec<crate::shell::state::RoomId>,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let rooms: Vec<_> = {
            let mut t = tree.write();
            // `bootstrap_demo` leaves the owner `None`; `may_administer`
            // reads it, so an unset owner would refuse every name below
            // for the wrong reason.
            for ws in &mut t.desktop.workspaces {
                let me = crate::shell::state::local_user_id_for_room(&ws.room);
                ws.set_owner(me);
            }
            t.desktop.workspaces.iter().map(|w| w.room).collect()
        };
        assert!(
            rooms.len() >= 2 && rooms.iter().all(|r| !r.is_nil()),
            "the fixture governs at least two rooms: {rooms:?}"
        );
        let net = ShellNetwork::spawn(
            tree.clone(),
            &tmp.path().join("peer-key"),
            &tmp.path().join("peer-sync"),
        )
        .expect("spawn");
        (tmp, tree, net, rooms)
    }

    fn name_in(
        tree: &crate::shell::state::WorkspaceTreeHandle,
        room: crate::shell::state::RoomId,
        peer: &iroh::EndpointId,
    ) -> Option<String> {
        tree.read()
            .room_peer_names
            .get(&room)
            .and_then(|names| names.get(&peer.to_string()))
            .cloned()
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn peer_lifecycle_retains_departure_cause_and_seeds_host_checkpoints() {
        fn peer_departure_retains_identity_route_and_cause_before_cleanup() {
            use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

            let tree = WorkspaceTree::bootstrap_demo().shared();
            let room = tree.read().rooms()[0];
            {
                let mut tree = tree.write();
                let local_user = crate::shell::state::local_user_id();
                for workspace in &mut tree.desktop.workspaces {
                    workspace.set_owner(local_user.clone());
                }
            }
            let (net, _cmd_rx) = shell_network_for_test(tree.clone(), 0xD3E0_7A7E);
            let authority_key = ed25519_dalek::SigningKey::from_bytes(&[82; 32]);
            let peer = iroh::SecretKey::from_bytes(&[83; 32]).public();
            let mut cert = DeviceCert {
                user_public_key: authority_key.verifying_key().to_bytes(),
                device_public_key: *peer.as_bytes(),
                device_label: "Vera's device".into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut cert, &authority_key).expect("certificate");
            let origin = crate::shell::state::PeerOrigin::peer(&peer);
            let now_ns = now_unix_nanos();
            net.peer_identities
                .insert(origin.clone(), cert.clone(), now_ns)
                .expect("admitted peer identity");
            tree.write()
                .room_peer_names
                .entry(room)
                .or_default()
                .insert(peer.to_string(), "Vera".into());

            net.event_tx
                .try_send(NetEvent::PeerLeft {
                    ws: room_topic_id(room),
                    peer,
                    cause: PeerDepartureCause::LivenessTimedOut,
                    membership_epoch: None,
                })
                .expect("queue peer departure");
            net.tick();

            let departures = net.peer_departures();
            let [departure] = departures.as_slice() else {
                panic!("one departure should be retained: {departures:?}");
            };
            assert_eq!(departure.endpoint_id, peer);
            assert_eq!(departure.generation, 1);
            assert_eq!(
                departure.session_handle,
                crate::network::behaviour::peer_liveness_handle(&peer)
            );
            assert_eq!(departure.route, CrdtRoute::cluster(room.to_string()));
            assert_eq!(departure.cause, PeerDepartureCause::LivenessTimedOut);
            assert_eq!(departure.user_public_key, Some(cert.user_public_key));
            assert_eq!(departure.participant_name.as_deref(), Some("Vera"));
            assert!(departure.observed_at <= SystemTime::now());
            assert!(
                net.peer_identities.get(&origin).is_none(),
                "identity cleanup happens only after departure diagnostics are captured"
            );
        }

        fn host_ready_seeds_shell_and_app_checkpoints() {
            let tree = WorkspaceTree::bootstrap_demo().shared();
            let room_id = tree.read().rooms()[0];
            let topic = room_topic_id(room_id);
            let (net, mut cmd_rx) = shell_network_for_test(tree, 0xCAFE);
            // Consume the normal first-tick anti-entropy request so the assertion
            // below is specifically about the host baseline edge.
            net.tick();
            while cmd_rx.try_recv().is_ok() {}

            net.event_tx
                .try_send(NetEvent::HostReady {
                    ws: topic,
                    peer: iroh::SecretKey::from_bytes(&[23; 32]).public(),
                })
                .unwrap();
            net.tick();

            // A slow runner can pass SYNC_KICK_INTERVAL between the two
            // ticks, so a periodic SyncReq may precede the checkpoint.
            let mut commands = Vec::new();
            while let Ok(cmd) = cmd_rx.try_recv() {
                commands.push(cmd);
            }
            assert!(
                commands.iter().any(|cmd| matches!(
                    cmd,
                    NetCmd::PublishCheckpoint {
                        ws,
                        msg: NeighbourMsg::SyncResp { room, .. },
                    } if *ws == topic && room == &room_id.to_string()
                )),
                "HostReady seeds the room checkpoint: {commands:?}"
            );
            let app_events = net.inbound_app_presence.lock().unwrap().clone();
            assert_eq!(
                app_events.len(),
                2,
                "Arrived + Left prompt a bounded app snapshot refresh"
            );
            let payloads: Vec<_> = app_events.iter().map(|event| &event.presence).collect();
            assert!(matches!(
                payloads[0],
                collab_types::PeerPresence {
                    payload: collab_types::PeerPresencePayload::Arrived,
                    ..
                }
            ));
            assert!(matches!(
                payloads[1],
                collab_types::PeerPresence {
                    payload: collab_types::PeerPresencePayload::Left,
                    ..
                }
            ));
        }

        peer_departure_retains_identity_route_and_cause_before_cleanup();
        host_ready_seeds_shell_and_app_checkpoints();
    }

    /// Build the inbound event a LAN peer's signed document publish arrives as.
    fn app_document_event(
        app_id: &str,
        cluster_id: &str,
        from_peer_hash: u64,
        peer: iroh::PublicKey,
        device: &ed25519_dalek::SigningKey,
    ) -> NetEvent {
        app_document_event_with(
            app_id,
            cluster_id,
            "note-1",
            "note-1",
            from_peer_hash,
            peer,
            device,
        )
    }

    /// As [`app_document_event`], for a local record id sent under
    /// `wire_record_id`.
    fn app_document_event_for(
        cluster_id: &str,
        record_id: &str,
        wire_record_id: &str,
        from_peer_hash: u64,
        peer: iroh::PublicKey,
        device: &ed25519_dalek::SigningKey,
    ) -> NetEvent {
        app_document_event_with(
            "so.tilo.notes",
            cluster_id,
            record_id,
            wire_record_id,
            from_peer_hash,
            peer,
            device,
        )
    }

    /// As [`app_document_event_for`], under an explicit app domain.
    fn app_document_event_with(
        app_id: &str,
        cluster_id: &str,
        record_id: &str,
        wire_record_id: &str,
        from_peer_hash: u64,
        peer: iroh::PublicKey,
        device: &ed25519_dalek::SigningKey,
    ) -> NetEvent {
        let mut envelope = collab_types::SignedOp::new_unsigned(
            collab_types::DocumentMutation::Incremental(vec![1, 2, 3, 4])
                .encode()
                .expect("typed mutation"),
            collab_types::document_artifact_id(app_id, cluster_id, record_id),
            collab_types::Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes()),
            collab_types::document_attestation_id(app_id, cluster_id, record_id),
            collab_types::OpClass::Splice,
            std::time::SystemTime::now(),
            1,
        );
        collab_types::sign_op(&mut envelope, device).expect("device signs document");
        NetEvent::Msg {
            ws: RoomId::parse(cluster_id)
                .filter(|room| !room.is_nil())
                .map_or(DEFAULT_WS_TOPIC, room_topic_id),
            peer,
            membership_epoch: None,
            replayed: false,
            msg: NeighbourMsg::AppDocument {
                peer: from_peer_hash,
                app_id: app_id.to_string(),
                cluster_id: cluster_id.to_string(),
                record_id: wire_record_id.to_string(),
                envelope: Box::new(envelope),
            },
        }
    }

    /// One signed person-room mutation of `record_id`, written by `device`.
    fn person_document(
        user: &[u8; 32],
        record_id: &str,
        device: &ed25519_dalek::SigningKey,
    ) -> collab_types::SignedOp {
        let cluster = collab_types::person_room(user).to_string();
        let mut envelope = collab_types::SignedOp::new_unsigned(
            collab_types::DocumentMutation::Snapshot(b"watched: one video".to_vec())
                .encode()
                .expect("typed mutation"),
            collab_types::document_artifact_id("so.tilo.tube", &cluster, record_id),
            collab_types::Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes()),
            collab_types::document_attestation_id("so.tilo.tube", &cluster, record_id),
            collab_types::OpClass::Splice,
            std::time::SystemTime::now(),
            1,
        );
        collab_types::sign_op(&mut envelope, device).expect("device signs document");
        envelope
    }

    /// `commands` hand one person-room document of `cluster` to the
    /// person's always-on hosts, on their continuity topic, and gossip it
    /// nowhere.
    fn assert_kept_for_the_persons_hosts(commands: &[NetCmd], user: &[u8; 32], cluster: &str) {
        assert!(
            commands.iter().all(|command| !matches!(
                command,
                NetCmd::Publish { .. } | NetCmd::NativePublish { .. } | NetCmd::Replay { .. }
            )),
            "a person-room document is never gossiped and never reaches a room topic"
        );
        let hosted: Vec<_> = commands
            .iter()
            .filter_map(|command| match command {
                NetCmd::PublishToHosts { ws, msg } => Some((*ws, msg)),
                _ => None,
            })
            .collect();
        assert!(
            matches!(hosted.as_slice(), [(ws, NeighbourMsg::AppDocument { cluster_id, .. })]
                if *ws == personal_continuity_topic_id(user) && cluster_id == cluster),
            "the person's always-on hosts keep it on their continuity topic: {hosted:?}"
        );
    }

    /// A record in the person's own room goes from one of their devices to
    /// another over the authenticated personal fabric, never on a room topic,
    /// and reaches the other device's apps as the room owner's write.
    #[test]
    fn person_room_documents_travel_only_between_one_persons_devices() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[0xE1; 32]);
        let user = root.verifying_key().to_bytes();
        let cluster = collab_types::person_room(&user).to_string();
        let record = "file:/home/ada/.local/share/tube/history";
        let laptop = iroh::SecretKey::from_bytes(&[0xE2; 32]);
        let laptop_signing = ed25519_dalek::SigningKey::from_bytes(&[0xE2; 32]);
        let phone = iroh::SecretKey::from_bytes(&[0xE3; 32]);

        let (sender, mut sent) = personal_continuity_network_for_test(user);
        authenticated_personal_peer(&sender, &root, &phone);
        while sent.try_recv().is_ok() {}
        sender.publish_document(
            "so.tilo.tube".into(),
            cluster.clone(),
            record,
            person_document(&user, record, &laptop_signing),
        );
        let commands: Vec<_> = std::iter::from_fn(|| sent.try_recv().ok()).collect();
        assert_kept_for_the_persons_hosts(&commands, &user, &cluster);
        let payloads: Vec<_> = commands
            .into_iter()
            .filter_map(|command| match command {
                NetCmd::PublishPersonalFabricTarget {
                    ws,
                    target,
                    payload,
                    ..
                } => {
                    assert_eq!(ws, personal_continuity_topic_id(&user));
                    assert_eq!(target, phone.public(), "only the person's own device");
                    Some(payload)
                }
                _ => None,
            })
            .collect();
        let [payload] = payloads.as_slice() else {
            panic!("exactly one personal fabric frame, got {}", payloads.len());
        };
        let encoded = postcard::to_allocvec(payload).expect("wire encoding");
        assert!(
            !encoded.windows(9).any(|window| window == b"/home/ada"),
            "the record's local path stays on the device"
        );

        let (receiver, _commands) = personal_continuity_network_for_test(user);
        receiver.register_local_record("so.tilo.tube", &cluster, record);
        let deliver = |from: &iroh::SecretKey, payload: PersonalFabricPayload| {
            receiver.dispatch_personal_fabric(
                personal_continuity_topic_id(&user),
                from.public(),
                crate::network::actor::MembershipEpoch::for_test(1),
                user,
                payload,
            );
        };
        deliver(&laptop, payload.clone());
        assert!(
            receiver
                .inbound_documents_handle()
                .lock()
                .unwrap()
                .is_empty(),
            "a device without an authenticated personal route is refused"
        );
        authenticated_personal_peer(&receiver, &root, &laptop);
        let impostor = ed25519_dalek::SigningKey::from_bytes(&[0xE4; 32]);
        deliver(
            &laptop,
            PersonalFabricPayload::Document {
                app_id: "so.tilo.tube".into(),
                record_id: crate::network::wire_record::wire_record_id(
                    &cluster,
                    "so.tilo.tube",
                    record,
                ),
                envelope: Box::new(person_document(&user, record, &impostor)),
            },
        );
        assert!(
            receiver
                .inbound_documents_handle()
                .lock()
                .unwrap()
                .is_empty(),
            "a document not signed by the sending device is refused"
        );
        deliver(&laptop, payload.clone());
        let inbound = receiver.inbound_documents_handle();
        let queued = inbound.lock().unwrap();
        let [document] = queued.as_slice() else {
            panic!("the document reaches the broker queue once");
        };
        assert_eq!(document.cluster_id, cluster);
        assert_eq!(document.record_id, record, "the app sees its own record id");
        assert_eq!(document.role, collab_types::Role::Owner);
        assert!(!document.replayed);
    }

    /// Content a person record references follows the record: it is asked
    /// of and answered by the person's own devices over their fabric, never
    /// on a room topic, whichever room the window showing it is in.
    #[test]
    fn a_person_records_content_travels_only_between_the_persons_devices() {
        use tilo_action_broker::record_content::{PIECE_BYTES, content_hash};

        let root = ed25519_dalek::SigningKey::from_bytes(&[0xD1; 32]);
        let user = root.verifying_key().to_bytes();
        let laptop = iroh::SecretKey::from_bytes(&[0xD2; 32]);
        let phone = iroh::SecretKey::from_bytes(&[0xD3; 32]);
        let (on_laptop, mut laptop_sent) = personal_continuity_network_for_test(user);
        let (on_phone, mut phone_sent) = personal_continuity_network_for_test(user);
        authenticated_personal_peer(&on_laptop, &root, &phone);
        authenticated_personal_peer(&on_phone, &root, &laptop);
        while laptop_sent.try_recv().is_ok() {}
        while phone_sent.try_recv().is_ok() {}
        let (on_laptop, on_phone) = (Arc::new(on_laptop), Arc::new(on_phone));
        let laptop_content = record_content::tests::content_over(&on_laptop);
        let phone_content = record_content::tests::content_over(&on_phone);
        let route = tilo_action_broker::route::CrdtRoute::record(
            "so.tilo.tube",
            collab_types::person_room(&user).to_string(),
            "history",
        );
        let poster: Vec<u8> = (0..PIECE_BYTES + 9)
            .map(|index| u8::try_from(index % 239).unwrap())
            .collect();
        let hash = content_hash(&poster);
        laptop_content.offer(&route, &hash, &poster).unwrap();
        let mut arrivals = phone_content.subscribe(&route);

        // Hand every fabric frame `sent` holds to `to`, from `from`.
        let carry =
            |sent: &mut mpsc::Receiver<NetCmd>,
             to: &Arc<ShellNetwork>,
             from: &iroh::SecretKey,
             content: &tilo_action_broker::record_content::RouteContent| {
                let mut frames = 0_usize;
                while let Ok(command) = sent.try_recv() {
                    match command {
                        NetCmd::PublishPersonalFabricTarget {
                            ws,
                            target,
                            payload,
                            ..
                        } => {
                            assert_eq!(ws, personal_continuity_topic_id(&user));
                            assert_ne!(target, from.public(), "never back to the sender");
                            to.dispatch_personal_fabric(
                                ws,
                                from.public(),
                                crate::network::actor::MembershipEpoch::for_test(1),
                                user,
                                payload,
                            );
                            frames = frames.saturating_add(1);
                        }
                        NetCmd::Publish { .. }
                        | NetCmd::PublishRecordContent(_)
                        | NetCmd::PublishToHosts { .. } => {
                            panic!("a person record's content never leaves the person's fabric")
                        }
                        _ => {}
                    }
                }
                let inbound = to.inbound_record_content_handle();
                assert!(crate::shell::record_content::drain(&inbound, content));
                frames
            };
        phone_content.want(&route, &hash);
        assert_eq!(
            carry(&mut phone_sent, &on_laptop, &phone, &laptop_content),
            1
        );
        assert_eq!(
            carry(&mut laptop_sent, &on_phone, &laptop, &phone_content),
            2
        );
        let arrival = arrivals.try_recv().expect("the poster reached the phone");
        assert_eq!(arrival.bytes, poster);

        // A collaborator asking a room topic for it is never answered.
        on_laptop.dispatch_neighbour_msg(
            room_topic_id(collab_types::person_room(&user).into()),
            phone.public(),
            None,
            false,
            NeighbourMsg::RecordContentWant {
                requester: crate::network::behaviour::peer_liveness_handle(&phone.public()),
                app_id: route.app_id,
                cluster_id: route.cluster_id,
                hash,
            },
        );
        assert!(
            on_laptop
                .inbound_record_content_handle()
                .lock()
                .unwrap()
                .is_empty()
        );
    }

    /// A collaborator never sees the person room: nothing about it is
    /// published where they listen, and nothing naming it is accepted from
    /// them, on a room topic or on the personal fabric.
    #[test]
    fn a_collaborator_never_sends_or_receives_person_room_records() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[0xE5; 32]);
        let user = root.verifying_key().to_bytes();
        let person_room = RoomId::from(collab_types::person_room(&user));
        let collaborator_root = ed25519_dalek::SigningKey::from_bytes(&[0xE6; 32]);
        let collaborator = iroh::SecretKey::from_bytes(&[0xE7; 32]);
        let collaborator_signing = ed25519_dalek::SigningKey::from_bytes(&[0xE7; 32]);

        // A workspace already pointing at the person room (as a tampered
        // state file might) and an ordinary shared room.
        let mut tree = WorkspaceTree::default();
        let shared = tree.append_empty_workspace("Shared");
        let shared_room = tree.room_of(shared).expect("a minted room");
        let held = tree.append_empty_workspace("Held");
        tree.desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == held)
            .expect("workspace")
            .room = person_room;
        let (mut net, mut sent) = shell_network_for_test(tree.shared(), 0xE8);
        net.personal_continuity_user = Some(user);
        authenticated_personal_peer_on_topic(
            &net,
            &collaborator_root,
            &collaborator,
            crate::network::actor::MembershipEpoch::for_test(1),
            personal_continuity_topic_id(&user),
        );
        while sent.try_recv().is_ok() {}

        let record = "history";
        let own_device = ed25519_dalek::SigningKey::from_bytes(&[0xE9; 32]);
        let envelope = person_document(&user, record, &own_device);
        let artifact = envelope.artifact;
        net.publish_document(
            "so.tilo.tube".into(),
            person_room.to_string(),
            record,
            envelope,
        );
        net.publish_app_presence(
            "so.tilo.tube".into(),
            person_room.to_string(),
            collab_types::PeerPresence {
                key: Vec::new(),
                payload: collab_types::PeerPresencePayload::Arrived,
            },
        );
        net.publish_scrollback_response(room_topic_id(person_room), artifact, Vec::new());
        net.publish_scrollback_request(&person_room.to_string(), artifact, u64::MAX, 8);
        net.request_room_replay(&person_room.to_string());
        let leaked: Vec<_> = std::iter::from_fn(|| sent.try_recv().ok()).collect();
        assert!(
            leaked.iter().all(|command| matches!(
                command,
                NetCmd::PublishToHosts { ws, .. } | NetCmd::Replay { ws }
                    if *ws == personal_continuity_topic_id(&user)
            )),
            "nothing about the person room leaves for anyone but the person's devices and hosts: {leaked:?}"
        );

        net.dispatch_scrollback_request(room_topic_id(person_room), 0xBEEF, artifact, u64::MAX, 8);
        assert!(
            net.inbound_scrollback_request.lock().unwrap().is_empty(),
            "a peer asking for a person-room record's history gets no page"
        );

        for room in [person_room, shared_room] {
            net.event_tx
                .try_send(app_document_event(
                    "so.tilo.tube",
                    &room.to_string(),
                    0xBBBB,
                    collaborator.public(),
                    &collaborator_signing,
                ))
                .expect("queue the collaborator's publish");
            net.tick();
        }
        let inbound = net.inbound_documents_handle();
        let queued = inbound.lock().unwrap();
        assert!(
            matches!(queued.as_slice(), [document] if document.cluster_id == shared_room.to_string()),
            "only the shared room's document is accepted from a room topic"
        );
        drop(queued);

        net.dispatch_personal_fabric(
            personal_continuity_topic_id(&user),
            collaborator.public(),
            crate::network::actor::MembershipEpoch::for_test(1),
            user,
            PersonalFabricPayload::Document {
                app_id: "so.tilo.tube".into(),
                record_id: record.into(),
                envelope: Box::new(person_document(&user, record, &collaborator_signing)),
            },
        );
        assert_eq!(
            inbound.lock().unwrap().len(),
            1,
            "another person's device is no route into the person room"
        );
    }

    /// A device that was away misses what its sibling wrote meanwhile; when
    /// it comes back on an authenticated route, this device asks its local
    /// apps for their person-room snapshots, once. Nobody else's arrival,
    /// and no ordinary room peer, asks for them.
    #[test]
    fn a_returning_device_is_caught_up_on_person_records() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[0xEB; 32]);
        let user = root.verifying_key().to_bytes();
        let laptop_signing = ed25519_dalek::SigningKey::from_bytes(&[0xEC; 32]);
        let phone = iroh::SecretKey::from_bytes(&[0xED; 32]);
        let (net, mut sent) = personal_continuity_network_for_test(user);

        net.publish_document(
            "so.tilo.tube".into(),
            collab_types::person_room(&user).to_string(),
            "history",
            person_document(&user, "history", &laptop_signing),
        );
        assert!(
            std::iter::from_fn(|| sent.try_recv().ok()).all(|command| !matches!(
                command,
                NetCmd::PublishPersonalFabricTarget { .. } | NetCmd::Publish { .. }
            )),
            "an edit made while the phone is away reaches no device directly"
        );
        assert_eq!(net.take_person_snapshot_request(), None);

        let stranger = ed25519_dalek::SigningKey::from_bytes(&[0xEE; 32]);
        authenticated_personal_peer_on_topic(
            &net,
            &stranger,
            &iroh::SecretKey::from_bytes(&[0xEF; 32]),
            crate::network::actor::MembershipEpoch::for_test(1),
            personal_continuity_topic_id(&user),
        );
        authenticated_personal_peer_on_topic(
            &net,
            &root,
            &phone,
            crate::network::actor::MembershipEpoch::for_test(1),
            room_topic_id(RoomId::new()),
        );
        assert_eq!(
            net.take_person_snapshot_request(),
            None,
            "another person's device, or a room peer, is owed nothing"
        );

        authenticated_personal_peer(&net, &root, &phone);
        assert_eq!(
            net.take_person_snapshot_request(),
            Some(collab_types::person_room(&user).to_string()),
            "the phone's return asks the local apps for their snapshots"
        );
        assert_eq!(net.take_person_snapshot_request(), None, "once");
    }

    /// What the person's always-on host kept on their continuity topic
    /// reaches a device that comes online when no other of their devices
    /// is: written by one of the person's devices, never by anyone else's,
    /// and never from a room topic.
    #[test]
    fn a_person_record_kept_by_their_host_reaches_a_device_alone_online() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[0xD1; 32]);
        let user = root.verifying_key().to_bytes();
        let cluster = collab_types::person_room(&user).to_string();
        let continuity = personal_continuity_topic_id(&user);
        let record = "history";
        let laptop = iroh::SecretKey::from_bytes(&[0xD2; 32]);
        let laptop_signing = ed25519_dalek::SigningKey::from_bytes(&[0xD2; 32]);
        let stranger_root = ed25519_dalek::SigningKey::from_bytes(&[0xD3; 32]);
        let stranger = iroh::SecretKey::from_bytes(&[0xD4; 32]);
        let stranger_signing = ed25519_dalek::SigningKey::from_bytes(&[0xD4; 32]);

        let (phone, mut sent) = personal_continuity_network_for_test(user);
        // A fresh app on the phone asks the person's hosts for what they kept.
        phone.register_local_record("so.tilo.tube", &cluster, record);
        phone.request_room_replay(&cluster);
        assert!(
            std::iter::from_fn(|| sent.try_recv().ok())
                .any(|command| matches!(command, NetCmd::Replay { ws } if ws == continuity)),
            "the replay is asked on the continuity topic"
        );

        // The host replays the laptop's write; the laptop itself is offline,
        // known to the phone only by the certificate the host replayed.
        authenticated_personal_peer_on_topic(
            &phone,
            &root,
            &laptop,
            crate::network::actor::MembershipEpoch::for_test(1),
            continuity,
        );
        authenticated_personal_peer_on_topic(
            &phone,
            &stranger_root,
            &stranger,
            crate::network::actor::MembershipEpoch::for_test(1),
            continuity,
        );
        let kept = |author: &iroh::SecretKey, signing: &ed25519_dalek::SigningKey| {
            let msg = NeighbourMsg::AppDocument {
                peer: 7,
                app_id: "so.tilo.tube".into(),
                cluster_id: cluster.clone(),
                record_id: crate::network::wire_record::wire_record_id(
                    &cluster,
                    "so.tilo.tube",
                    record,
                ),
                envelope: Box::new(person_document(&user, record, signing)),
            };
            (author.public(), msg)
        };
        let inbound = phone.inbound_documents_handle();
        let (author, msg) = kept(&stranger, &stranger_signing);
        phone.dispatch_neighbour_msg(continuity, author, None, true, msg);
        assert!(
            inbound.lock().unwrap().is_empty(),
            "another person's device writes nothing here"
        );
        let (author, msg) = kept(&laptop, &laptop_signing);
        phone.dispatch_neighbour_msg(
            room_topic_id(RoomId::new()),
            author,
            None,
            true,
            msg.clone(),
        );
        assert!(
            inbound.lock().unwrap().is_empty(),
            "never from a room topic"
        );
        phone.dispatch_neighbour_msg(continuity, author, None, true, msg);
        let queued = inbound.lock().unwrap();
        let [document] = queued.as_slice() else {
            panic!("the kept document reaches the broker queue once");
        };
        assert_eq!(document.cluster_id, cluster);
        assert_eq!(document.record_id, record);
        assert_eq!(document.role, collab_types::Role::Owner);
    }

    /// A person record larger than one frame travels as the segments its
    /// app signed, each well under the personal fabric's frame, and every
    /// segment reaches the other device's apps to be reassembled there.
    #[test]
    fn a_person_record_larger_than_a_frame_travels_as_segments() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[0xC5; 32]);
        let user = root.verifying_key().to_bytes();
        let cluster = collab_types::person_room(&user).to_string();
        let laptop = iroh::SecretKey::from_bytes(&[0xC6; 32]);
        let laptop_signing = ed25519_dalek::SigningKey::from_bytes(&[0xC6; 32]);
        let phone = iroh::SecretKey::from_bytes(&[0xC7; 32]);
        let large = collab_types::DocumentMutation::Snapshot(vec![
            7;
            crate::shell::device_fabric::MAX_FABRIC_CHECKPOINT_BYTES
                + 1
        ]);
        let frames = large.encode_frames().expect("segments");
        assert!(frames.len() > 4, "more than one frame's worth");
        let segments: Vec<_> = frames
            .into_iter()
            .enumerate()
            .map(|(seq, frame)| {
                let mut envelope = collab_types::SignedOp::new_unsigned(
                    frame,
                    collab_types::document_artifact_id("so.tilo.tube", &cluster, "history"),
                    collab_types::Ed25519PublicKey::from_bytes(
                        laptop_signing.verifying_key().to_bytes(),
                    ),
                    collab_types::document_attestation_id("so.tilo.tube", &cluster, "history"),
                    collab_types::OpClass::Splice,
                    std::time::SystemTime::now(),
                    u64::try_from(seq).unwrap() + 1,
                );
                collab_types::sign_op(&mut envelope, &laptop_signing).expect("signed segment");
                envelope
            })
            .collect();

        let (sender, mut sent) = personal_continuity_network_for_test(user);
        authenticated_personal_peer(&sender, &root, &phone);
        while sent.try_recv().is_ok() {}
        for segment in &segments {
            sender.publish_document(
                "so.tilo.tube".into(),
                cluster.clone(),
                "history",
                segment.clone(),
            );
        }
        let frames: Vec<_> = std::iter::from_fn(|| sent.try_recv().ok())
            .filter_map(|command| match command {
                NetCmd::PublishPersonalFabricTarget { payload, .. } => Some(payload),
                _ => None,
            })
            .collect();
        assert_eq!(
            frames.len(),
            segments.len(),
            "every segment is sent, none dropped"
        );

        let (receiver, _commands) = personal_continuity_network_for_test(user);
        receiver.register_local_record("so.tilo.tube", &cluster, "history");
        authenticated_personal_peer(&receiver, &root, &laptop);
        for payload in frames {
            receiver.dispatch_personal_fabric(
                personal_continuity_topic_id(&user),
                laptop.public(),
                crate::network::actor::MembershipEpoch::for_test(1),
                user,
                payload,
            );
        }
        assert_eq!(
            receiver.inbound_documents_handle().lock().unwrap().len(),
            segments.len(),
            "every segment reaches the apps that reassemble it"
        );
    }

    /// One device acting for two people — its own root and a workspace
    /// identity's — keeps each one's person record between that person's
    /// devices only: published to that root's other device on that root's
    /// continuity topic, and taken in only from a device of the root whose
    /// room it is.
    #[test]
    fn each_identity_a_device_acts_for_keeps_its_person_records_to_itself() {
        let ada_root = ed25519_dalek::SigningKey::from_bytes(&[0xA1; 32]);
        let work_root = ed25519_dalek::SigningKey::from_bytes(&[0xA2; 32]);
        let (ada, work) = (
            ada_root.verifying_key().to_bytes(),
            work_root.verifying_key().to_bytes(),
        );
        let ada_phone = iroh::SecretKey::from_bytes(&[0xA3; 32]);
        let work_laptop = iroh::SecretKey::from_bytes(&[0xA4; 32]);
        let work_laptop_signing = ed25519_dalek::SigningKey::from_bytes(&[0xA4; 32]);
        let here = ed25519_dalek::SigningKey::from_bytes(&[0xA5; 32]);
        let (mut net, mut sent) = personal_continuity_network_for_test(ada);
        net.workspace_person_roots = vec![work];
        authenticated_personal_peer(&net, &ada_root, &ada_phone);
        authenticated_personal_peer(&net, &work_root, &work_laptop);
        while sent.try_recv().is_ok() {}

        let fabric_targets = |sent: &mut mpsc::Receiver<NetCmd>| {
            std::iter::from_fn(|| sent.try_recv().ok())
                .filter_map(|command| match command {
                    NetCmd::PublishPersonalFabricTarget {
                        ws,
                        user_public_key,
                        target,
                        ..
                    } => Some((ws, user_public_key, target)),
                    NetCmd::Publish { .. } => panic!("a person record is never gossiped"),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        for (root, other_device) in [(ada, ada_phone.public()), (work, work_laptop.public())] {
            let cluster = collab_types::person_room(&root).to_string();
            net.publish_document(
                "so.tilo.tube".into(),
                cluster,
                "history",
                person_document(&root, "history", &here),
            );
            assert_eq!(
                fabric_targets(&mut sent),
                [(personal_continuity_topic_id(&root), root, other_device)],
                "each person's record goes to that person's other device alone"
            );
        }

        // The workspace identity's laptop writes its person's record here;
        // naming the device's own person's room instead, it is refused.
        let workspace_identity_room = collab_types::person_room(&work).to_string();
        let own_person_room = collab_types::person_room(&ada).to_string();
        net.register_local_record("so.tilo.tube", &workspace_identity_room, "history");
        net.register_local_record("so.tilo.tube", &own_person_room, "history");
        let from_laptop = |cluster: &str, user: &[u8; 32]| PersonalFabricPayload::Document {
            app_id: "so.tilo.tube".into(),
            record_id: crate::network::wire_record::wire_record_id(
                cluster,
                "so.tilo.tube",
                "history",
            ),
            envelope: Box::new(person_document(user, "history", &work_laptop_signing)),
        };
        net.dispatch_personal_fabric(
            personal_continuity_topic_id(&work),
            work_laptop.public(),
            crate::network::actor::MembershipEpoch::for_test(1),
            work,
            from_laptop(&own_person_room, &ada),
        );
        net.dispatch_personal_fabric(
            personal_continuity_topic_id(&ada),
            work_laptop.public(),
            crate::network::actor::MembershipEpoch::for_test(1),
            ada,
            from_laptop(&own_person_room, &ada),
        );
        assert!(
            net.inbound_documents_handle().lock().unwrap().is_empty(),
            "the workspace identity's device writes nothing into the device's own person room"
        );
        net.dispatch_personal_fabric(
            personal_continuity_topic_id(&work),
            work_laptop.public(),
            crate::network::actor::MembershipEpoch::for_test(1),
            work,
            from_laptop(&workspace_identity_room, &work),
        );
        let inbound = net.inbound_documents_handle();
        let queued = inbound.lock().unwrap();
        let [document] = queued.as_slice() else {
            panic!("the workspace identity's record reaches its local apps once");
        };
        assert_eq!(document.cluster_id, workspace_identity_room);
    }

    /// A device of the person on an operating system Tilo does not control
    /// writes their records with the foreign-device cap's role, not as the
    /// room's owner.
    #[test]
    fn a_foreign_device_writes_person_records_capped() {
        use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
        let root = ed25519_dalek::SigningKey::from_bytes(&[0xC1; 32]);
        let user = root.verifying_key().to_bytes();
        let cluster = collab_types::person_room(&user).to_string();
        let continuity = personal_continuity_topic_id(&user);
        let tablet = iroh::SecretKey::from_bytes(&[0xC2; 32]);
        let tablet_signing = ed25519_dalek::SigningKey::from_bytes(&[0xC2; 32]);
        let (phone, _sent) = personal_continuity_network_for_test(user);
        phone.register_local_record("so.tilo.tube", &cluster, "history");
        let mut cert = DeviceCert {
            user_public_key: user,
            device_public_key: *tablet.public().as_bytes(),
            device_label: "Tablet".into(),
            capabilities: DeviceCapabilities {
                foreign: true,
                ..DeviceCapabilities::member()
            },
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).expect("valid foreign device certificate");
        phone.dispatch_neighbour_msg(
            continuity,
            tablet.public(),
            Some(crate::network::actor::MembershipEpoch::for_test(1)),
            false,
            NeighbourMsg::IdentityHello {
                cert: Box::new(cert),
                revocations: Vec::new(),
            },
        );
        phone.dispatch_neighbour_msg(
            continuity,
            tablet.public(),
            None,
            true,
            NeighbourMsg::AppDocument {
                peer: 7,
                app_id: "so.tilo.tube".into(),
                cluster_id: cluster.clone(),
                record_id: crate::network::wire_record::wire_record_id(
                    &cluster,
                    "so.tilo.tube",
                    "history",
                ),
                envelope: Box::new(person_document(&user, "history", &tablet_signing)),
            },
        );
        let inbound = phone.inbound_documents_handle();
        let queued = inbound.lock().unwrap();
        let [document] = queued.as_slice() else {
            panic!("the foreign device's write reaches the broker queue once");
        };
        assert_eq!(document.role, collab_types::Role::Editor);
    }

    /// History of a person-room record kept by an earlier session is
    /// answered to nobody: not on the person room's topic, not on a shared
    /// room's. A shared room's history is still answered, on its own topic
    /// only.
    #[test]
    fn earlier_sessions_person_history_is_answered_to_nobody() {
        let user = [0xF0; 32];
        let person_room = RoomId::from(collab_types::person_room(&user));
        let shared_room = RoomId::new();
        let device = ed25519_dalek::SigningKey::from_bytes(&[0xF1; 32]);
        let sealed_op = |room: RoomId| {
            let cluster = room.to_string();
            let mut op = collab_types::SignedOp::new_unsigned_with_encryption(
                b"sealed history".to_vec(),
                collab_types::document_artifact_id("so.tilo.tube", &cluster, "history"),
                collab_types::Ed25519PublicKey::from_bytes(device.verifying_key().to_bytes()),
                collab_types::document_attestation_id("so.tilo.tube", &cluster, "history"),
                collab_types::OpClass::Splice,
                std::time::SystemTime::now(),
                1,
                collab_types::EncryptionMetadata::content_key(
                    1,
                    collab_types::document_channel_id("so.tilo.tube", &cluster, "history"),
                    collab_types::ContentRetention::Delta,
                ),
            );
            collab_types::sign_op(&mut op, &device).expect("device signs");
            op
        };
        let dir = tempfile::tempdir().expect("temp");
        let store = || {
            tilo_action_broker::collab_history::CollabHistoryStore::open(dir.path().join("history"))
                .expect("history store")
        };
        let person_op = sealed_op(person_room);
        let shared_op = sealed_op(shared_room);
        {
            let mut earlier = crate::shell::collab_broker::BrokerChannels {
                history: Some(store()),
                ..Default::default()
            };
            earlier.record_history(&person_room.to_string(), &person_op);
            earlier.record_history(&shared_room.to_string(), &shared_op);
        }

        let (net, _commands) = personal_continuity_network_for_test(user);
        assert!(!net.is_person_artifact(&person_op.artifact));
        let channels = Mutex::new(crate::shell::collab_broker::BrokerChannels {
            history: Some(store()),
            ..Default::default()
        });
        let answer = |artifact, room: RoomId| {
            crate::shell::collab_broker::scrollback_answer(
                &net,
                &channels,
                &InboundScrollbackRequest {
                    workspace_topic: room_topic_id(room),
                    requester: 0xBEEF,
                    artifact,
                    before_seq: u64::MAX,
                    max: 8,
                },
            )
        };
        assert_eq!(
            answer(shared_op.artifact, shared_room),
            vec![shared_op.clone()]
        );
        assert!(answer(shared_op.artifact, RoomId::new()).is_empty());
        assert!(answer(person_op.artifact, person_room).is_empty());
        assert!(answer(person_op.artifact, shared_room).is_empty());
    }

    /// No invite, enrollment or continuation turns the person's own room
    /// into a workspace others could share.
    #[test]
    #[serial_test::serial(process_env)]
    fn the_person_room_never_becomes_a_workspace() {
        let user = [0xEA; 32];
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_USER_ID", hex::encode(user));
        let person_room = RoomId::from(collab_types::person_room(&user));
        let mut tree = WorkspaceTree::default();
        let local = tree.append_empty_workspace("Mine");
        assert!(
            !tree.set_room(local, person_room),
            "an invite cannot name it"
        );
        assert!(!tree.join_enrolled_room(person_room), "nor an enrollment");
        assert!(
            !tree.adopt_continuation_room(person_room, "Laptop"),
            "nor a continuation"
        );
        assert!(!tree.rooms().contains(&person_room));
        let other = RoomId::new();
        assert!(
            tree.join_enrolled_room(other),
            "an ordinary room still joins"
        );
    }

    /// A shell whose room has admitted one certified peer device.
    fn network_with_admitted_peer() -> (
        ShellNetwork,
        crate::shell::state::RoomId,
        iroh::SecretKey,
        ed25519_dalek::SigningKey,
    ) {
        let mut tree = WorkspaceTree::default();
        let local = tree.append_empty_workspace("Mine");
        let our_room = tree.room_of(local).expect("a minted room");
        tree.desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == local)
            .expect("workspace")
            .set_owner(crate::shell::state::local_user_id_for_room(&our_room));
        let (net, _cmd_rx) = shell_network_for_test(tree.shared(), 0xAAAAu64);
        let root = ed25519_dalek::SigningKey::from_bytes(&[71; 32]);
        let device = iroh::SecretKey::from_bytes(&[72; 32]);
        let device_signing = ed25519_dalek::SigningKey::from_bytes(&[72; 32]);
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: *device.public().as_bytes(),
            device_label: "Peer laptop".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root).expect("device certificate");
        let _ = net
            .pending_identity_admissions
            .lock()
            .expect("pending admission")
            .insert(
                [3; 32],
                PendingInvitation {
                    id: [3; 32],
                    room: our_room,
                    name: "Peer".into(),
                    role: crate::network::invite::InviteRole::Editor,
                    token: "invite-three".into(),
                    recipient_user_public_key: None,
                    expiry_unix_seconds: u64::MAX,
                },
            );
        net.event_tx
            .try_send(NetEvent::Msg {
                ws: room_topic_id(our_room),
                peer: device.public(),
                membership_epoch: None,
                replayed: false,
                msg: NeighbourMsg::IdentityHello {
                    cert: Box::new(cert.clone()),
                    revocations: Vec::new(),
                },
            })
            .expect("queue identity proof");
        net.tick();
        net.redeem_invited_identity(room_topic_id(our_room), [3; 32], &device.public(), &cert);
        (net, our_room, device, device_signing)
    }

    /// Channel governance from an admitted device reaches the channel
    /// service's queue only on its own room's topic, and is published on
    /// that topic.
    #[test]
    fn channel_governance_is_queued_and_published_per_room() {
        let (net, room, device, device_signing) = network_with_admitted_peer();
        let channel = collab_types::ChannelId::new();
        let request =
            collab_types::MlsAccessRequestV1::signed(channel, vec![1], 1, &device_signing)
                .expect("signed request");
        let message = NeighbourMsg::MlsAccessRequest {
            room: room.to_string(),
            request: Box::new(request),
            delivery: 1,
        };
        for ws in [DEFAULT_WS_TOPIC, room_topic_id(room)] {
            net.event_tx
                .try_send(NetEvent::Msg {
                    ws,
                    peer: device.public(),
                    membership_epoch: None,
                    replayed: false,
                    msg: message.clone(),
                })
                .expect("queue governance");
            net.tick();
        }
        let queued = std::mem::take(
            &mut *net
                .inbound_channel_governance_handle()
                .lock()
                .expect("governance queue"),
        );
        assert!(
            matches!(
                queued.as_slice(),
                [InboundChannelGovernance::AccessRequest { room: queued_room, request }]
                    if *queued_room == room && request.channel == channel
            ),
            "only the copy on the room's own topic is queued: {queued:?}"
        );

        let (net, mut cmd_rx) = shell_network_for_test(WorkspaceTree::default().shared(), 7);
        assert!(net.publish_channel_governance(message));
        let Some((ws, msg)) =
            std::iter::from_fn(|| cmd_rx.try_recv().ok()).find_map(|command| match command {
                NetCmd::Publish { ws, msg } => Some((ws, msg)),
                _ => None,
            })
        else {
            panic!("governance must be published");
        };
        assert_eq!(ws, room_topic_id(room));
        assert!(matches!(msg, NeighbourMsg::MlsAccessRequest { .. }));
        assert!(!net.publish_channel_governance(NeighbourMsg::Heartbeat { peer: 1, ts: 1 }));
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn app_records_reach_only_their_room_without_leaking_paths() {
        /// The SDK guide promises a path-bearing record id never leaves the
        /// device. Check the exact bytes the actor would put on gossip and the
        /// host link, for a file record and a record derived from one.
        fn file_backed_records_never_put_a_path_on_the_wire() {
            let tree = WorkspaceTree::bootstrap_demo().shared();
            let (net, mut cmd_rx) = shell_network_for_test(tree, 0xFEED_u64);
            // Same routing as `publish_document_emits_a_publish_command`.
            let room = "cluster-7".to_string();
            let secret = "/home/alice/Secret Plans/merger-2026.odt";
            for record_id in [
                format!("file:{secret}"),
                format!("derived:annotations@file:{secret}"),
            ] {
                let envelope = collab_types::SignedOp::new_unsigned(
                    collab_types::DocumentMutation::Incremental(vec![7; 32])
                        .encode()
                        .expect("typed mutation"),
                    collab_types::document_artifact_id("so.tilo.writer", &room, &record_id),
                    collab_types::Ed25519PublicKey::from_bytes([1; 32]),
                    collab_types::document_attestation_id("so.tilo.writer", &room, &record_id),
                    collab_types::OpClass::Splice,
                    std::time::SystemTime::UNIX_EPOCH,
                    1,
                );
                net.publish_document("so.tilo.writer".into(), room.clone(), &record_id, envelope);
                let Ok(NetCmd::Publish { msg, .. }) = cmd_rx.try_recv() else {
                    panic!("publish_document must enqueue a publish");
                };
                let NeighbourMsg::AppDocument {
                    record_id: wire, ..
                } = &msg
                else {
                    panic!("expected AppDocument, got {msg:?}");
                };
                assert!(crate::network::wire_record::is_sealed(wire), "{wire}");
                let encoded = postcard::to_allocvec(&msg).expect("wire encoding");
                for needle in [secret.as_bytes(), b"/home/alice", b"file:", b"Secret"] {
                    assert!(
                        !encoded.windows(needle.len()).any(|window| window == needle),
                        "{:?} leaked onto the wire for {record_id}",
                        String::from_utf8_lossy(needle)
                    );
                }
            }
        }

        /// A peer holding the same file record in the same room still meets
        /// this device on one route: its sealed name resolves to the local id
        /// of the record a local app joined, and the envelope validates against
        /// that id. A sealed name nobody here holds is refused.
        fn a_sealed_record_name_reaches_the_local_app_holding_that_record() {
            let (net, room, device, device_signing) = network_with_admitted_peer();
            let cluster = room.to_string();
            let local = "file:/var/workspace/shared/report.odt";
            let wire =
                crate::network::wire_record::wire_record_id(&cluster, "so.tilo.notes", local);

            net.event_tx
                .try_send(app_document_event_for(
                    &cluster,
                    local,
                    &wire,
                    0xBBBB,
                    device.public(),
                    &device_signing,
                ))
                .expect("queue the publish");
            net.tick();
            assert!(
                net.inbound_documents_handle().lock().unwrap().is_empty(),
                "no local app holds the record yet, so its sealed name cannot be routed"
            );

            net.register_local_record("so.tilo.notes", &cluster, local);
            net.event_tx
                .try_send(app_document_event_for(
                    &cluster,
                    local,
                    &wire,
                    0xBBBB,
                    device.public(),
                    &device_signing,
                ))
                .expect("queue the publish");
            net.tick();
            let inbound = net.inbound_documents_handle();
            let delivered = inbound.lock().unwrap();
            assert_eq!(delivered.len(), 1);
            assert_eq!(
                delivered[0].record_id, local,
                "the local app sees its own record id"
            );
            drop(delivered);

            net.event_tx
                .try_send(app_document_event_for(
                    &cluster,
                    local,
                    local,
                    0xBBBB,
                    device.public(),
                    &device_signing,
                ))
                .expect("queue the publish");
            net.tick();
            assert_eq!(
                net.inbound_documents_handle().lock().unwrap().len(),
                1,
                "a peer that names a raw path is refused"
            );
        }

        /// A peer's app document reaches local apps only for a room this
        /// machine is in. Driven through the real receive path (`tick`),
        /// not the helper directly, with a positive control so the two
        /// refusals prove something rather than just "nothing arrives".
        fn app_document_from_a_room_we_are_not_in_never_reaches_the_broker() {
            let (net, our_room, device, device_signing) = network_with_admitted_peer();

            let theirs = crate::shell::state::RoomId::new();
            assert_ne!(theirs, our_room);

            net.event_tx
                .try_send(app_document_event(
                    "so.tilo.notes",
                    &theirs.to_string(),
                    0xBBBBu64,
                    device.public(),
                    &device_signing,
                ))
                .expect("queue the stranger's publish");
            net.tick();
            assert!(
                net.inbound_documents_handle().lock().unwrap().is_empty(),
                "a delta for a room this machine is not in must not reach the broker queue",
            );

            net.event_tx
                .try_send(app_document_event(
                    "so.tilo.notes",
                    "not-a-room",
                    0xBBBBu64,
                    device.public(),
                    &device_signing,
                ))
                .expect("queue the malformed publish");
            net.tick();
            assert!(
                net.inbound_documents_handle().lock().unwrap().is_empty(),
                "a cluster id that is not a uuid names no room, so it must be refused",
            );

            net.event_tx
                .try_send(app_document_event(
                    "so.tilo.notes",
                    &our_room.to_string(),
                    0xBBBBu64,
                    device.public(),
                    &device_signing,
                ))
                .expect("queue the member's publish");
            net.tick();
            let delivered = net.inbound_documents_handle().lock().unwrap().len();
            assert_eq!(
                delivered, 1,
                "a delta for a room this machine is in must still reach the broker queue",
            );
        }

        /// This device's own document comes back only as host-replayed
        /// history, queued as its own replayed edit; the same message as
        /// ordinary traffic under this device's session is an echo.
        fn a_replayed_own_document_reaches_the_broker_and_an_echo_does_not() {
            let (net, room, _, own_signing) = network_with_admitted_peer();
            let cluster = room.to_string();
            let own = net.local_peer_hash;
            let NetEvent::Msg { msg, .. } = app_document_event(
                "so.tilo.notes",
                &cluster,
                own,
                iroh::SecretKey::from_bytes(&[72; 32]).public(),
                &own_signing,
            ) else {
                unreachable!()
            };
            net.event_tx
                .try_send(NetEvent::Msg {
                    ws: room_topic_id(room),
                    peer: iroh::SecretKey::from_bytes(&[72; 32]).public(),
                    membership_epoch: None,
                    replayed: true,
                    msg: msg.clone(),
                })
                .expect("queue the echo");
            net.tick();
            assert!(net.inbound_documents_handle().lock().unwrap().is_empty());

            net.event_tx
                .try_send(NetEvent::OwnReplay {
                    ws: room_topic_id(room),
                    msg,
                })
                .expect("queue the replay");
            net.tick();
            let inbound = net.inbound_documents_handle();
            let delivered = inbound.lock().unwrap();
            assert_eq!(delivered.len(), 1);
            assert!(delivered[0].replayed);
            assert_eq!(delivered[0].peer, own);
            assert_eq!(delivered[0].role, collab_types::Role::Editor);
        }

        file_backed_records_never_put_a_path_on_the_wire();
        a_sealed_record_name_reaches_the_local_app_holding_that_record();
        app_document_from_a_room_we_are_not_in_never_reaches_the_broker();
        a_replayed_own_document_reaches_the_broker_and_an_echo_does_not();
    }

    #[test]
    fn a_peer_wears_the_colour_its_presence_said() {
        let (net, _cmds) = shell_network_for_test(WorkspaceTree::bootstrap_demo().shared(), 0x1234);
        assert_eq!(net.peer_colour(7), None);
        let before = net.peer_colours_epoch();
        net.note_peer_colour(7, Some("violet"));
        assert_eq!(net.peer_colour(7), Some(ui::tokens::PresenceHue::Violet));
        let heard = net.peer_colours_epoch();
        assert_ne!(heard, before, "a new colour is announced");
        net.note_peer_colour(7, Some("violet"));
        net.note_peer_colour(7, Some("chartreuse"));
        net.note_peer_colour(7, None);
        assert_eq!(
            net.peer_colour(7),
            Some(ui::tokens::PresenceHue::Violet),
            "an unknown or missing colour leaves what was heard"
        );
        assert_eq!(
            net.peer_colours_epoch(),
            heard,
            "restating a colour, or saying none, changes nothing to re-apply"
        );
        net.note_peer_colour(0x1234, Some("teal"));
        assert_eq!(net.peer_colour(0x1234), None, "never this session's own");
        assert_eq!(net.peer_colours_epoch(), heard);
        assert_eq!(
            net.peer_colours(),
            vec![(7, ui::tokens::PresenceHue::Violet)]
        );
    }

    #[test]
    fn presence_publishes_and_buffers_foreign_cursors_only() {
        fn publish_presence_emits_a_publish_command() {
            let tree = WorkspaceTree::bootstrap_demo().shared();
            let (net, mut cmd_rx) = shell_network_for_test(tree, 0xABCDu64);
            let expected_topic = net.active_ws_for_publish();
            net.publish_presence("local-cursor".into(), (100.0, 200.0));
            let cmd = cmd_rx
                .try_recv()
                .expect("publish_presence must enqueue a NetCmd");
            match cmd {
                NetCmd::Publish { ws, msg } => {
                    assert_eq!(ws, expected_topic);
                    match msg {
                        NeighbourMsg::PresenceMoved {
                            peer,
                            user,
                            key,
                            point,
                            ts: _,
                            colour,
                        } => {
                            assert_eq!(peer, 0xABCDu64);
                            assert_eq!(user, crate::shell::state::local_user_id());
                            assert!(!user.is_empty());
                            assert_eq!(key, "local-cursor");
                            assert_eq!(point, (100.0, 200.0));
                            let you = crate::shell::state::local_presence_hue();
                            assert_eq!(colour.as_deref(), Some(you.id()), "your colour travels");
                        }
                        other => panic!("expected PresenceMoved, got {other:?}"),
                    }
                }
                other => panic!("expected NetCmd::Publish, got {other:?}"),
            }
        }

        fn handle_presence_message_buffers_for_drain() {
            // Folded into `PresenceCtx` by the shell's frame drainer; every
            // field must round-trip so the overlay attributes the cursor
            // correctly.
            let tree = WorkspaceTree::bootstrap_demo().shared();
            let (net, _cmd_rx) = shell_network_for_test(tree, 0u64);
            let buf_handle = net.inbound_presence_handle();
            net.handle_presence_message(
                7u64,
                "bob".into(),
                "ws/0".into(),
                (10.0, 20.0),
                1_700_000_000,
            );
            let buf = buf_handle.lock().unwrap().clone();
            assert_eq!(buf.len(), 1);
            assert_eq!(buf[0].peer, 7u64);
            assert_eq!(buf[0].user, "bob");
            assert_eq!(buf[0].key, "ws/0");
            assert_eq!(buf[0].point, (10.0, 20.0));
            assert_eq!(buf[0].ts, 1_700_000_000);
        }

        fn a_session_does_not_take_its_own_presence_off_the_network() {
            // Gossip hands a session its own frames back; painting them
            // would draw a second cursor chasing the user's own hand.
            let tree = WorkspaceTree::bootstrap_default().shared();
            let (net, _cmd_rx) = shell_network_for_test(tree, 99u64);
            let buf_handle = net.inbound_presence_handle();
            net.handle_presence_message(
                99u64,
                "me".into(),
                "ws/0".into(),
                (10.0, 20.0),
                1_700_000_000,
            );
            assert!(
                buf_handle.lock().unwrap().is_empty(),
                "a session's own presence must not be drained back into its overlay"
            );
            net.handle_presence_message(
                7u64,
                "bob".into(),
                "ws/0".into(),
                (30.0, 40.0),
                1_700_000_001,
            );
            assert_eq!(
                buf_handle.lock().unwrap().len(),
                1,
                "another session's presence must still arrive"
            );
        }

        publish_presence_emits_a_publish_command();
        handle_presence_message_buffers_for_drain();
        a_session_does_not_take_its_own_presence_off_the_network();
    }

    fn make_envelope(seq: u64) -> collab_types::SignedOp {
        use collab_types::{ArtifactId, AttestationId, Ed25519PublicKey, OpClass, SignedOp};
        use std::time::SystemTime;
        SignedOp::new_unsigned(
            collab_types::DocumentMutation::Incremental(vec![0xAA, 0xBB])
                .encode()
                .expect("typed mutation"),
            ArtifactId::new(),
            Ed25519PublicKey::from_bytes([1u8; 32]),
            AttestationId::new(),
            OpClass::Splice,
            SystemTime::UNIX_EPOCH,
            seq,
        )
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn documents_and_scrollback_publish_to_the_active_topic_and_buffer_for_drain() {
        fn publish_document_emits_a_publish_command() {
            let tree = WorkspaceTree::bootstrap_demo().shared();
            let (net, mut cmd_rx) = shell_network_for_test(tree, 0xFEED_u64);
            let expected_topic = net.active_ws_for_publish();
            let envelope = make_envelope(7);
            net.publish_document(
                "notes".to_string(),
                "cluster-7".to_string(),
                "uuid:9f",
                envelope.clone(),
            );
            let cmd = cmd_rx
                .try_recv()
                .expect("publish_document must enqueue a NetCmd");
            match cmd {
                NetCmd::Publish { ws, msg } => {
                    assert_eq!(ws, expected_topic);
                    match msg {
                        NeighbourMsg::AppDocument {
                            peer,
                            app_id,
                            cluster_id,
                            record_id,
                            envelope: got,
                        } => {
                            assert_eq!(peer, 0xFEED_u64);
                            assert_eq!(app_id, "notes");
                            assert_eq!(cluster_id, "cluster-7");
                            assert_eq!(record_id, "uuid:9f");
                            assert_eq!(*got, envelope);
                        }
                        other => panic!("expected AppDocument, got {other:?}"),
                    }
                }
                other => panic!("expected NetCmd::Publish, got {other:?}"),
            }
        }

        fn handle_document_message_buffers_for_drain() {
            let tree = WorkspaceTree::bootstrap_demo().shared();
            let (net, _cmd_rx) = shell_network_for_test(tree, 0u64);
            let buf_handle = net.inbound_documents_handle();
            let envelope = make_envelope(13);
            net.handle_document_message(
                99u64,
                "notes".to_string(),
                "cluster-7".to_string(),
                "uuid:9f".to_string(),
                collab_types::Role::Editor,
                false,
                envelope.clone(),
            );
            let buf = buf_handle.lock().unwrap().clone();
            assert_eq!(buf.len(), 1);
            assert_eq!(buf[0].peer, 99u64);
            assert_eq!(buf[0].app_id, "notes");
            assert_eq!(buf[0].cluster_id, "cluster-7");
            assert_eq!(buf[0].record_id, "uuid:9f");
            assert_eq!(buf[0].role, collab_types::Role::Editor);
            assert_eq!(*buf[0].envelope, envelope);
        }

        fn active_ws_for_publish_returns_the_active_rooms_stable_topic() {
            let tree = WorkspaceTree::bootstrap_demo().shared();
            let expected = room_topic_id(tree.read().desktop.active_workspace().unwrap().room);
            let (net, _cmd_rx) = shell_network_for_test(tree, 0u64);
            assert_eq!(net.active_ws_for_publish(), expected);
        }

        fn publish_scrollback_request_emits_a_publish_command() {
            use collab_types::ArtifactId;
            let tree = WorkspaceTree::bootstrap_demo().shared();
            let room = tree.read().rooms()[0];
            let (net, mut cmd_rx) = shell_network_for_test(tree, 0xDEAD_u64);
            let expected_topic = room_topic_id(room);
            let artifact = ArtifactId::new();
            net.publish_scrollback_request(&room.to_string(), artifact, 100, 25);
            let cmd = cmd_rx
                .try_recv()
                .expect("publish_scrollback_request must enqueue a NetCmd");
            match cmd {
                NetCmd::Publish { ws, msg } => {
                    assert_eq!(ws, expected_topic);
                    match msg {
                        NeighbourMsg::ScrollbackRequest {
                            artifact: got_art,
                            before_seq,
                            max,
                            requester,
                        } => {
                            assert_eq!(got_art, artifact);
                            assert_eq!(before_seq, 100);
                            assert_eq!(max, 25);
                            assert_eq!(requester, 0xDEAD_u64);
                        }
                        other => panic!("expected ScrollbackRequest, got {other:?}"),
                    }
                }
                other => panic!("expected NetCmd::Publish, got {other:?}"),
            }
        }

        fn handle_scrollback_request_message_buffers_for_drain() {
            use collab_types::ArtifactId;
            let tree = WorkspaceTree::bootstrap_demo().shared();
            let (net, _cmd_rx) = shell_network_for_test(tree, 0u64);
            let handle = net.inbound_scrollback_request_handle();
            let artifact = ArtifactId::new();
            net.handle_scrollback_request_message(DEFAULT_WS_TOPIC, 7u64, artifact, 50, 10);
            let buf = handle.lock().unwrap().clone();
            assert_eq!(buf.len(), 1);
            assert_eq!(buf[0].requester, 7u64);
            assert_eq!(buf[0].artifact, artifact);
            assert_eq!(buf[0].before_seq, 50);
            assert_eq!(buf[0].max, 10);
        }

        fn handle_scrollback_response_message_buffers_for_drain() {
            use collab_types::ArtifactId;
            let tree = WorkspaceTree::bootstrap_demo().shared();
            let (net, _cmd_rx) = shell_network_for_test(tree, 0u64);
            let handle = net.inbound_scrollback_response_handle();
            let artifact = ArtifactId::new();
            let env = make_envelope(11);
            net.handle_scrollback_response_message(99u64, artifact, vec![env.clone()]);
            let buf = handle.lock().unwrap().clone();
            assert_eq!(buf.len(), 1);
            assert_eq!(buf[0].responder, 99u64);
            assert_eq!(buf[0].artifact, artifact);
            assert_eq!(buf[0].ops.len(), 1);
            assert_eq!(buf[0].ops[0], env);
        }

        publish_document_emits_a_publish_command();
        handle_document_message_buffers_for_drain();
        active_ws_for_publish_returns_the_active_rooms_stable_topic();
        publish_scrollback_request_emits_a_publish_command();
        handle_scrollback_request_message_buffers_for_drain();
        handle_scrollback_response_message_buffers_for_drain();
    }

    #[test]
    #[serial_test::serial(process_env)]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn network_announcements_and_revocations_govern_the_files_projection() {
        #[allow(
            clippy::similar_names,
            reason = "merged phases keep their original root/room binding names"
        )]
        fn network_announcement_governs_and_republishes_files_projection() {
            let runtime = tempfile::tempdir().unwrap();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_RUNTIME_DIR", runtime.path());

            let mut state = WorkspaceTree::default();
            let workspace = state.append_empty_workspace("Identity workflow");
            let room = state.room_of(workspace).unwrap();
            state.adopt_ownerless_workspaces(&crate::shell::state::local_user_id_for_room(&room));
            let tree = state.shared();
            let (network, _commands) = shell_network_for_test(tree.clone(), 0x1D);

            let device_secret = iroh::SecretKey::generate();
            let endpoint = device_secret.public();
            let root = ed25519_dalek::SigningKey::from_bytes(&[104; 32]);
            let mut certificate = tilo_user_identity::DeviceCert {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: *endpoint.as_bytes(),
                device_label: "Ada's laptop".into(),
                capabilities: tilo_user_identity::DeviceCapabilities::member(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();

            assert!(tree.write().name_peer(
                room,
                &crate::shell::state::local_user_id_for_room(&room),
                &endpoint.to_string(),
                "Ada Lovelace",
            ));
            assert!(network.handle_identity_announcement(
                &endpoint,
                &room.to_string(),
                &certificate,
                &[]
            ));
            let projection_path = runtime
                .path()
                .join("tilo/identity-projections")
                .join(room.to_string())
                .join("trusted-identities.json");
            let projected: tilo_user_identity::TrustedIdentityProjectionV2 =
                serde_json::from_slice(&std::fs::read(&projection_path).unwrap()).unwrap();
            assert_eq!(projected.humans.len(), 1);
            assert_eq!(projected.humans[0].display_name, "Ada Lovelace");
            assert_eq!(projected.humans[0].devices[0].certificate, certificate);

            let impostor = iroh::SecretKey::generate().public();
            assert!(
                !network.handle_identity_announcement(
                    &impostor,
                    &room.to_string(),
                    &certificate,
                    &[]
                ),
                "the signed certificate cannot be rebound to another transport endpoint"
            );

            let mut revocation = tilo_user_identity::RevocationEntry {
                user_public_key: root.verifying_key().to_bytes(),
                revoked_device_public_key: *endpoint.as_bytes(),
                revoked_at_ns: 2,
                reason: tilo_user_identity::RevocationReason::Retired,
                signature: [0; 64],
            };
            tilo_user_identity::sign_revocation_entry(&mut revocation, &root).unwrap();
            assert!(network.handle_identity_announcement(
                &endpoint,
                &room.to_string(),
                &certificate,
                &[revocation]
            ));
            let projected: tilo_user_identity::TrustedIdentityProjectionV2 =
                serde_json::from_slice(&std::fs::read(&projection_path).unwrap()).unwrap();
            assert!(
                projected.humans.is_empty(),
                "the root-signed revocation must immediately replace Files' live projection"
            );
            assert!(
                tree.read()
                    .trusted_identity_projection(room)
                    .humans
                    .is_empty()
            );
        }

        fn revocation_source_is_reloaded_and_cross_root_rows_fail_closed() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("revocations.bin");
            let root = ed25519_dalek::SigningKey::from_bytes(&[101; 32]);
            assert!(
                load_identity_revocations(&path, root.verifying_key().to_bytes())
                    .unwrap()
                    .is_empty()
            );
            let mut entry = tilo_user_identity::RevocationEntry {
                user_public_key: root.verifying_key().to_bytes(),
                revoked_device_public_key: [102; 32],
                revoked_at_ns: 5,
                reason: tilo_user_identity::RevocationReason::Compromised,
                signature: [0; 64],
            };
            tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
            std::fs::write(&path, postcard::to_allocvec(&vec![entry.clone()]).unwrap()).unwrap();
            assert_eq!(
                load_identity_revocations(&path, root.verifying_key().to_bytes()).unwrap(),
                vec![entry]
            );
            let other_root = ed25519_dalek::SigningKey::from_bytes(&[103; 32]);
            assert!(
                load_identity_revocations(&path, other_root.verifying_key().to_bytes()).is_err()
            );
        }

        network_announcement_governs_and_republishes_files_projection();
        revocation_source_is_reloaded_and_cross_root_rows_fail_closed();
    }

    /// The shared room both peers hold: A's person owns it, B's person is a
    /// member, and B runs a files app that publishes one action.
    fn remote_action_room(
        owner: &str,
        member: &str,
    ) -> (crate::shell::state::Workspace, crate::shell::state::RoomId) {
        let mut workspace = crate::shell::state::Workspace::new(
            crate::shell::state::WorkspaceId(51),
            "Shared room",
        );
        workspace.set_owner(owner.to_string());
        let room = workspace.room;
        workspace.grant_capability(collab_types::CapOverride::membership(
            collab_types::Subject::person(member.to_string()),
            room.to_string(),
            collab_types::Verb::Write,
        ));
        (workspace, room)
    }

    /// Carry the next remote-action frame `from` published to the peer it
    /// names, as the direct transport would.
    fn deliver_remote_action(
        from: &iroh::SecretKey,
        commands: &mut mpsc::Receiver<NetCmd>,
        to: &ShellNetwork,
        to_key: &iroh::SecretKey,
    ) {
        let (ws, target, msg) = std::iter::from_fn(|| commands.try_recv().ok())
            .find_map(|command| match command {
                NetCmd::PublishRemoteAction { ws, target, msg } => Some((ws, target, msg)),
                _ => None,
            })
            .expect("a remote-action frame was sent");
        assert_eq!(target, to_key.public(), "addressed to exactly one device");
        to.dispatch_neighbour_msg(ws, from.public(), None, false, msg);
    }
}
