//! Shell-side bridge into the peer-sync [`Actor`]: owns the
//! [`WorkspaceTreeReplica`], drives the actor's `tokio` runtime, and drains
//! `NetEvent`s into replica replies re-published as `NetCmd::Publish`s.
//! Wired in behind `--enable-peer-sync`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use iroh::address_lookup::memory::MemoryLookup;
use iroh::endpoint::presets;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::network::actor::{Actor, HostIdentity, NetCmd, NetEvent};
use crate::network::behaviour::load_or_create_keypair;
use crate::network::identity::PeerIdentityStore;
use crate::network::invite::accept_invite_into;
use crate::network::protocol::{MAX_PERSONAL_CONTINUITY_CHECKPOINT_BYTES, NeighbourMsg};
use crate::network::replica::WorkspaceTreeReplica;
#[cfg(test)]
use crate::shell::state::WorkspaceTree;
use crate::shell::state::{RoomId, WorkspaceTreeHandle};

/// Compatibility/control topic used only when a message has no room identity.
pub const DEFAULT_WS_TOPIC: u64 = 0;

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
    qos_relay::room_workspace_id(&room.to_string()).unwrap_or(DEFAULT_WS_TOPIC)
}

/// Stable private shard shared only by certified devices rooted in the same
/// person identity. The full root key is also carried in and authenticated on
/// every message; this 64-bit value is routing, never authority.
pub fn personal_continuity_topic_id(user_public_key: &[u8; 32]) -> u64 {
    qos_relay::personal_continuity_workspace_id(user_public_key)
}

/// Default on-disk path of the machine's device key, from which the
/// shell derives its Iroh identity. The FUSE mount reads the same
/// file, so one device has one identity rather than one per subsystem.
pub const DEFAULT_KEYPAIR_PATH: &str = crate::network::behaviour::DEVICE_KEY_PATH;

/// The active collaboration identity tree unless `QOS_PEER_KEY_PATH` names
/// somewhere else. Full-system sessions resolve to [`DEFAULT_KEYPAIR_PATH`];
/// host-installed sessions select an XDG-owned tree at process startup. The
/// override is what makes two sessions on one machine two
/// *peers*: the key is the shell's network identity, so two processes
/// reading one key file are one peer talking to itself.
pub fn peer_key_path() -> std::path::PathBuf {
    match std::env::var("QOS_PEER_KEY_PATH") {
        Ok(v) if !v.is_empty() => std::path::PathBuf::from(v),
        _ => crate::firstboot::collaboration_identity_paths().device_privkey,
    }
}

fn parse_host_addr(value: &str) -> Option<iroh::EndpointAddr> {
    serde_json::from_str(value).ok().or_else(|| {
        value
            .parse::<iroh::EndpointId>()
            .ok()
            .map(iroh::EndpointAddr::new)
    })
}

fn configured_host_addrs() -> Vec<iroh::EndpointAddr> {
    let mut hosts = Vec::new();
    if let Ok(value) = std::env::var("QOS_COLLAB_HOSTS") {
        match serde_json::from_str::<Vec<iroh::EndpointAddr>>(&value) {
            Ok(parsed) => hosts.extend(parsed),
            Err(error) => {
                tracing::warn!(%error, "QOS_COLLAB_HOSTS must be a JSON array of Iroh endpoint addresses");
            }
        }
    }
    if let Ok(value) = std::env::var("QOS_COLLAB_HOST") {
        if let Some(addr) = parse_host_addr(&value) {
            hosts.push(addr);
        } else {
            tracing::warn!("QOS_COLLAB_HOST is not a valid Iroh endpoint address or id");
        }
    }
    hosts.sort_by_key(|addr| addr.id.to_string());
    hosts.dedup_by_key(|addr| addr.id);
    hosts
}

const MAX_HOME_PEER_REGISTRY_BYTES: u64 = 1024 * 1024;
const MAX_HOME_PEERS: usize = 128;
const MAX_HOME_PEER_LABEL_BYTES: usize = 80;
const MAX_HOME_PEER_WORKSPACES: usize = 4096;

#[derive(Clone, Debug, Deserialize)]
struct HomePeerRegistration {
    device_node_id: String,
    relay_service_node_id: String,
    label: String,
    user_public_key: String,
    relay_workspace_ids: Vec<u64>,
}

fn home_peer_registry_path() -> PathBuf {
    std::env::var_os("QOS_HOME_PEERS_PATH")
        .filter(|path| !path.is_empty())
        .map_or_else(
            || {
                let paths = crate::firstboot::collaboration_identity_paths();
                paths
                    .user_pubkey
                    .parent()
                    .and_then(std::path::Path::parent)
                    .map_or_else(|| PathBuf::from("/var/lib/qos"), PathBuf::from)
                    .join("home-peers.json")
            },
            PathBuf::from,
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
) -> Vec<RegisteredHomePeer> {
    let Some(local_user) = local_user else {
        return Vec::new();
    };
    let path = home_peer_registry_path();
    let Ok(metadata) = std::fs::metadata(&path) else {
        return Vec::new();
    };
    if metadata.len() > MAX_HOME_PEER_REGISTRY_BYTES {
        tracing::warn!(path = %path.display(), "home-peer registry exceeds its bounded schema");
        return Vec::new();
    }
    let registrations: Vec<HomePeerRegistration> = match std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<HomePeerRegistration>>(&bytes).ok())
    {
        Some(registrations) if registrations.len() <= MAX_HOME_PEERS => registrations,
        _ => {
            tracing::warn!(path = %path.display(), "home-peer registry is invalid");
            return Vec::new();
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
        addrs.push(RegisteredHomePeer {
            addr: iroh::EndpointAddr::new(relay_id),
            expectation: crate::network::actor::HomePeerExpectation {
                user_public_key: local_user,
                device_public_key,
            },
        });
    }
    addrs.sort_by_key(|host| host.addr.id.to_string());
    addrs.dedup_by_key(|host| host.addr.id);
    addrs
}

/// How often a shell asks the rooms it is in for anything it is
/// missing. Room replication has no push side, so this interval is the
/// upper bound on how stale a guest's view of a room can be.
const SYNC_KICK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
const HOME_PEER_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

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

fn now_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn pending_invites_path(keypair_path: &Path) -> PathBuf {
    keypair_path.with_file_name("pending-invitations.json")
}

fn personal_continuity_path(keypair_path: &Path) -> PathBuf {
    keypair_path.with_file_name("personal-continuity.postcard")
}

#[derive(Serialize, Deserialize)]
struct PersistedPersonalContinuity {
    counter: u64,
    continuity: crate::shell::personal_continuity::PersonalContinuity,
}

fn load_personal_continuity(path: &Path) -> PersistedPersonalContinuity {
    let Ok(bytes) = std::fs::read(path) else {
        return PersistedPersonalContinuity {
            counter: 0,
            continuity: crate::shell::personal_continuity::PersonalContinuity::default(),
        };
    };
    postcard::from_bytes(&bytes).unwrap_or_else(|error| {
        tracing::warn!(%error, path = %path.display(), "ignoring invalid personal continuity store");
        PersistedPersonalContinuity {
            counter: 0,
            continuity: crate::shell::personal_continuity::PersonalContinuity::default(),
        }
    })
}

fn persist_personal_continuity(path: &Path, value: &PersistedPersonalContinuity) {
    let Ok(bytes) = postcard::to_allocvec(value) else {
        return;
    };
    let temporary = path.with_extension("postcard.tmp");
    if std::fs::write(&temporary, bytes)
        .and_then(|()| std::fs::rename(&temporary, path))
        .is_err()
    {
        let _ = std::fs::remove_file(&temporary);
        tracing::warn!(path = %path.display(), "personal continuity persistence failed");
    }
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
    if let Err(error) = atomic_file::write_with_mode(path, &bytes, 0o600) {
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
    /// Topics already subscribed in the actor. Avoids sending a Join command on
    /// every app delta while still allowing rooms accepted after startup.
    joined_topics: Mutex<std::collections::HashSet<u64>>,
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
    /// Always-on hosts included in newly issued invites.
    host_addrs: Mutex<Vec<iroh::EndpointAddr>>,
    /// Last bounded scan of the enrollment registry. A paired home peer may
    /// be approved after this shell started, so startup-only loading is not
    /// sufficient.
    last_home_peer_refresh: Mutex<Option<std::time::Instant>>,
    /// When [`Self::kick_periodic_sync`] last asked. `None` until the
    /// first tick, so the first frame after startup kicks a round.
    last_sync_kick: Mutex<Option<std::time::Instant>>,
    /// Private, same-person continuation state. It is intentionally separate
    /// from `replica`, whose documents are visible to room collaborators.
    personal_continuity: Mutex<crate::shell::personal_continuity::PersonalContinuity>,
    personal_continuity_user: Option<[u8; 32]>,
    personal_continuity_device: Option<crate::shell::personal_continuity::DeviceId>,
    personal_continuity_counter: Mutex<u64>,
    personal_continuity_fingerprint: Mutex<Option<[u8; 32]>>,
    personal_continuity_path: Option<PathBuf>,
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
    /// Received app-presence frames awaiting fan-out to local apps.
    inbound_app_presence: Arc<Mutex<Vec<InboundAppPresence>>>,
    /// Inbound cooperative-scrollback *requests*; the broker's drainer
    /// answers any it holds matching ops for.
    inbound_scrollback_request: Arc<Mutex<Vec<InboundScrollbackRequest>>>,
    /// Inbound cooperative-scrollback *responses*, forwarded by the
    /// broker's drainer to the local app that asked.
    inbound_scrollback_response: Arc<Mutex<Vec<InboundScrollbackResponse>>>,
}

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
    /// Boxed to mirror the wire shape and bound the queue entry size —
    /// `SignedOp` is large (32-byte pubkey + 64-byte sig + payload + UUIDs).
    pub envelope: Box<collab_types::SignedOp>,
}

/// One received cooperative-scrollback request. The broker's drainer
/// may answer with a `ScrollbackResponse` if its op log holds the range.
#[derive(Debug, Clone)]
pub struct InboundScrollbackRequest {
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
    /// Pointer position in the publisher's logical-pixel scene-space.
    pub point: (f64, f64),
    /// Wall-clock millisecond timestamp from the publisher.
    pub ts: u64,
}

/// Start mDNS LAN peer discovery when `QOS_COLLAB_LAN_DISCOVERY` requests
/// it, registering it with `endpoint`'s address lookup. Returns `None`
/// when discovery is disabled, disallowed by the env var, or the mDNS
/// service or address-lookup registry is unavailable — the caller then
/// simply skips [`spawn_lan_discovery_forwarding`].
fn init_lan_discovery(
    endpoint: &iroh::Endpoint,
) -> Option<iroh_mdns_address_lookup::MdnsAddressLookup> {
    let lan_discovery = std::env::var_os("QOS_COLLAB_LAN_DISCOVERY")
        .is_some_and(|value| value != "0" && !value.is_empty());
    if !lan_discovery {
        return None;
    }
    let mdns = match iroh_mdns_address_lookup::MdnsAddressLookup::builder()
        .service_name("qos-collab-v1")
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
        use futures_lite::StreamExt;
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
        "peer-sync: privacy-sensitive LAN discovery enabled; advertising on _qos-collab-v1._udp.local"
    );
}

impl ShellNetwork {
    /// Build a tokio runtime, spawn the iroh [`Actor`], wrap the
    /// shared [`WorkspaceTree`] in a [`WorkspaceTreeReplica`], and
    /// return a handle the shell can use to drive sync. The keypair is
    /// loaded if it already exists, otherwise a fresh ed25519 key is
    /// generated and persisted.
    ///
    /// # Errors
    ///
    /// Returns an error if the tokio runtime fails to build, the device
    /// keypair can't be loaded or generated at `keypair_path`, or the
    /// Iroh endpoint fails to bind.
    pub fn spawn(
        tree: WorkspaceTreeHandle,
        keypair_path: &std::path::Path,
    ) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let _enter = runtime.enter();

        let keypair = load_or_create_keypair(keypair_path)?;
        let local_peer_id = keypair.public();
        // Must use the same hash function `tick`'s receive path uses,
        // so a peer's own publishes hash to the same `peer` u64 on
        // both sides of the wire.
        let local_peer_hash = crate::network::behaviour::peer_liveness_handle(&keypair.public());
        let lookup = MemoryLookup::new();
        let endpoint = runtime
            .block_on(
                iroh::Endpoint::builder(presets::N0)
                    .secret_key(keypair.clone())
                    .address_lookup(lookup.clone())
                    .bind(),
            )
            .map_err(|e| std::io::Error::other(format!("iroh bind: {e}")))?;
        let mdns = init_lan_discovery(&endpoint);
        tracing::info!(
            local_peer_id = %local_peer_id,
            addr = ?endpoint.addr(),
            "peer-sync: Iroh endpoint started"
        );
        let listen_addrs: Arc<Mutex<Vec<iroh::EndpointAddr>>> = Arc::new(Mutex::new(Vec::new()));
        let host_identity = host_identity_for_key(&keypair, keypair_path);
        let personal_continuity_user = host_identity
            .as_ref()
            .map(|identity| identity.device_cert.user_public_key);
        let personal_continuity_device = host_identity.as_ref().and_then(|identity| {
            crate::shell::personal_continuity::DeviceId::new(hex::encode(
                identity.device_cert.device_public_key,
            ))
            .ok()
        });
        let (cmd_tx, _join) = Actor::spawn_reporting_addrs(
            endpoint,
            lookup,
            host_identity.clone(),
            Arc::clone(&listen_addrs),
        );
        if let Some(mdns) = mdns {
            spawn_lan_discovery_forwarding(&runtime, mdns, &cmd_tx);
        }

        let mut relay_workspace_ids: std::collections::BTreeSet<_> =
            tree.read().rooms().into_iter().map(room_topic_id).collect();
        if let Some(user) = personal_continuity_user {
            relay_workspace_ids.insert(personal_continuity_topic_id(&user));
        }
        let home_peers = registered_home_peer_addrs(personal_continuity_user, &relay_workspace_ids);
        let mut host_addrs = configured_host_addrs();
        host_addrs.extend(home_peers.iter().map(|host| host.addr.clone()));
        host_addrs.sort_by_key(|addr| addr.id.to_string());
        host_addrs.dedup_by_key(|addr| addr.id);
        for addr in host_addrs.iter().cloned() {
            let _ = cmd_tx.try_send(NetCmd::ConnectHost {
                addr,
                expectation: None,
            });
        }
        for host in home_peers {
            let _ = cmd_tx.try_send(NetCmd::ConnectHost {
                addr: host.addr,
                expectation: Some(host.expectation),
            });
        }

        // Peer id `1` is the replica-side identity for
        // `WorkspaceTreeReplica::handle`'s `_sender` arg; the actor's
        // Iroh `EndpointId` is transport-level and hashed separately.
        let replica = Arc::new(Mutex::new(WorkspaceTreeReplica::new(1, tree)));

        // Capacity matches `Actor::spawn`'s command-channel default.
        let (event_tx, event_rx) = mpsc::channel(64);

        let pending_invitations_path = pending_invites_path(keypair_path);
        let personal_continuity_path = personal_continuity_path(keypair_path);
        let persisted_continuity = load_personal_continuity(&personal_continuity_path);
        let now = now_unix_seconds();
        let pending_identity_admissions: std::collections::HashMap<_, _> =
            load_pending_invitations(&pending_invitations_path)
                .into_iter()
                .filter(|invite| !invite.is_expired(now))
                .map(|invite| (invite.id, invite))
                .collect();
        persist_pending_invitations(&pending_invitations_path, &pending_identity_admissions);

        Ok(Self {
            cmd_tx,
            replica,
            event_tx,
            event_rx: Mutex::new(event_rx),
            joined_topics: Mutex::new(std::collections::HashSet::new()),
            runtime: Some(runtime),
            local_peer_hash,
            keypair,
            host_identity,
            local_peer_id,
            listen_addrs,
            host_addrs: Mutex::new(host_addrs),
            last_home_peer_refresh: Mutex::new(None),
            last_sync_kick: Mutex::new(None),
            personal_continuity: Mutex::new(persisted_continuity.continuity),
            personal_continuity_user,
            personal_continuity_device,
            personal_continuity_counter: Mutex::new(persisted_continuity.counter),
            personal_continuity_fingerprint: Mutex::new(None),
            personal_continuity_path: Some(personal_continuity_path),
            peer_identities: PeerIdentityStore::new(),
            require_peer_identity: true,
            pending_identity_admissions: Mutex::new(pending_identity_admissions),
            pending_invitations_path: Some(pending_invitations_path),
            pending_redemptions: Mutex::new(std::collections::HashMap::new()),
            inbound_presence: Arc::new(Mutex::new(Vec::new())),
            inbound_documents: Arc::new(Mutex::new(Vec::new())),
            inbound_app_presence: Arc::new(Mutex::new(Vec::new())),
            inbound_scrollback_request: Arc::new(Mutex::new(Vec::new())),
            inbound_scrollback_response: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Pre-computed hash of the local iroh endpoint id. Stamped on every
    /// outbound presence move so peers can attribute the cursor to its
    /// publisher.
    pub fn local_peer_id_hash(&self) -> u64 {
        self.local_peer_hash
    }

    /// What the peers in `cluster_id` are called, keyed by the peer
    /// handle presence frames carry. `cluster_id` is a room id, so this
    /// is the room document's answer and nobody else's — names are
    /// written there by the room's authority and refused from anyone
    /// else. Empty for a cluster that is not a room this machine is in,
    /// or a room where nobody has been named; both leave every peer
    /// labelled with its own handle.
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

        replica.tree.read().room_peer_labels(room)
    }

    /// Workspace topic for session traffic associated with the active room.
    pub fn active_ws_for_publish(&self) -> u64 {
        let Ok(replica) = self.replica.lock() else {
            return DEFAULT_WS_TOPIC;
        };
        replica
            .tree
            .read()
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
        let room = match msg {
            NeighbourMsg::IdentityAnnouncement { room, .. }
            | NeighbourMsg::IdentityAnnouncementV2 { room, .. }
            | NeighbourMsg::SyncReq { room, .. }
            | NeighbourMsg::SyncResp { room, .. } => Some(room.as_str()),
            NeighbourMsg::AppDocument { cluster_id, .. }
            | NeighbourMsg::AppPresenceV2 { cluster_id, .. } => Some(cluster_id.as_str()),
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
        self.join_topic(ws) && self.cmd_tx.try_send(NetCmd::Publish { ws, msg }).is_ok()
    }

    fn publish_shell_checkpoint(&self, msg: NeighbourMsg) -> bool {
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
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let msg = NeighbourMsg::PresenceMoved {
            peer: self.local_peer_id_hash(),
            user: crate::shell::state::local_user_id(),
            key,
            point,
            ts,
        };
        let _ = self.publish_message(msg);
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
    }

    /// Is `cluster_id` a room this machine is in? An app's cluster is
    /// the room's uuid, so this is the same question `apply_room_changes`
    /// asks before storing anything. A `cluster_id` that fails to parse
    /// as a uuid is not a room, and refusing it is fail-closed.
    fn is_in_room(&self, cluster_id: &str) -> bool {
        let Some(room) = crate::shell::state::RoomId::parse(cluster_id) else {
            tracing::info!(
                target: "qos_collab_probe",
                cluster = %cluster_id,
                "PROBE is_in_room=false (not a uuid)",
            );
            return false;
        };
        if room.is_nil() {
            tracing::info!(
                target: "qos_collab_probe",
                cluster = %cluster_id,
                "PROBE is_in_room=false (nil)",
            );
            return false;
        }
        let Ok(replica) = self.replica.lock() else {
            return false;
        };
        let known = replica.tree.read().rooms();
        let in_room = known.contains(&room);
        drop(replica);
        tracing::info!(
            target: "qos_collab_probe",
            cluster = %cluster_id,
            in_room,
            known = ?known.iter().map(std::string::ToString::to_string).collect::<Vec<_>>(),
            "PROBE is_in_room",
        );
        in_room
    }

    /// Shared handle to inbound signed document envelopes.
    pub fn inbound_documents_handle(&self) -> Arc<Mutex<Vec<InboundDocument>>> {
        Arc::clone(&self.inbound_documents)
    }

    /// Publish one validated signed document mutation.
    pub fn publish_document(
        &self,
        app_id: String,
        cluster_id: String,
        record_id: String,
        envelope: collab_types::SignedOp,
    ) {
        let msg = NeighbourMsg::AppDocument {
            peer: self.local_peer_id_hash(),
            app_id,
            cluster_id,
            record_id,
            envelope: Box::new(envelope),
        };
        let _ = self.publish_message(msg);
    }

    /// Queue a received signed document for the broker's drainer.
    pub fn handle_document_message(
        &self,
        peer: u64,
        app_id: String,
        cluster_id: String,
        record_id: String,
        role: collab_types::Role,
        envelope: collab_types::SignedOp,
    ) {
        if let Ok(mut buf) = self.inbound_documents.lock() {
            buf.push(InboundDocument {
                peer,
                app_id,
                cluster_id,
                record_id,
                role,
                envelope: Box::new(envelope),
            });
        }
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
        let msg = NeighbourMsg::AppPresenceV2 {
            peer: self.local_peer_id_hash(),
            app_id,
            cluster_id,
            presence,
        };
        let _ = self.publish_message(msg);
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
    }

    /// Shared handle to the cooperative-scrollback request buffer,
    /// answered by the broker's drainer on each tick.
    pub fn inbound_scrollback_request_handle(&self) -> Arc<Mutex<Vec<InboundScrollbackRequest>>> {
        Arc::clone(&self.inbound_scrollback_request)
    }

    /// Shared handle to the cooperative-scrollback response buffer.
    pub fn inbound_scrollback_response_handle(&self) -> Arc<Mutex<Vec<InboundScrollbackResponse>>> {
        Arc::clone(&self.inbound_scrollback_response)
    }

    /// Publish a `NeighbourMsg::ScrollbackRequest`. Called when a local
    /// app's `FetchScrollback` returned `exhausted=true` with an empty
    /// batch — the broker has nothing more in-memory, but the app still
    /// wants the range.
    pub fn publish_scrollback_request(
        &self,
        artifact: collab_types::ArtifactId,
        before_seq: u64,
        max: u16,
    ) {
        let msg = NeighbourMsg::ScrollbackRequest {
            artifact,
            before_seq,
            max,
            requester: self.local_peer_id_hash(),
        };
        let _ = self.publish_message(msg);
    }

    /// Publish a `NeighbourMsg::ScrollbackResponse` for an artifact
    /// whose log has matching ops.
    pub fn publish_scrollback_response(
        &self,
        artifact: collab_types::ArtifactId,
        ops: Vec<Box<collab_types::SignedOp>>,
    ) {
        let msg = NeighbourMsg::ScrollbackResponse {
            artifact,
            ops,
            responder: self.local_peer_id_hash(),
        };
        let _ = self.publish_message(msg);
    }

    /// Queue a received `ScrollbackRequest` for the broker's drainer.
    pub fn handle_scrollback_request_message(
        &self,
        requester: u64,
        artifact: collab_types::ArtifactId,
        before_seq: u64,
        max: u16,
    ) {
        if let Ok(mut buf) = self.inbound_scrollback_request.lock() {
            buf.push(InboundScrollbackRequest {
                requester,
                artifact,
                before_seq,
                max,
            });
        }
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
    }

    /// Subscribe the actor to every durable room currently in the tree, plus
    /// the compatibility/control topic. Rooms accepted later are joined by
    /// [`Self::publish_message`].
    pub fn join_default_workspace(&self) -> bool {
        let mut topics = vec![DEFAULT_WS_TOPIC];
        if let Ok(replica) = self.replica.lock() {
            topics.extend(replica.tree.read().rooms().into_iter().map(room_topic_id));
        }
        if let Some(user_public_key) = self.personal_continuity_user {
            topics.push(personal_continuity_topic_id(&user_public_key));
        }
        topics.into_iter().all(|topic| self.join_topic(topic))
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
        let mut relay_workspace_ids = self
            .replica
            .lock()
            .ok()
            .map(|replica| {
                replica
                    .tree
                    .read()
                    .rooms()
                    .into_iter()
                    .map(room_topic_id)
                    .collect::<std::collections::BTreeSet<_>>()
            })
            .unwrap_or_default();
        if let Some(user) = self.personal_continuity_user {
            relay_workspace_ids.insert(personal_continuity_topic_id(&user));
        }
        let mut changed = false;
        for host in registered_home_peer_addrs(self.personal_continuity_user, &relay_workspace_ids)
        {
            let addr = host.addr;
            let known = self
                .host_addrs
                .lock()
                .is_ok_and(|hosts| hosts.iter().any(|known| known.id == addr.id));
            if !known {
                if let Ok(mut hosts) = self.host_addrs.lock() {
                    hosts.push(addr.clone());
                }
                changed |= self
                    .cmd_tx
                    .try_send(NetCmd::ConnectHost {
                        addr,
                        expectation: Some(host.expectation),
                    })
                    .is_ok();
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

    /// Invitations this authority can still resend or cancel. Expired
    /// credentials are removed from both memory and the private on-disk
    /// inventory before rows are returned to the UI.
    #[must_use]
    pub fn pending_invitations(&self, room: RoomId) -> Vec<PendingInvitation> {
        let Ok(mut pending) = self.pending_identity_admissions.lock() else {
            return Vec::new();
        };
        let before = pending.len();
        let now = now_unix_seconds();
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
            .filter(|invite| !invite.is_expired(now_unix_seconds()))
            .map(|invite| invite.token.clone())
            .ok_or_else(|| "invitation is no longer pending".to_string())
    }

    /// Cancel one exact bearer credential. A later redemption carrying its
    /// hash has no pending authority grant and is refused. Existing members
    /// are unaffected; removing them is a separate governance action.
    pub fn cancel_invitation(&self, id: &[u8; 32]) -> bool {
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

    /// Export the selected person identity as a non-secret card another qOS
    /// user can paste into the recipient-bound invite composer.
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
            "qos-identity:{}",
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

    /// [`Self::issue_invite`], for a person this machine will name.
    /// The name remains authority-side and is correlated with the exact
    /// signed invite when it is redeemed.
    ///
    /// # Errors
    ///
    /// Same conditions as [`Self::issue_invite`].
    pub fn issue_invite_named(
        &self,
        local: crate::shell::state::WorkspaceId,
        expiry_seconds_from_now: u64,
        invitee: &str,
    ) -> Result<String, String> {
        self.issue_invite_named_with_role(
            local,
            expiry_seconds_from_now,
            invitee,
            crate::network::invite::InviteRole::Editor,
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

    fn issue_invite_for(
        &self,
        local: crate::shell::state::WorkspaceId,
        expiry_seconds_from_now: u64,
        invitee: Option<&str>,
        role: crate::network::invite::InviteRole,
        recipient_user_public_key: Option<[u8; 32]>,
    ) -> Result<String, String> {
        let tree_handle = {
            let replica = self
                .replica
                .lock()
                .map_err(|_| "peer-sync replica lock poisoned".to_string())?;
            replica.tree.clone()
        };
        let tree = tree_handle.read();
        let Some(ws) = tree.workspaces.iter().find(|w| w.id == local) else {
            drop(tree);
            return Err(format!("no workspace {} to invite into", local.raw()));
        };
        let actor = crate::shell::state::local_user_id_for_room(&ws.room);
        if !tree.governs_room(ws.room)
            || !ws.allows(
                &ws.actor_for_person(&actor),
                &crate::shell::state::ResourceRef::Workspace,
                crate::shell::state::Verb::Invite,
            )
        {
            drop(tree);
            return Err("only a governing workspace administrator can create invitations".into());
        }
        let ws = ws.clone();
        drop(tree);
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
            let signed_invitee = recipient_user_public_key.and(invitee);
            let token = crate::network::invite::make_invite_token_with_recipient(
                &self.keypair,
                &ws,
                &addr,
                &hosts,
                &pins,
                Some(device_cert),
                role,
                recipient_user_public_key,
                signed_invitee,
                expiry_seconds_from_now,
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
            workspace.room
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
                .workspaces
                .iter()
                .find(|workspace| workspace.id == local)
                .ok_or_else(|| format!("no workspace {}", local.raw()))?;
            let actor = crate::shell::state::local_user_id_for_room(&workspace.room);
            if !tree.governs_room(workspace.room) || !workspace.may_administer(&actor) {
                return Err("only the governing workspace administrator can revoke invites".into());
            }
            workspace.room
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
        if !pending.get(&id).is_some_and(|invite| invite.room == room) {
            return Ok(false);
        }
        pending.remove(&id);
        if let Some(path) = self.pending_invitations_path.as_deref() {
            persist_pending_invitations(path, &pending);
        }
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
        device_cert: &qos_user_identity::DeviceCert,
        revocations: &[qos_user_identity::RevocationEntry],
    ) -> bool {
        let Some(room) = RoomId::parse(room) else {
            return false;
        };
        if device_cert.device_public_key != *peer.as_bytes()
            || qos_user_identity::verify_device_cert(device_cert).is_err()
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
    }

    fn local_continuity_snapshot_bytes(&self) -> Option<Vec<u8>> {
        let device = self.personal_continuity_device.clone()?;
        let tree_handle = self.replica.lock().ok()?.tree.clone();

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
        let fingerprint = *blake3::hash(&neutral_bytes).as_bytes();

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
            return Some(bytes);
        }
        let mut counter = self.personal_continuity_counter.lock().ok()?;
        *counter = counter.saturating_add(1);
        let report =
            crate::shell::personal_continuity::DeviceContinuationSnapshot::capture_with_registry(
                &tree_handle.read(),
                device.clone(),
                *counter,
                &reports,
            );
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
        self.persist_personal_continuity();
        Some(bytes)
    }

    fn persist_personal_continuity(&self) {
        let Some(path) = self.personal_continuity_path.as_deref() else {
            return;
        };
        let (Ok(counter), Ok(continuity)) = (
            self.personal_continuity_counter.lock(),
            self.personal_continuity.lock(),
        ) else {
            return;
        };
        persist_personal_continuity(
            path,
            &PersistedPersonalContinuity {
                counter: *counter,
                continuity: continuity.clone(),
            },
        );
    }

    fn publish_personal_continuity_request(&self) {
        let Some(user_public_key) = self.personal_continuity_user else {
            return;
        };
        let _ = self.publish_message(NeighbourMsg::PersonalContinuityRequest {
            user_public_key,
            requester: self.local_peer_id_hash(),
        });
    }

    fn publish_personal_continuity_if_changed(&self) {
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
        if self.join_topic(ws) {
            let _ = self.cmd_tx.try_send(NetCmd::PublishCheckpoint { ws, msg });
        }
    }

    /// [`Self::publish_sync_requests`], rate-limited to
    /// [`SYNC_KICK_INTERVAL`]. Called from every frame tick, which runs
    /// far more often than a room changes.
    fn kick_periodic_sync(&self) {
        let Ok(mut last) = self.last_sync_kick.lock() else {
            return;
        };
        let now = std::time::Instant::now();
        if let Some(prev) = *last
            && now.duration_since(prev) < SYNC_KICK_INTERVAL
        {
            return;
        }
        *last = Some(now);
        drop(last);
        self.publish_sync_requests();
    }

    /// Drain pending [`NetEvent`]s from the actor, forward `Msg`
    /// variants into the replica's `handle`, and publish each resulting
    /// outbound message via `cmd_tx`. Non-blocking — never stalls the
    /// render loop even on a starved channel.
    pub fn tick(&self) {
        // Room replication is pull-only — a host answers `SyncReq` and
        // never announces — so nothing else brings a stale guest's view
        // of a capability change up to date.
        self.kick_periodic_sync();
        self.refresh_home_peer_hosts_periodically();
        // Hold the lock for the whole drain so a concurrent frame-tick
        // never sees half-merged events. A poisoned mutex (a prior
        // holder panicked mid-drain) just skips this tick rather than
        // panicking this one too.
        let Ok(mut event_rx) = self.event_rx.lock() else {
            return;
        };
        while let Ok(event) = event_rx.try_recv() {
            self.dispatch_net_event(event);
        }
    }

    /// Route one drained [`NetEvent`] to its handler. Split out of
    /// [`Self::tick`], which owns only the drain loop and the
    /// `event_rx` lock.
    fn dispatch_net_event(&self, event: NetEvent) {
        match event {
            NetEvent::HostReady { ws, peer } => self.handle_host_ready(ws, peer),
            NetEvent::PeerJoined { .. } => self.publish_sync_requests(),
            // Without this the roster only ever grows: nothing else
            // tells it a session went quiet.
            NetEvent::PeerLeft { peer, .. } => {
                let handle = crate::network::behaviour::peer_liveness_handle(&peer);
                self.peer_identities
                    .remove(&crate::shell::state::PeerOrigin::peer(&peer));
                if let Ok(replica) = self.replica.lock() {
                    replica.tree.write().retire_session(handle);
                }
            }
            NetEvent::Msg { ws, peer, msg } => self.dispatch_neighbour_msg(ws, peer, msg),
        }
    }

    /// Seed a durable host's checkpoint once it finishes subscribing to
    /// `ws` — see [`NetEvent::HostReady`]. Unlike `SyncReq`, this does
    /// not need another device online to answer it.
    fn handle_host_ready(&self, ws: u64, peer: iroh::EndpointId) {
        if self
            .personal_continuity_user
            .is_some_and(|user| personal_continuity_topic_id(&user) == ws)
        {
            // PublishCheckpoint gives a newly enrolled/offline device a
            // retained baseline even when no interactive device is online.
            if let (Some(user_public_key), Some(snapshot)) = (
                self.personal_continuity_user,
                self.local_continuity_snapshot_bytes(),
            ) {
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
    fn dispatch_neighbour_msg(&self, ws: u64, peer: iroh::EndpointId, msg: NeighbourMsg) {
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
        let origin = crate::shell::state::PeerOrigin::peer(&peer);
        if let NeighbourMsg::IdentityHello { cert, revocations } = &msg {
            self.accept_identity_hello(ws, &origin, &peer, cert, revocations);
            return;
        }
        if self.require_peer_identity && self.peer_identities.get(&origin).is_none() {
            tracing::warn!(peer = %peer, "dropping collaboration message from an unauthenticated device");
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
        if self.dispatch_personal_continuity(ws, &origin, &msg) {
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
        if self.require_peer_identity && !matches!(&msg, NeighbourMsg::IdentityHello { .. }) {
            let required = Self::required_room_verb(&msg);
            let pinned_authority_may_bootstrap = matches!(&msg, NeighbourMsg::SyncResp { .. });
            if !self.peer_room_allows(&origin, ws, required, pinned_authority_may_bootstrap) {
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
            NeighbourMsg::IdentityAnnouncement { room, device_cert } => {
                self.handle_identity_announcement(&peer, &room, &device_cert, &[]);
            }
            NeighbourMsg::IdentityAnnouncementV2 {
                room,
                device_cert,
                revocations,
            } => {
                self.handle_identity_announcement(&peer, &room, &device_cert, &revocations);
            }
            // Ephemeral overlay state, not CRDT state — skips the replica entirely.
            NeighbourMsg::PresenceMoved {
                peer: peer_hash,
                user,
                key,
                point,
                ts,
            } => {
                self.handle_presence_message(peer_hash, user, key, point, ts);
            }
            NeighbourMsg::AppDocument {
                peer: peer_hash,
                app_id,
                cluster_id,
                record_id,
                envelope,
            } => {
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
                        peer_hash, app_id, cluster_id, record_id, role, *envelope,
                    );
                }
            }
            NeighbourMsg::AppPresenceV2 {
                peer: peer_hash,
                app_id,
                cluster_id,
                presence,
            } => self.dispatch_app_presence_v2(peer_hash, app_id, cluster_id, presence),
            // Cooperative scrollback: skip self-echoes.
            NeighbourMsg::ScrollbackRequest {
                artifact,
                before_seq,
                max,
                requester,
            } => self.dispatch_scrollback_request(requester, artifact, before_seq, max),
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
            other => self.dispatch_to_replica(peer, &other),
        }
    }

    /// Handle the private same-person lane before room authorization. Returns
    /// `true` for both accepted and rejected personal messages so neither can
    /// fall through into the collaboration-room replica.
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
            if self.join_topic(ws) {
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
        let Ok(incoming) = postcard::from_bytes::<
            crate::shell::personal_continuity::PersonalContinuity,
        >(snapshot_bytes) else {
            tracing::warn!("dropping invalid continuity checkpoint");
            return true;
        };
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
        if changed {
            self.persist_personal_continuity();
            let Some(merged) = merged else {
                return true;
            };
            // Reconcile from the accepted merged set, not from the wire
            // payload. A checkpoint can contain stale entries alongside one
            // newer entry; arrival order must never choose an app state.
            let mut winners = std::collections::BTreeMap::new();
            let mut rooms = Vec::new();
            for snapshot in merged.snapshots().values() {
                for workspace in &snapshot.workspaces {
                    rooms.push((workspace.room, snapshot.device.as_str().to_string()));
                    for app in &workspace.apps {
                        let key = (workspace.room, app.id);
                        let replace = winners.get(&key).is_none_or(
                            |current: &crate::shell::personal_continuity::AppInstanceState| {
                                (app.stamp.counter, app.stamp.device.as_str())
                                    > (current.stamp.counter, current.stamp.device.as_str())
                            },
                        );
                        if replace {
                            winners.insert(key, app.clone());
                        }
                    }
                }
            }
            let materializations: Vec<_> = winners
                .into_iter()
                .map(|((room, _), app)| {
                    let source = app.stamp.device.as_str().to_string();
                    (
                        source,
                        crate::shell::personal_continuity::WorkspaceContinuation {
                            room,
                            apps: vec![app],
                        },
                    )
                })
                .collect();
            let desired_tabs = materializations
                .iter()
                .flat_map(|(_, workspace)| workspace.apps.iter().map(|app| app.id.0))
                .collect::<std::collections::BTreeSet<_>>();
            let tree_handle = self.replica.lock().ok().map(|replica| replica.tree.clone());
            let adopted = tree_handle.is_some_and(|tree| {
                let mut tree = tree.write();
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
                tree.reconcile_continuation_tabs(&desired_tabs) || adopted_rooms || materialized
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
            | NeighbourMsg::AppDocument { peer, .. }
            | NeighbourMsg::AppPresenceV2 { peer, .. } => Some(*peer),
            NeighbourMsg::SurfaceStreamStart { source_peer, .. }
            | NeighbourMsg::SurfaceStreamEnd { source_peer, .. }
            | NeighbourMsg::SurfaceFrame { source_peer, .. } => Some(*source_peer),
            NeighbourMsg::ScrollbackRequest { requester, .. } => Some(*requester),
            NeighbourMsg::ScrollbackResponse { responder, .. } => Some(*responder),
            NeighbourMsg::PersonalContinuityRequest { requester, .. } => Some(*requester),
            NeighbourMsg::PersonalContinuityCheckpoint { publisher, .. } => Some(*publisher),
            NeighbourMsg::SyncReq { .. }
            | NeighbourMsg::SyncResp { .. }
            | NeighbourMsg::PermissionGrant { .. }
            | NeighbourMsg::IdentityHello { .. }
            | NeighbourMsg::IdentityAnnouncement { .. }
            | NeighbourMsg::IdentityAnnouncementV2 { .. } => None,
        }
    }

    /// Workspace capability required before variant-specific validation.
    /// Publishing media, injecting input, granting control, and changing room
    /// state are active collaboration actions and never ride on read access.
    fn required_room_verb(msg: &NeighbourMsg) -> crate::shell::state::Verb {
        match msg {
            NeighbourMsg::SyncResp { .. }
            | NeighbourMsg::SurfaceFrame { .. }
            | NeighbourMsg::SurfaceStreamStart { .. }
            | NeighbourMsg::SurfaceStreamEnd { .. }
            | NeighbourMsg::RemoteInput { .. }
            | NeighbourMsg::PermissionGrant { .. } => crate::shell::state::Verb::Write,
            NeighbourMsg::PersonalContinuityRequest { .. }
            | NeighbourMsg::PersonalContinuityCheckpoint { .. } => crate::shell::state::Verb::Read,
            _ => crate::shell::state::Verb::Read,
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
    /// is deliberately insufficient: any valid qOS identity can possess a
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
        // bootstrap the signed-invite room. The replica still checks that it
        // is the pinned governance writer before accepting the candidate.
        let pinned_inviter = pinned_authority_may_bootstrap
            && tree.room_authority(room)
                == crate::shell::state::RoomAuthority::Peer(origin.clone());
        let same_person_continuation = pinned_authority_may_bootstrap
            && tree.room_authority(room) == crate::shell::state::RoomAuthority::Unpinned
            && self
                .personal_continuity_user
                .is_some_and(|local_user| identity.is_same_user(&local_user));
        let allowed = if pinned_inviter || same_person_continuation {
            true
        } else {
            let person = collab_types::caps::Principal::person_from_key(
                collab_types::crypto::Ed25519PublicKey::from_bytes(identity.user_pubkey),
            )
            .id()
            .to_string();
            tree.workspaces
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
    ) {
        if peer_hash != self.local_peer_hash && self.is_in_room(&cluster_id) {
            self.handle_typed_app_presence_message(peer_hash, app_id, cluster_id, presence);
        }
    }

    /// `ScrollbackRequest`: skip self-echoes, otherwise queue for the
    /// broker's drainer. Split out of [`Self::dispatch_neighbour_msg`].
    fn dispatch_scrollback_request(
        &self,
        requester: u64,
        artifact: collab_types::ArtifactId,
        before_seq: u64,
        max: u16,
    ) {
        if requester != self.local_peer_hash {
            self.handle_scrollback_request_message(requester, artifact, before_seq, max);
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
        cert: &qos_user_identity::DeviceCert,
        revocations: &[qos_user_identity::RevocationEntry],
    ) {
        if cert.device_public_key != *peer.as_bytes() || revocations.len() > 4096 {
            tracing::warn!(peer = %peer, "rejecting invalid peer identity handshake");
            self.peer_identities.remove(origin);
            return;
        }
        let mut list = qos_user_identity::RevocationList::new();
        for entry in revocations {
            if entry.user_public_key != cert.user_public_key || list.insert(entry.clone()).is_err()
            {
                tracing::warn!(peer = %peer, "rejecting peer identity with invalid revocation set");
                self.peer_identities.remove(origin);
                return;
            }
        }
        self.peer_identities
            .set_revocations(cert.user_public_key, &list);
        let was_known = self.peer_identities.get(origin).is_some();
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
            .unwrap_or(u64::MAX);
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
            self.handle_identity_announcement(peer, &room.to_string(), cert, revocations);
        }
        // The initial SyncReq can race ahead of the identity hello and is
        // intentionally dropped. Re-kick anti-entropy after trust is
        // established so joining never waits for another topology event.
        if !was_known {
            self.publish_sync_requests();
        }
    }

    fn redeem_invited_identity(
        &self,
        workspace_topic: u64,
        invite_id: [u8; 32],
        peer: &iroh::EndpointId,
        cert: &qos_user_identity::DeviceCert,
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
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
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
        tree.flush_to_disk();
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
            .workspaces
            .iter()
            .find(|workspace| workspace.room == room)?;
        let role = workspace.role_of(&person)?;
        let artifact = collab_types::document_artifact_id(app_id, cluster_id, record_id);
        if envelope.artifact != artifact
            || envelope.attestation_id
                != collab_types::document_attestation_id(app_id, cluster_id, record_id)
            || envelope.encryption.is_some()
            || collab_types::DocumentMutation::decode(&envelope.payload).is_err()
        {
            return None;
        }
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
) -> Result<qos_user_identity::DeviceCert, String> {
    let profile = crate::identity_registry::selected_identity_profile(&room.to_string())
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "no identity is selected for this workspace".to_string())?;
    let paths = crate::firstboot::collaboration_identity_paths();
    let cert = qos_user_identity::persist::read_device_cert(
        &paths.device_cert_for_user(&profile.user_public_key),
    )
    .map_err(|error| error.to_string())?;
    qos_user_identity::verify_device_cert(&cert).map_err(|error| error.to_string())?;
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
    let paths = qos_user_identity::IdentityPaths::under(identity_root);
    let user_public_key = qos_user_identity::persist::read_public_key(&paths.user_pubkey).ok()?;
    let device_cert = qos_user_identity::persist::read_device_cert(&paths.device_cert).ok()?;
    if qos_user_identity::verify_device_cert(&device_cert).is_err()
        || device_cert.user_public_key != user_public_key
        || device_cert.device_public_key != *keypair.public().as_bytes()
    {
        tracing::warn!("ignoring invalid collaboration device certificate");
        return None;
    }
    let mut workspace_device_certs = std::collections::HashMap::new();
    let mut workspace_revocations = std::collections::HashMap::new();
    if let Ok(store) = crate::identity_registry::IdentityRegistryStore::from_env()
        && let Ok(registry) = store.migrate_legacy_primary()
    {
        for (room, identity_id) in &registry.workspace_identities {
            let Some(profile) = registry.identities.get(identity_id) else {
                continue;
            };
            let Some(room_id) = RoomId::parse(room) else {
                continue;
            };
            let cert_path = paths.device_cert_for_user(&profile.user_public_key);
            let Ok(cert) = qos_user_identity::persist::read_device_cert(&cert_path) else {
                tracing::warn!(
                    room = %room_id,
                    identity = %identity_id,
                    "workspace identity has no device certificate"
                );
                continue;
            };
            if qos_user_identity::verify_device_cert(&cert).is_ok()
                && cert.user_public_key == profile.user_public_key
                && cert.device_public_key == *keypair.public().as_bytes()
                && !crate::device_fleet::DeviceFleetStore::from_env()
                    .and_then(|store| store.load(identity_id))
                    .is_ok_and(|fleet| fleet.is_revoked(&cert.device_public_key))
            {
                workspace_device_certs.insert(room_topic_id(room_id), cert);
                if let Ok(fleet) = crate::device_fleet::DeviceFleetStore::from_env()
                    .and_then(|store| store.load(identity_id))
                {
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
) -> Result<Vec<qos_user_identity::RevocationEntry>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let entries: Vec<qos_user_identity::RevocationEntry> =
        postcard::from_bytes(&bytes).map_err(|error| error.to_string())?;
    if entries.len() > qos_user_identity::MAX_TRUSTED_IDENTITY_PROJECTION_DEVICES
        || entries.iter().any(|entry| {
            entry.user_public_key != user_public_key
                || qos_user_identity::verify_revocation_entry(entry).is_err()
        })
    {
        return Err("revocation entries are invalid, cross-root, or over capacity".into());
    }
    Ok(entries)
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
        joined_topics: Mutex::new(joined_topics),
        runtime: None,
        local_peer_hash,
        keypair,
        host_identity: None,
        local_peer_id,
        listen_addrs: Arc::new(Mutex::new(Vec::new())),
        host_addrs: Mutex::new(Vec::new()),
        last_home_peer_refresh: Mutex::new(None),
        last_sync_kick: Mutex::new(None),
        personal_continuity: Mutex::new(
            crate::shell::personal_continuity::PersonalContinuity::default(),
        ),
        personal_continuity_user: None,
        personal_continuity_device: None,
        personal_continuity_counter: Mutex::new(0),
        personal_continuity_fingerprint: Mutex::new(None),
        personal_continuity_path: None,
        peer_identities: PeerIdentityStore::new(),
        require_peer_identity: false,
        pending_identity_admissions: Mutex::new(std::collections::HashMap::new()),
        pending_invitations_path: None,
        pending_redemptions: Mutex::new(std::collections::HashMap::new()),
        inbound_presence: Arc::new(Mutex::new(Vec::new())),
        inbound_documents: Arc::new(Mutex::new(Vec::new())),
        inbound_app_presence: Arc::new(Mutex::new(Vec::new())),
        inbound_scrollback_request: Arc::new(Mutex::new(Vec::new())),
        inbound_scrollback_response: Arc::new(Mutex::new(Vec::new())),
    };
    (net, cmd_rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn personal_continuity_network_for_test(
        user_public_key: [u8; 32],
    ) -> (ShellNetwork, mpsc::Receiver<NetCmd>) {
        let (mut net, commands) =
            shell_network_for_test(WorkspaceTree::default().shared(), 0xC011_AB);
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

    fn authenticated_personal_peer(
        net: &ShellNetwork,
        root: &ed25519_dalek::SigningKey,
        device: &iroh::SecretKey,
    ) {
        use qos_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

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
        let origin = crate::shell::state::PeerOrigin::peer(&device.public());
        net.accept_identity_hello(
            personal_continuity_topic_id(&cert.user_public_key),
            &origin,
            &device.public(),
            &cert,
            &[],
        );
    }

    #[test]
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
        persist_personal_continuity(
            &path,
            &PersistedPersonalContinuity {
                counter: 41,
                continuity,
            },
        );
        let restored = load_personal_continuity(&path);
        assert_eq!(restored.counter, 41);
        assert_eq!(restored.continuity.snapshot_for(&device), Some(&snapshot));
    }

    #[test]
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

    #[test]
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

    #[test]
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
            NeighbourMsg::PersonalContinuityCheckpoint {
                user_public_key,
                publisher: crate::network::behaviour::peer_liveness_handle(&peer_device.public()),
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

    #[test]
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
            NeighbourMsg::PersonalContinuityCheckpoint {
                user_public_key,
                publisher: crate::network::behaviour::peer_liveness_handle(&peer_device.public()),
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
            tree.active_workspace().map(|workspace| workspace.id),
            active_before,
            "receiving a continuation must not move focus"
        );
        let adopted = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.room == room)
            .expect("continuation room is available for later user-directed restore");
        assert!(
            adopted.strips.is_empty(),
            "receiving a continuation must not recreate applications"
        );
    }

    #[test]
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

    #[test]
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
            crate::shell::state::Verb::Write,
        );
        assert_eq!(
            ShellNetwork::required_room_verb(&NeighbourMsg::PermissionRequest {
                peer: 42,
                capability: "remote-input".into(),
            }),
            crate::shell::state::Verb::Read,
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

    #[test]
    #[serial_test::serial(identity_env)]
    fn exact_invite_redeems_as_editor_and_revocation_removes_trust() {
        use qos_user_identity::{
            DeviceCapabilities, DeviceCert, RevocationEntry, RevocationReason, sign_device_cert,
            sign_revocation_entry,
        };

        let root = ed25519_dalek::SigningKey::from_bytes(&[61; 32]);
        let device = iroh::SecretKey::from_bytes(&[62; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: *device.public().as_bytes(),
            device_label: "Guest laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).expect("certificate");

        let room = RoomId::new();
        let mut workspace = crate::shell::state::Workspace::test_default(
            crate::shell::state::WorkspaceId(17),
            "Shared",
        );
        workspace.room = room;
        workspace.set_owner(crate::shell::state::local_user_id_for_room(&room));
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
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
                collab_types::crypto::Ed25519PublicKey::from_bytes(attacker_cert.user_public_key),
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
                msg: NeighbourMsg::SyncReq {
                    since: Vec::new(),
                    room: room.to_string(),
                    invite_id: Some([99; 32]),
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
                msg: NeighbourMsg::SyncReq {
                    since: Vec::new(),
                    room: room.to_string(),
                    invite_id: Some([1; 32]),
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
                .workspaces
                .iter()
                .find(|workspace| workspace.room == room)
                .expect("guest workspace")
                .role_of(&person),
            Some(crate::shell::state::CapRole::Editor),
            "the membership grant must be present in the replicated room document"
        );

        let route = crate::shell::collab_broker::CrdtRoute::record(
            "dev.qos.notes",
            room.to_string(),
            "meeting-notes",
        );
        let signing = ed25519_dalek::SigningKey::from_bytes(&device.to_bytes());
        let mut envelope = collab_types::SignedOp::new_unsigned(
            collab_types::DocumentMutation::Incremental(b"change".to_vec())
                .encode()
                .expect("typed mutation"),
            collab_types::document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id),
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
        sign_revocation_entry(&mut revoked, &root).expect("revocation");
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

    #[test]
    #[serial_test::serial(identity_env)]
    fn signed_viewer_invite_admits_a_viewer_not_an_editor() {
        use qos_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

        let root = ed25519_dalek::SigningKey::from_bytes(&[81; 32]);
        let device = iroh::SecretKey::from_bytes(&[82; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: *device.public().as_bytes(),
            device_label: "Viewer laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).expect("certificate");

        let room = RoomId::new();
        let mut workspace = crate::shell::state::Workspace::test_default(
            crate::shell::state::WorkspaceId(81),
            "Read only",
        );
        workspace.room = room;
        workspace.set_owner(crate::shell::state::local_user_id_for_room(&room));
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
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

    #[test]
    #[serial_test::serial(identity_env)]
    fn only_the_invite_pinned_authority_can_bootstrap_room_governance() {
        use qos_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

        let root = ed25519_dalek::SigningKey::from_bytes(&[91; 32]);
        let device = iroh::SecretKey::from_bytes(&[92; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: *device.public().as_bytes(),
            device_label: "Inviting device".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).expect("certificate");

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

        let stranger = crate::shell::state::PeerOrigin::peer(&iroh::SecretKey::generate().public());
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

    #[test]
    fn shell_network_spawns_with_default_keypair() {
        let tmp = tempfile::tempdir().unwrap();
        let key_path = tmp.path().join("peer-key");
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let net = ShellNetwork::spawn(tree, &key_path).expect("spawn");
        // `cmd_tx` capacity is the actor's bounded-channel size; just
        // assert it's nonzero so we know the channel was constructed.
        assert!(net.cmd_tx.capacity() > 0);
        // Persistence side-effect: the keypair must have been written.
        assert!(key_path.exists(), "keypair file must exist after spawn");
    }

    #[test]
    #[serial_test::serial(process_env)]
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
                    "device_node_id": hex::encode(device.as_bytes()),
                    "relay_service_node_id": hex::encode(service.as_bytes()),
                    "label": "Home server",
                    "user_public_key": hex::encode(own_user),
                    "relay_workspace_ids": [42],
                },
                {
                    "device_node_id": hex::encode(device.as_bytes()),
                    "relay_service_node_id": hex::encode(other_service.as_bytes()),
                    "label": "Other identity",
                    "user_public_key": hex::encode([0x99; 32]),
                    "relay_workspace_ids": [42],
                },
                {
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
        let previous = std::env::var_os("QOS_HOME_PEERS_PATH");
        // SAFETY: guarded by the process_env serial group.
        unsafe { std::env::set_var("QOS_HOME_PEERS_PATH", &registry) };
        let rooms = std::collections::BTreeSet::from([42]);
        let first_boot = registered_home_peer_addrs(Some(own_user), &rooms);
        let restarted = registered_home_peer_addrs(Some(own_user), &rooms);
        assert_eq!(first_boot, restarted, "registry is durable across restart");
        assert_eq!(first_boot.len(), 1);
        assert_eq!(first_boot[0].addr.id, service);

        // SAFETY: guarded by the process_env serial group.
        unsafe {
            match previous {
                Some(value) => std::env::set_var("QOS_HOME_PEERS_PATH", value),
                None => std::env::remove_var("QOS_HOME_PEERS_PATH"),
            }
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn home_peer_registry_defaults_to_the_active_identity_root() {
        let temp = tempfile::tempdir().expect("temp");
        let identity_root = temp.path().join("user-data/qos");
        let old_identity_root = std::env::var_os("QOS_IDENTITY_ROOT");
        let old_registry = std::env::var_os("QOS_HOME_PEERS_PATH");
        // SAFETY: guarded by the process_env serial group.
        unsafe {
            std::env::set_var("QOS_IDENTITY_ROOT", &identity_root);
            std::env::remove_var("QOS_HOME_PEERS_PATH");
        }
        assert_eq!(
            home_peer_registry_path(),
            identity_root.join("home-peers.json"),
            "the shell must read the approval registry beside its own user key, never a system copy"
        );
        // SAFETY: guarded by the process_env serial group.
        unsafe {
            match old_identity_root {
                Some(value) => std::env::set_var("QOS_IDENTITY_ROOT", value),
                None => std::env::remove_var("QOS_IDENTITY_ROOT"),
            }
            match old_registry {
                Some(value) => std::env::set_var("QOS_HOME_PEERS_PATH", value),
                None => std::env::remove_var("QOS_HOME_PEERS_PATH"),
            }
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
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
        let old_registry = std::env::var_os("QOS_HOME_PEERS_PATH");
        // SAFETY: guarded by the process_env serial group.
        unsafe { std::env::set_var("QOS_HOME_PEERS_PATH", &registry) };
        std::fs::write(
            &registry,
            serde_json::to_vec(&serde_json::json!([{
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

        // SAFETY: guarded by the process_env serial group.
        unsafe {
            match old_registry {
                Some(value) => std::env::set_var("QOS_HOME_PEERS_PATH", value),
                None => std::env::remove_var("QOS_HOME_PEERS_PATH"),
            }
        }
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
            t.adopt_roomless_workspaces();
            // `bootstrap_demo` leaves the owner `None`; `may_administer`
            // reads it, so an unset owner would refuse every name below
            // for the wrong reason.
            let me = crate::shell::state::local_user_id();
            for ws in &mut t.workspaces {
                ws.set_owner(me.clone());
            }
            t.workspaces.iter().map(|w| w.room).collect()
        };
        assert!(
            rooms.len() >= 2 && rooms.iter().all(|r| !r.is_nil()),
            "the fixture governs at least two rooms: {rooms:?}"
        );
        let net = ShellNetwork::spawn(tree.clone(), &tmp.path().join("peer-key")).expect("spawn");
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
    fn concurrent_invites_redeem_their_own_names_and_roles() {
        use qos_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

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
            .workspaces
            .iter()
            .find(|workspace| workspace.room == room)
            .cloned()
            .expect("workspace");
        let person = |cert: &DeviceCert| {
            let collab_types::Principal::Person(person) = collab_types::Principal::person_from_key(
                collab_types::Ed25519PublicKey::from_bytes(cert.user_public_key),
            ) else {
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

    #[test]
    fn tick_with_no_events_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let key_path = tmp.path().join("peer-key");
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let net = ShellNetwork::spawn(tree, &key_path).expect("spawn");
        net.tick();
    }

    #[test]
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

        assert!(matches!(
            cmd_rx.try_recv(),
            Ok(NetCmd::PublishCheckpoint {
                ws,
                msg: NeighbourMsg::SyncResp { ref room, .. },
            }) if ws == topic && room == &room_id.to_string()
        ));
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

    /// Build the inbound event a LAN peer's signed document publish arrives as.
    fn app_document_event(
        cluster_id: &str,
        from_peer_hash: u64,
        peer: iroh::PublicKey,
        device: &ed25519_dalek::SigningKey,
    ) -> NetEvent {
        let app_id = "dev.qos.notes";
        let record_id = "note-1";
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
            msg: NeighbourMsg::AppDocument {
                peer: from_peer_hash,
                app_id: app_id.to_string(),
                cluster_id: cluster_id.to_string(),
                record_id: record_id.to_string(),
                envelope: Box::new(envelope),
            },
        }
    }

    /// A peer's app document reaches local apps only for a room this
    /// machine is in. Driven through the real receive path (`tick`),
    /// not the helper directly, with a positive control so the two
    /// refusals prove something rather than just "nothing arrives".
    #[test]
    fn app_document_from_a_room_we_are_not_in_never_reaches_the_broker() {
        let mut tree = WorkspaceTree::default();
        let local = tree.append_empty_workspace("Mine");
        let our_room = tree.room_of(local).expect("a minted room");
        tree.workspaces
            .iter_mut()
            .find(|workspace| workspace.id == local)
            .expect("workspace")
            .set_owner(crate::shell::state::local_user_id_for_room(&our_room));
        let handle = tree.shared();
        let (net, _cmd_rx) = shell_network_for_test(handle, 0xAAAAu64);

        // Documents are accepted only after the sender has proven an
        // identity-to-device chain and received membership.
        let root = ed25519_dalek::SigningKey::from_bytes(&[71; 32]);
        let device = iroh::SecretKey::from_bytes(&[72; 32]);
        let device_signing = ed25519_dalek::SigningKey::from_bytes(&[72; 32]);
        let mut cert = qos_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: *device.public().as_bytes(),
            device_label: "Peer laptop".into(),
            capabilities: qos_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        qos_user_identity::sign_device_cert(&mut cert, &root).expect("device certificate");
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
                msg: NeighbourMsg::IdentityHello {
                    cert: Box::new(cert.clone()),
                    revocations: Vec::new(),
                },
            })
            .expect("queue identity proof");
        net.tick();
        net.redeem_invited_identity(room_topic_id(our_room), [3; 32], &device.public(), &cert);

        let stranger_room = crate::shell::state::RoomId::new();
        assert_ne!(stranger_room, our_room);

        net.event_tx
            .try_send(app_document_event(
                &stranger_room.to_string(),
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

    #[test]
    fn accepting_an_invite_joins_the_room_and_asks_it_for_a_catch_up() {
        use crate::network::invite::make_invite_token;
        use crate::shell::state::Workspace;

        let host_ws = Workspace::test_default(
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

    #[test]
    fn accepting_a_pinned_home_host_invite_queues_exact_expectation_once() {
        use crate::network::invite::{HomeHostPin, InviteRole, make_invite_token_with_recipient};
        use qos_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

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

        let host_workspace = crate::shell::state::Workspace::test_default(
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
            InviteRole::Editor,
            None,
            None,
            3600,
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

    #[test]
    fn a_guest_cannot_mint_an_apparently_authoritative_room_invite() {
        let mut tree = WorkspaceTree::default();
        let local = tree.append_empty_workspace("Joined room");
        let room = tree.room_of(local).expect("room");
        tree.workspaces
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
        assert!(error.contains("governing workspace administrator"));
    }

    /// A token that does not verify joins nothing and asks nothing.
    #[test]
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

    #[test]
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
                    } => {
                        assert_eq!(peer, 0xABCDu64);
                        assert_eq!(user, crate::shell::state::local_user_id());
                        assert!(!user.is_empty());
                        assert_eq!(key, "local-cursor");
                        assert_eq!(point, (100.0, 200.0));
                    }
                    other => panic!("expected PresenceMoved, got {other:?}"),
                }
            }
            other => panic!("expected NetCmd::Publish, got {other:?}"),
        }
    }

    #[test]
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

    #[test]
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
    fn publish_document_emits_a_publish_command() {
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, mut cmd_rx) = shell_network_for_test(tree, 0xFEED_u64);
        let expected_topic = net.active_ws_for_publish();
        let envelope = make_envelope(7);
        net.publish_document(
            "notes".to_string(),
            "cluster-7".to_string(),
            "uuid:9f".to_string(),
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

    #[test]
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

    #[test]
    fn active_ws_for_publish_returns_the_active_rooms_stable_topic() {
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let expected = room_topic_id(tree.read().active_workspace().unwrap().room);
        let (net, _cmd_rx) = shell_network_for_test(tree, 0u64);
        assert_eq!(net.active_ws_for_publish(), expected);
    }

    #[test]
    fn publish_scrollback_request_emits_a_publish_command() {
        use collab_types::ArtifactId;
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, mut cmd_rx) = shell_network_for_test(tree, 0xDEAD_u64);
        let expected_topic = net.active_ws_for_publish();
        let artifact = ArtifactId::new();
        net.publish_scrollback_request(artifact, 100, 25);
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

    #[test]
    fn handle_scrollback_request_message_buffers_for_drain() {
        use collab_types::ArtifactId;
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, _cmd_rx) = shell_network_for_test(tree, 0u64);
        let handle = net.inbound_scrollback_request_handle();
        let artifact = ArtifactId::new();
        net.handle_scrollback_request_message(7u64, artifact, 50, 10);
        let buf = handle.lock().unwrap().clone();
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].requester, 7u64);
        assert_eq!(buf[0].artifact, artifact);
        assert_eq!(buf[0].before_seq, 50);
        assert_eq!(buf[0].max, 10);
    }

    #[test]
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

    #[test]
    #[serial_test::serial(process_env)]
    fn network_announcement_governs_and_republishes_files_projection() {
        let runtime = tempfile::tempdir().unwrap();
        let previous_runtime = std::env::var_os("XDG_RUNTIME_DIR");
        // SAFETY: guarded by the process_env serial group shared with launcher tests.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", runtime.path()) };

        let mut state = WorkspaceTree::default();
        let workspace = state.append_empty_workspace("Identity workflow");
        let room = state.room_of(workspace).unwrap();
        state.adopt_ownerless_workspaces(&crate::shell::state::local_user_id());
        let tree = state.shared();
        let (network, _commands) = shell_network_for_test(tree.clone(), 0x1D);

        let device_secret = iroh::SecretKey::generate();
        let endpoint = device_secret.public();
        let root = ed25519_dalek::SigningKey::from_bytes(&[104; 32]);
        let mut certificate = qos_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: *endpoint.as_bytes(),
            device_label: "Ada's laptop".into(),
            capabilities: qos_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        qos_user_identity::sign_device_cert(&mut certificate, &root).unwrap();

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
            .join("qos/identity-projections")
            .join(room.to_string())
            .join("trusted-identities.json");
        let projected: qos_user_identity::TrustedIdentityProjectionV2 =
            serde_json::from_slice(&std::fs::read(&projection_path).unwrap()).unwrap();
        assert_eq!(projected.humans.len(), 1);
        assert_eq!(projected.humans[0].display_name, "Ada Lovelace");
        assert_eq!(projected.humans[0].devices[0].certificate, certificate);

        let impostor = iroh::SecretKey::generate().public();
        assert!(
            !network.handle_identity_announcement(&impostor, &room.to_string(), &certificate, &[]),
            "the signed certificate cannot be rebound to another transport endpoint"
        );

        let mut revocation = qos_user_identity::RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: *endpoint.as_bytes(),
            revoked_at_ns: 2,
            reason: qos_user_identity::RevocationReason::Retired,
            signature: [0; 64],
        };
        qos_user_identity::sign_revocation_entry(&mut revocation, &root).unwrap();
        assert!(network.handle_identity_announcement(
            &endpoint,
            &room.to_string(),
            &certificate,
            &[revocation]
        ));
        let projected: qos_user_identity::TrustedIdentityProjectionV2 =
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

        // SAFETY: restore the process-global environment under the same lock.
        unsafe {
            match previous_runtime {
                Some(value) => std::env::set_var("XDG_RUNTIME_DIR", value),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
    }

    #[test]
    fn revocation_source_is_reloaded_and_cross_root_rows_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("revocations.bin");
        let root = ed25519_dalek::SigningKey::from_bytes(&[101; 32]);
        assert!(
            load_identity_revocations(&path, root.verifying_key().to_bytes())
                .unwrap()
                .is_empty()
        );
        let mut entry = qos_user_identity::RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: [102; 32],
            revoked_at_ns: 5,
            reason: qos_user_identity::RevocationReason::Compromised,
            signature: [0; 64],
        };
        qos_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
        std::fs::write(&path, postcard::to_allocvec(&vec![entry.clone()]).unwrap()).unwrap();
        assert_eq!(
            load_identity_revocations(&path, root.verifying_key().to_bytes()).unwrap(),
            vec![entry]
        );
        let other_root = ed25519_dalek::SigningKey::from_bytes(&[103; 32]);
        assert!(load_identity_revocations(&path, other_root.verifying_key().to_bytes()).is_err());
    }
}
